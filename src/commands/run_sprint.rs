use clap::Args;
use owo_colors::OwoColorize;
use serde_json::json;

use super::CliConfig;
use crate::api_client::ApiClient;
use crate::event_sink::EventSink;
use crate::flow::{engine, loader};
use crate::types::*;

#[derive(Args)]
pub struct SprintRunArgs {
    /// Sprint ID to execute
    pub sprint_id: String,

    /// Model override for ALL nodes (overrides per-node YAML models)
    #[arg(long)]
    pub model: Option<String>,

    /// Effort override for ALL nodes (overrides per-node YAML effort)
    #[arg(long)]
    pub effort: Option<String>,

    /// Additional directories
    #[arg(long)]
    pub add_dir: Vec<String>,

    /// Flow file override (YAML path)
    #[arg(long)]
    pub flow: Option<String>,

    /// Override budget (USD) for all ceremony nodes
    #[arg(long)]
    pub budget_override: Option<f64>,
}

pub async fn run(
    args: SprintRunArgs,
    client: &ApiClient,
    _cli: &CliConfig,
) -> Result<(), Box<dyn std::error::Error>> {
    // 0. PRE-FLIGHT: Verify credentials before starting any work.
    // This is the child process's own auth check — catches credential forwarding
    // failures immediately instead of failing mid-ceremony (AUTH-002 / ER-024).
    client.verify_auth().await.map_err(|e| {
        format!(
            "Sprint-run pre-flight auth failed — credentials were not forwarded correctly.\n\
             Cause: {e}\n\
             This likely means the orchestrator did not pass --key or the key is invalid."
        )
    })?;
    tracing::info!("Pre-flight auth check passed in child process");

    // 1. Load sprint from DB
    let sprint_resp: serde_json::Value = client
        .get(&format!("/v1/er_sprints/{}", args.sprint_id))
        .await?;
    let sprint: Sprint = serde_json::from_value(sprint_resp)?;

    // 2. Load epic
    let epic_resp: serde_json::Value = client.get(&format!("/v1/epics/{}", sprint.epic_id)).await?;
    let epic: Epic = serde_json::from_value(epic_resp)?;

    // 3. Load product for repo_path (direct GET by ID, not query param)
    let product_resp: serde_json::Value = client
        .get(&format!("/v1/products/{}", epic.product_id))
        .await?;
    let product: Product = serde_json::from_value(product_resp)?;

    // 3b. Load previous sprint learnings (feedback loop: retro → next sprint)
    let previous_learnings = load_previous_learnings(client, &epic.code).await;

    // 4. Load ceremony flow (cascade: CLI override → per-epic patched → config → embedded)
    let config =
        crate::config::find_project_config().and_then(|p| crate::config::read_config(&p).ok());
    let flow = loader::load_flow(
        args.flow.as_deref(),
        config.as_ref().and_then(|c| c.ceremony_flow_id()),
        Some(&epic.code),
    )
    .await?;

    eprintln!(
        "{} Sprint {} of epic {}",
        "[sprint-run]".dimmed(),
        sprint.number.to_string().cyan().bold(),
        epic.code.yellow().bold()
    );
    eprintln!(
        "{} Flow: {} v{}",
        "[sprint-run]".dimmed(),
        flow.name.bold(),
        flow.version
    );
    eprintln!("{} Nodes: {}", "[sprint-run]".dimmed(), flow.nodes.len());

    // 5. Create event sink for real-time DB streaming.
    // Events flow: emit() → mpsc → background task → POST /v1/ceremony_events → WAL → SSE
    let (sink, sink_handle) = EventSink::spawn(client.clone());

    // 6. Update sprint status to executing (best-effort — don't abort if DB write fails)
    if let Err(e) = client
        .patch::<_, serde_json::Value>(
            &format!("/v1/er_sprints/{}", sprint.id),
            &json!({
                "status": "executing",
                "started_at": chrono::Utc::now().to_rfc3339(),
                "heartbeat_at": chrono::Utc::now().to_rfc3339(),
            }),
        )
        .await
    {
        tracing::warn!(error = %e, "Failed to update sprint status to executing — continuing");
    }

    // 6b. Sync worktree to main BEFORE ceremony starts.
    // This prevents stale-worktree rework: if the previous sprint merged to main but the
    // worktree branch wasn't updated, the builder would redo work already on main.
    // Also catches external commits pushed to main between sprints.
    {
        let repo_path = crate::repo_resolver::resolve_product_repo(
            product.repo_url.as_deref(),
            &product.repo_path,
        )
        .unwrap_or_else(|_| product.repo_path.clone());
        let wt_path = std::path::Path::new(&repo_path)
            .join(".claude/worktrees")
            .join(&epic.code);
        if wt_path.exists() {
            let default_branch = crate::flow::engine::detect_default_branch(repo_path.as_str());
            let origin_branch = format!("origin/{}", default_branch);
            // Fetch latest default branch first (the worktree might not see recent remote commits)
            let _ = std::process::Command::new("git")
                .args(["fetch", "origin", &default_branch])
                .current_dir(&repo_path)
                .output();

            let rebase = std::process::Command::new("git")
                .args(["rebase", &origin_branch])
                .current_dir(&wt_path)
                .output();
            match rebase {
                Ok(out) if out.status.success() => {
                    eprintln!(
                        "{} Synced worktree to {}",
                        "[sprint-run]".dimmed(),
                        origin_branch
                    );
                }
                Ok(out) => {
                    // Abort failed rebase, fall back to hard reset
                    let _ = std::process::Command::new("git")
                        .args(["rebase", "--abort"])
                        .current_dir(&wt_path)
                        .output();
                    let reset = std::process::Command::new("git")
                        .args(["reset", "--hard", &origin_branch])
                        .current_dir(&wt_path)
                        .output();
                    match reset {
                        Ok(r) if r.status.success() => {
                            eprintln!(
                                "{} Synced worktree to {} (via reset)",
                                "[sprint-run]".dimmed(),
                                origin_branch
                            );
                        }
                        _ => {
                            let err = String::from_utf8_lossy(&out.stderr);
                            eprintln!(
                                "{} Warning: worktree sync failed (non-fatal): {}",
                                "[sprint-run]".dimmed(),
                                err
                            );
                        }
                    }
                }
                Err(e) => {
                    eprintln!(
                        "{} Warning: worktree sync skipped: {}",
                        "[sprint-run]".dimmed(),
                        e
                    );
                }
            }
        }
    }

    // Stream sprint started event
    sink.emit(SprintEvent {
        sprint_id: sprint.session_id,
        event_type: SprintEventType::Started,
        node_id: None,
        node_label: None,
        summary: format!("Sprint {} started for epic {}", sprint.number, epic.code),
        detail: Some(json!({
            "flow": flow.name,
            "flow_version": flow.version,
            "nodes": flow.nodes.len(),
            "epic_code": epic.code,
        })),
        cost_usd: None,
        timestamp: chrono::Utc::now(),
    });

    // 7. Build flow context
    let stories = sprint.stories.clone().unwrap_or(json!([]));
    // Build product brief and DoD strings for agent context injection
    let product_brief = product.brief.clone().unwrap_or_default();
    let product_dod = product
        .definition_of_done
        .as_ref()
        .map(|dod| serde_json::to_string_pretty(dod).unwrap_or_default())
        .unwrap_or_default();

    let resolved_repo_path =
        crate::repo_resolver::resolve_product_repo(product.repo_url.as_deref(), &product.repo_path)
            .map_err(|e| -> Box<dyn std::error::Error> { e.into() })?;
    let ctx = engine::FlowContext {
        epic: epic.clone(),
        sprint: sprint.clone(),
        stories,
        default_branch: product
            .default_branch
            .clone()
            .unwrap_or_else(|| engine::detect_default_branch(&resolved_repo_path)),
        repo_path: resolved_repo_path,
        model_override: args.model.clone(),
        effort_override: args.effort.clone(),
        budget_override: args.budget_override,
        add_dirs: args.add_dir.clone(),
        previous_learnings,
        product_brief,
        product_definition_of_done: product_dod,
        current_story: None,
    };

    // 8. Execute the ceremony flow (nodes at each BFS level run in parallel)
    let results = match engine::execute_flow(&flow, &ctx, &sink, client).await {
        Ok(r) => r,
        Err(e) => {
            tracing::error!(error = %e, "Flow execution crashed — marking sprint as cancelled");
            sink.emit(SprintEvent {
                sprint_id: sprint.session_id,
                event_type: SprintEventType::Completed,
                node_id: None,
                node_label: None,
                summary: format!("Sprint {} cancelled (crash): {}", sprint.number, e),
                detail: Some(json!({ "cancelled_reason": e.to_string() })),
                cost_usd: None,
                timestamp: chrono::Utc::now(),
            });
            // Mark sprint as cancelled — it was interrupted, not failed.
            // Cancelled sprints are not retried; the orchestrator creates a fresh sprint.
            if let Err(patch_err) = client
                .patch::<_, serde_json::Value>(
                    &format!("/v1/er_sprints/{}", sprint.id),
                    &json!({
                        "status": "cancelled",
                        "finished_at": chrono::Utc::now().to_rfc3339(),
                    }),
                )
                .await
            {
                tracing::error!(error = %patch_err, "Failed to mark crashed sprint as cancelled in DB");
            }
            drop(sink);
            let _ = sink_handle.await;
            std::process::exit(1);
        }
    };

    // 8b. Write planning data back to DB (reference-based flow)
    // The planner outputs a JSON array of stories with acceptance_criteria, tasks, dependencies.
    // We PATCH each story record so the data persists across sprint retries. Future sprints
    // see pre-planned stories and skip re-planning unless the judge instructs otherwise.
    if let Some(groom_result) = results.iter().find(|r| r.key == "groom") {
        write_groom_results_to_stories(client, &sprint, groom_result).await;
    }

    // 8c. Write builder results back to story records in DB.
    // Per-story mode produces a BuilderOutput with per-story task completion,
    // AC verification, changed files, and log entries. PATCH each story record.
    if let Some(execute_result) = results.iter().find(|r| r.key == "execute") {
        if let Some(ref builder_output) = execute_result.builder_output {
            let patched = crate::builder::write_builder_results_to_stories(
                client,
                builder_output,
                &sprint.session_id.to_string(),
            )
            .await;
            tracing::info!(
                patched,
                total = builder_output.stories.len(),
                "Builder results written to story records"
            );
        }
    }

    // 8d. Generate sprint changelog from builder results + judge verdict
    if let Some(execute_result) = results.iter().find(|r| r.key == "execute") {
        if let Some(ref builder_output) = execute_result.builder_output {
            let changelog_path =
                generate_sprint_changelog(&epic, &sprint, builder_output, &results);
            if let Some(path) = changelog_path {
                tracing::info!(path = %path.display(), "Sprint changelog generated");
            }
        }
    }

    // 9. Determine outcome
    let judge_verdict = results.iter().find_map(|r| r.judge_verdict.clone());
    let any_impediment = results.iter().any(|r| r.impediment_raised);

    // Intent evaluation: if a judge ran, use its verdict.
    // If no judge ran (e.g. minimal flow), consider it satisfied if
    // all non-skipped nodes completed successfully.
    let intent_satisfied = if judge_verdict.is_some() {
        crate::judge::evaluate_verdict(&judge_verdict)
    } else {
        results.iter().all(|r| {
            matches!(
                r.status,
                crate::types::CeremonyStatus::Completed | crate::types::CeremonyStatus::Skipped
            )
        })
    };

    // Sprints never "fail" — they complete with whatever work got done.
    // The judge evaluates mission progress; if more work is needed, the
    // orchestrator creates another sprint. Only impediments block.
    let final_status = if any_impediment {
        "blocked"
    } else {
        "completed"
    };

    // Stream sprint finished event — sprints always complete (or get blocked)
    sink.emit(SprintEvent {
        sprint_id: sprint.session_id,
        event_type: if any_impediment {
            SprintEventType::Blocked
        } else {
            SprintEventType::Completed
        },
        node_id: None,
        node_label: None,
        summary: format!(
            "Sprint {} {}: {}",
            sprint.number,
            final_status,
            if intent_satisfied {
                "mission satisfied"
            } else {
                "more work needed"
            }
        ),
        detail: Some(json!({
            "intent_satisfied": intent_satisfied,
            "impediment": any_impediment,
            "total_cost_usd": results.iter().filter_map(|r| r.cost_usd).sum::<f64>(),
        })),
        cost_usd: Some(results.iter().filter_map(|r| r.cost_usd).sum::<f64>()),
        timestamp: chrono::Utc::now(),
    });

    // 10. Write results to DB
    let ceremony_log: Vec<serde_json::Value> = results
        .iter()
        .map(|r| {
            json!({
                "key": r.key,
                "status": format!("{:?}", r.status),
                "cost_usd": r.cost_usd,
            })
        })
        .collect();

    // Compute cost + node stats (used in velocity, metrics, and sprint PATCH)
    let total_cost: f64 = results.iter().filter_map(|r| r.cost_usd).sum();

    // Build per-ceremony cost breakdown
    let mut cost_map = serde_json::Map::new();
    for r in &results {
        if let Some(cost) = r.cost_usd {
            cost_map.insert(r.key.clone(), serde_json::Value::from(cost));
        }
    }
    let ceremony_costs = if cost_map.is_empty() {
        None
    } else {
        Some(serde_json::Value::Object(cost_map))
    };

    let completed_nodes = results
        .iter()
        .filter(|r| r.status == CeremonyStatus::Completed)
        .count();

    // v3: Compute velocity data for sprint capacity planning
    let stories_planned = sprint
        .stories
        .as_ref()
        .and_then(|s| s.as_array())
        .map(|a| a.len())
        .unwrap_or(0);
    let stories_completed_count = judge_verdict
        .as_ref()
        .and_then(|v| v.stories_completed.as_ref())
        .map(|sc| sc.len())
        .unwrap_or(0);
    let velocity = json!({
        "stories_planned": stories_planned,
        "stories_completed": stories_completed_count,
        "total_cost_usd": total_cost,
        "nodes_completed": completed_nodes,
        "mission_progress": judge_verdict.as_ref().and_then(|v| v.mission_progress),
    });

    // Include next_sprint_goal from judge verdict (if provided) so the orchestrator
    // can read it from the sprint record and use it for the next sprint's goal.
    let mut sprint_patch = json!({
        "status": final_status,
        "finished_at": chrono::Utc::now().to_rfc3339(),
        "ceremony_log": ceremony_log,
        "velocity": velocity,
        "cost_usd": total_cost,
        "ceremony_costs": ceremony_costs,
    });
    if let Some(ref verdict) = judge_verdict {
        if let Some(ref next_goal) = verdict.next_sprint_goal {
            sprint_patch
                .as_object_mut()
                .unwrap()
                .insert("next_sprint_goal".to_string(), json!(next_goal));
        }
    }

    if let Err(e) = client
        .patch::<_, serde_json::Value>(&format!("/v1/er_sprints/{}", sprint.id), &sprint_patch)
        .await
    {
        tracing::error!(error = %e, "Failed to write sprint results to DB — results lost");
    }

    // 11. Persist supervisor decisions + rubber duck sessions (best-effort)
    for result in &results {
        for decision in &result.supervisor_decisions {
            let _: Result<serde_json::Value, _> = client
                .post("/v1/supervisor_decisions", &serde_json::to_value(decision)?)
                .await;
        }
        for duck in &result.rubber_duck_sessions {
            let _: Result<serde_json::Value, _> = client
                .post("/v1/rubber_duck_sessions", &serde_json::to_value(duck)?)
                .await;
        }
    }

    // 11b. Extract learnings from SM retro and persist to sprint_learnings table
    // (feedback loop: retro action_items → sprint_learnings → next sprint's {{previous_learnings}})
    // Also auto-create backlog items from discovered_work via agentboard CLI.
    if let Some(retro_result) = results.iter().find(|r| r.key == "sm_retro") {
        save_sprint_learnings(client, &epic.code, sprint.number, retro_result).await;
        create_backlog_from_retro(&epic.code, sprint.number, retro_result);

        // v3: Update product brief (PRODUCTS.md) from retro insights + accumulated learnings
        let learnings = load_previous_learnings(client, &epic.code).await;
        update_product_brief(
            client,
            &epic.product_id.to_string(),
            &epic.code,
            sprint.number,
            retro_result.output.as_deref(),
            &learnings,
        )
        .await;
    }

    // 11c. v3: Create delta stories from judge verdict back into backlog
    if let Some(verdict) = &judge_verdict {
        if let Some(delta_stories) = &verdict.delta_stories {
            for delta in delta_stories {
                let body = json!({
                    "product_id": epic.product_id.to_string(),
                    "title": delta.title,
                    "description": delta.description,
                    "status": "draft",
                    "size": delta.size,
                    "tags": delta.tags,
                });
                // Write to v3 backlog_items table (best-effort)
                let _ = client
                    .post::<_, serde_json::Value>("/v1/backlog_items", &body)
                    .await;
                // Also write to v2 stories table for backward compat
                let _ = client
                    .post::<_, serde_json::Value>(
                        "/v1/stories",
                        &json!({
                            "product_id": epic.product_id.to_string(),
                            "title": delta.title,
                            "description": delta.description,
                            "status": "draft",
                        }),
                    )
                    .await;
                tracing::info!(title = %delta.title, "Judge created delta story → backlog");
            }
            if !delta_stories.is_empty() {
                eprintln!(
                    "{} Judge created {} delta stories for next sprint",
                    "[backlog]".dimmed(),
                    delta_stories.len()
                );
            }
        }
    }

    // 11d. Transition completed stories to "done" based on judge verdict
    if let Some(verdict) = &judge_verdict {
        if let Some(completed_codes) = &verdict.stories_completed {
            let story_list = sprint
                .stories
                .as_ref()
                .and_then(|s| s.as_array())
                .cloned()
                .unwrap_or_default();
            let mut done_count = 0usize;
            for code in completed_codes {
                // Find the story ID matching this code
                let story_id = story_list.iter().find_map(|s| {
                    if s.get("code").and_then(|c| c.as_str()) == Some(code.as_str()) {
                        s.get("id").and_then(|id| id.as_str()).map(String::from)
                    } else {
                        None
                    }
                });
                if let Some(sid) = story_id {
                    if let Err(e) = client
                        .patch::<_, serde_json::Value>(
                            &format!("/v1/stories/{}", sid),
                            &json!({ "status": "done" }),
                        )
                        .await
                    {
                        tracing::warn!(story_code = %code, error = %e, "Failed to mark story done");
                    } else {
                        done_count += 1;
                    }
                } else {
                    tracing::warn!(story_code = %code, "Story code from judge verdict not found in sprint stories");
                }
            }
            if done_count > 0 {
                eprintln!(
                    "{} Marked {} stories as done based on judge verdict",
                    "[stories]".dimmed(),
                    done_count
                );
            }
        }
    }

    // 11e. Handle stories the judge flagged for re-grooming.
    // PRINCIPLE: Work done is NEVER wasted. We don't clear ACs/tasks.
    // Instead: mark the original story as "rejected", preserve its commit,
    // and create a NEW story with a fresh plan. The original serves as
    // an audit trail of what was attempted and why it was rejected.
    if let Some(verdict) = &judge_verdict {
        if let Some(regroom_codes) = &verdict.stories_to_regroom {
            let story_list = sprint
                .stories
                .as_ref()
                .and_then(|s| s.as_array())
                .cloned()
                .unwrap_or_default();
            let mut rejected = 0usize;
            for code in regroom_codes {
                let story_data = story_list
                    .iter()
                    .find(|s| s.get("code").and_then(|c| c.as_str()) == Some(code.as_str()));
                let story_val = match story_data {
                    Some(s) => s,
                    None => continue,
                };
                let story_id = match story_val.get("id").and_then(|v| v.as_str()) {
                    Some(id) => id,
                    None => continue,
                };
                let title = story_val
                    .get("title")
                    .and_then(|v| v.as_str())
                    .unwrap_or("Untitled");

                // Mark original as rejected (preserves all work + audit trail)
                if let Err(e) = client
                    .patch::<_, serde_json::Value>(
                        &format!("/v1/stories/{}", story_id),
                        &json!({
                            "status": "rejected",
                            "log_entries": [{
                                "source": "judge",
                                "sprint": sprint.number,
                                "message": format!("Rejected by judge — plan was fundamentally wrong. New story will be created for re-planning."),
                                "timestamp": chrono::Utc::now().to_rfc3339(),
                            }],
                        }),
                    )
                    .await
                {
                    tracing::warn!(story_code = %code, error = %e, "Failed to reject story");
                    continue;
                }

                // Create a new replacement story (draft — needs grooming)
                let new_code = match super::backlog::next_story_code(
                    client,
                    &epic.product_id.to_string(),
                )
                .await
                {
                    Ok(c) => c,
                    Err(e) => {
                        tracing::warn!(error = %e, "Failed to generate new story code for regroom");
                        continue;
                    }
                };
                let body = json!({
                    "product_id": epic.product_id.to_string(),
                    "epic_code": epic.code,
                    "code": new_code,
                    "title": format!("[re-plan] {}", title),
                    "description": format!("Re-plan of {} which was rejected because the original plan was fundamentally wrong.", code),
                    "status": "draft",
                    "intent": story_val.get("intent").cloned().unwrap_or(json!(null)),
                    "persona": story_val.get("persona").cloned().unwrap_or(json!(null)),
                });
                if client
                    .post::<_, serde_json::Value>("/v1/stories", &body)
                    .await
                    .is_ok()
                {
                    rejected += 1;
                    eprintln!(
                        "{} {} rejected → {} created for re-planning",
                        "[regroom]".dimmed(),
                        code.yellow(),
                        new_code.cyan(),
                    );
                }
            }
            if rejected > 0 {
                eprintln!(
                    "{} {} stories rejected and replaced with new stories for re-planning",
                    "[regroom]".dimmed(),
                    rejected,
                );
            }
        }
    }

    // 11f. Apply judge story_updates — add tasks, flag blockers, attach reasons
    // to incomplete stories so the next sprint has specific guidance.
    if let Some(verdict) = &judge_verdict {
        if let Some(updates) = &verdict.story_updates {
            let story_list = sprint
                .stories
                .as_ref()
                .and_then(|s| s.as_array())
                .cloned()
                .unwrap_or_default();
            let mut updated_count = 0usize;
            for update in updates {
                let story_id = story_list.iter().find_map(|s| {
                    if s.get("code").and_then(|c| c.as_str()) == Some(update.code.as_str()) {
                        s.get("id").and_then(|id| id.as_str()).map(String::from)
                    } else {
                        None
                    }
                });
                if let Some(sid) = story_id {
                    let mut patch = json!({});

                    // Append new tasks from judge
                    if let Some(new_tasks) = &update.new_tasks {
                        // Load existing tasks, append new ones
                        let existing = story_list
                            .iter()
                            .find(|s| s.get("id").and_then(|v| v.as_str()) == Some(&sid))
                            .and_then(|s| s.get("tasks"))
                            .and_then(|t| t.as_array())
                            .cloned()
                            .unwrap_or_default();
                        let mut all_tasks: Vec<serde_json::Value> = existing;
                        for task in new_tasks {
                            all_tasks.push(json!({
                                "description": task.description,
                                "persona": task.persona,
                                "done": false,
                                "added_by": "judge",
                            }));
                        }
                        patch["tasks"] = json!(all_tasks);
                    }

                    // Flag story as blocked if judge says so
                    if update.blocked {
                        patch["status"] = json!("blocked");
                        if let Some(reason) = &update.blocked_reason {
                            patch["blocked_reason"] = json!(reason);
                        }
                    }

                    // Attach judge's reason as a log entry
                    if let Some(reason) = &update.reason {
                        // Add to log_entries array
                        let existing_log = story_list
                            .iter()
                            .find(|s| s.get("id").and_then(|v| v.as_str()) == Some(&sid))
                            .and_then(|s| s.get("log_entries"))
                            .and_then(|l| l.as_array())
                            .cloned()
                            .unwrap_or_default();
                        let mut logs = existing_log;
                        logs.push(json!({
                            "source": "judge",
                            "sprint": sprint.number,
                            "message": reason,
                            "timestamp": chrono::Utc::now().to_rfc3339(),
                        }));
                        patch["log_entries"] = json!(logs);
                    }

                    if patch.as_object().map(|o| !o.is_empty()).unwrap_or(false) {
                        if let Err(e) = client
                            .patch::<_, serde_json::Value>(&format!("/v1/stories/{}", sid), &patch)
                            .await
                        {
                            tracing::warn!(
                                story_code = %update.code,
                                error = %e,
                                "Failed to apply judge story update"
                            );
                        } else {
                            updated_count += 1;
                        }
                    }
                }
            }
            if updated_count > 0 {
                eprintln!(
                    "{} Applied judge updates to {} incomplete stories",
                    "[judge]".dimmed(),
                    updated_count
                );
            }
        }
    }

    // 12. Emit structured metrics summary (for aggregation + debugging)
    // (total_cost and completed_nodes already computed above for velocity)
    let failed_nodes = results
        .iter()
        .filter(|r| r.status == CeremonyStatus::Failed)
        .count();
    let skipped_nodes = results
        .iter()
        .filter(|r| r.status == CeremonyStatus::Skipped)
        .count();
    let total_decisions: usize = results.iter().map(|r| r.supervisor_decisions.len()).sum();
    let total_ducks: usize = results.iter().map(|r| r.rubber_duck_sessions.len()).sum();

    let metrics = json!({
        "epic_code": epic.code,
        "sprint_number": sprint.number,
        "status": final_status,
        "intent_satisfied": intent_satisfied,
        "nodes_completed": completed_nodes,
        "nodes_failed": failed_nodes,
        "nodes_skipped": skipped_nodes,
        "total_cost_usd": total_cost,
        "supervisor_decisions": total_decisions,
        "rubber_duck_sessions": total_ducks,
        "finished_at": chrono::Utc::now().to_rfc3339(),
    });
    tracing::info!(metrics = %serde_json::to_string(&metrics).unwrap_or_default(), "Sprint metrics");

    // 13. Flush event sink — drop sender, wait for background writer to finish
    drop(sink);
    let _ = sink_handle.await;

    eprintln!();
    let status_colored = match final_status {
        "completed" if intent_satisfied => "MISSION COMPLETE".green().bold().to_string(),
        "completed" => "completed (more work needed)".yellow().bold().to_string(),
        "blocked" => "BLOCKED".red().bold().to_string(),
        _ => final_status.to_string(),
    };
    eprintln!(
        "{} Sprint {} finished: {}",
        "[sprint-run]".dimmed(),
        sprint.number,
        status_colored
    );
    let satisfied_str = if intent_satisfied {
        "true".green().to_string()
    } else {
        "false — next sprint will continue".yellow().to_string()
    };
    eprintln!(
        "{} Intent satisfied: {}",
        "[sprint-run]".dimmed(),
        satisfied_str
    );

    // Display cost breakdown
    if total_cost > 0.0 {
        eprintln!(
            "{} Sprint cost: {}",
            "[sprint-run]".dimmed(),
            format!("${:.2}", total_cost).green().bold()
        );
        for r in &results {
            if let Some(cost) = r.cost_usd {
                let status_icon = match r.status {
                    CeremonyStatus::Completed => "✓",
                    CeremonyStatus::Failed => "✗",
                    CeremonyStatus::Skipped => "○",
                    _ => "?",
                };
                eprintln!(
                    "{}   {} {}: ${:.2}",
                    "[sprint-run]".dimmed(),
                    status_icon,
                    r.key,
                    cost
                );
            }
        }
    }

    // Exit codes for orchestrator:
    // 0 = mission complete (close epic)
    // 1 = more work needed (create next sprint — NOT a failure)
    // 2 = blocked by impediment (pause epic)
    if any_impediment {
        std::process::exit(2);
    } else if !intent_satisfied {
        std::process::exit(1);
    }
    // exit(0) = mission complete

    Ok(())
}

