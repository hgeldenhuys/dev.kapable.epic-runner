use clap::Args;
use owo_colors::OwoColorize;
use serde_json::json;
use uuid::Uuid;

use super::CliConfig;
use crate::api_client::{ApiClient, DataWrapper};
use crate::flow::loader;
use crate::flow::patcher;
use crate::scrum_master::{self, NodeOutcome, SprintHistory};
use crate::types::*;

#[derive(Args)]
pub struct OrchestrateArgs {
    /// Epic code to orchestrate (e.g. AUTH-001)
    pub epic_code: String,

    /// Maximum number of sprints
    #[arg(long, default_value = "20")]
    pub max_sprints: i32,

    /// Model override for all ceremonies
    #[arg(long, default_value = "opus")]
    pub model: String,

    /// Effort override
    #[arg(long, default_value = "max")]
    pub effort: String,

    /// Additional directories to add
    #[arg(long)]
    pub add_dir: Vec<String>,

    /// Flow definition file (YAML) — overrides embedded default
    #[arg(long)]
    pub flow: Option<String>,

    /// Override budget (USD) for all ceremony nodes
    #[arg(long)]
    pub budget_override: Option<f64>,

    /// Dry run — plan sprints without executing
    #[arg(long, default_value = "false")]
    pub dry_run: bool,
}

