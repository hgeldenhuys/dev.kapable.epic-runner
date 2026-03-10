use clap::Args;
use serde_json::json;
use uuid::Uuid;

use super::CliConfig;
use crate::api_client::{ApiClient, DataWrapper};
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

    /// Dry run — plan sprints without executing
    #[arg(long, default_value = "false")]
    pub dry_run: bool,
}

pub async fn run(
    args: OrchestrateArgs,
    client: &ApiClient,
    _cli: &CliConfig,
) -> Result<(), Box<dyn std::error::Error>> {
    let project_id = crate::config::resolve_project_id()?;

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

    // 1. Look up epic
    let epics: DataWrapper<Vec<serde_json::Value>> = client
        .get(&format!(
            "/v1/data/{project_id}/epics?code={}",
            args.epic_code
        ))
        .await?;
    let epic_data = epics
        .data
        .first()
        .ok_or(format!("Epic {} not found", args.epic_code))?;
    let epic: Epic = serde_json::from_value(epic_data.clone())?;

    if epic.status != EpicStatus::Active {
        return Err(format!("Epic {} is {}, not active", epic.code, epic.status).into());
    }

    // 2. Check impediments
    let blocking =
        crate::impediments::check_blocking_impediments(client, &project_id, &epic.code).await?;
    if !blocking.is_empty() {
        return Err(format!(
            "Epic {} has {} open impediment(s)",
            epic.code,
            blocking.len()
        )
        .into());
    }

    eprintln!("Epic Runner — Orchestrate {}: {}", epic.code, epic.title);
    eprintln!("Intent: {}", epic.intent);
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
        eprintln!("\n═══════════════════════════════════════");
        eprintln!("  SPRINT {sprint_num} of epic {}", epic.code);
        eprintln!("═══════════════════════════════════════");

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
                    eprintln!("[orchestrate] WARNING: worktree is dirty, stashing...");
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
        let sprint_resp: DataWrapper<serde_json::Value> = client
            .post(&format!("/v1/data/{project_id}/sprints"), &sprint_body)
            .await?;
        let sprint_id = sprint_resp.data["id"]
            .as_str()
            .ok_or("Sprint creation failed")?;

        // Load and assign stories (take up to 5 ready stories)
        let stories: DataWrapper<Vec<serde_json::Value>> = client
            .get(&format!(
                "/v1/data/{project_id}/stories?epic_code={}&status=ready",
                epic.code
            ))
            .await?;

        if stories.data.is_empty() && sprint_num > 1 {
            eprintln!("No ready stories remaining — epic complete.");
            break;
        }

        let batch: Vec<&serde_json::Value> = stories.data.iter().take(5).collect();
        let story_ids: Vec<String> = batch
            .iter()
            .filter_map(|s| s["id"].as_str().map(String::from))
            .collect();

        let _: DataWrapper<serde_json::Value> = client
            .patch(
                &format!("/v1/data/{project_id}/sprints/{sprint_id}"),
                &json!({ "stories": serde_json::to_value(&batch)? }),
            )
            .await?;

        // Transition stories to planned
        for sid in &story_ids {
            let _: Result<DataWrapper<serde_json::Value>, _> = client
                .patch(
                    &format!("/v1/data/{project_id}/stories/{sid}"),
                    &json!({ "status": "planned" }),
                )
                .await;
        }

        // SPAWN SPRINT RUNNER AS CHILD PROCESS
        eprintln!("[orchestrate] Spawning sprint-run process...");
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

        let exit_status = cmd.status()?;
        let exit_code = exit_status.code().unwrap_or(-1);

        eprintln!("[orchestrate] Sprint process exited with code {exit_code}");

        match exit_code {
            0 => {
                // Intent satisfied — close epic
                eprintln!("Intent satisfied — closing epic {}", epic.code);
                let _: DataWrapper<serde_json::Value> = client
                    .patch(
                        &format!("/v1/data/{project_id}/epics/{}", epic.id),
                        &json!({ "status": "closed", "closed_at": chrono::Utc::now().to_rfc3339() }),
                    )
                    .await?;
                break;
            }
            1 => {
                // Failed but not blocked — try next sprint
                eprintln!("Sprint failed. Replenishing for next sprint...");
            }
            2 => {
                // Blocked — impediment raised
                eprintln!("Epic {} is BLOCKED by impediment", epic.code);
                let _: DataWrapper<serde_json::Value> = client
                    .patch(
                        &format!("/v1/data/{project_id}/epics/{}", epic.id),
                        &json!({ "status": "blocked" }),
                    )
                    .await?;
                break;
            }
            _ => {
                // Unexpected exit (crash, context exhaustion, SIGKILL)
                eprintln!(
                    "Sprint process died unexpectedly (exit {}). Continuing...",
                    exit_code
                );
            }
        }
    }

    eprintln!("\nEpic runner finished for {}", epic.code);
    Ok(())
}
