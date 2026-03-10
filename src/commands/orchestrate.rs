use clap::Args;

use super::CliConfig;
use crate::api_client::ApiClient;

#[derive(Args)]
pub struct OrchestrateArgs {
    /// Epic code to orchestrate (e.g. AUTH-001)
    pub epic_code: String,

    /// Maximum number of sprints
    #[arg(long, default_value = "20")]
    pub max_sprints: i32,

    /// Model override for all ceremonies
    #[arg(long)]
    pub model: Option<String>,

    /// Flow definition file (YAML) — overrides embedded default
    #[arg(long)]
    pub flow: Option<String>,

    /// Dry run — plan sprints without executing
    #[arg(long, default_value = "false")]
    pub dry_run: bool,
}

pub async fn run(
    _args: OrchestrateArgs,
    _client: &ApiClient,
    _cli: &CliConfig,
) -> Result<(), Box<dyn std::error::Error>> {
    eprintln!("TODO: orchestrate — thin supervisor loop");
    Ok(())
}