pub async fn run(
    args: OrchestrateArgs,
    client: &ApiClient,
    _cli: &CliConfig,
) -> Result<(), Box<dyn std::error::Error>> {
    // 0. PRE-FLIGHT
    let claude_check = std::process::Command::new("claude")
        .arg("--version")
        .output();
    if claude_check.is_err() {
        return Err("claude CLI not found in PATH".into());
    }

    // Lock file
    let lock_path = format!(".epic-runner/{}.lock", args.epic_code);
    if std::path::Path::new(&lock_path).exists() {
        let pid = std::fs::read_to_string(&lock_path).unwrap_or_default();
        return Err(format!(
            "Epic {} already running (PID {})",
            args.epic_code,
            pid.trim()
        )
        .into());
    }
    std::fs::create_dir_all(".epic-runner").ok();
    std::fs::write(&lock_path, std::process::id().to_string())?;
    let lock_path_clone = lock_path.clone();
    let _guard = scopeguard::guard((), move |_| {
        std::fs::remove_file(&lock_path_clone).ok();
    });

    // 1. Look up epic (client-side filter — JSONB tables ignore query params)
    let all_epics: DataWrapper<Vec<serde_json::Value>> = client.get("/v1/epics").await?;
    let epic_data = all_epics
        .data
        .iter()
        .find(|e| e["code"].as_str() == Some(args.epic_code.as_str()))
        .ok_or(format!("Epic {} not found", args.epic_code))?;
    let epic: Epic = serde_json::from_value(epic_data.clone())?;

    if epic.status != EpicStatus::Active {
        return Err(format!("Epic {} is {}, not active", epic.code, epic.status).into());
    }

    // 2. Check impediments
    let blocking = crate::impediments::check_blocking_impediments(client, &epic.code).await?;
    if !blocking.is_empty() {
        return Err(format!(
            "Epic {} has {} open impediment(s)",
            epic.code,
            blocking.len()
        )
        .into());
    }

    eprintln!(
        "{} {} {}: {}",
        "Epic Runner —".bold(),
        "Orchestrate".cyan().bold(),
        epic.code.yellow().bold(),
        epic.title
    );
    eprintln!("Intent: {}", epic.intent.dimmed());

    // Clean up stale sprints from previous crashed runs
    cleanup_stale_sprints(client, &epic.id.to_string()).await;
    eprintln!("Max sprints: {}", args.max_sprints);

    if args.dry_run {
        eprintln!(
            "[DRY RUN] Would execute with up to {} sprints",
            args.max_sprints
        );
        return Ok(());
    }

    // 3. Sprint loop
    for sprint_num in 1..=args.max_sprints {
        eprintln!(
            "\n{}",
            format!("═══ SPRINT {sprint_num} of epic {} ═══", epic.code)
                .cyan()
                .bold()
        );

        // Worktree health check (committee invariant #5)
        let worktree_path = format!(".claude/worktrees/{}", epic.code);
        if std::path::Path::new(&worktree_path).exists() {
            let wt_status = std::process::Command::new("git")
                .args(["status", "--porcelain"])
                .current_dir(&worktree_path)
                .output();
            if let Ok(output) = wt_status {
                let dirty = String::from_utf8_lossy(&output.stdout);
                if !dirty.trim().is_empty() {
                    tracing::warn!(path = %worktree_path, "Worktree is dirty — stashing before sprint");
                    std::process::Command::new("git")
                        .args(["stash", "--include-untracked"])
                        .current_dir(&worktree_path)
                        .output()
                        .ok();
                }
            }

            // Rebase worktree to latest main (WoW: Sprint Discipline — process improvement 2026-03-11)
            // This prevents stale worktrees from redoing work already merged to main.
            rebase_worktree_to_main(&worktree_path);
        }

        // Create sprint in DB
        let session_id = Uuid::new_v4();
        let sprint_body = json!({
            "epic_id": epic.id.to_string(),
            "number": sprint_num,
            "session_id": session_id.to_string(),
            "status": "planning",
        });
        let sprint_resp: serde_json::Value = client.post("/v1/er_sprints", &sprint_body).await?;
        let sprint_id = sprint_resp["id"].as_str().ok_or("Sprint creation failed")?;

        // Load and assign stories (client-side filter for eligible stories in this epic)
        // Priority: ready > planned > draft (most refined first)
        let all_stories: DataWrapper<Vec<serde_json::Value>> = client.get("/v1/stories").await?;
        let mut eligible_stories: Vec<&serde_json::Value> = all_stories
            .data
            .iter()
            .filter(|s| {
                s["epic_code"].as_str() == Some(epic.code.as_str())
                    && matches!(s["status"].as_str(), Some("ready" | "planned" | "draft"))
            })
            .collect();
        eligible_stories.sort_by_key(|s| match s["status"].as_str() {
            Some("ready") => 0,
            Some("planned") => 1,
            Some("draft") => 2,
            _ => 3,
        });

        if eligible_stories.is_empty() {
            if sprint_num > 1 {
                eprintln!("No ready stories remaining — epic complete.");
            } else {
                eprintln!("No eligible stories found for epic {}.", epic.code);
            }
            break;
        }

        let batch: Vec<&serde_json::Value> = eligible_stories.into_iter().take(5).collect();
        let story_ids: Vec<String> = batch
            .iter()
            .filter_map(|s| s["id"].as_str().map(String::from))
            .collect();

        if let Err(e) = client
            .patch::<_, serde_json::Value>(
                &format!("/v1/er_sprints/{sprint_id}"),
                &json!({ "stories": serde_json::to_value(&batch)? }),
            )
            .await
        {
            tracing::warn!(error = %e, "Failed to attach stories to sprint — continuing");
        }

        // Transition stories to planned
        for sid in &story_ids {
            let _: Result<serde_json::Value, _> = client
                .patch(
                    &format!("/v1/stories/{sid}"),
                    &json!({ "status": "planned" }),
                )
                .await;
        }

        // SPAWN SPRINT RUNNER AS CHILD PROCESS
        tracing::info!(sprint_id, sprint_num, "Spawning sprint-run child process");
        let mut cmd = std::process::Command::new(std::env::current_exe()?);
        cmd.arg("sprint-run").arg(sprint_id);
        cmd.arg("--model").arg(&args.model);
        cmd.arg("--effort").arg(&args.effort);
        for dir in &args.add_dir {
            cmd.arg("--add-dir").arg(dir);
        }
        if let Some(flow) = &args.flow {
            cmd.arg("--flow").arg(flow);
        }
        if let Some(budget) = args.budget_override {
            cmd.arg("--budget-override").arg(budget.to_string());
        }

        let exit_status = cmd.status()?;
        let exit_code = exit_status.code().unwrap_or(-1);

        tracing::info!(exit_code, "Sprint-run process exited");

        match exit_code {
            0 => {
                // Intent satisfied — close epic
                eprintln!(
                    "{} — closing epic {}",
                    "Intent satisfied".green().bold(),
                    epic.code
                );
                if let Err(e) = client
                    .patch::<_, serde_json::Value>(
                        &format!("/v1/epics/{}", epic.id),
                        &json!({ "status": "closed", "closed_at": chrono::Utc::now().to_rfc3339() }),
                    )
                    .await
                {
                    tracing::error!(error = %e, "Failed to close epic in DB");
                }
                break;
            }
            1 => {
                // Failed but not blocked — re-groom and try next sprint
                eprintln!(
                    "{}",
                    "Sprint failed. Re-grooming stories for next sprint...".yellow()
                );

                // 1. Reset failed stories back to ready so they're eligible for next sprint
                let reset_count = reset_failed_stories(client, &epic.code, sprint_num).await;
                if reset_count > 0 {
                    eprintln!(
                        "{} Reset {} stories back to ready",
                        "[re-groom]".dimmed(),
                        reset_count,
                    );
                }

                // 2. Re-groom: use retro learnings to split/adjust stories for next sprint
                regroom_stories(client, &epic, sprint_num).await;

                // 3. SM inter-sprint adaptation: analyze ceremony history, patch flow for next sprint
                adapt_ceremony_flow(client, &epic.code, sprint_num).await;
            }
            2 => {
                // Blocked — impediment raised
                eprintln!(
                    "Epic {} is {} by impediment",
                    epic.code,
                    "BLOCKED".red().bold()
                );
                if let Err(e) = client
                    .patch::<_, serde_json::Value>(
                        &format!("/v1/epics/{}", epic.id),
                        &json!({ "status": "blocked" }),
                    )
                    .await
                {
                    tracing::error!(error = %e, "Failed to mark epic as blocked in DB");
                }
                break;
            }
            _ => {
                // Unexpected exit (crash, context exhaustion, SIGKILL)
                tracing::warn!(
                    exit_code,
                    "Sprint process died unexpectedly — marking sprint failed, continuing"
                );
                // Mark this sprint as failed so it doesn't stay stuck in executing
                if let Err(e) = client
                    .patch::<_, serde_json::Value>(
                        &format!("/v1/er_sprints/{sprint_id}"),
                        &json!({
                            "status": "failed",
                            "finished_at": chrono::Utc::now().to_rfc3339(),
                        }),
                    )
                    .await
                {
                    tracing::error!(error = %e, "Failed to mark crashed sprint as failed");
                }
            }
        }
    }

    eprintln!("\nEpic runner finished for {}", epic.code);
    Ok(())
}

