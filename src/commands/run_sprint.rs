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

    /// Effort override
    #[arg(long, default_value = "max")]
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

    // 4. Load ceremony flow
    let config =
        crate::config::find_project_config().and_then(|p| crate::config::read_config(&p).ok());
    let flow = loader::load_flow(
        args.flow.as_deref(),
        config.as_ref().and_then(|c| c.ceremony_flow_id()),
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
        repo_path: product.repo_path.clone(),
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
            tracing::error!(error = %e, "Flow execution failed — exiting with failure");
            sink.emit(SprintEvent {
                sprint_id: sprint.session_id,
                event_type: SprintEventType::Failed,
                summary: format!("Sprint {} crashed: {}", sprint.number, e),
                detail: None,
                timestamp: chrono::Utc::now(),
            });
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

    let final_status = if any_impediment {
        "blocked"
    } else if intent_satisfied {
        "completed"
    } else {
        "failed"
    };

    // Stream sprint finished event
    sink.emit(SprintEvent {
        sprint_id: sprint.session_id,
        event_type: if intent_satisfied {
            SprintEventType::Completed
        } else {
            SprintEventType::Failed
        },
        summary: format!("Sprint {} finished: {}", sprint.number, final_status),
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

    if let Err(e) = client
        .patch::<_, serde_json::Value>(
            &format!("/v1/er_sprints/{}", sprint.id),
            &json!({
                "status": final_status,
                "finished_at": chrono::Utc::now().to_rfc3339(),
                "ceremony_log": ceremony_log,
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
    }

    // 12. Flush event sink — drop sender, wait for background writer to finish
    drop(sink);
    let _ = sink_handle.await;

    eprintln!();
    let status_colored = match final_status {
        "completed" => "completed".green().bold().to_string(),
        "blocked" => "BLOCKED".red().bold().to_string(),
        _ => "failed".yellow().bold().to_string(),
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
        "false".red().to_string()
    };
    eprintln!(
        "{} Intent satisfied: {}",
        "[sprint-run]".dimmed(),
        satisfied_str
    );

    // Exit with appropriate code for orchestrator to read
    if any_impediment {
        std::process::exit(2); // blocked
    } else if !intent_satisfied {
        std::process::exit(1); // failed but not blocked
    }
    // exit(0) = success

    Ok(())
}

/// Load learnings from previous sprints of this epic.
/// Returns a formatted string for injection into the {{previous_learnings}} template variable.
/// Best-effort: returns empty string on any failure (network, parse, no data).
async fn load_previous_learnings(client: &ApiClient, epic_code: &str) -> String {
    use crate::api_client::DataWrapper;

    let resp: Result<DataWrapper<Vec<serde_json::Value>>, _> =
        client.get("/v1/sprint_learnings").await;

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
