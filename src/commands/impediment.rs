use clap::{Args, Subcommand};

use super::CliConfig;
use crate::api_client::ApiClient;

#[derive(Args)]
pub struct ImpedimentArgs {
    #[command(subcommand)]
    pub action: ImpedimentAction,
}

#[derive(Subcommand)]
pub enum ImpedimentAction {
    /// List impediments
    List {
        #[arg(long)]
        product: Option<String>,
        #[arg(long)]
        epic: Option<String>,
        #[arg(long)]
        status: Option<String>,
    },
    /// Raise a new impediment
    Raise {
        #[arg(long)]
        blocking_epic: String,
        #[arg(long)]
        blocked_by: Option<String>,
        #[arg(long)]
        title: String,
        #[arg(long)]
        description: Option<String>,
    },
    /// Resolve an impediment
    Resolve { id: String },
}

pub async fn run(
    _args: ImpedimentArgs,
    _client: &ApiClient,
    _cli: &CliConfig,
) -> Result<(), Box<dyn std::error::Error>> {
    eprintln!("TODO: impediment management");
    Ok(())
}