/// Reset stories that were consumed by a failed sprint back to "ready" so the next sprint
/// can pick them up. Stories that have been attempted 3+ times get marked "blocked" instead.
async fn reset_failed_stories(client: &ApiClient, epic_code: &str, _sprint_num: i32) -> usize {
    let all_stories: Result<DataWrapper<Vec<serde_json::Value>>, _> =
        client.get("/v1/stories").await;
    let stories = match all_stories {
        Ok(d) => d.data,
        Err(e) => {
            tracing::warn!(error = %e, "Failed to load stories for reset");
            return 0;
        }
    };

    let mut reset_count = 0;
    for story in &stories {
        if story["epic_code"].as_str() != Some(epic_code) {
            continue;
        }
        let status = story["status"].as_str().unwrap_or("");
        // Reset in_progress and planned stories (they didn't complete)
        if status != "in_progress" && status != "planned" {
            continue;
        }
        let story_id = match story["id"].as_str() {
            Some(id) => id,
            None => continue,
        };

        // Check attempt count — if tried 3+ times, mark as blocked
        let attempt_count = story["attempt_count"].as_i64().unwrap_or(0) + 1;
        let (new_status, new_count) = if attempt_count >= 3 {
            ("blocked", attempt_count)
        } else {
            ("ready", attempt_count)
        };

        if let Err(e) = client
            .patch::<_, serde_json::Value>(
                &format!("/v1/stories/{story_id}"),
                &json!({
                    "status": new_status,
                    "attempt_count": new_count,
                }),
            )
            .await
        {
            tracing::warn!(error = %e, story_id, "Failed to reset story status");
        } else {
            if new_status == "blocked" {
                eprintln!(
                    "{} Story {} auto-blocked after {} failed attempts",
                    "[re-groom]".dimmed(),
                    story["code"].as_str().unwrap_or(story_id).yellow(),
                    attempt_count,
                );
            }
            reset_count += 1;
        }
    }

    reset_count
}

