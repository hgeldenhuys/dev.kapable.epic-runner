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

/// Outcome of a pipeline sprint execution.
#[derive(Debug)]
enum SprintOutcome {
    /// Judge says epic mission is satisfied — close the epic.
    MissionComplete,
    /// Judge says more work needed — continue to next sprint.
    MoreWorkNeeded,
    /// Judge found an impediment — pause the epic.
    Blocked(String),
    /// Pipeline infrastructure failed — epic in error, stop immediately.
    Error(String),
}

#[derive(Debug)]
struct JudgeVerdictSummary {
    intent_satisfied: bool,
    #[allow(dead_code)]
    mission_progress: i32,
    blocked: bool,
    action_items: Vec<String>,
    #[allow(dead_code)]
    next_sprint_goal: Option<String>,
}

#[derive(Args)]
pub struct OrchestrateArgs {
    /// Epic code to orchestrate (e.g. AUTH-001)
    pub epic_code: String,

    /// Maximum number of sprints
    #[arg(long, default_value = "20")]
    pub max_sprints: i32,

    /// Model override for ALL ceremonies (overrides per-node YAML models).
    /// When omitted, each ceremony node uses its own model from the flow YAML.
    #[arg(long)]
    pub model: Option<String>,

    /// Effort override for ALL ceremonies (overrides per-node YAML effort).
    /// When omitted, each ceremony node uses its own effort from the flow YAML.
    #[arg(long)]
    pub effort: Option<String>,

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

    /// Sprint timeout in minutes (kills runaway sprint processes)
    #[arg(long, default_value = "90")]
    pub sprint_timeout: u64,

    /// Maximum retries for transient sprint failures (network timeout, rate limit)
    #[arg(long, default_value = "2")]
    pub max_retries: u32,

    /// Directory for log files (daemon mode — stdout/stderr redirected to disk)
    #[arg(long, default_value = ".epic-runner/logs")]
    pub log_dir: String,

    /// Execution engine: "ceremony" (default, legacy flow) or "pipeline" (distributed agent)
    #[arg(long, default_value = "ceremony")]
    pub engine: String,
}