/// Load learnings from previous sprints of this epic.
/// Returns a formatted string for injection into the {{previous_learnings}} template variable.
/// Best-effort: returns empty string on any failure (network, parse, no data).
async fn load_previous_learnings(client: &ApiClient, epic_code: &str) -> String {
    use crate::api_client::DataWrapper;

    let resp: Result<DataWrapper<Vec<serde_json::Value>>, _> = client
        .get_with_params("/v1/sprint_learnings", &[("epic_code", epic_code)])
        .await;

    let learnings = match resp {
        Ok(wrapper) => wrapper.data,
        Err(e) => {
            tracing::debug!(error = %e, "Could not load sprint_learnings — starting fresh");
            return String::new();
        }
    };

    // Client-side filter: match epic_code, sort by sprint_number
    let mut relevant: Vec<&serde_json::Value> = learnings
        .iter()
        .filter(|l| l["epic_code"].as_str() == Some(epic_code))
        .collect();
    relevant.sort_by_key(|l| l["sprint_number"].as_i64().unwrap_or(0));

    if relevant.is_empty() {
        return String::new();
    }

    let mut out = String::from("Learnings from previous sprints:\n");
    for learning in &relevant {
        let sprint_num = learning["sprint_number"].as_i64().unwrap_or(0);
        if let Some(items) = learning["action_items"].as_array() {
            for item in items {
                if let Some(text) = item.as_str() {
                    out.push_str(&format!("- [Sprint {}] {}\n", sprint_num, text));
                }
            }
        }
        if let Some(patterns) = learning["patterns_to_codify"].as_array() {
            for p in patterns {
                if let Some(text) = p.as_str() {
                    out.push_str(&format!("- [Sprint {} pattern] {}\n", sprint_num, text));
                }
            }
        }
    }
    out
}

