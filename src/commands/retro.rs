use clap::Args;

use super::CliConfig;
use crate::api_client::ApiClient;

#[derive(Args)]
pub struct RetroArgs {
    /// Sprint ID to retrospect
    pub sprint_id: String,
}

pub async fn run(
    _args: RetroArgs,
    _client: &ApiClient,
    _cli: &CliConfig,
) -> Result<(), Box<dyn std::error::Error>> {
    eprintln!("TODO: standalone retrospective");
    Ok(())
}
