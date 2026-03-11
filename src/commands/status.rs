use clap::Args;
use comfy_table::{Cell, Color, Table};
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
        table.set_header(vec![
            "Epic", "Status", "Sprints", "Stories", "Progress", "Cost",
        ]);

        let mut total_cost = 0.0_f64;

        for epic_val in &product_epics {
            let code = epic_val["code"].as_str().unwrap_or("?");
            let epic_status = epic_val["status"].as_str().unwrap_or("?");
            let epic_id = epic_val["id"].as_str().unwrap_or("?");

            // Sprints for this epic (client-side filter)
            let epic_sprints: Vec<&serde_json::Value> = all_sprints
                .data
                .iter()
                .filter(|s| s["epic_id"].as_str() == Some(epic_id))
                .collect();

            // Sum cost from ceremony_log in sprints
            let epic_cost: f64 = epic_sprints
                .iter()
                .filter_map(|s| {
                    s["ceremony_log"].as_array().map(|log| {
                        log.iter()
                            .filter_map(|entry| entry["cost_usd"].as_f64())
                            .sum::<f64>()
                    })
                })
                .sum();
            total_cost += epic_cost;

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

            let total_stories = epic_stories.len();
            let progress_bar = render_progress_bar(done_count, total_stories, 15);

            let status_color = match epic_status {
                "active" => Color::Green,
                "blocked" => Color::Red,
                "closed" => Color::DarkGrey,
                _ => Color::Yellow,
            };

            table.add_row(vec![
                Cell::new(code),
                Cell::new(epic_status).fg(status_color),
                Cell::new(epic_sprints.len()),
                Cell::new(format!("{}/{}", done_count, total_stories)),
                Cell::new(&progress_bar),
                Cell::new(if epic_cost > 0.0 {
                    format!("${:.2}", epic_cost)
                } else {
                    "-".to_string()
                }),
            ]);
        }

        println!("{table}");

        if total_cost > 0.0 {
            eprintln!("  Total cost: {}", format!("${:.2}", total_cost).yellow());
        }

        // Recent sprint history (last 5)
        let mut recent_sprints: Vec<&serde_json::Value> = all_sprints
            .data
            .iter()
            .filter(|s| {
                // Match sprints belonging to any epic in this product
                product_epics
                    .iter()
                    .any(|e| e["id"].as_str() == s["epic_id"].as_str())
            })
            .collect();
        recent_sprints.sort_by(|a, b| {
            let a_time = a["finished_at"].as_str().unwrap_or("");
            let b_time = b["finished_at"].as_str().unwrap_or("");
            b_time.cmp(a_time)
        });

        if !recent_sprints.is_empty() {
            eprintln!(
                "\n  {} (last {})",
                "Recent Sprints".bold(),
                recent_sprints.len().min(5)
            );
            for sprint in recent_sprints.iter().take(5) {
                let num = sprint["number"].as_i64().unwrap_or(0);
                let status = sprint["status"].as_str().unwrap_or("?");
                let status_icon = match status {
                    "completed" => "✓".green().to_string(),
                    "failed" => "✗".red().to_string(),
                    "blocked" => "⊘".red().bold().to_string(),
                    "executing" => "▶".yellow().to_string(),
                    _ => "?".dimmed().to_string(),
                };
                eprintln!("    {} Sprint {} — {}", status_icon, num, status);
            }
        }

        // Open impediments
        let open_impediments: Vec<&serde_json::Value> = all_impediments
            .data
            .iter()
            .filter(|i| i["status"].as_str() == Some("open"))
            .collect();

        if !open_impediments.is_empty() {
            eprintln!(
                "\n  {} {} open impediment(s)",
                "⚠".yellow().bold(),
                open_impediments.len()
            );
            for imp in &open_impediments {
                let blocking = imp["blocking_epic"].as_str().unwrap_or("?");
                let title = imp["title"].as_str().unwrap_or("?");
                eprintln!("    [{}] {}", blocking.red(), title);
            }
        }
        eprintln!();
    }

    Ok(())
}

/// Render a text-based progress bar: [████████░░░░░░░] 60%
fn render_progress_bar(done: usize, total: usize, width: usize) -> String {
    if total == 0 {
        return format!("[{}] -", "░".repeat(width));
    }
    let pct = (done as f64 / total as f64 * 100.0) as usize;
    let filled = (done as f64 / total as f64 * width as f64) as usize;
    let empty = width.saturating_sub(filled);
    format!("[{}{}] {}%", "█".repeat(filled), "░".repeat(empty), pct)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn progress_bar_empty() {
        let bar = render_progress_bar(0, 10, 10);
        assert!(bar.contains("0%"));
        assert!(bar.contains("░░░░░░░░░░"));
    }

    #[test]
    fn progress_bar_full() {
        let bar = render_progress_bar(10, 10, 10);
        assert!(bar.contains("100%"));
        assert!(bar.contains("██████████"));
    }

    #[test]
    fn progress_bar_half() {
        let bar = render_progress_bar(5, 10, 10);
        assert!(bar.contains("50%"));
    }

    #[test]
    fn progress_bar_zero_total() {
        let bar = render_progress_bar(0, 0, 10);
        assert!(bar.contains("-"));
    }
}
