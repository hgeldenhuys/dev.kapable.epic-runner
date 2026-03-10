use clap::Args;

use super::CliConfig;
use crate::api_client::ApiClient;

#[derive(Args)]
pub struct SprintRunArgs {
    /// Sprint ID to execute
    pub sprint_id: String,

    /// Flow definition file (YAML) — overrides embedded default
    #[arg(long)]
    pub flow: Option<String>,

    /// Model override for all ceremonies
    #[arg(long)]
    pub model: Option<String>,

    /// Effort override
    #[arg(long)]
    pub effort: Option<String>,
}

pub async fn run(
    _args: SprintRunArgs,
    _client: &ApiClient,
    _cli: &CliConfig,
) -> Result<(), Box<dyn std::error::Error>> {
    eprintln!("TODO: sprint-run — fat ceremony executor");
    Ok(())
}