/// Re-groom stories using retro learnings from the failed sprint.
/// Calls Claude headless with Sonnet to analyze what went wrong and adjust stories:
/// - Split overly large stories
/// - Add missing prerequisite stories
/// - Adjust ACs based on friction points discovered during execution
async fn regroom_stories(client: &ApiClient, epic: &Epic, sprint_num: i32) {
    // Load retro from the last sprint
    let retro = load_retro_for_sprint(client, &epic.code, sprint_num).await;
    let retro_context = match retro {
        Some(r) => {
            let mut parts = Vec::new();
            if !r.friction_points.is_empty() {
                parts.push(format!("Friction points: {}", r.friction_points.join("; ")));
            }
            if !r.action_items.is_empty() {
                parts.push(format!("Action items: {}", r.action_items.join("; ")));
            }
            if parts.is_empty() {
                tracing::debug!("Retro has no friction points or action items — skipping re-groom");
                return;
            }
            parts.join("\n")
        }
        None => {
            tracing::debug!(
                "No retro available for sprint {} — skipping re-groom",
                sprint_num
            );
            return;
        }
    };

    // Load current ready stories for context
    let all_stories: DataWrapper<Vec<serde_json::Value>> = match client.get("/v1/stories").await {
        Ok(d) => d,
        Err(_) => return,
    };
    let epic_stories: Vec<&serde_json::Value> = all_stories
        .data
        .iter()
        .filter(|s| {
            s["epic_code"].as_str() == Some(epic.code.as_str())
                && matches!(s["status"].as_str(), Some("ready" | "planned" | "draft"))
        })
        .collect();

    if epic_stories.is_empty() {
        tracing::debug!("No eligible stories to re-groom — skipping");
        return;
    }

    let stories_json = serde_json::to_string_pretty(&epic_stories).unwrap_or_default();

    // Build re-groom prompt
    let prompt = format!(
        r#"You are re-grooming stories for epic {} after sprint {} failed.

Epic intent: {}

Sprint {} retro findings:
{}

Current stories (JSON):
{}

INSTRUCTIONS:
1. Analyze what went wrong based on the retro findings
2. If a story is too large (>5 points), split it into smaller stories
3. If a prerequisite is missing (e.g., env config, dependency), create a new story for it
4. Adjust acceptance criteria based on lessons learned
5. Output ONLY valid JSON array of story objects, each with:
   {{"id": "existing-id-or-null", "title": "...", "acceptance_criteria": ["AC1", "AC2"], "points": N, "status": "ready"}}
   - Use the existing ID for modified stories, null for new ones
   - Keep stories small (1-5 points)
   - Be specific about file paths and commands in ACs"#,
        epic.code, sprint_num, epic.intent, sprint_num, retro_context, stories_json,
    );

    // Spawn Claude headless (Sonnet, low budget — this is grooming, not execution)
    let session_id = uuid::Uuid::new_v4();
    let mut cmd = tokio::process::Command::new("claude");
    cmd.arg("--print")
        .arg("--output-format")
        .arg("text")
        .arg("--model")
        .arg("sonnet")
        .arg("--max-turns")
        .arg("3")
        .arg("--dangerously-skip-permissions")
        .arg("--session-id")
        .arg(session_id.to_string())
        .arg("--prompt")
        .arg(&prompt);

    // Limit tools — re-groom is analysis-only
    cmd.arg("--allowedTools")
        .arg("Read,Glob,Grep,Bash(git *),Bash(ls *)");

    eprintln!(
        "{} Re-grooming {} stories with retro context from sprint {}...",
        "[re-groom]".dimmed(),
        epic_stories.len(),
        sprint_num,
    );

    let output = match cmd.output().await {
        Ok(o) => o,
        Err(e) => {
            tracing::warn!(error = %e, "Re-groom Claude process failed — skipping");
            return;
        }
    };

    let stdout = String::from_utf8_lossy(&output.stdout);

    // Parse the JSON output — look for a JSON array in the response
    let json_start = stdout.find('[');
    let json_end = stdout.rfind(']');
    let regroom_result: Option<Vec<serde_json::Value>> = match (json_start, json_end) {
        (Some(start), Some(end)) if end > start => serde_json::from_str(&stdout[start..=end]).ok(),
        _ => None,
    };

    let stories = match regroom_result {
        Some(s) if !s.is_empty() => s,
        _ => {
            tracing::warn!("Re-groom produced no valid JSON output — skipping");
            return;
        }
    };

    // Apply re-groomed stories: update existing, create new
    let mut updated = 0;
    let mut created = 0;
    for story in &stories {
        let story_id = story["id"]
            .as_str()
            .filter(|s| !s.is_empty() && *s != "null");
        let title = story["title"].as_str().unwrap_or("Untitled");

        if let Some(id) = story_id {
            // Update existing story
            let patch = json!({
                "title": title,
                "acceptance_criteria": story["acceptance_criteria"],
                "points": story["points"],
                "status": "ready",
            });
            if client
                .patch::<_, serde_json::Value>(&format!("/v1/stories/{id}"), &patch)
                .await
                .is_ok()
            {
                updated += 1;
            }
        } else {
            // Create new story (prerequisite discovered by re-groom)
            let code =
                match super::backlog::next_story_code(client, &epic.product_id.to_string()).await {
                    Ok(c) => c,
                    Err(e) => {
                        tracing::warn!("Failed to generate story code: {e}");
                        continue;
                    }
                };
            let body = json!({
                "product_id": epic.product_id.to_string(),
                "epic_code": epic.code,
                "code": code,
                "title": title,
                "acceptance_criteria": story["acceptance_criteria"],
                "points": story["points"].as_i64().unwrap_or(2),
                "status": "ready",
                "attempt_count": 0,
            });
            if client
                .post::<_, serde_json::Value>("/v1/stories", &body)
                .await
                .is_ok()
            {
                created += 1;
            }
        }
    }

    if updated > 0 || created > 0 {
        eprintln!(
            "{} Re-groomed: {} updated, {} new stories created",
            "[re-groom]".dimmed(),
            updated.to_string().green(),
            created.to_string().cyan(),
        );
    }
}

