use clap::Args;
use comfy_table::{Cell, Table};

use super::CliConfig;
use crate::api_client::{ApiClient, DataWrapper};

#[derive(Args)]
pub struct StatusArgs {
    /// Product slug to show status for (all products if omitted)
    #[arg(long)]
    pub product: Option<String>,
}

pub async fn run(
    args: StatusArgs,
    client: &ApiClient,
    cli: &CliConfig,
) -> Result<(), Box<dyn std::error::Error>> {
    let project_id = crate::config::resolve_project_id()?;

    // Load products
    let products_query = if let Some(slug) = &args.product {
        format!("/v1/data/{project_id}/products?slug={slug}")
    } else {
        format!("/v1/data/{project_id}/products")
    };
    let products: DataWrapper<Vec<serde_json::Value>> = client.get(&products_query).await?;

    if cli.json {
        let mut status = serde_json::Map::new();
        status.insert(
            "products".to_string(),
            serde_json::to_value(&products.data)?,
        );
        println!("{}", serde_json::to_string_pretty(&status)?);
        return Ok(());
    }

    eprintln!("═══ Epic Runner Dashboard ═══\n");

    for product in &products.data {
        let name = product["name"].as_str().unwrap_or("?");
        let pid = product["id"].as_str().unwrap_or("?");
        eprintln!("Product: {name}");

        // Load epics
        let epics: DataWrapper<Vec<serde_json::Value>> = client
            .get(&format!("/v1/data/{project_id}/epics?product_id={pid}"))
            .await?;

        let mut table = Table::new();
        table.set_header(vec!["Epic", "Status", "Sprints", "Stories"]);

        for epic_val in &epics.data {
            let code = epic_val["code"].as_str().unwrap_or("?");
            let epic_status = epic_val["status"].as_str().unwrap_or("?");
            let epic_id = epic_val["id"].as_str().unwrap_or("?");

            // Count sprints
            let sprints: DataWrapper<Vec<serde_json::Value>> = client
                .get(&format!("/v1/data/{project_id}/sprints?epic_id={epic_id}"))
                .await?;

            // Count stories
            let stories: DataWrapper<Vec<serde_json::Value>> = client
                .get(&format!("/v1/data/{project_id}/stories?epic_code={code}"))
                .await?;

            let done_count = stories
                .data
                .iter()
                .filter(|s| {
                    s["status"].as_str() == Some("done") || s["status"].as_str() == Some("deployed")
                })
                .count();

            table.add_row(vec![
                Cell::new(code),
                Cell::new(epic_status),
                Cell::new(sprints.data.len()),
                Cell::new(format!("{}/{}", done_count, stories.data.len())),
            ]);
        }

        println!("{table}");

        // Impediments
        let impediments: DataWrapper<Vec<serde_json::Value>> = client
            .get(&format!("/v1/data/{project_id}/impediments?status=open"))
            .await?;
        if !impediments.data.is_empty() {
            eprintln!("\n⚠ {} open impediment(s)", impediments.data.len());
            for imp in &impediments.data {
                let blocking = imp["blocking_epic"].as_str().unwrap_or("?");
                let title = imp["title"].as_str().unwrap_or("?");
                eprintln!("  [{blocking}] {title}");
            }
        }
        eprintln!();
    }

    Ok(())
}
