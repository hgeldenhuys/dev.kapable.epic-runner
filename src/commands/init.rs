use clap::Args;
use serde_json::json;

use super::CliConfig;
use crate::api_client::{ApiClient, DataWrapper};

#[derive(Args)]
pub struct InitArgs {
    /// Project name (defaults to directory name)
    #[arg(long)]
    pub name: Option<String>,

    /// Kapable project ID (if already provisioned)
    #[arg(long)]
    pub project_id: Option<String>,
}

const TABLES: &[(&str, &str)] = &[
    (
        "products",
        r#"[
        {"name": "id", "column_type": "uuid", "primary_key": true, "default_value": "gen_random_uuid()"},
        {"name": "name", "column_type": "text", "nullable": false},
        {"name": "slug", "column_type": "text", "nullable": false},
        {"name": "repo_path", "column_type": "text", "nullable": false},
        {"name": "description", "column_type": "text"},
        {"name": "created_at", "column_type": "timestamptz", "default_value": "now()"}
    ]"#,
    ),
    (
        "stories",
        r#"[
        {"name": "id", "column_type": "uuid", "primary_key": true, "default_value": "gen_random_uuid()"},
        {"name": "product_id", "column_type": "uuid", "nullable": false},
        {"name": "title", "column_type": "text", "nullable": false},
        {"name": "description", "column_type": "text"},
        {"name": "epic_code", "column_type": "text"},
        {"name": "status", "column_type": "text", "default_value": "'draft'"},
        {"name": "points", "column_type": "integer"},
        {"name": "acceptance_criteria", "column_type": "jsonb"},
        {"name": "file_paths", "column_type": "jsonb"},
        {"name": "dod_checklist", "column_type": "jsonb"},
        {"name": "created_at", "column_type": "timestamptz", "default_value": "now()"},
        {"name": "updated_at", "column_type": "timestamptz", "default_value": "now()"}
    ]"#,
    ),
    (
        "epics",
        r#"[
        {"name": "id", "column_type": "uuid", "primary_key": true, "default_value": "gen_random_uuid()"},
        {"name": "product_id", "column_type": "uuid", "nullable": false},
        {"name": "code", "column_type": "text", "nullable": false},
        {"name": "domain", "column_type": "text", "nullable": false},
        {"name": "instance", "column_type": "integer", "default_value": "1"},
        {"name": "title", "column_type": "text", "nullable": false},
        {"name": "intent", "column_type": "text", "nullable": false},
        {"name": "success_criteria", "column_type": "jsonb"},
        {"name": "status", "column_type": "text", "default_value": "'active'"},
        {"name": "worktree_name", "column_type": "text", "nullable": false},
        {"name": "created_at", "column_type": "timestamptz", "default_value": "now()"},
        {"name": "closed_at", "column_type": "timestamptz"}
    ]"#,
    ),
    (
        "sprints",
        r#"[
        {"name": "id", "column_type": "uuid", "primary_key": true, "default_value": "gen_random_uuid()"},
        {"name": "epic_id", "column_type": "uuid", "nullable": false},
        {"name": "number", "column_type": "integer", "nullable": false},
        {"name": "session_id", "column_type": "uuid", "nullable": false},
        {"name": "status", "column_type": "text", "default_value": "'planning'"},
        {"name": "goal", "column_type": "text"},
        {"name": "system_prompt", "column_type": "text"},
        {"name": "stories", "column_type": "jsonb"},
        {"name": "ceremony_log", "column_type": "jsonb"},
        {"name": "rubber_duck_insights", "column_type": "jsonb"},
        {"name": "started_at", "column_type": "timestamptz"},
        {"name": "finished_at", "column_type": "timestamptz"},
        {"name": "created_at", "column_type": "timestamptz", "default_value": "now()"}
    ]"#,
    ),
    (
        "impediments",
        r#"[
        {"name": "id", "column_type": "uuid", "primary_key": true, "default_value": "gen_random_uuid()"},
        {"name": "product_id", "column_type": "uuid", "nullable": false},
        {"name": "blocking_epic", "column_type": "text", "nullable": false},
        {"name": "blocked_by_epic", "column_type": "text"},
        {"name": "title", "column_type": "text", "nullable": false},
        {"name": "description", "column_type": "text"},
        {"name": "status", "column_type": "text", "default_value": "'open'"},
        {"name": "raised_by_sprint", "column_type": "uuid"},
        {"name": "resolved_by_sprint", "column_type": "uuid"},
        {"name": "created_at", "column_type": "timestamptz", "default_value": "now()"},
        {"name": "resolved_at", "column_type": "timestamptz"}
    ]"#,
    ),
    (
        "supervisor_decisions",
        r#"[
        {"name": "id", "column_type": "uuid", "primary_key": true, "default_value": "gen_random_uuid()"},
        {"name": "sprint_id", "column_type": "uuid", "nullable": false},
        {"name": "stop_hook_count", "column_type": "integer"},
        {"name": "decision", "column_type": "text"},
        {"name": "reasoning", "column_type": "text"},
        {"name": "rubber_duck_insights", "column_type": "text"},
        {"name": "timestamp", "column_type": "timestamptz", "default_value": "now()"}
    ]"#,
    ),
    (
        "rubber_duck_sessions",
        r#"[
        {"name": "id", "column_type": "uuid", "primary_key": true, "default_value": "gen_random_uuid()"},
        {"name": "sprint_id", "column_type": "uuid", "nullable": false},
        {"name": "trigger_reason", "column_type": "text"},
        {"name": "stuck_state_summary", "column_type": "text"},
        {"name": "insights", "column_type": "jsonb"},
        {"name": "recommended_action", "column_type": "text"},
        {"name": "cost_usd", "column_type": "numeric"},
        {"name": "timestamp", "column_type": "timestamptz", "default_value": "now()"}
    ]"#,
    ),
];

