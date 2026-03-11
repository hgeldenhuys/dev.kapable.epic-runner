use clap::Args;
use comfy_table::{Cell, Table};
use owo_colors::OwoColorize;

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
    // Load all data upfront (JSONB tables don't support server-side filtering)
    let all_products: DataWrapper<Vec<serde_json::Value>> = client.get("/v1/products").await?;
    let all_epics: DataWrapper<Vec<serde_json::Value>> = client.get("/v1/epics").await?;
    let all_stories: DataWrapper<Vec<serde_json::Value>> = client.get("/v1/stories").await?;
    let all_sprints: DataWrapper<Vec<serde_json::Value>> = client.get("/v1/er_sprints").await?;
    let all_impediments: DataWrapper<Vec<serde_json::Value>> =
        client.get("/v1/impediments").await?;

    // Filter products by slug if specified
    let products: Vec<&serde_json::Value> = all_products
        .data
        .iter()
        .filter(|p| {
            if let Some(slug) = &args.product {
                p["slug"].as_str() == Some(slug.as_str())
            } else {
                true
            }
        })
        .collect();

    if cli.json {
        let mut status = serde_json::Map::new();
        status.insert("products".to_string(), serde_json::to_value(&products)?);
        println!("{}", serde_json::to_string_pretty(&status)?);
        return Ok(());
    }

    eprintln!("{}\n", "═══ Epic Runner Dashboard ═══".cyan().bold());

    for product in &products {
        let name = product["name"].as_str().unwrap_or("?");
        let pid = product["id"].as_str().unwrap_or("?");
        eprintln!("Product: {}", name.bold());

        // Filter epics by product_id (client-side)
        let product_epics: Vec<&serde_json::Value> = all_epics
            .data
            .iter()
            .filter(|e| e["product_id"].as_str() == Some(pid))
            .collect();

        let mut table = Table::new();
        table.set_header(vec!["Epic", "Status", "Sprints", "Stories"]);

        for epic_val in &product_epics {
            let code = epic_val["code"].as_str().unwrap_or("?");
            let epic_status = epic_val["status"].as_str().unwrap_or("?");
            let epic_id = epic_val["id"].as_str().unwrap_or("?");

            // Count sprints for this epic (client-side filter)
            let sprint_count = all_sprints
                .data
                .iter()
                .filter(|s| s["epic_id"].as_str() == Some(epic_id))
                .count();

            // Count stories for this epic (client-side filter)
            let epic_stories: Vec<&serde_json::Value> = all_stories
                .data
                .iter()
                .filter(|s| s["epic_code"].as_str() == Some(code))
                .collect();

            let done_count = epic_stories
                .iter()
                .filter(|s| {
                    let st = s["status"].as_str();
                    st == Some("done") || st == Some("deployed")
                })
                .count();

            table.add_row(vec![
                Cell::new(code),
                Cell::new(epic_status),
                Cell::new(sprint_count),
                Cell::new(format!("{}/{}", done_count, epic_stories.len())),
            ]);
        }

        println!("{table}");

        // Open impediments
        let open_impediments: Vec<&serde_json::Value> = all_impediments
            .data
            .iter()
            .filter(|i| i["status"].as_str() == Some("open"))
            .collect();

        if !open_impediments.is_empty() {
            eprintln!("\n⚠ {} open impediment(s)", open_impediments.len());
            for imp in &open_impediments {
                let blocking = imp["blocking_epic"].as_str().unwrap_or("?");
                let title = imp["title"].as_str().unwrap_or("?");
                eprintln!("  [{blocking}] {title}");
            }
        }
        eprintln!();
    }

    Ok(())
}
