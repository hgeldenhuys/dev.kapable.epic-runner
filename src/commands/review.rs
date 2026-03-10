use clap::Args;

use super::CliConfig;
use crate::api_client::ApiClient;

#[derive(Args)]
pub struct ReviewArgs {
    /// Epic code to review
    pub epic_code: String,
}

pub async fn run(
    _args: ReviewArgs,
    _client: &ApiClient,
    _cli: &CliConfig,
) -> Result<(), Box<dyn std::error::Error>> {
    eprintln!("TODO: standalone business review");
    Ok(())
}
