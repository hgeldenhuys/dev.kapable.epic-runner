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

    /// Model override
    #[arg(long, default_value = "opus")]
    pub model: String,

    /// Effort override (low, medium, high)
    #[arg(long, default_value = "high")]
    pub effort: String,

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
            &json!({ "status": "executing", "started_at": chrono::Utc::now().to_rfc3339() }),
        )
        .await
    {
        tracing::warn!(error = %e, "Failed to update sprint status to executing — continuing");
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
        timestamp: chrono::Utc::now(),
    });

    // 7. Build flow context
    let stories = sprint.stories.clone().unwrap_or(json!([]));
    let ctx = engine::FlowContext {
        epic: epic.clone(),
        sprint: sprint.clone(),
        stories,
        repo_path: crate::repo_resolver::resolve_product_repo(
            product.repo_url.as_deref(),
            &product.repo_path,
        )
        .map_err(|e| -> Box<dyn std::error::Error> { e.into() })?,
        model_override: Some(args.model.clone()),
        effort_override: Some(args.effort.clone()),
        budget_override: args.budget_override,
        add_dirs: args.add_dir.clone(),
        previous_learnings,
    };

    // 8. Execute the ceremony flow (nodes at each BFS level run in parallel)
    let results = match engine::execute_flow(&flow, &ctx, &sink).await {
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

    if let Err(e) = client
        .patch::<_, serde_json::Value>(
            &format!("/v1/er_sprints/{}", sprint.id),
            &json!({
                "status": final_status,
                "finished_at": chrono::Utc::now().to_rfc3339(),
                "ceremony_log": ceremony_log,
                "velocity": velocity,
                "cost_usd": total_cost,
                "ceremony_costs": ceremony_costs,
            }),
        )
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

    // Parse retro JSON output (may be wrapped in markdown code fences)
    let json_str = output
        .trim()
        .strip_prefix("```json")
        .or_else(|| output.trim().strip_prefix("```"))
        .unwrap_or(output.trim())
        .trim_end_matches("```")
        .trim();

    let parsed: serde_json::Value = match serde_json::from_str(json_str) {
        Ok(v) => v,
        Err(e) => {
            tracing::warn!(error = %e, "Could not parse retro output as JSON — skipping learnings save");
            return;
        }
    };

    let action_items = parsed["action_items"].clone();
    let patterns = parsed["patterns_to_codify"].clone();
    let friction = parsed["friction_points"].clone();

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

    let json_str = output
        .trim()
        .strip_prefix("```json")
        .or_else(|| output.trim().strip_prefix("```"))
        .unwrap_or(output.trim())
        .trim_end_matches("```")
        .trim();

    let parsed: serde_json::Value = match serde_json::from_str(json_str) {
        Ok(v) => v,
        Err(_) => return,
    };

    let items = match parsed["discovered_work"].as_array() {
        Some(arr) if !arr.is_empty() => arr,
        _ => return,
    };

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

    // Build updated brief
    let mut brief = format!(
        "# {name}\n\n\
        **Slug:** {slug}\n\
        **Description:** {description}\n\n"
    );

    // Include accumulated learnings
    if !learnings.is_empty() {
        brief.push_str("## Key Learnings\n\n");
        brief.push_str(learnings);
        brief.push('\n');
    }

    // Append latest retro insights
    if let Some(retro) = retro_output {
        let json_str = retro
            .trim()
            .strip_prefix("```json")
            .or_else(|| retro.trim().strip_prefix("```"))
            .unwrap_or(retro.trim())
            .trim_end_matches("```")
            .trim();

        if let Ok(parsed) = serde_json::from_str::<serde_json::Value>(json_str) {
            if let Some(patterns) = parsed["patterns_to_codify"].as_array() {
                if !patterns.is_empty() {
                    brief.push_str("## Patterns & Conventions\n\n");
                    for p in patterns {
                        if let Some(text) = p.as_str() {
                            brief.push_str(&format!("- {text}\n"));
                        }
                    }
                    brief.push('\n');
                }
            }
        }
    }

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