pub async fn run(
    args: InitArgs,
    client: &ApiClient,
    _cli: &CliConfig,
) -> Result<(), Box<dyn std::error::Error>> {
    let project_id = if let Some(pid) = args.project_id {
        eprintln!("Using existing project: {pid}");
        pid
    } else {
        let name = args.name.unwrap_or_else(|| "epic-runner".to_string());
        eprintln!("Creating project: {name}");
        let resp: DataWrapper<serde_json::Value> = client
            .post(
                "/v1/projects",
                &json!({ "name": name, "slug": name.to_lowercase().replace(' ', "-") }),
            )
            .await?;
        let pid = resp.data["id"]
            .as_str()
            .ok_or("Project creation failed")?
            .to_string();
        eprintln!("Project created: {pid}");
        pid
    };

    // Create tables
    for (table_name, columns_json) in TABLES {
        let columns: serde_json::Value = serde_json::from_str(columns_json)?;
        eprintln!("Creating table: {table_name}...");
        let result: Result<DataWrapper<serde_json::Value>, _> = client
            .post(
                &format!("/v1/projects/{project_id}/tables"),
                &json!({ "name": table_name, "columns": columns }),
            )
            .await;
        match result {
            Ok(_) => eprintln!("  ✓ {table_name}"),
            Err(e) => eprintln!("  ⚠ {table_name}: {e} (may already exist)"),
        }
    }

    // Write config
    std::fs::create_dir_all(".epic-runner")?;
    let config_content = format!(
        r#"[api]
base_url = "{}"

[project]
project_id = "{}"
"#,
        client.base_url, project_id
    );
    std::fs::write(".epic-runner/config.toml", &config_content)?;
    eprintln!("\nConfig written to .epic-runner/config.toml");
    eprintln!("Project ID: {project_id}");
    eprintln!("\nNext: epic-runner product create --name <name> --slug <slug> --repo-path <path>");

    Ok(())
}
