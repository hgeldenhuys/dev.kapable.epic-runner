use clap::{Args, Subcommand};
use comfy_table::{Cell, Color, Table};
use owo_colors::OwoColorize;

use super::CliConfig;
use crate::api_client::{ApiClient, DataWrapper};
use crate::types::Sprint;

#[derive(Args)]
pub struct SprintArgs {
    #[command(subcommand)]
    pub action: SprintAction,
}

#[derive(Subcommand)]
pub enum SprintAction {
    /// List sprints for an epic
    List {
        #[arg(long)]
        epic: String,
    },
    /// Show sprint details
    Show { id: String },
}

pub async fn run(
    args: SprintArgs,
    client: &ApiClient,
    cli: &CliConfig,
) -> Result<(), Box<dyn std::error::Error>> {
    match args.action {
        SprintAction::List { epic } => {
            // Resolve epic code to ID
            let epics: DataWrapper<Vec<serde_json::Value>> =
                client.get(&format!("/v1/epics?code={epic}")).await?;
            let epic_data = epics
                .data
                .first()
                .ok_or(format!("Epic '{epic}' not found"))?;
            let epic_id = epic_data["id"].as_str().ok_or("Epic has no id")?;

            let resp: DataWrapper<Vec<serde_json::Value>> = client
                .get(&format!("/v1/er_sprints?epic_id={epic_id}"))
                .await?;

            if cli.json {
                println!("{}", serde_json::to_string_pretty(&resp.data)?);
            } else {
                let mut table = Table::new();
                table.set_header(vec![
                    "#", "ID", "Status", "Stories", "Cost", "Progress", "Started", "Finished",
                ]);
                for row in &resp.data {
                    let s: Sprint = serde_json::from_value(row.clone())?;
                    let status_color = match s.status.to_string().as_str() {
                        "completed" => Color::Green,
                        "executing" => Color::Yellow,
                        "cancelled" => Color::DarkGrey,
                        "blocked" => Color::Red,
                        _ => Color::White,
                    };

                    // Extract velocity data
                    let (stories_str, cost_str, progress_str) =
                        if let Some(vel) = row.get("velocity") {
                            let planned = vel["stories_planned"].as_i64().unwrap_or(0);
                            let completed = vel["stories_completed"].as_i64().unwrap_or(0);
                            let cost = vel["total_cost_usd"].as_f64().unwrap_or(0.0);
                            let progress = vel["mission_progress"].as_f64();
                            (
                                format!("{completed}/{planned}"),
                                if cost > 0.0 {
                                    format!("${cost:.2}")
                                } else {
                                    "-".to_string()
                                },
                                progress
                                    .map(|p| format!("{:.0}%", p * 100.0))
                                    .unwrap_or("-".to_string()),
                            )
                        } else {
                            ("-".to_string(), "-".to_string(), "-".to_string())
                        };

                    table.add_row(vec![
                        Cell::new(s.number),
                        Cell::new(&s.id.to_string()[..8]),
                        Cell::new(s.status.to_string()).fg(status_color),
                        Cell::new(stories_str),
                        Cell::new(cost_str),
                        Cell::new(progress_str),
                        Cell::new(
                            s.started_at
                                .map(|t| t.format("%m-%d %H:%M").to_string())
                                .unwrap_or("-".to_string()),
                        ),
                        Cell::new(
                            s.finished_at
                                .map(|t| t.format("%m-%d %H:%M").to_string())
                                .unwrap_or("-".to_string()),
                        ),
                    ]);
                }
                println!("{table}");
                eprintln!("{} sprints for {epic}", resp.data.len());
            }
        }
        SprintAction::Show { id } => {
            let resp: serde_json::Value = client.get(&format!("/v1/er_sprints/{id}")).await?;

            if cli.json {
                println!("{}", serde_json::to_string_pretty(&resp)?);
            } else {
                let sprint: Sprint = serde_json::from_value(resp.clone())?;
                let status_colored = match sprint.status.to_string().as_str() {
                    "completed" => sprint.status.to_string().green().to_string(),
                    "executing" => sprint.status.to_string().yellow().to_string(),
                    "cancelled" => sprint.status.to_string().dimmed().to_string(),
                    "blocked" => sprint.status.to_string().red().to_string(),
                    _ => sprint.status.to_string(),
                };

                eprintln!(
                    "{} Sprint {} — {}",
                    "═══".cyan(),
                    sprint.number.to_string().cyan().bold(),
                    status_colored,
                );
                eprintln!("  ID: {}", sprint.id.to_string().dimmed());

                if let Some(goal) = &sprint.goal {
                    eprintln!("  Goal: {}", goal);
                }

                if let Some(started) = sprint.started_at {
                    eprint!("  Started: {}", started.format("%Y-%m-%d %H:%M UTC"));
                    if let Some(finished) = sprint.finished_at {
                        let duration = finished - started;
                        eprintln!(
                            " → {} ({} min)",
                            finished.format("%H:%M"),
                            duration.num_minutes()
                        );
                    } else {
                        eprintln!();
                    }
                }

                // v3: Velocity data
                if let Some(vel) = resp.get("velocity") {
                    let planned = vel["stories_planned"].as_i64().unwrap_or(0);
                    let completed = vel["stories_completed"].as_i64().unwrap_or(0);
                    let cost = vel["total_cost_usd"].as_f64().unwrap_or(0.0);
                    let nodes = vel["nodes_completed"].as_i64().unwrap_or(0);
                    let progress = vel["mission_progress"].as_f64();

                    eprintln!("\n  {}", "Velocity".bold());
                    eprintln!(
                        "    Stories: {}/{} completed",
                        completed.to_string().green(),
                        planned
                    );
                    eprintln!("    Nodes completed: {}", nodes);
                    if cost > 0.0 {
                        eprintln!("    Cost: {}", format!("${cost:.2}").yellow());
                    }
                    if let Some(p) = progress {
                        eprintln!(
                            "    Mission progress: {}",
                            format!("{:.0}%", p * 100.0).cyan()
                        );
                    }
                }

                // Ceremony log
                if let Some(log) = resp["ceremony_log"].as_array() {
                    eprintln!("\n  {}", "Ceremony Log".bold());
                    let mut ceremony_table = Table::new();
                    ceremony_table.set_header(vec!["Node", "Status", "Cost"]);
                    for entry in log {
                        let key = entry["key"].as_str().unwrap_or("?");
                        let status = entry["status"].as_str().unwrap_or("?");
                        let cost = entry["cost_usd"].as_f64();
                        let status_color = match status {
                            "Completed" => Color::Green,
                            "Failed" => Color::Red,
                            "Skipped" => Color::DarkGrey,
                            _ => Color::Yellow,
                        };
                        ceremony_table.add_row(vec![
                            Cell::new(key),
                            Cell::new(status).fg(status_color),
                            Cell::new(cost.map(|c| format!("${c:.2}")).unwrap_or("-".to_string())),
                        ]);
                    }
                    println!("  {ceremony_table}");
                }

                // Sprint assignments
                let assignments: DataWrapper<Vec<serde_json::Value>> = client
                    .get("/v1/sprint_assignments")
                    .await
                    .unwrap_or(DataWrapper { data: vec![] });
                let sprint_assignments: Vec<&serde_json::Value> = assignments
                    .data
                    .iter()
                    .filter(|a| a["sprint_id"].as_str() == Some(&id))
                    .collect();
                if !sprint_assignments.is_empty() {
                    eprintln!("\n  {}", "Assignments".bold());
                    let backlog: DataWrapper<Vec<serde_json::Value>> = client
                        .get("/v1/backlog_items")
                        .await
                        .unwrap_or(DataWrapper { data: vec![] });
                    for a in &sprint_assignments {
                        let a_status = a["status"].as_str().unwrap_or("?");
                        let item_id = a["backlog_item_id"].as_str().unwrap_or("?");
                        let title = backlog
                            .data
                            .iter()
                            .find(|b| b["id"].as_str() == Some(item_id))
                            .and_then(|b| b["title"].as_str())
                            .unwrap_or(item_id);
                        let icon = match a_status {
                            "completed" => "✓".green().to_string(),
                            "in_progress" => "▶".yellow().to_string(),
                            "deferred" => "⊘".dimmed().to_string(),
                            _ => "·".dimmed().to_string(),
                        };
                        eprintln!("    {} {} — {}", icon, title, a_status.dimmed());
                    }
                }
            }
        }
    }

    Ok(())
}
