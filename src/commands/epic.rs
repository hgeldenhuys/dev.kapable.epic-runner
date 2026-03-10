use clap::{Args, Subcommand};

use super::CliConfig;
use crate::api_client::ApiClient;

#[derive(Args)]
pub struct EpicArgs {
    #[command(subcommand)]
    pub action: EpicAction,
}

#[derive(Subcommand)]
pub enum EpicAction {
    /// Create a new epic
    Create {
        #[arg(long)]
        product: String,
        #[arg(long)]
        domain: String,
        #[arg(long)]
        title: String,
        #[arg(long)]
        intent: String,
    },
    /// List epics
    List {
        #[arg(long)]
        product: Option<String>,
        #[arg(long)]
        status: Option<String>,
    },
    /// Show epic details
    Show { code: String },
    /// Close an epic
    Close { code: String },
    /// Abandon an epic
    Abandon { code: String },
}

pub async fn run(
    _args: EpicArgs,
    _client: &ApiClient,
    _cli: &CliConfig,
) -> Result<(), Box<dyn std::error::Error>> {
    eprintln!("TODO: epic CRUD + worktree lifecycle");
    Ok(())
}