/// Extract action_items and patterns from SM retro output, persist to sprint_learnings table.
/// Best-effort — failures are logged but don't abort the sprint.
async fn save_sprint_learnings(
    client: &ApiClient,
    epic_code: &str,
    sprint_number: i32,
    retro_result: &crate::flow::engine::NodeResult,
) {
    let output = match &retro_result.output {
        Some(o) => o,
        None => return,
    };

    // Parse retro JSON output — may contain multiple per-story JSON objects
    // separated by markdown headers/dividers.
    // Try primary output first, then fall back to searching all assistant texts.
    let mut all_parsed = crate::json_extract::extract_all_json_objects(output);
    if all_parsed.is_empty() {
        // Fallback: search all assistant text blocks for JSON objects
        for text in retro_result.all_assistant_texts.iter().rev() {
            let parsed = crate::json_extract::extract_all_json_objects(text);
            if !parsed.is_empty() {
                tracing::info!(
                    block_len = text.len(),
                    objects = parsed.len(),
                    "Found retro JSON in assistant text block (fallback)"
                );
                all_parsed = parsed;
                break;
            }
        }
    }
    if all_parsed.is_empty() {
        tracing::warn!(
            "Could not extract JSON from retro output or assistant texts — skipping learnings save"
        );
        return;
    }

    // Merge all per-story retro results into combined arrays
    let mut action_items = Vec::new();
    let mut patterns = Vec::new();
    let mut friction = Vec::new();
    for parsed in &all_parsed {
        if let Some(arr) = parsed["action_items"].as_array() {
            action_items.extend(arr.iter().cloned());
        }
        if let Some(arr) = parsed["patterns_to_codify"].as_array() {
            patterns.extend(arr.iter().cloned());
        }
        if let Some(arr) = parsed["friction_points"].as_array() {
            friction.extend(arr.iter().cloned());
        }
    }
    let action_items = serde_json::Value::Array(action_items);
    let patterns = serde_json::Value::Array(patterns);
    let friction = serde_json::Value::Array(friction);

    // Only save if there's something worth remembering
    if action_items.as_array().is_none_or(|a| a.is_empty())
        && patterns.as_array().is_none_or(|a| a.is_empty())
    {
        tracing::debug!("No action_items or patterns in retro — nothing to save");
        return;
    }

    let body = serde_json::json!({
        "epic_code": epic_code,
        "sprint_number": sprint_number,
        "action_items": action_items,
        "patterns_to_codify": patterns,
        "friction_points": friction,
        "saved_at": chrono::Utc::now().to_rfc3339(),
    });

    match client
        .post::<_, serde_json::Value>("/v1/sprint_learnings", &body)
        .await
    {
        Ok(_) => tracing::info!(epic_code, sprint_number, "Saved sprint learnings to DB"),
        Err(e) => tracing::warn!(error = %e, "Failed to save sprint learnings — continuing"),
    }
}

