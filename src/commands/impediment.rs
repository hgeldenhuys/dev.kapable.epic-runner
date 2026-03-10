use clap::{Args, Subcommand};
use comfy_table::{Cell, Table};
use serde_json::json;

use super::CliConfig;
use crate::api_client::{ApiClient, DataWrapper};

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
        #[arg(long)]
        product: String,
    },
    /// Resolve an impediment
    Resolve { id: String },
}

pub async fn run(
    args: ImpedimentArgs,
    client: &ApiClient,
    cli: &CliConfig,
) -> Result<(), Box<dyn std::error::Error>> {
    let project_id = crate::config::resolve_project_id()?;

    match args.action {
        ImpedimentAction::List { epic, status } => {
            let mut query = format!("/v1/data/{project_id}/impediments?");
            if let Some(e) = &epic {
                query.push_str(&format!("blocking_epic={e}&"));
            }
            if let Some(s) = &status {
                query.push_str(&format!("status={s}&"));
            }

            let resp: DataWrapper<Vec<serde_json::Value>> = client.get(&query).await?;

            if cli.json {
                println!("{}", serde_json::to_string_pretty(&resp.data)?);
            } else {
                let mut table = Table::new();
                table.set_header(vec!["ID", "Blocking", "Blocked By", "Title", "Status"]);
                for row in &resp.data {
                    table.add_row(vec![
                        Cell::new(row["id"].as_str().map(|s| &s[..8]).unwrap_or("?")),
                        Cell::new(row["blocking_epic"].as_str().unwrap_or("?")),
                        Cell::new(row["blocked_by_epic"].as_str().unwrap_or("-")),
                        Cell::new(row["title"].as_str().unwrap_or("?")),
                        Cell::new(row["status"].as_str().unwrap_or("?")),
                    ]);
                }
                println!("{table}");
                eprintln!("{} impediment(s)", resp.data.len());
            }
        }
        ImpedimentAction::Raise {
            blocking_epic,
            blocked_by,
            title,
            description,
            product,
        } => {
            // Resolve product slug
            let products: DataWrapper<Vec<serde_json::Value>> = client
                .get(&format!("/v1/data/{project_id}/products?slug={product}"))
                .await?;
            let product_id = products
                .data
                .first()
                .and_then(|p| p["id"].as_str())
                .ok_or(format!("Product '{product}' not found"))?;

            let body = json!({
                "product_id": product_id,
                "blocking_epic": blocking_epic,
                "blocked_by_epic": blocked_by,
                "title": title,
                "description": description,
                "status": "open",
            });
            let resp: DataWrapper<serde_json::Value> = client
                .post(&format!("/v1/data/{project_id}/impediments"), &body)
                .await?;

            if cli.json {
                println!("{}", serde_json::to_string_pretty(&resp.data)?);
            } else {
                let id = resp.data["id"].as_str().unwrap_or("?");
                eprintln!("Impediment raised: {id}");
                eprintln!("  Blocking: {blocking_epic}");
                if let Some(by) = &blocked_by {
                    eprintln!("  Blocked by: {by}");
                }
            }
        }
        ImpedimentAction::Resolve { id } => {
            let body = json!({
                "status": "resolved",
                "resolved_at": chrono::Utc::now().to_rfc3339(),
            });
            let _: DataWrapper<serde_json::Value> = client
                .patch(&format!("/v1/data/{project_id}/impediments/{id}"), &body)
                .await?;
            eprintln!("Impediment {id} resolved");
        }
    }

    Ok(())
}
