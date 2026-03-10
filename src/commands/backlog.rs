use clap::{Args, Subcommand};

use super::CliConfig;
use crate::api_client::ApiClient;

#[derive(Args)]
pub struct BacklogArgs {
    #[command(subcommand)]
    pub action: BacklogAction,
}

#[derive(Subcommand)]
pub enum BacklogAction {
    /// Add a story to the backlog
    Add {
        #[arg(long)]
        title: String,
        #[arg(long)]
        product: String,
        #[arg(long)]
        epic: Option<String>,
        #[arg(long)]
        description: Option<String>,
        #[arg(long)]
        points: Option<i32>,
    },
    /// List backlog stories
    List {
        #[arg(long)]
        product: Option<String>,
        #[arg(long)]
        epic: Option<String>,
        #[arg(long)]
        status: Option<String>,
    },
    /// Show story details
    Show { id: String },
    /// Transition story status
    Transition {
        id: String,
        #[arg(long)]
        status: String,
    },
    /// Delete a story
    Delete { id: String },
}

pub async fn run(
    _args: BacklogArgs,
    _client: &ApiClient,
    _cli: &CliConfig,
) -> Result<(), Box<dyn std::error::Error>> {
    eprintln!("TODO: backlog CRUD");
    Ok(())
}