/// Auto-create agentboard backlog items from SM retro's discovered_work array.
/// Uses the agentboard CLI (shells out) so we inherit its config resolution.
/// Best-effort — failures are logged but don't abort.
fn create_backlog_from_retro(
    epic_code: &str,
    sprint_number: i32,
    retro_result: &crate::flow::engine::NodeResult,
) {
    let output = match &retro_result.output {
        Some(o) => o,
        None => return,
    };

    // Parse multi-story retro output and merge discovered_work.
    // Try primary output first, then fall back to all assistant texts.
    let mut all_parsed = crate::json_extract::extract_all_json_objects(output);
    if all_parsed.is_empty() {
        for text in retro_result.all_assistant_texts.iter().rev() {
            let parsed = crate::json_extract::extract_all_json_objects(text);
            if !parsed.is_empty() {
                tracing::info!("Found retro backlog JSON in assistant text block (fallback)");
                all_parsed = parsed;
                break;
            }
        }
    }
    let mut discovered: Vec<serde_json::Value> = Vec::new();
    for parsed in &all_parsed {
        if let Some(arr) = parsed["discovered_work"].as_array() {
            discovered.extend(arr.iter().cloned());
        }
    }
    if discovered.is_empty() {
        return;
    }
    let items = &discovered;

    let mut created = 0;
    for item in items {
        let title = match item.as_str() {
            Some(t) if !t.is_empty() => t,
            _ => continue,
        };

        let description = format!(
            "Discovered during sprint {} of epic {}. Auto-created by SM retro.",
            sprint_number, epic_code
        );

        let result = std::process::Command::new("agentboard")
            .args([
                "backlog",
                "add",
                "--title",
                title,
                "--type",
                "story",
                "--description",
                &description,
            ])
            .output();

        match result {
            Ok(output) if output.status.success() => {
                created += 1;
                tracing::debug!(title, "Created backlog item from retro");
            }
            Ok(output) => {
                let stderr = String::from_utf8_lossy(&output.stderr);
                tracing::warn!(title, stderr = %stderr, "Failed to create backlog item");
            }
            Err(e) => {
                tracing::warn!(error = %e, "agentboard CLI not available — skipping backlog creation");
                return; // No point trying more if CLI is missing
            }
        }
    }

    if created > 0 {
        tracing::info!(
            created,
            epic_code,
            sprint_number,
            "Created backlog items from retro discovered_work"
        );
    }
}

