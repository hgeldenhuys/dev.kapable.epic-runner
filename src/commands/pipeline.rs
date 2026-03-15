//! Pipeline subcommands for agent-facing flexibility.
//!
//! `epic-runner pipeline generate EPIC_CODE` — outputs pipeline YAML to stdout
//! `epic-runner pipeline submit FILE` — submits YAML to pipeline API

use clap::{Args, Subcommand};
use uuid::Uuid;

use super::CliConfig;
use crate::api_client::{ApiClient, DataWrapper};
use crate::pipeline_generator::{
    build_hooks_settings, generate_sprint_pipeline, SprintPipelineContext, StoryContext,
};
use crate::types::*;

#[derive(Args)]
pub struct PipelineArgs {
    #[command(subcommand)]
    pub action: PipelineAction,
}

#[derive(Subcommand)]
pub enum PipelineAction {
    /// Generate sprint pipeline YAML for an epic (outputs to stdout)
    Generate(GenerateArgs),
    /// Submit a pipeline YAML file for execution
    Submit(SubmitArgs),
}

#[derive(Args)]
pub struct GenerateArgs {
    /// Epic code (e.g. AUTH-001)
    pub epic_code: String,

    /// Sprint number (default: next available)
    #[arg(long)]
    pub sprint: Option<i32>,

    /// Run stories in parallel (default: serial)
    #[arg(long, default_value = "false")]
    pub parallel: bool,

    /// Model override for all agent steps
    #[arg(long)]
    pub model: Option<String>,

    /// Budget override per story (USD)
    #[arg(long)]
    pub budget: Option<f64>,

    /// Additional directories to add to agent sessions
    #[arg(long)]
    pub add_dir: Vec<String>,
}

#[derive(Args)]
pub struct SubmitArgs {
    /// Path to pipeline YAML file
    pub file: String,

    /// Poll for completion (default: submit and return immediately)
    #[arg(long, default_value = "false")]
    pub wait: bool,

    /// Git repo URL for agent workspace (agent daemon clones/pulls this)
    #[arg(long)]
    pub repo_url: Option<String>,

    /// Git branch/ref for the workspace checkout
    #[arg(long)]
    pub repo_ref: Option<String>,
}

pub async fn run(
    args: PipelineArgs,
    client: &ApiClient,
    cli: &CliConfig,
) -> Result<(), Box<dyn std::error::Error>> {
    match args.action {
        PipelineAction::Generate(args) => generate(args, client, cli).await,
        PipelineAction::Submit(args) => submit(args, client, cli).await,
    }
}

