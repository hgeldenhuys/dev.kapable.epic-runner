use clap::Args;

use super::CliConfig;
use crate::api_client::ApiClient;

#[derive(Args)]
pub struct InitArgs {
    /// Project name
    #[arg(long)]
    pub name: Option<String>,
}

pub async fn run(
    _args: InitArgs,
    _client: &ApiClient,
    _cli: &CliConfig,
) -> Result<(), Box<dyn std::error::Error>> {
    eprintln!("TODO: init — provision Kapable project + tables + upload default flow");
    Ok(())
}