/// Try to extract a stories JSON array from a text block.
/// Handles both direct JSON arrays and `{"stories": [...]}` wrapper objects.
fn extract_stories_json(text: &str) -> Option<Vec<serde_json::Value>> {
    if let Some(arr) = crate::json_extract::extract_json_array(text) {
        return Some(arr);
    }
    if let Some(obj) = crate::json_extract::extract_json_object(text) {
        if let Some(arr) = obj.get("stories").and_then(|s| s.as_array()) {
            return Some(arr.clone());
        }
    }
    None
}

/// Write planning data (ACs, tasks, deps) back to story records in DB.
/// This is the key mechanism for reference-based data flow: planner output gets persisted
/// TO the story, so future sprints see pre-planned stories and skip re-planning.
/// Also refreshes the sprint's stories field with the enriched data so downstream
/// ceremony nodes (builder, judge) see the groomed version via {{stories}}.
async fn write_groom_results_to_stories(
    client: &ApiClient,
    sprint: &Sprint,
    groom_result: &crate::flow::engine::NodeResult,
) {
    let output = match &groom_result.output {
        Some(o) => o,
        None => return,
    };

    // Skip if the groom node was conditionally skipped
    if output.starts_with("Skipped") {
        tracing::debug!("Groom was skipped — no write-back needed");
        return;
    }

    // Parse groomer's JSON array output (may be wrapped in markdown fences).
    // First try the primary output (final result text), then fall back to
    // searching ALL assistant text blocks from the session — the groomer often
    // produces its JSON array mid-conversation, not in the final message.
    let stories: Vec<serde_json::Value> = match extract_stories_json(output) {
        Some(arr) => arr,
        None => {
            // Fallback: search all assistant text blocks for the JSON array
            tracing::info!("Primary output not JSON — searching all assistant text blocks");
            let mut found = None;
            for text in groom_result.all_assistant_texts.iter().rev() {
                if let Some(arr) = extract_stories_json(text) {
                    tracing::info!(
                        block_len = text.len(),
                        stories = arr.len(),
                        "Found stories JSON in assistant text block"
                    );
                    found = Some(arr);
                    break;
                }
            }
            match found {
                Some(arr) => arr,
                None => {
                    tracing::warn!(
                        "Could not parse groom output as JSON from result or any assistant text block"
                    );
                    return;
                }
            }
        }
    };

    let mut patched = 0usize;
    let mut enriched_stories = Vec::new();

    for story in &stories {
        // Each story MUST have an "id" field for write-back
        let story_id = match story.get("id").and_then(|id| id.as_str()) {
            Some(id) => id,
            None => {
                tracing::debug!("Groomed story missing 'id' field — skipping write-back");
                enriched_stories.push(story.clone());
                continue;
            }
        };

        // Build PATCH payload — persist planning fields to the story record.
        // These fields make each story a self-contained "work packet" that the builder
        // can execute without additional context assembly.
        let mut patch = serde_json::Map::new();

        let planning_fields = [
            "acceptance_criteria",
            "tasks",
            "dependencies",
            "points",
            "intent",
            "persona",
            "plan",
        ];

        for field in &planning_fields {
            if let Some(val) = story.get(*field) {
                if !val.is_null() {
                    patch.insert(field.to_string(), val.clone());
                }
            }
        }

        // Timestamp when this story was last planned (used to detect stale plans)
        if !patch.is_empty() {
            patch.insert(
                "planned_at".to_string(),
                serde_json::Value::String(chrono::Utc::now().to_rfc3339()),
            );
        }

        if patch.is_empty() {
            enriched_stories.push(story.clone());
            continue;
        }

        match client
            .patch::<_, serde_json::Value>(
                &format!("/v1/stories/{}", story_id),
                &serde_json::Value::Object(patch),
            )
            .await
        {
            Ok(_) => {
                patched += 1;
                enriched_stories.push(story.clone());
            }
            Err(e) => {
                tracing::warn!(story_id, error = %e, "Failed to write groom data to story");
                enriched_stories.push(story.clone());
            }
        }
    }

    // Update sprint's stories field with enriched data so downstream nodes see it via {{stories}}
    if !enriched_stories.is_empty() {
        let enriched_json = serde_json::Value::Array(enriched_stories);
        if let Err(e) = client
            .patch::<_, serde_json::Value>(
                &format!("/v1/er_sprints/{}", sprint.id),
                &serde_json::json!({ "stories": enriched_json }),
            )
            .await
        {
            tracing::warn!(error = %e, "Failed to update sprint with enriched stories");
        }
    }

    if patched > 0 {
        eprintln!(
            "{} Wrote planning data to {} story records (ACs, tasks, deps)",
            "[plan→db]".dimmed(),
            patched
        );
    }
}

