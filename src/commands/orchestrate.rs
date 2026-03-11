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

        // Load and assign stories (client-side filter for ready stories in this epic)
        let all_stories: DataWrapper<Vec<serde_json::Value>> = client.get("/v1/stories").await?;
        let ready_stories: Vec<&serde_json::Value> = all_stories
            .data
            .iter()
            .filter(|s| {
                s["epic_code"].as_str() == Some(epic.code.as_str())
                    && s["status"].as_str() == Some("ready")
            })
            .collect();

        if ready_stories.is_empty() && sprint_num > 1 {
            eprintln!("No ready stories remaining — epic complete.");
            break;
        }

        let batch: Vec<&serde_json::Value> = ready_stories.into_iter().take(5).collect();
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
                // Failed but not blocked — try next sprint
                eprintln!(
                    "{}",
                    "Sprint failed. Replenishing for next sprint...".yellow()
                );
                // SM inter-sprint adaptation: analyze ceremony history, patch flow for next sprint
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
                    "Sprint process died unexpectedly — continuing to next sprint"
                );
            }
        }
    }

    eprintln!("\nEpic runner finished for {}", epic.code);
    Ok(())
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
