use std::collections::HashMap;

use clap::Args;
use comfy_table::{Cell, Color, Table};
use owo_colors::OwoColorize;
use serde_json::json;

use super::CliConfig;
use crate::api_client::{ApiClient, DataWrapper};

#[derive(Args)]
pub struct StatusArgs {
    /// Product slug to show status for (all products if omitted)
    #[arg(long)]
    pub product: Option<String>,

    /// Show verbose sprint details (velocity, goals, assignments)
    #[arg(long, short)]
    pub verbose: bool,

    /// Run failure analysis on recent sprints
    #[arg(long)]
    pub failure_analysis: bool,
}

pub async fn run(
    args: StatusArgs,
    client: &ApiClient,
    cli: &CliConfig,
) -> Result<(), Box<dyn std::error::Error>> {
    if args.failure_analysis {
        return run_failure_analysis(client, cli).await;
    }

    // Load all data upfront (JSONB tables don't support server-side filtering)
    let all_products: DataWrapper<Vec<serde_json::Value>> = client.get("/v1/products").await?;
    let all_epics: DataWrapper<Vec<serde_json::Value>> = client.get("/v1/epics").await?;
    let all_stories: DataWrapper<Vec<serde_json::Value>> = client.get("/v1/stories").await?;
    let all_sprints: DataWrapper<Vec<serde_json::Value>> = client.get("/v1/er_sprints").await?;
    let all_impediments: DataWrapper<Vec<serde_json::Value>> =
        client.get("/v1/impediments").await?;
    // v3: Load backlog items and sprint assignments (best-effort — tables may not exist yet)
    let all_backlog: DataWrapper<Vec<serde_json::Value>> = client
        .get("/v1/backlog_items")
        .await
        .unwrap_or(DataWrapper { data: vec![] });
    let all_assignments: DataWrapper<Vec<serde_json::Value>> = client
        .get("/v1/sprint_assignments")
        .await
        .unwrap_or(DataWrapper { data: vec![] });

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
                    "cancelled" => "⊘".yellow().to_string(),
                    "blocked" => "⊘".red().bold().to_string(),
                    "executing" => "▶".yellow().to_string(),
                    _ => "?".dimmed().to_string(),
                };

                // v3: Show velocity data inline
                let velocity_str = if let Some(vel) = sprint.get("velocity") {
                    let planned = vel["stories_planned"].as_i64().unwrap_or(0);
                    let completed = vel["stories_completed"].as_i64().unwrap_or(0);
                    let cost = vel["total_cost_usd"].as_f64().unwrap_or(0.0);
                    let progress = vel["mission_progress"].as_f64();
                    let mut parts = vec![format!("{completed}/{planned} stories")];
                    if cost > 0.0 {
                        parts.push(format!("${cost:.2}"));
                    }
                    if let Some(p) = progress {
                        parts.push(format!("{:.0}% mission", p * 100.0));
                    }
                    format!(" ({})", parts.join(", "))
                } else {
                    String::new()
                };

                eprintln!(
                    "    {} Sprint {} — {}{}",
                    status_icon,
                    num,
                    status,
                    velocity_str.dimmed()
                );

                // v3: Show sprint goal in verbose mode
                if args.verbose {
                    if let Some(goal) = sprint["goal"].as_str() {
                        eprintln!("      {}: {}", "Goal".dimmed(), goal);
                    }
                    // Show assignments for this sprint
                    let sprint_id = sprint["id"].as_str().unwrap_or("");
                    let assignments: Vec<&serde_json::Value> = all_assignments
                        .data
                        .iter()
                        .filter(|a| a["sprint_id"].as_str() == Some(sprint_id))
                        .collect();
                    if !assignments.is_empty() {
                        for a in &assignments {
                            let a_status = a["status"].as_str().unwrap_or("?");
                            let item_id = a["backlog_item_id"].as_str().unwrap_or("?");
                            // Try to find the backlog item title
                            let title = all_backlog
                                .data
                                .iter()
                                .find(|b| b["id"].as_str() == Some(item_id))
                                .and_then(|b| b["title"].as_str())
                                .unwrap_or(item_id);
                            let a_icon = match a_status {
                                "completed" => "✓".green().to_string(),
                                "in_progress" => "▶".yellow().to_string(),
                                "deferred" => "⊘".dimmed().to_string(),
                                _ => "·".dimmed().to_string(),
                            };
                            eprintln!("      {} {} — {}", a_icon, title, a_status.dimmed());
                        }
                    }
                }
            }
        }

        // v3: Backlog summary
        let product_backlog: Vec<&serde_json::Value> = all_backlog
            .data
            .iter()
            .filter(|b| b["product_id"].as_str() == Some(pid))
            .collect();
        if !product_backlog.is_empty() {
            let draft = product_backlog
                .iter()
                .filter(|b| b["status"].as_str() == Some("draft"))
                .count();
            let refined = product_backlog
                .iter()
                .filter(|b| b["status"].as_str() == Some("refined"))
                .count();
            let ready = product_backlog
                .iter()
                .filter(|b| b["status"].as_str() == Some("ready"))
                .count();
            let done = product_backlog
                .iter()
                .filter(|b| b["status"].as_str() == Some("done"))
                .count();
            eprintln!(
                "\n  {} {} items — {} draft, {} refined, {} ready, {} done",
                "Backlog:".bold(),
                product_backlog.len(),
                draft.to_string().dimmed(),
                refined.to_string().yellow(),
                ready.to_string().green(),
                done.to_string().cyan(),
            );
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

/// Classify an error detail string into a root cause category.
fn classify_error(detail: &str) -> &'static str {
    let lower = detail.to_lowercase();
    if lower.contains("heartbeat") || lower.contains("timeout") || lower.contains("timed out") {
        "timeout"
    } else if lower.contains("auth") || lower.contains("401") || lower.contains("403") {
        "auth"
    } else if lower.contains("rate") || lower.contains("429") || lower.contains("throttl") {
        "rate-limit"
    } else if lower.contains("oom") || lower.contains("memory") || lower.contains("killed") {
        "resource"
    } else if lower.contains("network") || lower.contains("connect") || lower.contains("dns") {
        "network"
    } else {
        "other"
    }
}