/// v3: Update the product brief (PRODUCTS.md equivalent) from retro learnings.
/// The brief is stored on the product record and injected into agent system prompts.
/// This is the key mechanism for cutting agent orientation cost across sprints.
async fn update_product_brief(
    client: &ApiClient,
    product_id: &str,
    epic_code: &str,
    sprint_number: i32,
    retro_output: Option<&str>,
    learnings: &str,
) {
    // Fetch current product data
    let product: Result<serde_json::Value, _> =
        client.get(&format!("/v1/products/{product_id}")).await;
    let product = match product {
        Ok(p) => p,
        Err(_) => return,
    };

    let name = product["name"].as_str().unwrap_or("Unknown");
    let slug = product["slug"].as_str().unwrap_or("unknown");
    let description = product["description"].as_str().unwrap_or("");
    let existing_brief = product["brief"].as_str().unwrap_or("");

    // Determine if brief has rich hand-seeded content (architecture, file maps, etc.)
    // Rich briefs contain sections beyond the auto-generated template.
    let is_rich_brief = existing_brief.contains("## Architecture")
        || existing_brief.contains("## File Map")
        || existing_brief.contains("## Data API")
        || existing_brief.contains("## What This Is")
        || existing_brief.contains("## Key Route Groups")
        || existing_brief.contains("## Crate Map");

    let brief = if is_rich_brief {
        // MERGE mode: preserve the rich brief, update Key Learnings + Patterns sections
        merge_into_rich_brief(existing_brief, learnings, retro_output)
    } else {
        // REPLACE mode: auto-generate brief from template (legacy behavior)
        let mut b = format!(
            "# {name}\n\n\
            **Slug:** {slug}\n\
            **Description:** {description}\n\n"
        );
        if !learnings.is_empty() {
            b.push_str("## Key Learnings\n\n");
            b.push_str(learnings);
            b.push('\n');
        }
        if let Some(retro) = retro_output {
            if let Some(parsed) = crate::json_extract::extract_json_object(retro) {
                if let Some(patterns) = parsed["patterns_to_codify"].as_array() {
                    if !patterns.is_empty() {
                        b.push_str("## Patterns & Conventions\n\n");
                        for p in patterns {
                            if let Some(text) = p.as_str() {
                                b.push_str(&format!("- {text}\n"));
                            }
                        }
                        b.push('\n');
                    }
                }
            }
        }
        b
    };

    // Add changelog entry
    let changelog_entry = json!({
        "epic_code": epic_code,
        "sprint_number": sprint_number,
        "timestamp": chrono::Utc::now().to_rfc3339(),
        "summary": format!("Sprint {} of {}", sprint_number, epic_code),
    });

    // Merge with existing changelog (append, keep last 20)
    let mut changelog: Vec<serde_json::Value> =
        product["changelog"].as_array().cloned().unwrap_or_default();
    changelog.push(changelog_entry);
    if changelog.len() > 20 {
        changelog = changelog.split_off(changelog.len() - 20);
    }

    // Only update if brief actually changed
    if brief.trim() != existing_brief.trim() {
        let _ = client
            .patch::<_, serde_json::Value>(
                &format!("/v1/products/{product_id}"),
                &json!({
                    "brief": brief,
                    "changelog": changelog,
                }),
            )
            .await;
        tracing::info!(product_id, "Updated product brief (PRODUCTS.md)");
    }
}