pub async fn run(
    args: OrchestrateArgs,
    client: &ApiClient,
    _cli: &CliConfig,
) -> Result<(), Box<dyn std::error::Error>> {
    // 0. LOG REDIRECT (daemon mode — all output goes to disk)
    #[cfg(unix)]
    let log_file_path = setup_log_redirect(&args.log_dir, &args.epic_code)?;
    #[cfg(unix)]
    eprintln!("[log] Output redirected to {}", log_file_path.display());

    // 1. PRE-FLIGHT
    let claude_check = std::process::Command::new("claude")
        .arg("--version")
        .output();
    if claude_check.is_err() {
        return Err("claude CLI not found in PATH".into());
    }

    // Verify API credentials before doing any work. This catches invalid keys,
    // expired tokens, and misconfigured credential forwarding BEFORE we create
    // sprints or spawn child processes. Without this, invalid creds cause the
    // child process to 401 on its first API call — wasting an entire sprint
    // record each time (see AUTH-002: 5 sprints burned on VALIDATE-001).
    eprintln!("{} Verifying API credentials...", "[preflight]".dimmed());
    client.verify_auth().await.map_err(|e| {
        format!(
            "Pre-flight auth check failed — refusing to start orchestration.\n\
             Cause: {e}\n\
             Fix: Check --key flag, KAPABLE_DATA_KEY env var, or .epic-runner/config.toml"
        )
    })?;
    let key_prefix: String = client.api_key().chars().take(12).collect();
    eprintln!(
        "{} API credentials verified ({}...)",
        "[preflight]".dimmed(),
        key_prefix.dimmed()
    );

    // Lock file with dead PID detection
    let lock_dir = std::path::Path::new(".epic-runner");
    let lock_outcome = crate::lock::acquire_epic_lock(lock_dir, &args.epic_code)?;
    let lock_path = match lock_outcome {
        crate::lock::LockOutcome::Acquired(path) => path,
        crate::lock::LockOutcome::AlreadyRunning { epic_code, pid } => {
            return Err(format!("Epic {} already running (PID {})", epic_code, pid).into());
        }
        crate::lock::LockOutcome::StaleRecovered {
            lock_path,
            dead_pid,
        } => {
            eprintln!(
                "{} Removing stale lock for {} (PID {} is dead)",
                "[cleanup]".dimmed(),
                args.epic_code,
                dead_pid,
            );
            lock_path
        }
    };
    let lock_path_clone = lock_path.clone();
    let _guard = scopeguard::guard((), move |_| {
        crate::lock::release_lock(&lock_path_clone);
    });

    // Snapshot the binary to a temp location so rebuilds during orchestration
    // don't invalidate macOS code signing for child process spawns (SIGKILL).
    let exe_snapshot = std::env::temp_dir().join(format!("epic-runner-{}", std::process::id()));
    std::fs::copy(std::env::current_exe()?, &exe_snapshot)?;
    let exe_snapshot_clone = exe_snapshot.clone();
    let _exe_guard = scopeguard::guard((), move |_| {
        std::fs::remove_file(&exe_snapshot_clone).ok();
    });

    // Register daemon status for health monitoring
    let daemon_record_id = match client
        .daemon_register(&args.epic_code, std::process::id())
        .await
    {
        Ok(id) => {
            eprintln!(
                "{} Daemon registered ({})",
                "[preflight]".dimmed(),
                &id[..8],
            );
            Some(id)
        }
        Err(e) => {
            // Non-fatal — daemon can run without health monitoring
            tracing::warn!(error = %e, "Failed to register daemon status (table may not exist — run epic-runner init)");
            None
        }
    };

    // 1. Look up epic (try server-side filter, fall back to client-side)
    let epics_resp: DataWrapper<Vec<serde_json::Value>> = client
        .get_with_params("/v1/epics", &[("code", args.epic_code.as_str())])
        .await?;
    let epic_data = match epics_resp
        .data
        .iter()
        .find(|e| e["code"].as_str() == Some(args.epic_code.as_str()))
    {
        Some(e) => e.clone(),
        None => {
            // Fallback: server may have ignored the filter — full scan
            tracing::debug!("Server may have ignored 'code' filter — falling back to full scan");
            let all_epics: DataWrapper<Vec<serde_json::Value>> = client.get("/v1/epics").await?;
            all_epics
                .data
                .iter()
                .find(|e| e["code"].as_str() == Some(args.epic_code.as_str()))
                .ok_or(format!("Epic {} not found", args.epic_code))?
                .clone()
        }
    };
    let epic: Epic = serde_json::from_value(epic_data)?;

    if epic.status != EpicStatus::Active {
        if let Some(ref daemon_id) = daemon_record_id {
            client.daemon_stopped(daemon_id).await;
        }
        return Err(format!("Epic {} is {}, not active", epic.code, epic.status).into());
    }

    // 2. Check impediments
    let blocking = crate::impediments::check_blocking_impediments(client, &epic.code).await?;
    if !blocking.is_empty() {
        if let Some(ref daemon_id) = daemon_record_id {
            client.daemon_stopped(daemon_id).await;
        }
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
        if let Some(ref daemon_id) = daemon_record_id {
            client.daemon_stopped(daemon_id).await;
        }
        return Ok(());
    }

    // 3. Determine starting sprint number from existing sprints
    let epic_id_str = epic.id.to_string();
    let existing_sprints: DataWrapper<Vec<serde_json::Value>> = client
        .get_with_params("/v1/er_sprints", &[("epic_id", &epic_id_str)])
        .await?;
    // Client-side fallback — server may return all sprints
    let max_existing = existing_sprints
        .data
        .iter()
        .filter(|s| s["epic_id"].as_str() == Some(epic_id_str.as_str()))
        .filter_map(|s| s["number"].as_i64())
        .max()
        .unwrap_or(0) as i32;

    // Sprint loop — start numbering after the highest existing sprint
    let mut consecutive_failures = 0u32;
    for i in 1..=args.max_sprints {
        let sprint_num = max_existing + i;
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

            // Rebase worktree to latest default branch (main/master/custom)
            // This prevents stale worktrees from redoing work already merged.
            rebase_worktree_to_default_branch(&worktree_path);
        }

        // Create sprint in DB with sprint goal.
        // First sprint inherits the epic goal. Subsequent sprints use the judge's
        // refined next_sprint_goal from the previous sprint (if available).
        let session_id = Uuid::new_v4();
        let sprint_goal = if sprint_num == 1 {
            format!("Initial sprint: {}", &epic.intent)
        } else {
            // Try to read next_sprint_goal from the previous sprint record
            let prev_goal = read_previous_sprint_goal(client, &epic, sprint_num - 1).await;
            prev_goal.unwrap_or_else(|| {
                format!(
                    "Sprint {} — continue epic mission: {}",
                    sprint_num, &epic.intent
                )
            })
        };
        let sprint_body = json!({
            "epic_id": epic.id.to_string(),
            "number": sprint_num,
            "session_id": session_id.to_string(),
            "status": "planning",
            "goal": sprint_goal,
        });
        let sprint_resp: serde_json::Value = client.post("/v1/er_sprints", &sprint_body).await?;
        let sprint_id = sprint_resp["id"].as_str().ok_or("Sprint creation failed")?;

        // Load and assign stories (try server-side filter, fall back to client-side)
        // Priority: ready > planned > draft (most refined first)
        let all_stories: DataWrapper<Vec<serde_json::Value>> = client
            .get_with_params("/v1/stories", &[("epic_code", epic.code.as_str())])
            .await?;
        // Client-side fallback — server may return all stories.
        // Filter via StoryStatus::is_eligible_for_sprint() whitelist.
        let mut eligible_stories: Vec<&serde_json::Value> = all_stories
            .data
            .iter()
            .filter(|s| {
                s["epic_code"].as_str() == Some(epic.code.as_str())
                    && s["status"]
                        .as_str()
                        .and_then(|st| serde_json::from_value::<StoryStatus>(json!(st)).ok())
                        .is_some_and(|st| st.is_eligible_for_sprint())
            })
            .collect();
        // Sort: fewest attempts first (untried foundations before retried dependents),
        // then by status (ready > planned > draft), then by story code ascending
        // (lower codes are typically prerequisites created first).
        eligible_stories.sort_by(|a, b| {
            let attempts_a = a["attempt_count"].as_i64().unwrap_or(0);
            let attempts_b = b["attempt_count"].as_i64().unwrap_or(0);
            attempts_a
                .cmp(&attempts_b)
                .then_with(|| {
                    let status_ord = |s: &serde_json::Value| match s["status"].as_str() {
                        Some("ready") => 0,
                        Some("planned") => 1,
                        Some("draft") => 2,
                        _ => 3,
                    };
                    status_ord(a).cmp(&status_ord(b))
                })
                .then_with(|| {
                    let code_a = a["code"].as_str().unwrap_or("zzz");
                    let code_b = b["code"].as_str().unwrap_or("zzz");
                    code_a.cmp(code_b)
                })
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

        // ── DoR soft warning: log ungroomed stories but continue ──
        // Builder agents self-groom inline (optimistic grooming), so we no longer block.
        let mut ungroomed: Vec<String> = Vec::new();
        for story in &batch {
            let code = story["code"].as_str().unwrap_or("?");
            let has_acs = story["acceptance_criteria"]
                .as_array()
                .is_some_and(|a| !a.is_empty());
            let has_tasks = story["tasks"].as_array().is_some_and(|t| !t.is_empty());
            if !has_acs || !has_tasks {
                ungroomed.push(code.to_string());
            }
        }
        if !ungroomed.is_empty() {
            eprintln!(
                "\n\x1b[1;33m⚠ DoR WARNING: Ungroomed stories (builder will self-groom)\x1b[0m"
            );
            for code in &ungroomed {
                eprintln!("  • {code} — missing ACs or tasks, builder will generate inline");
            }
            eprintln!();
        }

        let story_ids: Vec<String> = batch
            .iter()
            .filter_map(|s| s["id"].as_str().map(String::from))
            .collect();

        // v2 compat: attach stories to sprint as inline JSON
        if let Err(e) = client
            .patch::<_, serde_json::Value>(
                &format!("/v1/er_sprints/{sprint_id}"),
                &json!({ "stories": serde_json::to_value(&batch)? }),
            )
            .await
        {
            tracing::warn!(error = %e, "Failed to attach stories to sprint — continuing");
        }

        // v3: create sprint_assignments (decoupled story-sprint relationship)
        for story in &batch {
            if let Some(sid) = story["id"].as_str() {
                let assignment = json!({
                    "sprint_id": sprint_id,
                    "backlog_item_id": sid,
                    "status": "assigned",
                });
                let _ = client
                    .post::<_, serde_json::Value>("/v1/sprint_assignments", &assignment)
                    .await;
            }
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

        // ── Pipeline engine branch ──
        if args.engine == "pipeline" {
            let outcome = run_pipeline_engine(
                &args, client, &epic, sprint_num, session_id, sprint_id, &batch,
            )
            .await;

            match outcome {
                Ok(SprintOutcome::MissionComplete) => {
                    eprintln!(
                        "{} — closing epic {}",
                        "Intent satisfied".green().bold(),
                        epic.code
                    );
                    let _ = client
                        .patch::<_, serde_json::Value>(
                            &format!("/v1/epics/{}", epic.id),
                            &json!({ "status": "closed", "closed_at": chrono::Utc::now().to_rfc3339() }),
                        )
                        .await;
                    break;
                }
                Ok(SprintOutcome::MoreWorkNeeded) => {
                    eprintln!(
                        "{}",
                        format!("Sprint {sprint_num} complete — more work needed").yellow()
                    );
                    // continue to next sprint
                }
                Ok(SprintOutcome::Blocked(reason)) => {
                    eprintln!("{}: {}", "Epic blocked".red().bold(), reason);
                    let _ = client
                        .patch::<_, serde_json::Value>(
                            &format!("/v1/epics/{}", epic.id),
                            &json!({ "status": "blocked" }),
                        )
                        .await;
                    break;
                }
                Ok(SprintOutcome::Error(err)) => {
                    eprintln!(
                        "{}: {}",
                        "Pipeline error — epic NOT retried".red().bold(),
                        err
                    );
                    break;
                }
                Err(e) => {
                    eprintln!("{}: {}", "Pipeline submission error".red().bold(), e);
                    break;
                }
            }
            continue;
        }

        // SPAWN SPRINT RUNNER AS CHILD PROCESS (with retry loop for transient failures)
        let sprint_id_owned = sprint_id.to_string();
        let mut should_break = false;
        loop {
            tracing::info!(sprint_id = %sprint_id_owned, sprint_num, "Spawning sprint-run child process");
            let mut cmd = std::process::Command::new(&exe_snapshot);
            // Forward API credentials so child process can access the same project
            cmd.arg("--url")
                .arg(&client.base_url)
                .arg("--key")
                .arg(client.api_key())
                .arg("sprint-run")
                .arg(&sprint_id_owned);
            // Only pass --model/--effort when user explicitly overrides.
            // When omitted, sprint-run uses per-node models from the flow YAML.
            if let Some(model) = &args.model {
                cmd.arg("--model").arg(model);
            }
            if let Some(effort) = &args.effort {
                cmd.arg("--effort").arg(effort);
            }
            for dir in &args.add_dir {
                cmd.arg("--add-dir").arg(dir);
            }
            if let Some(flow) = &args.flow {
                cmd.arg("--flow").arg(flow);
            }
            if let Some(budget) = args.budget_override {
                cmd.arg("--budget-override").arg(budget.to_string());
            }

            let mut child = cmd.spawn()?;
            let child_pid = child.id();
            let timeout_mins = args.sprint_timeout;
            let timeout_duration = std::time::Duration::from_secs(timeout_mins * 60);

            // Wait with timeout + outward heartbeat — prevents runaway processes from burning
            // unlimited credits AND keeps the sprint's heartbeat_at field fresh in the DB so
            // external observers (console UI) can detect zombie sprints.
            // Update daemon status to executing
            if let Some(ref daemon_id) = daemon_record_id {
                client
                    .daemon_heartbeat(daemon_id, "executing", Some(sprint_num))
                    .await;
            }

            let exit_code = match wait_with_heartbeat(
                &mut child,
                timeout_duration,
                client,
                &sprint_id_owned,
                daemon_record_id.as_deref(),
            )
            .await
            {
                Ok(status) => status.code().unwrap_or(-1),
                Err(_) => {
                    eprintln!(
                        "{} Sprint timed out after {} minutes — killing PID {}",
                        "[timeout]".red().bold(),
                        timeout_mins,
                        child_pid,
                    );
                    // Kill the child process tree (cross-platform)
                    kill_process_tree(child_pid);
                    let _ = child.kill();
                    let _ = child.wait();
                    -1 // Treated as unexpected exit → sprint cancelled
                }
            };

            tracing::info!(exit_code, "Sprint-run process exited");

            match exit_code {
                0 => {
                    // Intent satisfied — close epic
                    consecutive_failures = 0;
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
                    should_break = true;
                    break; // break retry loop
                }
                1 => {
                    // Exit code 1 can mean either:
                    // a) Sprint completed normally — more work needed (run_sprint exits 1)
                    // b) Sprint crashed (run_sprint crash handler exits 1)
                    // Distinguish by checking sprint status in DB.
                    let is_crash = match client
                        .get::<serde_json::Value>(&format!("/v1/er_sprints/{}", sprint_id_owned))
                        .await
                    {
                        Ok(sprint_data) => sprint_data["status"].as_str() == Some("cancelled"),
                        Err(_) => false, // assume normal on API error
                    };

                    if is_crash {
                        // Crashed sprint — candidate for retry
                        if consecutive_failures < args.max_retries {
                            consecutive_failures += 1;
                            emit_retry_event(
                                client,
                                &sprint_id_owned,
                                sprint_num,
                                exit_code,
                                consecutive_failures,
                                args.max_retries,
                            )
                            .await;
                            backoff_sleep(consecutive_failures - 1).await;
                            continue; // retry same sprint
                        }
                        // Exhausted retries — fall through to mark cancelled
                        tracing::warn!(
                            exit_code,
                            consecutive_failures,
                            "Sprint crashed — retries exhausted, marking cancelled"
                        );
                        if let Err(e) = client
                            .patch::<_, serde_json::Value>(
                                &format!("/v1/er_sprints/{}", sprint_id_owned),
                                &json!({
                                    "status": "cancelled",
                                    "finished_at": chrono::Utc::now().to_rfc3339(),
                                }),
                            )
                            .await
                        {
                            tracing::error!(error = %e, "Failed to mark crashed sprint as cancelled");
                        }
                        consecutive_failures = 0;
                        break; // break retry loop, continue to next sprint
                    }

                    // Normal completion — more work needed for epic mission.
                    consecutive_failures = 0;

                    // Check if this was a provisional verdict (code OK, deploy/browser pending).
                    // If so, do NOT create a full implementation sprint — close the epic instead.
                    let is_provisional = match client
                        .get::<serde_json::Value>(&format!("/v1/er_sprints/{}", sprint_id_owned))
                        .await
                    {
                        Ok(sprint_data) => sprint_data
                            .get("handoff_summary")
                            .and_then(|hs| hs.get("verdict"))
                            .and_then(|v| v.get("provisional"))
                            .and_then(|p| p.as_bool())
                            .unwrap_or(false),
                        Err(_) => false,
                    };

                    if is_provisional {
                        eprintln!(
                            "{} — code quality passed but deploy/browser verification pending. Closing epic as done_pending_deploy.",
                            "Provisional verdict".yellow().bold()
                        );
                        if let Err(e) = client
                            .patch::<_, serde_json::Value>(
                                &format!("/v1/epics/{}", epic.id),
                                &json!({ "status": "closed", "closed_at": chrono::Utc::now().to_rfc3339() }),
                            )
                            .await
                        {
                            tracing::error!(error = %e, "Failed to close epic as done_pending_deploy");
                        }
                        should_break = true;
                        break; // break retry loop — no new sprint needed
                    }

                    // The judge has already transitioned stories in run_sprint.rs:
                    //   - stories_completed → "done"
                    //   - stories_to_regroom → ACs/tasks cleared for re-planning
                    //   - delta_stories → new stories in backlog
                    // We do NOT blindly reset stories. Incomplete stories stay in_progress
                    // and get re-assigned to the next sprint with their existing context.
                    eprintln!(
                        "{}",
                        "Sprint completed — more work needed. Preparing next sprint...".yellow()
                    );

                    // 1. Transition remaining in_progress stories to "ready" so they're
                    // eligible for next sprint assignment. This preserves their ACs, tasks,
                    // and all context — it just makes them available for the next sprint picker.
                    let readied = ready_incomplete_stories(client, &epic.code).await;
                    if readied > 0 {
                        eprintln!(
                            "{} {} incomplete stories available for next sprint",
                            "[backlog]".dimmed(),
                            readied,
                        );
                    }

                    // 2. Check if all stories are done — if so, close the epic
                    //    even if judge said intent_satisfied=false (deploy issue != code issue)
                    let (has_workable, all_done) = check_story_status(client, &epic.code).await;
                    if all_done {
                        eprintln!(
                            "All stories {} — closing epic despite judge not marking intent satisfied",
                            "done".green().bold()
                        );
                        if let Err(e) = client
                            .patch::<_, serde_json::Value>(
                                &format!("/v1/epics/{}", epic.id),
                                &json!({ "status": "done" }),
                            )
                            .await
                        {
                            eprintln!("Failed to close epic: {}", e);
                        }
                        should_break = true;
                        break; // break retry loop
                    }
                    if !has_workable {
                        eprintln!(
                            "All remaining stories are {} — blocking epic",
                            "blocked".red().bold()
                        );
                        if let Err(e) = client
                            .patch::<_, serde_json::Value>(
                                &format!("/v1/epics/{}", epic.id),
                                &json!({ "status": "blocked" }),
                            )
                            .await
                        {
                            tracing::error!(error = %e, "Failed to mark epic as blocked");
                        }
                        should_break = true;
                        break; // break retry loop
                    }

                    // 3. SM inter-sprint adaptation: analyze ceremony history, patch flow for next sprint
                    adapt_ceremony_flow(client, &epic.code, sprint_num).await;
                    break; // break retry loop, continue to next sprint
                }
                2 => {
                    // Blocked — impediment raised (not transient, no retry)
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
                    should_break = true;
                    break; // break retry loop
                }
                _ => {
                    // Unexpected exit (crash, context exhaustion, SIGKILL) — candidate for retry
                    if consecutive_failures < args.max_retries {
                        consecutive_failures += 1;
                        emit_retry_event(
                            client,
                            &sprint_id_owned,
                            sprint_num,
                            exit_code,
                            consecutive_failures,
                            args.max_retries,
                        )
                        .await;
                        backoff_sleep(consecutive_failures - 1).await;
                        continue; // retry same sprint
                    }
                    // Exhausted retries
                    tracing::warn!(
                        exit_code,
                        consecutive_failures,
                        "Sprint process died unexpectedly — retries exhausted, marking cancelled"
                    );
                    if let Err(e) = client
                        .patch::<_, serde_json::Value>(
                            &format!("/v1/er_sprints/{}", sprint_id_owned),
                            &json!({
                                "status": "cancelled",
                                "finished_at": chrono::Utc::now().to_rfc3339(),
                            }),
                        )
                        .await
                    {
                        tracing::error!(error = %e, "Failed to mark crashed sprint as cancelled");
                    }
                    consecutive_failures = 0;
                    break; // break retry loop, continue to next sprint
                }
            }
        } // end retry loop

        if should_break {
            break; // break sprint loop
        }

        // Between sprints, mark daemon as idle
        if let Some(ref daemon_id) = daemon_record_id {
            client.daemon_heartbeat(daemon_id, "idle", None).await;
        }
    }

    // Mark daemon as stopped on normal exit
    if let Some(ref daemon_id) = daemon_record_id {
        client.daemon_stopped(daemon_id).await;
    }

    eprintln!("\nEpic runner finished for {}", epic.code);
    Ok(())
}

/// Exponential backoff sleep for transient sprint failures.
/// Base delay 30s, doubles each attempt, capped at 300s.
async fn backoff_sleep(attempt: u32) {
    let delay = std::cmp::min(30 * 2u64.pow(attempt), 300);
    eprintln!(
        "{} Waiting {}s before retry (attempt {})...",
        "[retry]".yellow().bold(),
        delay,
        attempt + 1,
    );
    tokio::time::sleep(std::time::Duration::from_secs(delay)).await;
}

/// Emit a ceremony_event for retry observability.
async fn emit_retry_event(
    client: &ApiClient,
    sprint_id: &str,
    sprint_num: i32,
    exit_code: i32,
    consecutive_failures: u32,
    max_retries: u32,
) {
    let retry_event = json!({
        "sprint_id": sprint_id,
        "event_type": "retry",
        "node_key": serde_json::Value::Null,
        "summary": format!(
            "Retrying sprint {} (attempt {}/{})",
            sprint_num, consecutive_failures, max_retries
        ),
        "detail": json!({
            "exit_code": exit_code,
            "retry_attempt": consecutive_failures,
        }),
    });
    let _ = client
        .post::<_, serde_json::Value>("/v1/ceremony_events", &retry_event)
        .await;
}

/// Read the previous sprint's next_sprint_goal from the DB.
/// The judge sets this to guide what the next sprint should focus on.
async fn read_previous_sprint_goal(
    client: &ApiClient,
    epic: &Epic,
    prev_sprint_num: i32,
) -> Option<String> {
    let sprints: Result<DataWrapper<Vec<serde_json::Value>>, _> = client
        .get_with_params(
            "/v1/er_sprints",
            &[("epic_id", epic.id.to_string().as_str())],
        )
        .await;
    let sprints = sprints.ok()?.data;
    sprints
        .iter()
        .find(|s| s["number"].as_i64() == Some(prev_sprint_num as i64))
        .and_then(|s| s["next_sprint_goal"].as_str())
        .map(String::from)
}

/// Transition incomplete stories (in_progress/planned) back to "ready" so the next
/// sprint can pick them up. Unlike the old `reset_failed_stories`, this preserves all
/// story context (ACs, tasks, changed_files) — it only changes the status field.
/// The judge has already handled story-level decisions (done, regroom, blocked) in run_sprint.rs.
async fn ready_incomplete_stories(client: &ApiClient, epic_code: &str) -> usize {
    let all_stories: Result<DataWrapper<Vec<serde_json::Value>>, _> = client
        .get_with_params("/v1/stories", &[("epic_code", epic_code)])
        .await;
    let stories = match all_stories {
        Ok(d) => d.data,
        Err(e) => {
            tracing::warn!(error = %e, "Failed to load stories for ready transition");
            return 0;
        }
    };

    let mut count = 0;
    for story in &stories {
        if story["epic_code"].as_str() != Some(epic_code) {
            continue;
        }
        let status = story["status"].as_str().unwrap_or("");
        // Only transition in_progress/planned — leave done, blocked, draft alone
        if status != "in_progress" && status != "planned" {
            continue;
        }
        let story_id = match story["id"].as_str() {
            Some(id) => id,
            None => continue,
        };

        if let Err(e) = client
            .patch::<_, serde_json::Value>(
                &format!("/v1/stories/{story_id}"),
                &json!({ "status": "ready" }),
            )
            .await
        {
            tracing::warn!(error = %e, story_id, "Failed to transition story to ready");
        } else {
            count += 1;
        }
    }

    count
}

/// Check story status for the epic. Returns (has_workable, all_done).
/// - has_workable: true if any stories are ready/draft/planned/in_progress
/// - all_done: true if ALL stories assigned to this epic are done
async fn check_story_status(client: &ApiClient, epic_code: &str) -> (bool, bool) {
    let all_stories: Result<DataWrapper<Vec<serde_json::Value>>, _> = client
        .get_with_params("/v1/stories", &[("epic_code", epic_code)])
        .await;
    let stories = match all_stories {
        Ok(d) => d.data,
        Err(_) => return (true, false), // assume workable on error
    };

    let epic_stories: Vec<_> = stories
        .iter()
        .filter(|s| s["epic_code"].as_str() == Some(epic_code))
        .collect();

    if epic_stories.is_empty() {
        return (false, false);
    }

    let has_workable = epic_stories.iter().any(|s| {
        let status = s["status"].as_str().unwrap_or("");
        matches!(status, "ready" | "draft" | "planned" | "in_progress")
    });

    let all_done = epic_stories
        .iter()
        .all(|s| s["status"].as_str() == Some("done"));

    (has_workable, all_done)
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
    // Load epic to find epic_id (try server-side filter)
    let epics_resp: DataWrapper<Vec<serde_json::Value>> = client
        .get_with_params("/v1/epics", &[("code", epic_code)])
        .await
        .ok()?;
    // Client-side fallback — server may return all epics
    let epic_id = epics_resp
        .data
        .iter()
        .find(|e| e["code"].as_str() == Some(epic_code))?
        .get("id")?
        .as_str()?;

    // Load sprints for this epic (try server-side filter)
    let all_sprints: DataWrapper<Vec<serde_json::Value>> = client
        .get_with_params("/v1/er_sprints", &[("epic_id", epic_id)])
        .await
        .ok()?;

    // Client-side fallback — filter sprints for this epic, sort by number
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

/// Rebase the epic worktree to the latest default branch (auto-detected).
/// If rebase conflicts, aborts and continues on the current base.
/// Logs the base commit SHA for sprint context awareness.
fn rebase_worktree_to_default_branch(worktree_path: &str) {
    let default_branch = crate::flow::engine::detect_default_branch(worktree_path);

    // Fetch latest default branch from origin
    let fetch = std::process::Command::new("git")
        .args(["fetch", "origin", &default_branch])
        .current_dir(worktree_path)
        .output();

    let origin_ref = format!("origin/{default_branch}");

    if let Err(e) = fetch {
        tracing::warn!(error = %e, branch = %default_branch, "Failed to fetch — skipping rebase");
        return;
    }
    let fetch_output = fetch.unwrap();
    if !fetch_output.status.success() {
        let stderr = String::from_utf8_lossy(&fetch_output.stderr);
        tracing::warn!(stderr = %stderr, branch = %default_branch, "git fetch failed — skipping rebase");
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

    // Get origin/{default_branch} SHA for comparison
    let origin_head = std::process::Command::new("git")
        .args(["rev-parse", "--short", &origin_ref])
        .current_dir(worktree_path)
        .output()
        .ok()
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .unwrap_or_default();

    if head_before == origin_head {
        tracing::debug!(
            head = %head_before,
            branch = %default_branch,
            "Worktree already at origin — no rebase needed"
        );
        return;
    }

    // Attempt rebase
    let rebase = std::process::Command::new("git")
        .args(["rebase", &origin_ref])
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
                "{} Worktree rebased to {} ({} → {})",
                "[rebase]".dimmed(),
                origin_ref.cyan(),
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
                "{} Rebase conflicted — continuing on {} ({} is {})",
                "[rebase]".yellow(),
                head_before.dimmed(),
                origin_ref.dimmed(),
                origin_head.dimmed(),
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
    let all: DataWrapper<Vec<serde_json::Value>> = client
        .get_with_params("/v1/sprint_learnings", &[("epic_code", epic_code)])
        .await
        .ok()?;

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
///
/// Two-pass cleanup:
/// 1. **Epic-scoped**: Any sprint for THIS epic in "executing"/"planning" status → cancelled.
///    These are definitely stale because we're the orchestrator for this epic.
/// 2. **Cross-epic heartbeat**: Any sprint in "executing" with heartbeat_at > 5 minutes old
///    (or no heartbeat_at) → cancelled. Catches zombie sprints from other epics.
async fn cleanup_stale_sprints(client: &ApiClient, epic_id: &str) {
    // Pass 1: Epic-scoped cleanup (same as before — all stuck sprints for this epic)
    let epic_sprints: Result<DataWrapper<Vec<serde_json::Value>>, _> = client
        .get_with_params("/v1/er_sprints", &[("epic_id", epic_id)])
        .await;
    let mut cleaned = 0;

    if let Ok(resp) = epic_sprints {
        for sprint in &resp.data {
            if sprint["epic_id"].as_str() != Some(epic_id) {
                continue;
            }
            let status = sprint["status"].as_str().unwrap_or("");
            if status != "executing" && status != "planning" {
                continue;
            }
            if cancel_stale_sprint(client, sprint, "epic-scoped").await {
                cleaned += 1;
            }
        }
    }

    // Pass 2: Cross-epic heartbeat-based cleanup
    // Fetch ALL executing sprints and check heartbeat freshness
    let all_sprints: Result<DataWrapper<Vec<serde_json::Value>>, _> = client
        .get_with_params("/v1/er_sprints", &[("status", "executing")])
        .await;

    if let Ok(resp) = all_sprints {
        let now = chrono::Utc::now();
        let stale_threshold = chrono::Duration::minutes(5);

        for sprint in &resp.data {
            let status = sprint["status"].as_str().unwrap_or("");
            if status != "executing" {
                continue; // server may ignore the filter
            }
            // Skip this epic's sprints — already handled in pass 1
            if sprint["epic_id"].as_str() == Some(epic_id) {
                continue;
            }

            let is_stale = match sprint["heartbeat_at"].as_str() {
                Some(ts) => {
                    if let Ok(heartbeat) = chrono::DateTime::parse_from_rfc3339(ts) {
                        now.signed_duration_since(heartbeat.with_timezone(&chrono::Utc))
                            > stale_threshold
                    } else {
                        true // unparseable timestamp = stale
                    }
                }
                None => {
                    // No heartbeat at all — check started_at.
                    // If started >10 min ago with no heartbeat, it's a pre-heartbeat zombie.
                    match sprint["started_at"].as_str() {
                        Some(ts) => {
                            if let Ok(started) = chrono::DateTime::parse_from_rfc3339(ts) {
                                now.signed_duration_since(started.with_timezone(&chrono::Utc))
                                    > chrono::Duration::minutes(10)
                            } else {
                                true
                            }
                        }
                        None => true, // no started_at either = definitely stale
                    }
                }
            };

            if is_stale && cancel_stale_sprint(client, sprint, "stale-heartbeat").await {
                cleaned += 1;
            }
        }
    }

    if cleaned > 0 {
        eprintln!(
            "{} Marked {} stale sprint(s) as cancelled",
            "[cleanup]".dimmed(),
            cleaned,
        );
    }
}

/// Cancel a single stale sprint. Returns true if successfully cancelled.
async fn cancel_stale_sprint(client: &ApiClient, sprint: &serde_json::Value, reason: &str) -> bool {
    let sprint_id = match sprint["id"].as_str() {
        Some(id) => id,
        None => return false,
    };
    let number = sprint["number"].as_i64().unwrap_or(0);
    let status = sprint["status"].as_str().unwrap_or("unknown");
    let epic_id = sprint["epic_id"].as_str().unwrap_or("?");

    if let Err(e) = client
        .patch::<_, serde_json::Value>(
            &format!("/v1/er_sprints/{sprint_id}"),
            &json!({
                "status": "cancelled",
                "finished_at": chrono::Utc::now().to_rfc3339(),
            }),
        )
        .await
    {
        tracing::warn!(error = %e, sprint_id, "Failed to clean up stale sprint");
        false
    } else {
        eprintln!(
            "{} Cleaned up stale sprint #{} (was {}, reason: {}, epic: {})",
            "[cleanup]".dimmed(),
            number.to_string().yellow(),
            status.red(),
            reason,
            &epic_id[..8.min(epic_id.len())],
        );
        true
    }
}

/// Wait for a child process with a timeout, emitting a DB heartbeat every ~30 seconds.
///
/// The heartbeat PATCHes `heartbeat_at` on the sprint record so that external observers
/// (console UI, other orchestrators) can detect zombie sprints. If a sprint's heartbeat_at
/// is older than 5 minutes, it's considered dead and can be cleaned up.
///
/// Returns Ok(ExitStatus) if the process exits within the timeout, Err(()) if it times out.
async fn wait_with_heartbeat(
    child: &mut std::process::Child,
    timeout: std::time::Duration,
    client: &ApiClient,
    sprint_id: &str,
    daemon_record_id: Option<&str>,
) -> Result<std::process::ExitStatus, ()> {
    let deadline = std::time::Instant::now() + timeout;
    let poll_interval = std::time::Duration::from_millis(2000);
    let heartbeat_interval = std::time::Duration::from_secs(30);
    let daemon_heartbeat_interval = std::time::Duration::from_secs(60);
    let mut last_heartbeat = std::time::Instant::now();
    let mut last_daemon_heartbeat = std::time::Instant::now();

    // Send initial heartbeat
    send_heartbeat(client, sprint_id).await;

    loop {
        match child.try_wait() {
            Ok(Some(status)) => return Ok(status),
            Ok(None) => {
                if std::time::Instant::now() >= deadline {
                    return Err(());
                }
                // Sprint heartbeat every ~30 seconds
                if last_heartbeat.elapsed() >= heartbeat_interval {
                    send_heartbeat(client, sprint_id).await;
                    last_heartbeat = std::time::Instant::now();
                }
                // Daemon heartbeat every ~60 seconds
                if let Some(daemon_id) = daemon_record_id {
                    if last_daemon_heartbeat.elapsed() >= daemon_heartbeat_interval {
                        client.daemon_heartbeat(daemon_id, "executing", None).await;
                        last_daemon_heartbeat = std::time::Instant::now();
                    }
                }
                tokio::time::sleep(poll_interval).await;
            }
            Err(e) => {
                tracing::error!(error = %e, "Error waiting for child process");
                return Err(());
            }
        }
    }
}

/// PATCH heartbeat_at on a sprint record. Best-effort — never blocks or fails loudly.
async fn send_heartbeat(client: &ApiClient, sprint_id: &str) {
    let now = chrono::Utc::now().to_rfc3339();
    if let Err(e) = client
        .patch::<_, serde_json::Value>(
            &format!("/v1/er_sprints/{sprint_id}"),
            &json!({ "heartbeat_at": now }),
        )
        .await
    {
        tracing::debug!(error = %e, "Heartbeat PATCH failed (non-fatal)");
    }
}

/// Kill a process tree. On Unix, sends SIGTERM to the process group, waits 2 seconds,
/// then sends SIGKILL. On Windows, uses `taskkill /F /T /PID`.
#[cfg(unix)]
fn kill_process_tree(pid: u32) {
    // Send SIGTERM to the process group (negative PID = process group)
    // SAFETY: kill with negative pid targets the process group — standard POSIX behavior.
    unsafe {
        libc::kill(-(pid as i32), libc::SIGTERM);
    }
    std::thread::sleep(std::time::Duration::from_secs(2));
    // SAFETY: Escalate to SIGKILL if the process group is still alive.
    unsafe {
        libc::kill(-(pid as i32), libc::SIGKILL);
    }
}

/// Kill a process tree (Windows). Uses `taskkill /F /T /PID` to forcefully
/// terminate the process and its entire child tree.
#[cfg(windows)]
fn kill_process_tree(pid: u32) {
    let _ = std::process::Command::new("taskkill")
        .args(["/F", "/T", "/PID", &pid.to_string()])
        .output();
}

/// Rotate log files for an epic, keeping at most `max_keep - 1` files
/// (the caller is about to create one more, so total stays at `max_keep`).
fn rotate_logs(
    log_dir: &std::path::Path,
    epic_code: &str,
    max_keep: usize,
) -> Result<(), Box<dyn std::error::Error>> {
    let prefix = format!("{}-", epic_code);
    let mut logs: Vec<std::path::PathBuf> = Vec::new();

    if let Ok(entries) = std::fs::read_dir(log_dir) {
        for entry in entries.flatten() {
            let name = entry.file_name();
            let name_str = name.to_string_lossy();
            if name_str.starts_with(&prefix) && name_str.ends_with(".log") {
                logs.push(entry.path());
            }
        }
    }

    // Timestamp in filename ensures alphabetical sort == chronological
    logs.sort();

    // Delete oldest until we have room for the new file
    while logs.len() >= max_keep {
        if let Some(oldest) = logs.first() {
            std::fs::remove_file(oldest)?;
        }
        logs.remove(0);
    }

    Ok(())
}

/// Set up log file for daemon mode: create directory, rotate old logs, open new
/// log file, and redirect stdout + stderr to it via dup2.
///
/// After this call, all `eprintln!`, `tracing::*!`, and child process output
/// goes to `.epic-runner/logs/{EPIC_CODE}-{timestamp}.log`.
#[cfg(unix)]
fn setup_log_redirect(
    log_dir: &str,
    epic_code: &str,
) -> Result<std::path::PathBuf, Box<dyn std::error::Error>> {
    use std::os::unix::io::AsRawFd;

    let dir = std::path::Path::new(log_dir);
    std::fs::create_dir_all(dir)?;

    // Rotate: keep at most 4 existing logs (+ 1 new = 5 total)
    rotate_logs(dir, epic_code, 5)?;

    // Create new log file with ISO timestamp
    let timestamp = chrono::Utc::now().format("%Y%m%dT%H%M%SZ");
    let filename = format!("{}-{}.log", epic_code, timestamp);
    let file_path = dir.join(&filename);
    let file = std::fs::File::create(&file_path)?;

    // Redirect stdout (fd 1) and stderr (fd 2) to the log file.
    // Child processes inherit these fds automatically via Stdio::inherit() (default).
    let fd = file.as_raw_fd();
    unsafe {
        libc::dup2(fd, libc::STDOUT_FILENO);
        libc::dup2(fd, libc::STDERR_FILENO);
    }
    // `file` drops here, closing the original fd — but fds 1 and 2 remain valid.

    Ok(file_path)
}

// detect_default_branch is now `pub fn` in crate::flow::engine — use that instead.

/// Execute a sprint via the pipeline engine instead of the ceremony child process.
///
/// Generates a PipelineDefinition from the sprint's stories, submits it to the
/// platform API, and polls until completion. Loads product brief, deploy profile,
/// and previous learnings for full context injection.
///
/// Returns `SprintOutcome` to drive the sprint loop:
/// - Pipeline failure → `Error` (stop, don't retry)
/// - Judge satisfied → `MissionComplete` (close epic)
/// - Judge blocked → `Blocked` (pause epic)
/// - Otherwise → `MoreWorkNeeded` (next sprint)
#[allow(clippy::too_many_arguments)]
async fn run_pipeline_engine(
    args: &OrchestrateArgs,
    client: &ApiClient,
    epic: &Epic,
    sprint_num: i32,
    session_id: Uuid,
    sprint_id: &str,
    stories: &[&serde_json::Value],
) -> Result<SprintOutcome, Box<dyn std::error::Error>> {
    use crate::pipeline_generator::{
        generate_sprint_pipeline, SprintPipelineContext, StoryContext,
    };
    use crate::pipeline_submitter::{submit_pipeline, wait_for_completion};

    eprintln!(
        "[pipeline] Generating pipeline for sprint {}...",
        sprint_num
    );

    // Convert stories to StoryContext, skipping any without a code
    let mut story_contexts: Vec<StoryContext> = Vec::new();
    for s in stories {
        let code = match s["code"].as_str() {
            Some(c) if !c.is_empty() => c.to_string(),
            _ => {
                eprintln!(
                    "[pipeline] Skipping story without code: {}",
                    s["title"].as_str().unwrap_or("(untitled)")
                );
                continue;
            }
        };
        story_contexts.push(StoryContext {
            code,
            id: s["id"]
                .as_str()
                .unwrap_or(&Uuid::new_v4().to_string())
                .to_string(),
            title: s["title"].as_str().unwrap_or("").to_string(),
            description: s["description"].as_str().unwrap_or("").to_string(),
            acceptance_criteria: s["acceptance_criteria"]
                .as_array()
                .map(|a| {
                    let mut v = Vec::new();
                    for item in a {
                        if let Some(text) = item.as_str() {
                            v.push(text.to_string());
                        }
                    }
                    v
                })
                .unwrap_or_default(),
            tasks: s["tasks"]
                .as_array()
                .map(|a| {
                    let mut v = Vec::new();
                    for item in a {
                        if let Some(text) = item.as_str() {
                            v.push(text.to_string());
                        }
                    }
                    v
                })
                .unwrap_or_default(),
            story_json: (*s).clone(),
        });
    }

    // Load agent definitions from embedded agents
    let builder_content = load_agent_content("builder");
    let judge_content = load_agent_content("code-judge");
    let scrum_master_content = load_agent_content("scrum-master");

    let cwd = std::env::current_dir()?.display().to_string();

    // Build hooks settings from hook file paths
    let hooks_settings = crate::pipeline_generator::build_hooks_settings(&cwd);

    // Load product for brief + deploy profile
    let product: serde_json::Value = client
        .get(&format!("/v1/products/{}", epic.product_id))
        .await
        .unwrap_or_default();
    let product_brief = product["brief"].as_str().map(String::from);
    let deploy_profile = product["deploy_profile"]
        .as_str()
        .unwrap_or("none")
        .to_string();
    let deploy_app_id = product["deploy_app_id"].as_str().map(String::from);
    let product_dod = product["definition_of_done"].as_str().map(String::from);

    // Load previous learnings from past sprints (if any)
    let previous_learnings = if sprint_num > 1 {
        let epic_id_str = epic.id.to_string();
        let learnings_resp: Result<DataWrapper<Vec<serde_json::Value>>, _> = client
            .get_with_params("/v1/sprint_learnings", &[("epic_id", &epic_id_str)])
            .await;
        match learnings_resp {
            Ok(resp) => {
                let combined: String = resp
                    .data
                    .iter()
                    .filter_map(|l| l["learnings"].as_str())
                    .collect::<Vec<_>>()
                    .join("\n");
                if combined.is_empty() {
                    None
                } else {
                    Some(combined)
                }
            }
            Err(_) => None,
        }
    } else {
        None
    };

    let ctx = SprintPipelineContext {
        epic_code: epic.code.clone(),
        sprint_number: sprint_num,
        session_id: session_id.to_string(),
        stories: story_contexts,
        product_brief,
        epic_intent: epic.intent.clone(),
        builder_agent_content: builder_content,
        judge_agent_content: judge_content,
        scrum_master_agent_content: scrum_master_content,
        working_dir: cwd,
        model_override: args.model.clone(),
        effort_override: args.effort.clone(),
        budget_override: args.budget_override,
        add_dirs: args.add_dir.clone(),
        hooks_settings: Some(hooks_settings),
        deploy_profile,
        deploy_app_id,
        api_url: client.base_url.clone(),
        api_key: client.api_key().to_string(),
        product_definition_of_done: product_dod,
        previous_learnings,
        serial: true,
        epic_branch: format!("epic/{}", epic.code.to_lowercase()),
    };

    let pipeline = generate_sprint_pipeline(&ctx);
    eprintln!(
        "[pipeline] Generated pipeline '{}' with {} stages",
        pipeline.name,
        pipeline.stages.len()
    );

    // Submit to platform
    let result = submit_pipeline(
        client,
        &pipeline,
        None, // repo_url -- agent runs locally
        None,
        None,
        Some(vec!["claude".to_string()]),
    )
    .await?;

    eprintln!(
        "[pipeline] Submitted -- run_id: {}, job_id: {}",
        result.run_id, result.job_id
    );

    // Monitor until completion
    let final_status = wait_for_completion(client, result.run_id, 10).await?;

    eprintln!(
        "[pipeline] Sprint {} completed -- status: {}",
        sprint_num, final_status.status
    );

    // Pipeline FAILED = infrastructure error, not "more work needed"
    if final_status.status == "failed" {
        let error_msg = final_status
            .error_message
            .clone()
            .unwrap_or_else(|| "Pipeline execution failed".to_string());
        // Update sprint status
        let _ = client
            .patch::<_, serde_json::Value>(
                &format!("/v1/er_sprints/{}", sprint_id),
                &json!({ "status": "failed" }),
            )
            .await;
        return Ok(SprintOutcome::Error(error_msg));
    }

    if final_status.status == "cancelled" {
        let _ = client
            .patch::<_, serde_json::Value>(
                &format!("/v1/er_sprints/{}", sprint_id),
                &json!({ "status": "cancelled" }),
            )
            .await;
        return Ok(SprintOutcome::Error("Pipeline was cancelled".to_string()));
    }

    // Pipeline SUCCEEDED — extract judge verdict from logs
    let verdict = extract_judge_verdict(client, result.run_id).await;
    let outcome = match &verdict {
        Some(v) if v.intent_satisfied => SprintOutcome::MissionComplete,
        Some(v) if v.blocked => SprintOutcome::Blocked(v.action_items.join(", ")),
        _ => SprintOutcome::MoreWorkNeeded,
    };

    // Update sprint status with verdict summary
    let sprint_status = match &outcome {
        SprintOutcome::MissionComplete => "completed",
        SprintOutcome::MoreWorkNeeded => "completed",
        SprintOutcome::Blocked(_) => "blocked",
        SprintOutcome::Error(_) => "failed",
    };
    let _ = client
        .patch::<_, serde_json::Value>(
            &format!("/v1/er_sprints/{}", sprint_id),
            &json!({
                "status": sprint_status,
                "finished_at": chrono::Utc::now().to_rfc3339(),
            }),
        )
        .await;

    Ok(outcome)
}

/// Extract judge verdict from the judge-code stage's log output.
///
/// The judge outputs structured JSON with `intent_satisfied`, `mission_progress`, etc.
/// This function fetches the judge stage logs and parses the JSON verdict.
async fn extract_judge_verdict(client: &ApiClient, run_id: Uuid) -> Option<JudgeVerdictSummary> {
    use crate::pipeline_submitter::fetch_run_logs;

    let logs = fetch_run_logs(client, run_id, Some("judge-code"), Some("judge"))
        .await
        .ok()?;

    // Parse verdict JSON from logs — it may be wrapped in markdown code blocks
    let verdict = crate::json_extract::extract_json_object(&logs)?;

    Some(JudgeVerdictSummary {
        intent_satisfied: verdict["intent_satisfied"].as_bool().unwrap_or(false),
        mission_progress: verdict["mission_progress"].as_f64().unwrap_or(0.0) as i32,
        blocked: verdict
            .get("blocked")
            .and_then(|b| b.as_bool())
            .unwrap_or(false),
        action_items: verdict["action_items"]
            .as_array()
            .map(|a| {
                let mut items = Vec::new();
                for v in a {
                    if let Some(s) = v.as_str() {
                        items.push(s.to_string());
                    }
                }
                items
            })
            .unwrap_or_default(),
        next_sprint_goal: verdict["next_sprint_goal"].as_str().map(String::from),
    })
}

/// Load embedded agent content by name.
///
/// Uses the agents module's resolve_agent_path to write to temp dir, then reads it back.
/// This extracts the raw content for injection into pipeline prompts.
fn load_agent_content(name: &str) -> String {
    let fake_repo = std::path::PathBuf::from("/tmp/epic-runner-pipeline-agents");
    match crate::agents::resolve_agent_path(name, &fake_repo) {
        Some(path) => {
            std::fs::read_to_string(&path).unwrap_or_else(|_| format!("You are a {} agent.", name))
        }
        None => format!("You are a {} agent.", name),
    }
}
