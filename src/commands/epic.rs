use clap::{Args, Subcommand};
use comfy_table::{Cell, Table};
use serde_json::json;

use super::CliConfig;
use crate::api_client::{ApiClient, DataWrapper};
use crate::types::Epic;

#[derive(Args)]
pub struct EpicArgs {
    #[command(subcommand)]
    pub action: EpicAction,
}

#[derive(Subcommand)]
pub enum EpicAction {
    /// Create a new epic (also creates git worktree)
    Create {
        #[arg(long)]
        product: String,
        #[arg(long)]
        domain: String,
        #[arg(long)]
        title: String,
        #[arg(long)]
        intent: String,
        /// Success criteria (JSON array of objects with description + verification_method)
        #[arg(long)]
        criteria: Option<String>,
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
    /// Close an epic (intent satisfied)
    Close { code: String },
    /// Abandon an epic
    Abandon { code: String },
}

pub async fn run(
    args: EpicArgs,
    client: &ApiClient,
    cli: &CliConfig,
) -> Result<(), Box<dyn std::error::Error>> {
    match args.action {
        EpicAction::Create {
            product,
            domain,
            title,
            intent,
            criteria,
        } => {
            // Resolve product slug
            let products: DataWrapper<Vec<serde_json::Value>> =
                client.get(&format!("/v1/products?slug={product}")).await?;
            let product_data = products
                .data
                .first()
                .ok_or(format!("Product '{product}' not found"))?;
            let product_id = product_data["id"].as_str().ok_or("Product has no id")?;

            // Determine instance number globally (not per-product) to prevent code collisions
            let all_epics_global: DataWrapper<Vec<serde_json::Value>> =
                client.get("/v1/epics").await?;
            let domain_upper = domain.to_uppercase();
            let domain_count = all_epics_global
                .data
                .iter()
                .filter(|e| {
                    e["domain"]
                        .as_str()
                        .map(|d| d.to_uppercase() == domain_upper)
                        .unwrap_or(false)
                })
                .count();
            let instance = domain_count as i32 + 1;
            let code = format!("{}-{:03}", domain_upper, instance);

            // Verify code doesn't already exist (defense-in-depth)
            let code_exists = all_epics_global
                .data
                .iter()
                .any(|e| e["code"].as_str() == Some(code.as_str()));
            if code_exists {
                return Err(format!(
                    "Epic code {code} already exists globally. Epic codes must be unique across all products."
                ).into());
            }
            let worktree_name = code.clone();

            let success_criteria: Option<serde_json::Value> =
                criteria.map(|c| serde_json::from_str(&c)).transpose()?;

            let body = json!({
                "product_id": product_id,
                "code": code,
                "domain": domain,
                "instance": instance,
                "title": title,
                "intent": intent,
                "success_criteria": success_criteria,
                "status": "active",
                "worktree_name": worktree_name,
            });
            let resp: serde_json::Value = client.post("/v1/epics", &body).await?;

            if cli.json {
                println!("{}", serde_json::to_string_pretty(&resp)?);
            } else {
                eprintln!("Epic created: {code}");
                eprintln!("  Title: {title}");
                eprintln!("  Intent: {intent}");
                eprintln!("  Worktree: {worktree_name}");
            }
        }
        EpicAction::List { product, status } => {
            // Resolve product slug to ID for filtering
            let product_id = if let Some(p) = &product {
                let products: DataWrapper<Vec<serde_json::Value>> =
                    client.get("/v1/products").await?;
                products
                    .data
                    .iter()
                    .find(|row| row["slug"].as_str() == Some(p.as_str()))
                    .and_then(|row| row["id"].as_str().map(String::from))
            } else {
                None
            };

            let resp: DataWrapper<Vec<serde_json::Value>> = client.get("/v1/epics").await?;
            let filtered: Vec<&serde_json::Value> = resp
                .data
                .iter()
                .filter(|row| {
                    if let Some(pid) = &product_id {
                        if row["product_id"].as_str() != Some(pid.as_str()) {
                            return false;
                        }
                    }
                    if let Some(s) = &status {
                        if row["status"].as_str() != Some(s.as_str()) {
                            return false;
                        }
                    }
                    true
                })
                .collect();

            if cli.json {
                println!("{}", serde_json::to_string_pretty(&filtered)?);
            } else {
                let mut table = Table::new();
                table.set_header(vec!["Code", "Title", "Status", "Intent"]);
                for row in &filtered {
                    let e: Epic = serde_json::from_value((*row).clone())?;
                    table.add_row(vec![
                        Cell::new(&e.code),
                        Cell::new(truncate(&e.title, 30)),
                        Cell::new(e.status.to_string()),
                        Cell::new(truncate(&e.intent, 40)),
                    ]);
                }
                println!("{table}");
                eprintln!("{} epics", filtered.len());
            }
        }
        EpicAction::Show { code } => {
            let epic = find_epic_by_code(client, &code).await?;
            println!("{}", serde_json::to_string_pretty(&epic)?);
        }
        EpicAction::Close { code } => {
            let epic = find_epic_by_code(client, &code).await?;
            let id = epic["id"].as_str().ok_or("Epic has no id")?;
            let body = json!({ "status": "closed", "closed_at": chrono::Utc::now().to_rfc3339() });
            let _: serde_json::Value = client.patch(&format!("/v1/epics/{id}"), &body).await?;
            eprintln!("Epic {code} closed");
        }
        EpicAction::Abandon { code } => {
            let epic = find_epic_by_code(client, &code).await?;
            let id = epic["id"].as_str().ok_or("Epic has no id")?;
            let body = json!({ "status": "abandoned" });
            let _: serde_json::Value = client.patch(&format!("/v1/epics/{id}"), &body).await?;
            eprintln!("Epic {code} abandoned");
        }
    }

    Ok(())
}

/// Find an epic by its code, using client-side filtering since JSONB
/// tables may not support server-side query param filtering.
async fn find_epic_by_code(
    client: &ApiClient,
    code: &str,
) -> Result<serde_json::Value, Box<dyn std::error::Error>> {
    let all: DataWrapper<Vec<serde_json::Value>> = client.get("/v1/epics").await?;
    all.data
        .into_iter()
        .find(|e| e["code"].as_str() == Some(code))
        .ok_or_else(|| format!("Epic '{code}' not found").into())
}

fn truncate(s: &str, max: usize) -> &str {
    if s.len() <= max {
        return s;
    }
    // Find the last char boundary at or before `max`
    let mut end = max;
    while end > 0 && !s.is_char_boundary(end) {
        end -= 1;
    }
    &s[..end]
}