/// Merge retro learnings and patterns into an existing rich brief without destroying
/// hand-seeded architecture/file map content. Replaces only the `## Key Learnings`
/// and `## Patterns & Conventions` sections, preserving everything else.
fn merge_into_rich_brief(existing: &str, learnings: &str, retro_output: Option<&str>) -> String {
    // Build the new Key Learnings section
    let new_learnings = if !learnings.is_empty() {
        format!("## Key Learnings\n\n{learnings}\n")
    } else {
        "## Key Learnings\n\n".to_string()
    };

    // Build the new Patterns & Conventions section
    let mut new_patterns = String::new();
    if let Some(retro) = retro_output {
        // Parse multi-story retro output and merge patterns
        let all_parsed = crate::json_extract::extract_all_json_objects(retro);
        let mut all_patterns = Vec::new();
        for parsed in &all_parsed {
            if let Some(patterns) = parsed["patterns_to_codify"].as_array() {
                all_patterns.extend(patterns.iter().cloned());
            }
        }
        if !all_patterns.is_empty() {
            new_patterns.push_str("## Patterns & Conventions\n\n");
            for p in &all_patterns {
                if let Some(text) = p.as_str() {
                    new_patterns.push_str(&format!("- {text}\n"));
                }
            }
            new_patterns.push('\n');
        }
    }
    if new_patterns.is_empty() {
        new_patterns = "## Patterns & Conventions\n".to_string();
    }

    // Replace sections in the existing brief
    let mut result = existing.to_string();

    // Replace Key Learnings section (everything from "## Key Learnings" to next "## " or EOF)
    result = replace_section(&result, "## Key Learnings", &new_learnings);

    // Replace Patterns & Conventions section
    result = replace_section(&result, "## Patterns & Conventions", &new_patterns);

    result
}

