use clap::{Args, Subcommand};

use super::CliConfig;
use crate::api_client::ApiClient;

#[derive(Args)]
pub struct ProductArgs {
    #[command(subcommand)]
    pub action: ProductAction,
}

#[derive(Subcommand)]
pub enum ProductAction {
    /// Create a new product
    Create {
        #[arg(long)]
        name: String,
        #[arg(long)]
        slug: String,
        #[arg(long)]
        repo_path: String,
        #[arg(long)]
        description: Option<String>,
    },
    /// List all products
    List,
    /// Show product details
    Show {
        /// Product ID or slug
        id: String,
    },
}

pub async fn run(
    _args: ProductArgs,
    _client: &ApiClient,
    _cli: &CliConfig,
) -> Result<(), Box<dyn std::error::Error>> {
    eprintln!("TODO: product CRUD");
    Ok(())
}
