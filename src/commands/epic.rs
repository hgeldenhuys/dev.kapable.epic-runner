use clap::{Args, Subcommand};
use comfy_table::{Cell, Color, Table};
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
    /// Show sprint health & convergence summary for an epic
    Health { code: String },
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
            tracing::debug!(
                "Full scan of /v1/epics for uniqueness check (acceptable for creation)"
            );
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

            // Defense-in-depth: generated code must not already exist globally
            let code_exists = all_epics_global
                .data
                .iter()
                .any(|e| e["code"].as_str() == Some(code.as_str()));
            if code_exists {
                return Err(format!(
                    "Epic code {code} already exists globally. \
                     Epic codes must be unique across all products."
                )
                .into());
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

            // Build server-side filter params (optimistic — API may ignore them)
            let mut params: Vec<(&str, &str)> = Vec::new();
            let pid_str;
            if let Some(pid) = &product_id {
                pid_str = pid.clone();
                params.push(("product_id", &pid_str));
            }
            if let Some(s) = &status {
                params.push(("status", s.as_str()));
            }
            let resp: DataWrapper<Vec<serde_json::Value>> =
                client.get_with_params("/v1/epics", &params).await?;
            // Client-side fallback — server may ignore query params on JSONB tables
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

            // Show cumulative cost across all sprints
            let epic_id = epic["id"].as_str().unwrap_or("");
            let all_sprints: DataWrapper<Vec<serde_json::Value>> = client
                .get_with_params("/v1/er_sprints", &[("epic_id", epic_id)])
                .await
                .unwrap_or(DataWrapper { data: vec![] });

            let mut total_epic_cost = 0.0f64;
            let mut sprint_count = 0;
            for sprint in &all_sprints.data {
                // Check epic_id match (server may ignore filter)
                if sprint["epic_id"].as_str() != Some(epic_id) {
                    continue;
                }
                if let Some(cost) = sprint["cost_usd"].as_f64() {
                    total_epic_cost += cost;
                    sprint_count += 1;
                } else if let Some(vel) = sprint.get("velocity") {
                    // Fallback: read from velocity JSON for older sprints
                    if let Some(cost) = vel["total_cost_usd"].as_f64() {
                        total_epic_cost += cost;
                        sprint_count += 1;
                    }
                }
            }

            if sprint_count > 0 {
                eprintln!(
                    "\nEpic cumulative cost: ${:.2} across {} sprint(s)",
                    total_epic_cost, sprint_count
                );
            }
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
        EpicAction::Health { code } => {
            let epic = find_epic_by_code(client, &code).await?;
            let epic_id = epic["id"].as_str().ok_or("Epic has no id")?;

            // Fetch sprints for this epic
            let all_sprints: DataWrapper<Vec<serde_json::Value>> = client
                .get_with_params("/v1/er_sprints", &[("epic_id", epic_id)])
                .await?;

            // Client-side filter (server may ignore param) + sort by number
            let mut sprints: Vec<&serde_json::Value> = all_sprints
                .data
                .iter()
                .filter(|s| s["epic_id"].as_str() == Some(epic_id))
                .collect();
            sprints.sort_by_key(|s| s["number"].as_i64().unwrap_or(0));

            if sprints.is_empty() {
                eprintln!("No sprints found for {code}");
                return Ok(());
            }

            // Extract progress values for convergence analysis
            let progress_values: Vec<Option<f64>> = sprints
                .iter()
                .map(|s| {
                    s.get("velocity")
                        .and_then(|v| v["mission_progress"].as_f64())
                })
                .collect();

            let verdict = compute_convergence(&progress_values);

            if cli.json {
                let json_out = serde_json::json!({
                    "epic_code": code,
                    "verdict": format!("{verdict}"),
                    "sprints": sprints,
                });
                println!("{}", serde_json::to_string_pretty(&json_out)?);
            } else {
                eprintln!("Health: {code}\n");

                let mut table = Table::new();
                table.set_header(vec![
                    "Sprint#",
                    "Status",
                    "Cost",
                    "Stories Done",
                    "Progress",
                    "Trend",
                ]);

                let mut prev_progress: Option<f64> = None;
                for s in &sprints {
                    let number = s["number"].as_i64().unwrap_or(0);
                    let status = s["status"].as_str().unwrap_or("?");
                    let status_color = match status {
                        "completed" => Color::Green,
                        "executing" => Color::Yellow,
                        "cancelled" => Color::DarkGrey,
                        "blocked" | "failed" => Color::Red,
                        _ => Color::White,
                    };

                    let (stories_str, cost_str, progress_val) = if let Some(vel) = s.get("velocity")
                    {
                        let planned = vel["stories_planned"].as_i64().unwrap_or(0);
                        let completed = vel["stories_completed"].as_i64().unwrap_or(0);
                        let cost = s["cost_usd"]
                            .as_f64()
                            .or_else(|| vel["total_cost_usd"].as_f64())
                            .unwrap_or(0.0);
                        let progress = vel["mission_progress"].as_f64();
                        (
                            format!("{completed}/{planned}"),
                            if cost > 0.0 {
                                format!("${cost:.2}")
                            } else {
                                "-".to_string()
                            },
                            progress,
                        )
                    } else {
                        let cost = s["cost_usd"].as_f64().unwrap_or(0.0);
                        (
                            "-".to_string(),
                            if cost > 0.0 {
                                format!("${cost:.2}")
                            } else {
                                "-".to_string()
                            },
                            None,
                        )
                    };

                    let progress_str = progress_val
                        .map(|p| format!("{:.0}%", p * 100.0))
                        .unwrap_or("-".to_string());

                    let trend = match (prev_progress, progress_val) {
                        (Some(prev), Some(curr)) if curr > prev => "\u{2191}", // ↑
                        (Some(prev), Some(curr)) if curr < prev => "\u{2193}", // ↓
                        (Some(_), Some(_)) => "\u{2192}",                      // →
                        _ => "-",
                    };
                    let trend_color = match trend {
                        "\u{2191}" => Color::Green,
                        "\u{2193}" => Color::Red,
                        _ => Color::Yellow,
                    };

                    table.add_row(vec![
                        Cell::new(number),
                        Cell::new(status).fg(status_color),
                        Cell::new(cost_str),
                        Cell::new(stories_str),
                        Cell::new(progress_str),
                        Cell::new(trend).fg(trend_color),
                    ]);

                    if progress_val.is_some() {
                        prev_progress = progress_val;
                    }
                }

                println!("{table}");

                let verdict_color = match verdict {
                    Convergence::Converging => Color::Green,
                    Convergence::Diverging => Color::Red,
                    Convergence::Stable => Color::Yellow,
                    Convergence::InsufficientData => Color::DarkGrey,
                };
                let mut verdict_table = Table::new();
                verdict_table.set_header(vec!["Convergence Verdict"]);
                verdict_table.add_row(vec![Cell::new(format!("{verdict}")).fg(verdict_color)]);
                println!("{verdict_table}");
            }
        }
    }

    Ok(())
}