/// Replace a markdown section (from header to next same-level header or EOF).
fn replace_section(doc: &str, header: &str, replacement: &str) -> String {
    let Some(start) = doc.find(header) else {
        // Section doesn't exist — append it
        let mut result = doc.trim_end().to_string();
        result.push_str("\n\n");
        result.push_str(replacement);
        return result;
    };

    // Find the end of this section (next "## " header or EOF)
    let after_header = start + header.len();
    let end = doc[after_header..]
        .find("\n## ")
        .map(|pos| after_header + pos + 1) // +1 to include the newline
        .unwrap_or(doc.len());

    let mut result = String::with_capacity(doc.len());
    result.push_str(&doc[..start]);
    result.push_str(replacement);
    if end < doc.len() {
        result.push_str(&doc[end..]);
    }

    result
}

/// Generate a sprint changelog from builder results + ceremony outcomes.
/// Written to `.epic-runner/changelogs/EPIC-sprint-N.md` in the repo.
/// Returns the path if successfully written.
fn generate_sprint_changelog(
    epic: &Epic,
    sprint: &Sprint,
    builder_output: &crate::builder::BuilderOutput,
    results: &[engine::NodeResult],
) -> Option<std::path::PathBuf> {
    let mut changelog = String::new();
    changelog.push_str(&format!(
        "# Changelog — {} Sprint {}\n\n",
        epic.code, sprint.number
    ));
    changelog.push_str(&format!(
        "**Date:** {}\n",
        chrono::Utc::now().format("%Y-%m-%d")
    ));
    changelog.push_str(&format!("**Epic:** {} — {}\n", epic.code, epic.title));
    if let Some(ref goal) = sprint.goal {
        changelog.push_str(&format!("**Sprint Goal:** {}\n", goal));
    }
    changelog.push('\n');

    // Stories section
    changelog.push_str("## Stories\n\n");
    for story in &builder_output.stories {
        let status_mark = match story.status.as_str() {
            "done" => "[x]",
            "blocked" => "[-]",
            _ => "[ ]",
        };
        let code = story.code.as_deref().unwrap_or(&story.id);
        changelog.push_str(&format!(
            "- {} **{}** — {}\n",
            status_mark, code, story.status
        ));

        for task in &story.tasks {
            let mark = if task.done { "x" } else { " " };
            changelog.push_str(&format!("  - [{}] {}", mark, task.description));
            if let Some(ref outcome) = task.outcome {
                changelog.push_str(&format!(" — {}", outcome));
            }
            changelog.push('\n');
        }

        for ac in &story.acceptance_criteria {
            let mark = if ac.verified { "x" } else { " " };
            changelog.push_str(&format!("  - AC [{}] {}", mark, ac.criterion));
            if let Some(ref evidence) = ac.evidence {
                changelog.push_str(&format!(" — {}", evidence));
            }
            changelog.push('\n');
        }

        if let Some(ref reason) = story.blocked_reason {
            changelog.push_str(&format!("  - Blocked: {}\n", reason));
        }
        changelog.push('\n');
    }

    // Changed files section
    if !builder_output
        .stories
        .iter()
        .all(|s| s.changed_files.is_empty())
    {
        changelog.push_str("## Changed Files\n\n");
        let mut unique_files: Vec<&str> = builder_output
            .stories
            .iter()
            .flat_map(|s| s.changed_files.iter().map(|f| f.as_str()))
            .collect();
        unique_files.sort();
        unique_files.dedup();
        for f in &unique_files {
            changelog.push_str(&format!("- `{}`\n", f));
        }
        changelog.push('\n');
    }

    // Ceremony cost summary
    let total_cost: f64 = results.iter().filter_map(|r| r.cost_usd).sum();
    changelog.push_str("## Ceremony Costs\n\n");
    changelog.push_str("| Node | Cost |\n|------|------|\n");
    for r in results {
        if let Some(cost) = r.cost_usd {
            changelog.push_str(&format!("| {} | ${:.4} |\n", r.key, cost));
        }
    }
    changelog.push_str(&format!("| **Total** | **${:.4}** |\n\n", total_cost));

    // Write to disk
    let changelog_dir = std::path::PathBuf::from(".epic-runner/changelogs");
    if std::fs::create_dir_all(&changelog_dir).is_err() {
        return None;
    }
    let filename = format!("{}-sprint-{}.md", epic.code, sprint.number);
    let path = changelog_dir.join(&filename);
    std::fs::write(&path, &changelog).ok()?;
    Some(path)
}
