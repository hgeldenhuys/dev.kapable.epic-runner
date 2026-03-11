use clap::Args;

use super::CliConfig;
use crate::api_client::ApiClient;
use crate::executor::{self, ExecutorConfig};
use crate::types::SprintEvent;

#[derive(Args)]
pub struct RetroArgs {
    /// Sprint ID to retrospect
    pub sprint_id: String,

    /// Model to use
    #[arg(long, default_value = "sonnet")]
    pub model: String,
}

pub async fn run(
    args: RetroArgs,
    client: &ApiClient,
    _cli: &CliConfig,
) -> Result<(), Box<dyn std::error::Error>> {
    // Load sprint
    let sprint: serde_json::Value = client
        .get(&format!("/v1/er_sprints/{}", args.sprint_id))
        .await?;

    let ceremony_log = sprint["ceremony_log"]
        .as_array()
        .map(|a| serde_json::to_string_pretty(a).unwrap_or_default())
        .unwrap_or_else(|| "No ceremony log".to_string());

    let epic_id = sprint["epic_id"].as_str().unwrap_or("?");
    let sprint_number = sprint["number"].as_i64().unwrap_or(0);

    let config = ExecutorConfig {
        model: args.model,
        effort: "high".to_string(),
        worktree_name: String::new(),
        session_id: uuid::Uuid::new_v4(),
        repo_path: ".".to_string(),
        add_dirs: vec![],
        system_prompt: Some(format!(
            "You are the Scrum Master observer. Analyze sprint {} of epic {}.",
            sprint_number, epic_id
        )),
        prompt: format!(
            "Sprint {} retro.\n\nCeremony log:\n{}\n\nProvide structured retro:\n1. What went well\n2. Friction points\n3. Action items\n4. Discovered backlog items\n\nOutput as JSON matching RetroOutput schema.",
            sprint_number, ceremony_log
        ),
        chrome: false,
        max_budget_usd: Some(2.0),
        allowed_tools: Some(vec![
            "Read".to_string(),
            "Glob".to_string(),
            "Grep".to_string(),
        ]),
        resume_session: false,
        agent: None,
        heartbeat_timeout_secs: 180,
    };

    eprintln!("Running retrospective for sprint {}...", args.sprint_id);
    let result = executor::execute(config, &|e: SprintEvent| {
        eprintln!("[retro/{}] {}", e.event_type_str(), e.summary);
    })
    .await?;

    if let Some(text) = &result.result_text {
        // Try to parse as structured retro
        if let Some(retro) = crate::scrum_master::parse_retro(Some(text)) {
            println!("{}", serde_json::to_string_pretty(&retro)?);
        } else {
            println!("{text}");
        }
    }
    if let Some(cost) = result.cost_usd {
        eprintln!("\nRetro cost: ${cost:.2}");
    }

    Ok(())
}
