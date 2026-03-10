use clap::{Args, Subcommand};
use comfy_table::{Cell, Table};
use serde_json::json;

use super::CliConfig;
use crate::api_client::{ApiClient, DataWrapper};
use crate::types::Product;

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
    Show { id: String },
}

pub async fn run(
    args: ProductArgs,
    client: &ApiClient,
    cli: &CliConfig,
) -> Result<(), Box<dyn std::error::Error>> {
    let project_id = crate::config::resolve_project_id()?;

    match args.action {
        ProductAction::Create {
            name,
            slug,
            repo_path,
            description,
        } => {
            let body = json!({
                "name": name,
                "slug": slug,
                "repo_path": repo_path,
                "description": description,
            });
            let resp: DataWrapper<serde_json::Value> = client
                .post(&format!("/v1/data/{project_id}/products"), &body)
                .await?;
            if cli.json {
                println!("{}", serde_json::to_string_pretty(&resp.data)?);
            } else {
                let id = resp.data["id"].as_str().unwrap_or("?");
                eprintln!("Product created: {id} ({name})");
            }
        }
        ProductAction::List => {
            let resp: DataWrapper<Vec<serde_json::Value>> = client
                .get(&format!("/v1/data/{project_id}/products"))
                .await?;
            if cli.json {
                println!("{}", serde_json::to_string_pretty(&resp.data)?);
            } else {
                let mut table = Table::new();
                table.set_header(vec!["ID", "Name", "Slug", "Repo Path"]);
                for row in &resp.data {
                    let p: Product = serde_json::from_value(row.clone())?;
                    table.add_row(vec![
                        Cell::new(&p.id.to_string()[..8]),
                        Cell::new(&p.name),
                        Cell::new(&p.slug),
                        Cell::new(&p.repo_path),
                    ]);
                }
                println!("{table}");
            }
        }
        ProductAction::Show { id } => {
            let resp: DataWrapper<serde_json::Value> = client
                .get(&format!("/v1/data/{project_id}/products/{id}"))
                .await?;
            println!("{}", serde_json::to_string_pretty(&resp.data)?);
        }
    }

    Ok(())
}