async fn generate(
    args: GenerateArgs,
    client: &ApiClient,
    _cli: &CliConfig,
) -> Result<(), Box<dyn std::error::Error>> {
    // 1. Load epic
    let epics: DataWrapper<Vec<serde_json::Value>> = client
        .get_with_params("/v1/epics", &[("code", args.epic_code.as_str())])
        .await?;
    let epic_data = epics
        .data
        .iter()
        .find(|e| e["code"].as_str() == Some(args.epic_code.as_str()))
        .ok_or(format!("Epic {} not found", args.epic_code))?;
    let epic: Epic = serde_json::from_value(epic_data.clone())?;

    // 2. Load product
    let product: serde_json::Value = client
        .get(&format!("/v1/products/{}", epic.product_id))
        .await?;

    // 3. Load stories for this epic (filter by epic_code, not product)
    let stories_resp: DataWrapper<Vec<serde_json::Value>> = client
        .get_with_params("/v1/stories", &[("epic_code", args.epic_code.as_str())])
        .await?;

    let story_contexts: Vec<StoryContext> = stories_resp
        .data
        .iter()
        .filter(|s| {
            // Client-side filter: server may ignore the epic_code param
            let matches_epic = s["epic_code"].as_str() == Some(args.epic_code.as_str());
            let status = s["status"].as_str().unwrap_or("");
            let eligible = status == "ready" || status == "planned";
            let has_code = s["code"].as_str().is_some_and(|c| !c.is_empty());
            matches_epic && eligible && has_code
        })
        .map(|s| StoryContext {
            code: s["code"].as_str().unwrap_or("?").to_string(),
            id: s["id"].as_str().unwrap_or_default().to_string(),
            title: s["title"].as_str().unwrap_or("").to_string(),
            description: s["description"].as_str().unwrap_or("").to_string(),
            acceptance_criteria: extract_string_array(&s["acceptance_criteria"]),
            tasks: extract_string_array(&s["tasks"]),
            story_json: s.clone(),
        })
        .collect();

    if story_contexts.is_empty() {
        return Err(format!("No ready/planned stories found for epic {}", args.epic_code).into());
    }

    eprintln!(
        "[generate] Found {} stories for epic {}",
        story_contexts.len(),
        args.epic_code
    );

    // Hooks live in the epic-runner directory (CWD where generate runs)
    let hooks_dir = std::env::current_dir()?.display().to_string();
    let hooks_settings = build_hooks_settings(&hooks_dir);

    let ctx = SprintPipelineContext {
        epic_code: epic.code.clone(),
        sprint_number: args.sprint.unwrap_or(1),
        session_id: Uuid::new_v4().to_string(),
        stories: story_contexts,
        product_brief: product["brief"].as_str().map(String::from),
        epic_intent: epic.intent.clone(),
        builder_agent_content: load_agent_content("builder"),
        judge_agent_content: load_agent_content("code-judge"),
        scrum_master_agent_content: load_agent_content("scrum-master"),
        model_override: args.model,
        effort_override: None,
        budget_override: args.budget,
        add_dirs: args.add_dir,
        hooks_settings: Some(hooks_settings),
        deploy_profile: product["deploy_profile"]
            .as_str()
            .unwrap_or("none")
            .to_string(),
        deploy_app_id: product["deploy_app_id"].as_str().map(String::from),
        api_url: client.base_url.clone(),
        api_key: client.api_key().to_string(),
        product_definition_of_done: product["definition_of_done"].as_str().map(String::from),
        previous_learnings: None,
        serial: !args.parallel,
        epic_branch: format!("epic/{}", args.epic_code.to_lowercase()),
    };

    let pipeline = generate_sprint_pipeline(&ctx);

    // Serialize to YAML and print to stdout
    let yaml = serde_yaml::to_string(&pipeline)?;
    println!("{}", yaml);

    eprintln!(
        "[generate] Pipeline '{}' with {} stages written to stdout",
        pipeline.name,
        pipeline.stages.len()
    );

    Ok(())
}

async fn submit(
    args: SubmitArgs,
    client: &ApiClient,
    _cli: &CliConfig,
) -> Result<(), Box<dyn std::error::Error>> {
    let yaml_content = std::fs::read_to_string(&args.file)?;
    let pipeline: kapable_pipeline::types::PipelineDefinition =
        serde_yaml::from_str(&yaml_content)?;

    eprintln!(
        "[submit] Submitting pipeline '{}' ({} stages)...",
        pipeline.name,
        pipeline.stages.len()
    );

    let result = crate::pipeline_submitter::submit_pipeline(
        client,
        &pipeline,
        args.repo_url,
        args.repo_ref,
        None,
        None,
    )
    .await?;

    eprintln!(
        "[submit] Submitted: run_id={}, job_id={}",
        result.run_id, result.job_id
    );

    if args.wait {
        let final_status =
            crate::pipeline_submitter::wait_for_completion(client, result.run_id, 10).await?;
        eprintln!("[submit] Final status: {}", final_status.status);
        if final_status.status == "failed" {
            if let Some(ref err) = final_status.error_message {
                eprintln!("[submit] Error: {}", err);
            }
            std::process::exit(1);
        }
    }

    // Print run_id to stdout for scripting
    println!("{}", result.run_id);
    Ok(())
}

fn extract_string_array(val: &serde_json::Value) -> Vec<String> {
    val.as_array()
        .map(|a| {
            let mut v = Vec::new();
            for item in a {
                if let Some(text) = item.as_str() {
                    v.push(text.to_string());
                } else if let Some(obj) = item.as_object() {
                    // Handle structured ACs: extract criterion or title field
                    if let Some(text) = obj
                        .get("criterion")
                        .or(obj.get("title"))
                        .and_then(|v| v.as_str())
                    {
                        v.push(text.to_string());
                    }
                }
            }
            v
        })
        .unwrap_or_default()
}

fn load_agent_content(name: &str) -> String {
    let fake_repo = std::path::PathBuf::from("/tmp/epic-runner-pipeline-agents");
    crate::agents::resolve_agent_path(name, &fake_repo)
        .and_then(|p| std::fs::read_to_string(p).ok())
        .unwrap_or_else(|| format!("You are a {} agent.", name))
}
