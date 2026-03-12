use clap::Args;
use serde_json::json;

use super::CliConfig;
use crate::api_client::ApiClient;

#[derive(Args)]
pub struct InitArgs {
    /// Project name (defaults to directory name)
    #[arg(long)]
    pub name: Option<String>,

    /// Kapable project ID (if already provisioned — skips project creation)
    #[arg(long)]
    pub project_id: Option<String>,

    /// Project-scoped API key (sk_live_*) — required if --project-id is used
    #[arg(long)]
    pub data_key: Option<String>,
}

/// Table definitions using the Kapable _meta/tables API format.
/// storage_mode: "jsonb" means schemaless (all data stored in JSONB columns).
///
/// NOTE: `er_sprints` (not `sprints`) — the platform's agentboard module registers
/// `/v1/sprints` as a management route, which shadows any data table named `sprints`.
/// The `er_` prefix avoids this route collision. See: 69d56ad.
const TABLES: &[&str] = &[
    "products",
    "stories",
    "epics",
    "er_sprints",
    "impediments",
    "supervisor_decisions",
    "rubber_duck_sessions",
    "ceremony_events",
    "sprint_learnings",
    // v3: backlog-first model
    "backlog_items",
    "sprint_assignments",
];

pub async fn run(
    args: InitArgs,
    client: &ApiClient,
    _cli: &CliConfig,
) -> Result<(), Box<dyn std::error::Error>> {
    let (project_id, live_key) = if let Some(pid) = args.project_id {
        // Existing project — need a data key
        let key = args
            .data_key
            .ok_or("When using --project-id, you must also provide --data-key (sk_live_*)")?;
        eprintln!("Using existing project: {pid}");
        (pid, key)
    } else {
        // Create new project via admin key (the client was initialized with admin key)
        let name = args.name.unwrap_or_else(|| "epic-runner".to_string());
        eprintln!("Creating project: {name}");

        let resp: serde_json::Value = client
            .post(
                "/v1/projects",
                &json!({ "name": name, "slug": name.to_lowercase().replace(' ', "-") }),
            )
            .await?;

        let pid = resp["project"]["id"]
            .as_str()
            .ok_or("Project creation failed — no id in response")?
            .to_string();

        // Extract the auto-generated live key from the creation response
        let live_key = resp["api_keys"]
            .as_array()
            .and_then(|keys| {
                keys.iter().find_map(|k| {
                    let key_str = k["key"].as_str()?;
                    if key_str.starts_with("sk_live_") {
                        Some(key_str.to_string())
                    } else {
                        None
                    }
                })
            })
            .ok_or("Project created but no sk_live_ key in response")?;

        eprintln!("Project created: {pid}");
        eprintln!("Live key: {}", &live_key[..16]);
        (pid, live_key)
    };

    // Create a data client using the project-scoped live key
    let data_client = ApiClient::new(&client.base_url, &live_key);

    // Create tables via PUT /v1/_meta/tables/{name} (jsonb mode)
    for table_name in TABLES {
        eprintln!("Creating table: {table_name}...");
        let result: Result<serde_json::Value, _> = data_client
            .put(
                &format!("/v1/_meta/tables/{table_name}"),
                &json!({ "storage_mode": "jsonb" }),
            )
            .await;
        match result {
            Ok(_) => eprintln!("  ✓ {table_name}"),
            Err(e) => eprintln!("  ⚠ {table_name}: {e} (may already exist)"),
        }
    }

    // Write config with both keys
    std::fs::create_dir_all(".epic-runner")?;
    let config_content = format!(
        r#"[api]
base_url = "{}"

[project]
project_id = "{}"
data_key = "{}"
"#,
        client.base_url, project_id, live_key
    );
    std::fs::write(".epic-runner/config.toml", &config_content)?;
    eprintln!("\nConfig written to .epic-runner/config.toml");
    eprintln!("Project ID: {project_id}");
    eprintln!("\nNext: epic-runner product create --name <name> --slug <slug> --repo-path <path>");

    Ok(())
}
