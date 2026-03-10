use clap::{Args, Subcommand};

use super::CliConfig;
use crate::api_client::ApiClient;

#[derive(Args)]
pub struct SprintArgs {
    #[command(subcommand)]
    pub action: SprintAction,
}

#[derive(Subcommand)]
pub enum SprintAction {
    /// List sprints for an epic
    List {
        #[arg(long)]
        epic: String,
    },
    /// Show sprint details
    Show { id: String },
}

pub async fn run(
    _args: SprintArgs,
    _client: &ApiClient,
    _cli: &CliConfig,
) -> Result<(), Box<dyn std::error::Error>> {
    eprintln!("TODO: sprint CRUD");
    Ok(())
}