/// Analyze ceremony results across sprints and patch the flow YAML for the next sprint.
/// This is the SM's inter-sprint adaptation: retro findings → flow patches → saved YAML.
async fn adapt_ceremony_flow(client: &ApiClient, epic_code: &str, current_sprint: i32) {
    // 1. Load sprint history from DB (ceremony_log has node results)
    let history = match load_sprint_history(client, epic_code).await {
        Some(h) if h.len() >= 2 => h, // Need at least 2 sprints to detect patterns
        _ => {
            tracing::debug!("Not enough sprint history for adaptation — skipping");
            return;
        }
    };

    // 2. Load the current ceremony flow (which may already be patched)
    let current_flow = match loader::load_flow(None, None, Some(epic_code)).await {
        Ok(f) => f,
        Err(e) => {
            tracing::warn!(error = %e, "Could not load current flow for adaptation — skipping");
            return;
        }
    };

    // 3. Ask SM to recommend patches
    let patches = scrum_master::recommend_flow_patches(&history, &current_flow);
    if patches.is_empty() {
        tracing::info!(
            epic_code,
            "SM recommends no flow patches — ceremony unchanged"
        );
        return;
    }

    eprintln!(
        "{} SM recommends {} flow patch(es) for next sprint:",
        "[adapt]".dimmed(),
        patches.len()
    );

    // 4. Apply patches
    let result = patcher::apply_patches(&current_flow, &patches);
    for desc in &result.applied {
        eprintln!("  {} {}", "✓".green(), desc);
    }
    for desc in &result.skipped {
        eprintln!("  {} {}", "⊘".yellow(), desc);
    }

    if result.applied.is_empty() {
        tracing::info!("All SM patches were skipped — flow unchanged");
        return;
    }

    // 5. Bump flow version to track adaptation
    let mut patched = result.flow;
    patched.version = format!("1.1.{}", current_sprint);
    patched.description = Some(format!(
        "Adapted after sprint {} based on SM retro findings. Patches: {}",
        current_sprint,
        result.applied.join("; ")
    ));

    // 6. Save patched flow for the next sprint
    if let Err(e) = loader::save_epic_flow(epic_code, &patched) {
        tracing::error!(error = %e, "Failed to save patched flow — next sprint will use unpatched flow");
        return;
    }

    // 7. Log the adaptation event to the sprint_learnings table (best-effort)
    let body = json!({
        "epic_code": epic_code,
        "sprint_number": current_sprint,
        "action_items": result.applied,
        "patterns_to_codify": ["SM adapted ceremony flow between sprints"],
        "friction_points": [],
        "saved_at": chrono::Utc::now().to_rfc3339(),
    });
    let _: Result<serde_json::Value, _> = client.post("/v1/sprint_learnings", &body).await;

    eprintln!(
        "{} Ceremony flow adapted — {} nodes in patched flow (v{})",
        "[adapt]".dimmed(),
        patched.nodes.len(),
        patched.version,
    );
}

