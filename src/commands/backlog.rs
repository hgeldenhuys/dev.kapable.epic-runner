use clap::{Args, Subcommand};
use comfy_table::{Cell, Table};
use serde_json::json;

use super::CliConfig;
use crate::api_client::{ApiClient, DataWrapper};
use crate::types::Story;

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
    args: BacklogArgs,
    client: &ApiClient,
    cli: &CliConfig,
) -> Result<(), Box<dyn std::error::Error>> {
    match args.action {
        BacklogAction::Add {
            title,
            product,
            epic,
            description,
            points,
        } => {
            // Look up product by slug
            let products: DataWrapper<Vec<serde_json::Value>> =
                client.get(&format!("/v1/products?slug={product}")).await?;
            let product_id = products
                .data
                .first()
                .and_then(|p| p["id"].as_str())
                .ok_or(format!("Product '{product}' not found"))?;

            let body = json!({
                "product_id": product_id,
                "title": title,
                "epic_code": epic,
                "description": description,
                "points": points,
                "status": "draft",
            });
            let resp: DataWrapper<serde_json::Value> = client.post("/v1/stories", &body).await?;
            if cli.json {
                println!("{}", serde_json::to_string_pretty(&resp.data)?);
            } else {
                let id = resp.data["id"].as_str().unwrap_or("?");
                eprintln!("Story created: {id}");
            }
        }
        BacklogAction::List {
            product,
            epic,
            status,
        } => {
            let mut query = "/v1/stories?".to_string();
            if let Some(e) = &epic {
                query.push_str(&format!("epic_code={e}&"));
            }
            if let Some(s) = &status {
                query.push_str(&format!("status={s}&"));
            }
            if let Some(p) = &product {
                // Resolve product slug to ID
                let products: DataWrapper<Vec<serde_json::Value>> =
                    client.get(&format!("/v1/products?slug={p}")).await?;
                if let Some(pid) = products.data.first().and_then(|p| p["id"].as_str()) {
                    query.push_str(&format!("product_id={pid}&"));
                }
            }

            let resp: DataWrapper<Vec<serde_json::Value>> = client.get(&query).await?;
            if cli.json {
                println!("{}", serde_json::to_string_pretty(&resp.data)?);
            } else {
                let mut table = Table::new();
                table.set_header(vec!["ID", "Title", "Epic", "Status", "Pts"]);
                for row in &resp.data {
                    let s: Story = serde_json::from_value(row.clone())?;
                    table.add_row(vec![
                        Cell::new(&s.id.to_string()[..8]),
                        Cell::new(truncate(&s.title, 40)),
                        Cell::new(s.epic_code.as_deref().unwrap_or("-")),
                        Cell::new(s.status.to_string()),
                        Cell::new(s.points.map(|p| p.to_string()).unwrap_or("-".to_string())),
                    ]);
                }
                println!("{table}");
                eprintln!("{} stories", resp.data.len());
            }
        }
        BacklogAction::Show { id } => {
            let resp: DataWrapper<serde_json::Value> =
                client.get(&format!("/v1/stories/{id}")).await?;
            println!("{}", serde_json::to_string_pretty(&resp.data)?);
        }
        BacklogAction::Transition { id, status } => {
            let body = json!({ "status": status, "updated_at": chrono::Utc::now().to_rfc3339() });
            let _: DataWrapper<serde_json::Value> =
                client.patch(&format!("/v1/stories/{id}"), &body).await?;
            eprintln!("Story {id} → {status}");
        }
        BacklogAction::Delete { id } => {
            client.delete(&format!("/v1/stories/{id}")).await?;
            eprintln!("Story {id} deleted");
        }
    }

    Ok(())
}

fn truncate(s: &str, max: usize) -> &str {
    if s.len() > max {
        &s[..max]
    } else {
        s
    }
}
