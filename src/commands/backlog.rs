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
            let resp: serde_json::Value = client.post("/v1/stories", &body).await?;
            if cli.json {
                println!("{}", serde_json::to_string_pretty(&resp)?);
            } else {
                let id = resp["id"].as_str().unwrap_or("?");
                eprintln!("Story created: {id}");
            }
        }
        BacklogAction::List {
            product: _,
            epic,
            status,
        } => {
            // Fetch all stories and apply client-side filters (JSONB tables
            // don't support arbitrary query param filtering)
            let resp: DataWrapper<Vec<serde_json::Value>> =
                client.get("/v1/stories").await?;

            let filtered: Vec<&serde_json::Value> = resp
                .data
                .iter()
                .filter(|row| {
                    if let Some(e) = &epic {
                        if row["epic_code"].as_str() != Some(e.as_str()) {
                            return false;
                        }
                    }
                    if let Some(s) = &status {
                        if row["status"].as_str() != Some(s.as_str()) {
                            return false;
                        }
                    }
                    // product filtering would require resolving slug→product_id,
                    // skip for now since stories are project-scoped by API key
                    true
                })
                .collect();

            if cli.json {
                println!("{}", serde_json::to_string_pretty(&filtered)?);
            } else {
                let mut table = Table::new();
                table.set_header(vec!["ID", "Title", "Epic", "Status", "Pts"]);
                for row in &filtered {
                    let s: Story = serde_json::from_value((*row).clone())?;
                    table.add_row(vec![
                        Cell::new(&s.id.to_string()[..8]),
                        Cell::new(truncate(&s.title, 40)),
                        Cell::new(s.epic_code.as_deref().unwrap_or("-")),
                        Cell::new(s.status.to_string()),
                        Cell::new(s.points.map(|p| p.to_string()).unwrap_or("-".to_string())),
                    ]);
                }
                println!("{table}");
                eprintln!("{} stories", filtered.len());
            }
        }
        BacklogAction::Show { id } => {
            let full_id = client.resolve_id("stories", &id).await?;
            let resp: serde_json::Value = client.get(&format!("/v1/stories/{full_id}")).await?;
            println!("{}", serde_json::to_string_pretty(&resp)?);
        }
        BacklogAction::Transition { id, status } => {
            let full_id = client.resolve_id("stories", &id).await?;
            let body = json!({ "status": status, "updated_at": chrono::Utc::now().to_rfc3339() });
            let _: serde_json::Value = client.patch(&format!("/v1/stories/{full_id}"), &body).await?;
            eprintln!("Story {full_id} → {status}");
        }
        BacklogAction::Delete { id } => {
            let full_id = client.resolve_id("stories", &id).await?;
            client.delete(&format!("/v1/stories/{full_id}")).await?;
            eprintln!("Story {full_id} deleted");
        }
    }

    Ok(())
}

fn truncate(s: &str, max: usize) -> &str {
    if s.len() <= max {
        return s;
    }
    let mut end = max;
    while end > 0 && !s.is_char_boundary(end) {
        end -= 1;
    }
    &s[..end]
}
