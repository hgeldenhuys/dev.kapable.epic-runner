use clap::{Args, Subcommand};
use comfy_table::{Cell, Table};
use serde_json::json;

use super::CliConfig;
use crate::api_client::{ApiClient, DataWrapper};
use crate::types::Story;

/// Generate the next sequential story code for a product.
/// Fetches the product prefix and counts existing stories to derive `{PREFIX}-{NNN}`.
pub async fn next_story_code(
    client: &ApiClient,
    product_id: &str,
) -> Result<String, Box<dyn std::error::Error>> {
    let all_products: DataWrapper<Vec<serde_json::Value>> = client.get("/v1/products").await?;
    let product_data = all_products
        .data
        .iter()
        .find(|p| p["id"].as_str() == Some(product_id))
        .ok_or("Product not found")?;
    let prefix = product_data["story_prefix"].as_str().unwrap_or("S");

    let all_stories: DataWrapper<Vec<serde_json::Value>> = client.get("/v1/stories").await?;
    let story_count = all_stories
        .data
        .iter()
        .filter(|s| s["product_id"].as_str() == Some(product_id))
        .count();
    Ok(format!("{}-{:03}", prefix, story_count + 1))
}

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
        /// T-shirt size for context capacity planning (xs/s/m/l/xl)
        #[arg(long)]
        size: Option<String>,
        /// Tags for groomer matching (comma-separated)
        #[arg(long, value_delimiter = ',')]
        tags: Option<Vec<String>>,
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
            size,
            tags,
        } => {
            // Look up product by slug
            let all_products: DataWrapper<Vec<serde_json::Value>> =
                client.get("/v1/products").await?;
            let product_data = all_products
                .data
                .iter()
                .find(|p| p["slug"].as_str() == Some(product.as_str()))
                .ok_or(format!("Product '{product}' not found"))?;
            let product_id = product_data["id"].as_str().ok_or("Product has no id")?;

            let code = next_story_code(client, product_id).await?;

            let mut body = json!({
                "product_id": product_id,
                "code": code,
                "title": title,
                "epic_code": epic,
                "description": description,
                "points": points,
                "status": "draft",
            });
            // v3 fields: size and tags for backlog-first model
            if let Some(s) = &size {
                body["size"] = json!(s);
            }
            if let Some(t) = &tags {
                body["tags"] = json!(t);
            }

            // Also create in backlog_items table for v3 (dual-write)
            let v3_body = json!({
                "product_id": product_id,
                "code": code,
                "title": title,
                "description": description,
                "size": size,
                "tags": tags,
                "status": "draft",
            });
            // v3 table write — fail silently if table doesn't exist yet
            let _ = client
                .post::<_, serde_json::Value>("/v1/backlog_items", &v3_body)
                .await;

            let resp: serde_json::Value = client.post("/v1/stories", &body).await?;
            if cli.json {
                println!("{}", serde_json::to_string_pretty(&resp)?);
            } else {
                eprintln!("Story created: {code}");
                eprintln!("  Title: {title}");
                if let Some(e) = &epic {
                    eprintln!("  Epic: {e}");
                }
            }
        }
        BacklogAction::List {
            product: _,
            epic,
            status,
        } => {
            // Fetch all stories and apply client-side filters (JSONB tables
            // don't support arbitrary query param filtering)
            let resp: DataWrapper<Vec<serde_json::Value>> = client.get("/v1/stories").await?;

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
                table.set_header(vec!["Code", "Title", "Epic", "Status", "Pts"]);
                for row in &filtered {
                    let s: Story = serde_json::from_value((*row).clone())?;
                    let id_short = s.id.to_string();
                    let code_display = s.code.as_deref().unwrap_or(&id_short[..8]);
                    table.add_row(vec![
                        Cell::new(code_display),
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
            let full_id = resolve_story_id(client, &id).await?;
            let resp: serde_json::Value = client.get(&format!("/v1/stories/{full_id}")).await?;
            println!("{}", serde_json::to_string_pretty(&resp)?);
        }
        BacklogAction::Transition { id, status } => {
            let full_id = resolve_story_id(client, &id).await?;
            let body = json!({ "status": status, "updated_at": chrono::Utc::now().to_rfc3339() });
            let _: serde_json::Value = client
                .patch(&format!("/v1/stories/{full_id}"), &body)
                .await?;
            eprintln!("Story {id} → {status}");
        }
        BacklogAction::Delete { id } => {
            let full_id = resolve_story_id(client, &id).await?;
            client.delete(&format!("/v1/stories/{full_id}")).await?;
            eprintln!("Story {id} deleted");
        }
    }

    Ok(())
}

/// Resolve a story identifier — accepts either a story code (e.g. "ER-042")
/// or a UUID/UUID prefix. Code lookup is tried first, then falls back to UUID resolution.
async fn resolve_story_id(
    client: &ApiClient,
    id_or_code: &str,
) -> Result<String, Box<dyn std::error::Error>> {
    // If it looks like a story code (letters-digits), try code lookup first
    if id_or_code.contains('-') && id_or_code.chars().next().is_some_and(|c| c.is_alphabetic()) {
        let all: DataWrapper<Vec<serde_json::Value>> = client.get("/v1/stories").await?;
        if let Some(story) = all
            .data
            .iter()
            .find(|s| s["code"].as_str() == Some(id_or_code))
        {
            if let Some(id) = story["id"].as_str() {
                return Ok(id.to_string());
            }
        }
    }
    // Fall back to UUID prefix resolution
    Ok(client.resolve_id("stories", id_or_code).await?)
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
