use clap::Args;

use super::CliConfig;
use crate::api_client::ApiClient;

#[derive(Args)]
pub struct StatusArgs {
    /// Product slug to show status for
    #[arg(long)]
    pub product: Option<String>,
}

pub async fn run(
    _args: StatusArgs,
    _client: &ApiClient,
    _cli: &CliConfig,
) -> Result<(), Box<dyn std::error::Error>> {
    eprintln!("TODO: dashboard status");
    Ok(())
}