/// Convergence verdict for an epic's sprint health
#[derive(Debug, PartialEq)]
pub enum Convergence {
    Converging,
    Diverging,
    Stable,
    InsufficientData,
}

impl std::fmt::Display for Convergence {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Convergence::Converging => write!(f, "CONVERGING \u{2191}"),
            Convergence::Diverging => write!(f, "DIVERGING \u{2193}"),
            Convergence::Stable => write!(f, "STABLE \u{2192}"),
            Convergence::InsufficientData => write!(f, "INSUFFICIENT DATA"),
        }
    }
}

/// Compute convergence from a sequence of mission_progress values.
/// Looks at the last 2 non-None values:
/// - Both increasing → CONVERGING
/// - Both decreasing → DIVERGING
/// - Otherwise → STABLE
/// - Fewer than 2 data points → INSUFFICIENT DATA
pub fn compute_convergence(progress: &[Option<f64>]) -> Convergence {
    let values: Vec<f64> = progress.iter().filter_map(|v| *v).collect();
    if values.len() < 2 {
        return Convergence::InsufficientData;
    }

    let last = values[values.len() - 1];
    let prev = values[values.len() - 2];

    if last > prev {
        Convergence::Converging
    } else if last < prev {
        Convergence::Diverging
    } else {
        Convergence::Stable
    }
}

/// Find an epic by its code.
/// Tries server-side filtering first, falls back to client-side if the API ignores the param.
async fn find_epic_by_code(
    client: &ApiClient,
    code: &str,
) -> Result<serde_json::Value, Box<dyn std::error::Error>> {
    let resp: DataWrapper<Vec<serde_json::Value>> = client
        .get_with_params("/v1/epics", &[("code", code)])
        .await?;
    // Check if server-side filter worked (single matching result)
    if let Some(epic) = resp.data.iter().find(|e| e["code"].as_str() == Some(code)) {
        return Ok(epic.clone());
    }
    // Fallback: if server returned empty (filter may have worked but no match)
    // or returned everything (filter was ignored), do client-side scan
    if resp.data.is_empty() {
        return Err(format!("Epic '{code}' not found").into());
    }
    tracing::debug!("Server may have ignored 'code' filter — falling back to client-side");
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn converging_when_last_two_increasing() {
        let progress = vec![Some(0.2), Some(0.4), Some(0.6)];
        assert_eq!(compute_convergence(&progress), Convergence::Converging);
    }

    #[test]
    fn diverging_when_last_two_decreasing() {
        let progress = vec![Some(0.6), Some(0.4), Some(0.2)];
        assert_eq!(compute_convergence(&progress), Convergence::Diverging);
    }

    #[test]
    fn stable_when_last_two_equal() {
        let progress = vec![Some(0.3), Some(0.5), Some(0.5)];
        assert_eq!(compute_convergence(&progress), Convergence::Stable);
    }

    #[test]
    fn insufficient_data_with_zero_sprints() {
        let progress: Vec<Option<f64>> = vec![];
        assert_eq!(
            compute_convergence(&progress),
            Convergence::InsufficientData
        );
    }

    #[test]
    fn insufficient_data_with_one_sprint() {
        let progress = vec![Some(0.5)];
        assert_eq!(
            compute_convergence(&progress),
            Convergence::InsufficientData
        );
    }

    #[test]
    fn skips_none_values_for_analysis() {
        // Only two non-None values: 0.3 → 0.1 = DIVERGING
        let progress = vec![Some(0.3), None, Some(0.1)];
        assert_eq!(compute_convergence(&progress), Convergence::Diverging);
    }

    #[test]
    fn insufficient_when_all_none() {
        let progress = vec![None, None, None];
        assert_eq!(
            compute_convergence(&progress),
            Convergence::InsufficientData
        );
    }

    #[test]
    fn converging_with_only_two_data_points() {
        let progress = vec![Some(0.2), Some(0.8)];
        assert_eq!(compute_convergence(&progress), Convergence::Converging);
    }

    #[test]
    fn display_formats_correctly() {
        assert!(format!("{}", Convergence::Converging).contains("CONVERGING"));
        assert!(format!("{}", Convergence::Diverging).contains("DIVERGING"));
        assert!(format!("{}", Convergence::Stable).contains("STABLE"));
        assert!(format!("{}", Convergence::InsufficientData).contains("INSUFFICIENT DATA"));
    }
}
