use clap::Args;
use serde_json::json;

use super::CliConfig;
use crate::api_client::{ApiClient, DataWrapper};
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
}

pub async fn run(
    args: SprintRunArgs,
    client: &ApiClient,
    _cli: &CliConfig,
) -> Result<(), Box<dyn std::error::Error>> {
    let project_id = crate::config::resolve_project_id()?;

    // 1. Load sprint from DB
    let sprint_resp: DataWrapper<serde_json::Value> = client
        .get(&format!("/v1/data/{project_id}/sprints/{}", args.sprint_id))
        .await?;
    let sprint: Sprint = serde_json::from_value(sprint_resp.data)?;

    // 2. Load epic
    let epic_resp: DataWrapper<serde_json::Value> = client
        .get(&format!("/v1/data/{project_id}/epics/{}", sprint.epic_id))
        .await?;
    let epic: Epic = serde_json::from_value(epic_resp.data)?;

    // 3. Load product for repo_path
    let product_resp: DataWrapper<Vec<serde_json::Value>> = client
        .get(&format!(
            "/v1/data/{project_id}/products?id={}",
            epic.product_id
        ))
        .await?;
    let product: Product = serde_json::from_value(
        product_resp
            .data
            .first()
            .ok_or("Product not found")?
            .clone(),
    )?;

    // 4. Load ceremony flow
    let config =
        crate::config::find_project_config().and_then(|p| crate::config::read_config(&p).ok());
    let flow = loader::load_flow(
        args.flow.as_deref(),
        config.as_ref().and_then(|c| c.ceremony_flow_id()),
    )
    .await?;

    eprintln!(
        "[sprint-run] Sprint {} of epic {}",
        sprint.number, epic.code
    );
    eprintln!("[sprint-run] Flow: {} v{}", flow.name, flow.version);
    eprintln!("[sprint-run] Nodes: {}", flow.nodes.len());

    // 5. Update sprint status to executing
    let _: DataWrapper<serde_json::Value> = client
        .patch(
            &format!("/v1/data/{project_id}/sprints/{}", sprint.id),
            &json!({ "status": "executing", "started_at": chrono::Utc::now().to_rfc3339() }),
        )
        .await?;

    // 6. Build flow context
    let stories = sprint.stories.clone().unwrap_or(json!([]));
    let ctx = engine::FlowContext {
        epic: epic.clone(),
        sprint: sprint.clone(),
        stories,
        repo_path: product.repo_path.clone(),
        model_override: Some(args.model.clone()),
        effort_override: Some(args.effort.clone()),
        add_dirs: args.add_dir.clone(),
        project_id: project_id.clone(),
    };

    // 7. Execute the ceremony flow
    let results = engine::execute_flow(&flow, &ctx).await?;

    // 8. Determine outcome
    let judge_verdict = results.iter().find_map(|r| r.judge_verdict.clone());
    let intent_satisfied = crate::judge::evaluate_verdict(&judge_verdict);
    let any_impediment = results.iter().any(|r| r.impediment_raised);
    let total_cost: f64 = results.iter().filter_map(|r| r.cost_usd).sum();

    let final_status = if any_impediment {
        "blocked"
    } else if intent_satisfied {
        "completed"
    } else {
        "failed"
    };

    // 9. Write results to DB
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

    let _: DataWrapper<serde_json::Value> = client
        .patch(
            &format!("/v1/data/{project_id}/sprints/{}", sprint.id),
            &json!({
                "status": final_status,
                "finished_at": chrono::Utc::now().to_rfc3339(),
                "ceremony_log": ceremony_log,
            }),
        )
        .await?;

    // 10. Persist supervisor decisions + rubber duck sessions (best-effort)
    for result in &results {
        for decision in &result.supervisor_decisions {
            let _: Result<DataWrapper<serde_json::Value>, _> = client
                .post(
                    &format!("/v1/data/{project_id}/supervisor_decisions"),
                    &serde_json::to_value(decision)?,
                )
                .await;
        }
        for duck in &result.rubber_duck_sessions {
            let _: Result<DataWrapper<serde_json::Value>, _> = client
                .post(
                    &format!("/v1/data/{project_id}/rubber_duck_sessions"),
                    &serde_json::to_value(duck)?,
                )
                .await;
        }
    }

    eprintln!();
    eprintln!(
        "[sprint-run] Sprint {} finished: {}",
        sprint.number, final_status
    );
    eprintln!("[sprint-run] Intent satisfied: {}", intent_satisfied);
    eprintln!("[sprint-run] Total cost: ${:.2}", total_cost);

    // Exit with appropriate code for orchestrator to read
    if any_impediment {
        std::process::exit(2); // blocked
    } else if !intent_satisfied {
        std::process::exit(1); // failed but not blocked
    }
    // exit(0) = success

    Ok(())
}
