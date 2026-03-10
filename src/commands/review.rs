use clap::Args;

use super::CliConfig;
use crate::api_client::{ApiClient, DataWrapper};
use crate::executor::{self, ExecutorConfig};

#[derive(Args)]
pub struct ReviewArgs {
    /// Epic code to review
    pub epic_code: String,

    /// Model to use
    #[arg(long, default_value = "sonnet")]
    pub model: String,
}

pub async fn run(
    args: ReviewArgs,
    client: &ApiClient,
    _cli: &CliConfig,
) -> Result<(), Box<dyn std::error::Error>> {
    // Load epic
    let epics: DataWrapper<Vec<serde_json::Value>> = client
        .get(&format!("/v1/epics?code={}", args.epic_code))
        .await?;
    let epic = epics
        .data
        .first()
        .ok_or(format!("Epic '{}' not found", args.epic_code))?;

    let intent = epic["intent"].as_str().unwrap_or("?");
    let title = epic["title"].as_str().unwrap_or("?");
    let epic_id = epic["id"].as_str().unwrap_or("?");

    // Load sprints
    let sprints: DataWrapper<Vec<serde_json::Value>> = client
        .get(&format!("/v1/sprints?epic_id={epic_id}"))
        .await?;

    let sprint_summary = serde_json::to_string_pretty(&sprints.data)?;

    let config = ExecutorConfig {
        model: args.model,
        effort: "high".to_string(),
        worktree_name: String::new(),
        session_id: uuid::Uuid::new_v4(),
        repo_path: ".".to_string(),
        add_dirs: vec![],
        system_prompt: Some(format!(
            "You are a business reviewer for epic {} — {}.\nIntent: {}\nReview all sprint results and produce a business review.",
            args.epic_code, title, intent
        )),
        prompt: format!(
            "Business review for epic {}.\n\nSprint history:\n{}\n\nProvide:\n1. Intent achievement assessment\n2. Sprint-by-sprint progress\n3. Quality observations\n4. Recommendations",
            args.epic_code, sprint_summary
        ),
        chrome: true,
        max_budget_usd: Some(3.0),
        allowed_tools: None,
        resume_session: false,
        agent: None,
        heartbeat_timeout_secs: 300,
    };

    eprintln!("Running business review for epic {}...", args.epic_code);
    let result = executor::execute(config, |e| {
        eprintln!("[review/{}] {}", e.event_type_str(), e.summary);
    })
    .await?;

    if let Some(text) = result.result_text {
        println!("{text}");
    }
    if let Some(cost) = result.cost_usd {
        eprintln!("\nReview cost: ${cost:.2}");
    }

    Ok(())
}