/// Load sprint history from DB ceremony_log entries.
/// Returns None on any failure. Each sprint's ceremony_log is a JSON array of
/// `{key, status, cost_usd}` objects written by run_sprint.rs.
async fn load_sprint_history(client: &ApiClient, epic_code: &str) -> Option<Vec<SprintHistory>> {
    // Load all sprints for this epic
    let all_sprints: DataWrapper<Vec<serde_json::Value>> =
        client.get("/v1/er_sprints").await.ok()?;

    // Load all epics to find the epic_id for this code
    let all_epics: DataWrapper<Vec<serde_json::Value>> = client.get("/v1/epics").await.ok()?;
    let epic_id = all_epics
        .data
        .iter()
        .find(|e| e["code"].as_str() == Some(epic_code))?
        .get("id")?
        .as_str()?;

    // Filter sprints for this epic, sort by number
    let mut sprints: Vec<&serde_json::Value> = all_sprints
        .data
        .iter()
        .filter(|s| s["epic_id"].as_str() == Some(epic_id))
        .collect();
    sprints.sort_by_key(|s| s["number"].as_i64().unwrap_or(0));

    let mut history = Vec::new();
    for sprint in &sprints {
        let number = sprint["number"].as_i64().unwrap_or(0) as i32;
        let ceremony_log = sprint["ceremony_log"].as_array();

        let node_results: Vec<NodeOutcome> = match ceremony_log {
            Some(log) => log
                .iter()
                .map(|entry| NodeOutcome {
                    key: entry["key"].as_str().unwrap_or("").to_string(),
                    status: entry["status"].as_str().unwrap_or("").to_string(),
                    cost_usd: entry["cost_usd"].as_f64(),
                })
                .collect(),
            None => vec![],
        };

        // Try to load retro from sprint_learnings (best-effort)
        let retro = load_retro_for_sprint(client, epic_code, number).await;

        history.push(SprintHistory {
            sprint_number: number,
            node_results,
            retro,
        });
    }

    Some(history)
}

/// Rebase the epic worktree to the latest origin/main.
/// If rebase conflicts, aborts and continues on the current base.
/// Logs the base commit SHA for sprint context awareness.
fn rebase_worktree_to_main(worktree_path: &str) {
    // Fetch latest main from origin
    let fetch = std::process::Command::new("git")
        .args(["fetch", "origin", "main"])
        .current_dir(worktree_path)
        .output();

    if let Err(e) = fetch {
        tracing::warn!(error = %e, "Failed to fetch origin/main — skipping rebase");
        return;
    }
    let fetch_output = fetch.unwrap();
    if !fetch_output.status.success() {
        let stderr = String::from_utf8_lossy(&fetch_output.stderr);
        tracing::warn!(stderr = %stderr, "git fetch origin main failed — skipping rebase");
        return;
    }

    // Get current HEAD before rebase for logging
    let head_before = std::process::Command::new("git")
        .args(["rev-parse", "--short", "HEAD"])
        .current_dir(worktree_path)
        .output()
        .ok()
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .unwrap_or_default();

    // Get origin/main SHA for comparison
    let origin_main = std::process::Command::new("git")
        .args(["rev-parse", "--short", "origin/main"])
        .current_dir(worktree_path)
        .output()
        .ok()
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .unwrap_or_default();

    if head_before == origin_main {
        tracing::debug!(
            head = %head_before,
            "Worktree already at origin/main — no rebase needed"
        );
        return;
    }

    // Attempt rebase
    let rebase = std::process::Command::new("git")
        .args(["rebase", "origin/main"])
        .current_dir(worktree_path)
        .output();

    match rebase {
        Ok(output) if output.status.success() => {
            let head_after = std::process::Command::new("git")
                .args(["rev-parse", "--short", "HEAD"])
                .current_dir(worktree_path)
                .output()
                .ok()
                .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
                .unwrap_or_default();

            eprintln!(
                "{} Worktree rebased to origin/main ({} → {})",
                "[rebase]".dimmed(),
                head_before.dimmed(),
                head_after.green(),
            );
        }
        Ok(output) => {
            // Rebase failed (conflicts) — abort and continue on current base
            let stderr = String::from_utf8_lossy(&output.stderr);
            tracing::warn!(
                stderr = %stderr,
                "Rebase conflicted — aborting and continuing on current base ({})",
                head_before,
            );
            std::process::Command::new("git")
                .args(["rebase", "--abort"])
                .current_dir(worktree_path)
                .output()
                .ok();

            eprintln!(
                "{} Rebase conflicted — continuing on {} (origin/main is {})",
                "[rebase]".yellow(),
                head_before.dimmed(),
                origin_main.dimmed(),
            );
        }
        Err(e) => {
            tracing::warn!(error = %e, "git rebase command failed to execute");
        }
    }
}