/// Run failure analysis on ceremony_events and er_sprints, printing a
/// formatted report and returning structured JSON.
async fn run_failure_analysis(
    client: &ApiClient,
    cli: &CliConfig,
) -> Result<(), Box<dyn std::error::Error>> {
    // Fetch ceremony_events (best-effort — table may not exist)
    let all_events: DataWrapper<Vec<serde_json::Value>> = match client
        .get("/v1/ceremony_events")
        .await
    {
        Ok(data) => data,
        Err(e) => {
            eprintln!(
                "{} Could not fetch ceremony_events: {}",
                "WARNING:".yellow().bold(),
                e
            );
            eprintln!("  The ceremony_events table may not exist yet. Skipping failure analysis.");
            return Ok(());
        }
    };

    // Fetch er_sprints (best-effort)
    let all_sprints: DataWrapper<Vec<serde_json::Value>> = match client.get("/v1/er_sprints").await
    {
        Ok(data) => data,
        Err(e) => {
            eprintln!(
                "{} Could not fetch er_sprints: {}",
                "WARNING:".yellow().bold(),
                e
            );
            eprintln!("  The er_sprints table may not exist yet. Skipping failure analysis.");
            return Ok(());
        }
    };

    // Filter ceremony_events for failures (event_type == "failed" or "cancelled")
    let failure_events: Vec<&serde_json::Value> = all_events
        .data
        .iter()
        .filter(|ev| {
            let et = ev["event_type"].as_str().unwrap_or("");
            et == "failed" || et == "cancelled"
        })
        .collect();

    if failure_events.is_empty() {
        eprintln!("{}\n", "═══ Failure Analysis ═══".cyan().bold());
        eprintln!("  {} No failure or cancellation events found.", "✓".green());
        if cli.json {
            println!(
                "{}",
                serde_json::to_string_pretty(&json!({
                    "total_failures": 0,
                    "hotspots_by_node": [],
                    "categories": {},
                    "sprints_by_failures": []
                }))?
            );
        }
        return Ok(());
    }

    // --- Group failures by node_key ---
    let mut by_node: HashMap<String, Vec<&serde_json::Value>> = HashMap::new();
    for ev in &failure_events {
        let node_key = ev["node_key"].as_str().unwrap_or("unknown").to_string();
        by_node.entry(node_key).or_default().push(ev);
    }

    // Sort nodes by failure count descending
    let mut node_counts: Vec<(String, usize)> =
        by_node.iter().map(|(k, v)| (k.clone(), v.len())).collect();
    node_counts.sort_by(|a, b| b.1.cmp(&a.1));

    // --- Group failures by error category ---
    let mut by_category: HashMap<&str, usize> = HashMap::new();
    for ev in &failure_events {
        // Extract detail text from the event — could be in "detail", "error", or "message"
        let detail_text = ev["detail"]
            .as_str()
            .or_else(|| ev["error"].as_str())
            .or_else(|| ev["message"].as_str())
            // Also check nested detail object
            .or_else(|| {
                ev["detail"]
                    .as_object()
                    .and_then(|d| d.get("error").and_then(|e| e.as_str()))
            })
            .unwrap_or("");
        let category = classify_error(detail_text);
        *by_category.entry(category).or_insert(0) += 1;
    }

    // --- Correlate with sprints: count failures per sprint ---
    let mut by_sprint: HashMap<String, usize> = HashMap::new();
    for ev in &failure_events {
        let sprint_id = ev["sprint_id"].as_str().unwrap_or("unknown").to_string();
        *by_sprint.entry(sprint_id).or_insert(0) += 1;
    }

    // Build sprint lookup for display
    let mut sprint_lookup: HashMap<String, &serde_json::Value> = HashMap::new();
    for sprint in &all_sprints.data {
        if let Some(id) = sprint["id"].as_str() {
            sprint_lookup.insert(id.to_string(), sprint);
        }
    }

    // Sort sprints by failure count descending
    let mut sprint_counts: Vec<(String, usize)> =
        by_sprint.iter().map(|(k, v)| (k.clone(), *v)).collect();
    sprint_counts.sort_by(|a, b| b.1.cmp(&a.1));

    // --- JSON output ---
    if cli.json {
        let hotspots: Vec<serde_json::Value> = node_counts
            .iter()
            .map(|(node, count)| {
                json!({
                    "node_key": node,
                    "failure_count": count
                })
            })
            .collect();

        let sprint_details: Vec<serde_json::Value> = sprint_counts
            .iter()
            .map(|(sid, count)| {
                let sprint_info = sprint_lookup.get(sid);
                let number = sprint_info.and_then(|s| s["number"].as_i64()).unwrap_or(0);
                let status = sprint_info
                    .and_then(|s| s["status"].as_str())
                    .unwrap_or("unknown");
                let epic_id = sprint_info
                    .and_then(|s| s["epic_id"].as_str())
                    .unwrap_or("unknown");
                json!({
                    "sprint_id": sid,
                    "sprint_number": number,
                    "sprint_status": status,
                    "epic_id": epic_id,
                    "failure_count": count
                })
            })
            .collect();

        let categories: serde_json::Value = by_category
            .iter()
            .map(|(k, v)| (k.to_string(), json!(v)))
            .collect::<serde_json::Map<String, serde_json::Value>>()
            .into();

        let result = json!({
            "total_failures": failure_events.len(),
            "hotspots_by_node": hotspots,
            "categories": categories,
            "sprints_by_failures": sprint_details
        });

        println!("{}", serde_json::to_string_pretty(&result)?);
        return Ok(());
    }

    // --- Formatted table output ---
    eprintln!("{}\n", "═══ Failure Analysis ═══".cyan().bold());
    eprintln!(
        "  Total failure/cancellation events: {}\n",
        failure_events.len().to_string().red().bold()
    );

    // Node hotspot table
    eprintln!("  {}", "Failure Hotspots by Node".bold());
    let mut node_table = Table::new();
    node_table.set_header(vec!["Node Key", "Failures", "% of Total"]);
    for (node, count) in &node_counts {
        let pct = (*count as f64 / failure_events.len() as f64) * 100.0;
        let color = if pct > 30.0 {
            Color::Red
        } else if pct > 15.0 {
            Color::Yellow
        } else {
            Color::White
        };
        node_table.add_row(vec![
            Cell::new(node),
            Cell::new(count).fg(color),
            Cell::new(format!("{:.1}%", pct)).fg(color),
        ]);
    }
    println!("{node_table}\n");

    // Error category table
    eprintln!("  {}", "Root Cause Categories".bold());
    let mut cat_table = Table::new();
    cat_table.set_header(vec!["Category", "Count", "% of Total"]);
    let mut sorted_cats: Vec<(&&str, &usize)> = by_category.iter().collect();
    sorted_cats.sort_by(|a, b| b.1.cmp(a.1));
    for (category, count) in &sorted_cats {
        let pct = (**count as f64 / failure_events.len() as f64) * 100.0;
        let color = match **category {
            "timeout" => Color::Red,
            "auth" => Color::Magenta,
            "rate-limit" => Color::Yellow,
            "resource" => Color::Red,
            "network" => Color::DarkYellow,
            _ => Color::White,
        };
        cat_table.add_row(vec![
            Cell::new(category).fg(color),
            Cell::new(count).fg(color),
            Cell::new(format!("{:.1}%", pct)).fg(color),
        ]);
    }
    println!("{cat_table}\n");

    // Sprint failure table (top 10)
    eprintln!("  {}", "Sprints with Most Failures".bold());
    let mut sprint_table = Table::new();
    sprint_table.set_header(vec!["Sprint", "Status", "Epic", "Failures"]);
    for (sid, count) in sprint_counts.iter().take(10) {
        let sprint_info = sprint_lookup.get(sid);
        let number = sprint_info
            .and_then(|s| s["number"].as_i64())
            .map(|n| format!("#{}", n))
            .unwrap_or_else(|| sid[..8.min(sid.len())].to_string());
        let status = sprint_info
            .and_then(|s| s["status"].as_str())
            .unwrap_or("unknown");
        let epic_id = sprint_info
            .and_then(|s| s["epic_id"].as_str())
            .unwrap_or("?");
        let epic_short = if epic_id.len() > 8 {
            &epic_id[..8]
        } else {
            epic_id
        };

        let status_color = match status {
            "completed" => Color::Green,
            "cancelled" => Color::Yellow,
            "blocked" => Color::Red,
            "executing" => Color::Cyan,
            _ => Color::White,
        };

        sprint_table.add_row(vec![
            Cell::new(&number),
            Cell::new(status).fg(status_color),
            Cell::new(epic_short),
            Cell::new(count).fg(Color::Red),
        ]);
    }
    println!("{sprint_table}");

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

    #[test]
    fn classify_error_timeout() {
        assert_eq!(classify_error("heartbeat missed after 30s"), "timeout");
        assert_eq!(classify_error("Request timeout"), "timeout");
        assert_eq!(classify_error("Connection timed out"), "timeout");
    }

    #[test]
    fn classify_error_auth() {
        assert_eq!(classify_error("auth token expired"), "auth");
        assert_eq!(classify_error("HTTP 401 Unauthorized"), "auth");
        assert_eq!(classify_error("Forbidden 403"), "auth");
    }

    #[test]
    fn classify_error_rate_limit() {
        assert_eq!(classify_error("rate limit exceeded"), "rate-limit");
        assert_eq!(classify_error("HTTP 429 Too Many Requests"), "rate-limit");
        assert_eq!(classify_error("API throttled"), "rate-limit");
    }

    #[test]
    fn classify_error_resource() {
        assert_eq!(classify_error("OOM killed"), "resource");
        assert_eq!(classify_error("out of memory"), "resource");
        assert_eq!(classify_error("process killed by signal"), "resource");
    }

    #[test]
    fn classify_error_network() {
        assert_eq!(classify_error("network unreachable"), "network");
        assert_eq!(classify_error("connection refused"), "network");
        assert_eq!(classify_error("DNS resolution failed"), "network");
    }

    #[test]
    fn classify_error_other() {
        assert_eq!(classify_error("something went wrong"), "other");
        assert_eq!(classify_error(""), "other");
    }
}
