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

            // Determine instance number (count existing epics in this domain)
            let existing: DataWrapper<Vec<serde_json::Value>> = client
                .get(&format!(
                    "/v1/epics?product_id={product_id}&domain={domain}"
                ))
                .await?;
            let instance = existing.data.len() as i32 + 1;
            let code = format!("{}-{:03}", domain.to_uppercase(), instance);
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
            let resp: DataWrapper<serde_json::Value> = client.post("/v1/epics", &body).await?;

            if cli.json {
                println!("{}", serde_json::to_string_pretty(&resp.data)?);
            } else {
                eprintln!("Epic created: {code}");
                eprintln!("  Title: {title}");
                eprintln!("  Intent: {intent}");
                eprintln!("  Worktree: {worktree_name}");
            }
        }
        EpicAction::List { product, status } => {
            let mut query = "/v1/epics?".to_string();
            if let Some(s) = &status {
                query.push_str(&format!("status={s}&"));
            }
            if let Some(p) = &product {
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
                table.set_header(vec!["Code", "Title", "Status", "Intent"]);
                for row in &resp.data {
                    let e: Epic = serde_json::from_value(row.clone())?;
                    table.add_row(vec![
                        Cell::new(&e.code),
                        Cell::new(truncate(&e.title, 30)),
                        Cell::new(e.status.to_string()),
                        Cell::new(truncate(&e.intent, 40)),
                    ]);
                }
                println!("{table}");
                eprintln!("{} epics", resp.data.len());
            }
        }
        EpicAction::Show { code } => {
            let resp: DataWrapper<Vec<serde_json::Value>> =
                client.get(&format!("/v1/epics?code={code}")).await?;
            let epic = resp
                .data
                .first()
                .ok_or(format!("Epic '{code}' not found"))?;
            println!("{}", serde_json::to_string_pretty(epic)?);
        }
        EpicAction::Close { code } => {
            let resp: DataWrapper<Vec<serde_json::Value>> =
                client.get(&format!("/v1/epics?code={code}")).await?;
            let epic = resp
                .data
                .first()
                .ok_or(format!("Epic '{code}' not found"))?;
            let id = epic["id"].as_str().ok_or("Epic has no id")?;
            let body = json!({ "status": "closed", "closed_at": chrono::Utc::now().to_rfc3339() });
            let _: DataWrapper<serde_json::Value> =
                client.patch(&format!("/v1/epics/{id}"), &body).await?;
            eprintln!("Epic {code} closed");
        }
        EpicAction::Abandon { code } => {
            let resp: DataWrapper<Vec<serde_json::Value>> =
                client.get(&format!("/v1/epics?code={code}")).await?;
            let epic = resp
                .data
                .first()
                .ok_or(format!("Epic '{code}' not found"))?;
            let id = epic["id"].as_str().ok_or("Epic has no id")?;
            let body = json!({ "status": "abandoned" });
            let _: DataWrapper<serde_json::Value> =
                client.patch(&format!("/v1/epics/{id}"), &body).await?;
            eprintln!("Epic {code} abandoned");
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
