use clap::{Args, Subcommand};
use comfy_table::{Cell, Table};

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
                table.set_header(vec!["#", "ID", "Status", "Started", "Finished"]);
                for row in &resp.data {
                    let s: Sprint = serde_json::from_value(row.clone())?;
                    table.add_row(vec![
                        Cell::new(s.number),
                        Cell::new(&s.id.to_string()[..8]),
                        Cell::new(s.status.to_string()),
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
            println!("{}", serde_json::to_string_pretty(&resp)?);
        }
    }

    Ok(())
}