/// Load retro output for a specific sprint from sprint_learnings table.
async fn load_retro_for_sprint(
    client: &ApiClient,
    epic_code: &str,
    sprint_number: i32,
) -> Option<scrum_master::RetroOutput> {
    let all: DataWrapper<Vec<serde_json::Value>> = client.get("/v1/sprint_learnings").await.ok()?;

    let learning = all.data.iter().find(|l| {
        l["epic_code"].as_str() == Some(epic_code)
            && l["sprint_number"].as_i64() == Some(sprint_number as i64)
    })?;

    // Reconstruct a minimal RetroOutput from the stored fields
    let friction_points: Vec<String> = learning["friction_points"]
        .as_array()
        .map(|arr| {
            arr.iter()
                .filter_map(|v| v.as_str().map(String::from))
                .collect()
        })
        .unwrap_or_default();

    let action_items: Vec<String> = learning["action_items"]
        .as_array()
        .map(|arr| {
            arr.iter()
                .filter_map(|v| v.as_str().map(String::from))
                .collect()
        })
        .unwrap_or_default();

    Some(scrum_master::RetroOutput {
        went_well: vec![],
        friction_points,
        action_items,
        discovered_work: vec![],
        observations: vec![],
    })
}

/// Clean up stale sprints from previous crashed orchestrator runs.
/// Any sprint in "executing" or "planning" status with no active process is marked "failed".
async fn cleanup_stale_sprints(client: &ApiClient, epic_id: &str) {
    let all_sprints: Result<DataWrapper<Vec<serde_json::Value>>, _> =
        client.get("/v1/er_sprints").await;
    let sprints = match all_sprints {
        Ok(d) => d.data,
        Err(_) => return,
    };

    let mut cleaned = 0;
    for sprint in &sprints {
        if sprint["epic_id"].as_str() != Some(epic_id) {
            continue;
        }
        let status = sprint["status"].as_str().unwrap_or("");
        if status != "executing" && status != "planning" {
            continue;
        }
        // This sprint is stuck — mark it as failed
        let sprint_id = match sprint["id"].as_str() {
            Some(id) => id,
            None => continue,
        };
        let number = sprint["number"].as_i64().unwrap_or(0);

        if let Err(e) = client
            .patch::<_, serde_json::Value>(
                &format!("/v1/er_sprints/{sprint_id}"),
                &json!({
                    "status": "failed",
                    "finished_at": chrono::Utc::now().to_rfc3339(),
                }),
            )
            .await
        {
            tracing::warn!(error = %e, sprint_id, "Failed to clean up stale sprint");
        } else {
            cleaned += 1;
            eprintln!(
                "{} Cleaned up stale sprint {} (was {})",
                "[cleanup]".dimmed(),
                format!("#{number}").yellow(),
                status.red(),
            );
        }
    }

    if cleaned > 0 {
        eprintln!(
            "{} Marked {} stale sprint(s) as failed",
            "[cleanup]".dimmed(),
            cleaned,
        );
    }
}
