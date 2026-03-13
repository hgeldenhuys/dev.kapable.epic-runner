use clap::{Args, Subcommand};
use comfy_table::{Cell, Color, Table};
use serde_json::json;

use super::CliConfig;
use crate::api_client::{ApiClient, DataWrapper};
use crate::types::Story;

/// Generate the next sequential story code for a product.
/// Parses the max existing code number to derive `{PREFIX}-{NNN}`.
pub async fn next_story_code(
    client: &ApiClient,
    product_id: &str,
) -> Result<String, Box<dyn std::error::Error>> {
    let all_products: DataWrapper<Vec<serde_json::Value>> = client.get("/v1/products").await?;
    let product_data = all_products
        .data
        .iter()
        .find(|p| p["id"].as_str() == Some(product_id))
        .ok_or("Product not found")?;
    let prefix = product_data["story_prefix"].as_str().unwrap_or("S");

    let all_stories: DataWrapper<Vec<serde_json::Value>> = client.get("/v1/stories").await?;
    let max_num = all_stories
        .data
        .iter()
        .filter(|s| s["product_id"].as_str() == Some(product_id))
        .filter_map(|s| {
            let code = s["code"].as_str()?;
            let suffix = code.strip_prefix(prefix)?.strip_prefix('-')?;
            suffix.parse::<u32>().ok()
        })
        .max()
        .unwrap_or(0);
    Ok(format!("{}-{:03}", prefix, max_num + 1))
}

#[derive(Args)]
pub struct BacklogArgs {
    #[command(subcommand)]
    pub action: BacklogAction,
}

#[derive(Subcommand)]
pub enum BacklogAction {
    /// Add a story to the backlog
    Add {
        #[arg(long)]
        title: String,
        #[arg(long)]
        product: String,
        #[arg(long)]
        epic: Option<String>,
        /// The WHY — "so that [measurable outcome]"
        #[arg(long)]
        intent: Option<String>,
        /// The WHO — "as a [specific persona]"
        #[arg(long)]
        persona: Option<String>,
        #[arg(long)]
        description: Option<String>,
        #[arg(long)]
        points: Option<i32>,
        /// Story codes this depends on (comma-separated, e.g. ER-001,ER-002)
        #[arg(long, value_delimiter = ',')]
        depends_on: Option<Vec<String>>,
        /// T-shirt size for context capacity planning (xs/s/m/l/xl)
        #[arg(long)]
        size: Option<String>,
        /// Tags for groomer matching (comma-separated)
        #[arg(long, value_delimiter = ',')]
        tags: Option<Vec<String>>,
    },
    /// List backlog stories
    List {
        #[arg(long)]
        product: Option<String>,
        #[arg(long)]
        epic: Option<String>,
        #[arg(long)]
        status: Option<String>,
    },
    /// Show story details
    Show { id: String },
    /// Transition story status
    Transition {
        id: String,
        #[arg(long)]
        status: String,
    },
    /// Delete a story
    Delete { id: String },
    /// Mark a task as done (updates API + local story file)
    #[command(name = "task-done")]
    TaskDone {
        /// Story code or ID (e.g. "ER-042")
        story: String,
        /// Zero-based task index
        index: usize,
        /// Optional outcome note
        #[arg(long)]
        outcome: Option<String>,
    },
    /// Mark an acceptance criterion as verified (updates API + local story file)
    #[command(name = "ac-verify")]
    AcVerify {
        /// Story code or ID (e.g. "ER-042")
        story: String,
        /// Zero-based AC index
        index: usize,
        /// Optional evidence (test output, screenshot path, etc.)
        #[arg(long)]
        evidence: Option<String>,
    },
    /// Mark a story as blocked with a reason
    Block {
        /// Story code or ID (e.g. "ER-042")
        story: String,
        /// Why the story is blocked
        #[arg(long)]
        reason: String,
    },
    /// Park a story — shelve it for later consideration (excluded from orchestration)
    Park {
        /// Story code or ID (e.g. "ER-042")
        story: String,
        /// Why the story is being parked
        #[arg(long)]
        reason: Option<String>,
    },
}

pub async fn run(
    args: BacklogArgs,
    client: &ApiClient,
    cli: &CliConfig,
) -> Result<(), Box<dyn std::error::Error>> {
    match args.action {
        BacklogAction::Add {
            title,
            product,
            epic,
            intent,
            persona,
            description,
            points,
            depends_on,
            size,
            tags,
        } => {
            // Look up product by slug
            let all_products: DataWrapper<Vec<serde_json::Value>> =
                client.get("/v1/products").await?;
            let product_data = all_products
                .data
                .iter()
                .find(|p| p["slug"].as_str() == Some(product.as_str()))
                .ok_or(format!("Product '{product}' not found"))?;
            let product_id = product_data["id"].as_str().ok_or("Product has no id")?;

            let code = next_story_code(client, product_id).await?;

            let mut body = json!({
                "product_id": product_id,
                "code": code,
                "title": title,
                "epic_code": epic,
                "intent": intent,
                "persona": persona,
                "description": description,
                "points": points,
                "dependencies": depends_on,
                "status": "draft",
            });
            // Optional fields
            if let Some(s) = &size {
                body["size"] = json!(s);
            }
            if let Some(t) = &tags {
                body["tags"] = json!(t);
            }

            // Also create in backlog_items table for v3 (dual-write)
            let v3_body = json!({
                "product_id": product_id,
                "code": code,
                "title": title,
                "description": description,
                "size": size,
                "tags": tags,
                "status": "draft",
            });
            // v3 table write — fail silently if table doesn't exist yet
            let _ = client
                .post::<_, serde_json::Value>("/v1/backlog_items", &v3_body)
                .await;

            let resp: serde_json::Value = client.post("/v1/stories", &body).await?;
            if cli.json {
                println!("{}", serde_json::to_string_pretty(&resp)?);
            } else {
                eprintln!("Story created: {code}");
                eprintln!("  Title: {title}");
                if let Some(e) = &epic {
                    eprintln!("  Epic: {e}");
                }
            }
        }
        BacklogAction::List {
            product: _,
            epic,
            status,
        } => {
            // Fetch all stories and apply client-side filters (JSONB tables
            // don't support arbitrary query param filtering)
            let resp: DataWrapper<Vec<serde_json::Value>> = client.get("/v1/stories").await?;

            let filtered: Vec<&serde_json::Value> = resp
                .data
                .iter()
                .filter(|row| {
                    if let Some(e) = &epic {
                        if row["epic_code"].as_str() != Some(e.as_str()) {
                            return false;
                        }
                    }
                    if let Some(s) = &status {
                        if row["status"].as_str() != Some(s.as_str()) {
                            return false;
                        }
                    }
                    // product filtering would require resolving slug→product_id,
                    // skip for now since stories are project-scoped by API key
                    true
                })
                .collect();

            if cli.json {
                println!("{}", serde_json::to_string_pretty(&filtered)?);
            } else {
                let mut table = Table::new();
                table.set_header(vec![
                    "Code", "Title", "Epic", "Status", "Tasks", "ACs", "Pts", "Planned",
                ]);
                for row in &filtered {
                    let s: Story = serde_json::from_value((*row).clone())?;
                    let id_short = s.id.to_string();
                    let code_display = s.code.as_deref().unwrap_or(&id_short[..8]);
                    let planned = s.planned_at.as_deref().map(|d| &d[..10]).unwrap_or("—");

                    let (tasks_done, tasks_total) = s
                        .tasks
                        .as_ref()
                        .map(|t| (t.iter().filter(|task| task.done).count(), t.len()))
                        .unwrap_or((0, 0));
                    let (acs_verified, acs_total) = s
                        .acceptance_criteria
                        .as_ref()
                        .map(|a| (a.iter().filter(|ac| ac.verified).count(), a.len()))
                        .unwrap_or((0, 0));

                    table.add_row(vec![
                        Cell::new(code_display),
                        Cell::new(truncate(&s.title, 40)),
                        Cell::new(s.epic_code.as_deref().unwrap_or("-")),
                        Cell::new(s.status.to_string()),
                        colored_fraction(tasks_done, tasks_total),
                        colored_fraction(acs_verified, acs_total),
                        Cell::new(s.points.map(|p| p.to_string()).unwrap_or("-".to_string())),
                        Cell::new(planned),
                    ]);
                }
                println!("{table}");
                eprintln!("{} stories", filtered.len());
            }
        }
        BacklogAction::Show { id } => {
            let full_id = resolve_story_id(client, &id).await?;
            let resp: serde_json::Value = client.get(&format!("/v1/stories/{full_id}")).await?;

            if cli.json {
                println!("{}", serde_json::to_string_pretty(&resp)?);
            } else {
                let s: Story = serde_json::from_value(resp)?;
                let id_short = s.id.to_string();
                let code_display = s.code.as_deref().unwrap_or(&id_short[..8]);

                // ── Header ──
                println!("╭─ {} ─ {}", code_display, s.title);
                println!(
                    "│ Status: {}  Epic: {}  Points: {}",
                    s.status,
                    s.epic_code.as_deref().unwrap_or("-"),
                    s.points
                        .map(|p| p.to_string())
                        .unwrap_or_else(|| "-".into())
                );
                if let Some(intent) = &s.intent {
                    println!("│ Intent: {intent}");
                }
                if let Some(persona) = &s.persona {
                    println!("│ Persona: {persona}");
                }
                if let Some(desc) = &s.description {
                    if !desc.is_empty() {
                        println!("│ Description: {desc}");
                    }
                }
                if let Some(deps) = &s.dependencies {
                    if !deps.is_empty() {
                        println!("│ Dependencies: {}", deps.join(", "));
                    }
                }

                // ── Tasks ──
                if let Some(tasks) = &s.tasks {
                    let done_count = tasks.iter().filter(|t| t.done).count();
                    println!("│");
                    println!("│ Tasks ({done_count}/{}):", tasks.len());
                    for (i, task) in tasks.iter().enumerate() {
                        let check = if task.done { "✓" } else { "✗" };
                        let persona_tag = if task.persona.is_empty() {
                            String::new()
                        } else {
                            format!(" ({})", task.persona)
                        };
                        println!("│   [{i}] {check} {}{persona_tag}", task.description);
                        if !task.done {
                            println!("│       → epic-runner backlog task-done {code_display} {i}");
                        }
                    }
                }

                // ── Acceptance Criteria ──
                if let Some(acs) = &s.acceptance_criteria {
                    let verified_count = acs.iter().filter(|a| a.verified).count();
                    println!("│");
                    println!("│ Acceptance Criteria ({verified_count}/{}):", acs.len());
                    for (i, ac) in acs.iter().enumerate() {
                        let check = if ac.verified { "✓" } else { "✗" };
                        println!("│   [{i}] {check} {}", ac.display_text());
                        if let Some(tb) = &ac.testable_by {
                            println!("│       test: {tb}");
                        }
                        if !ac.verified {
                            println!("│       → epic-runner backlog ac-verify {code_display} {i}");
                        }
                    }
                }

                println!("╰─");
            }
        }
        BacklogAction::Transition { id, status } => {
            let full_id = resolve_story_id(client, &id).await?;
            let body = json!({ "status": status, "updated_at": chrono::Utc::now().to_rfc3339() });
            let _: serde_json::Value = client
                .patch(&format!("/v1/stories/{full_id}"), &body)
                .await?;
            eprintln!("Story {id} → {status}");
        }
        BacklogAction::Delete { id } => {
            let full_id = resolve_story_id(client, &id).await?;
            client.delete(&format!("/v1/stories/{full_id}")).await?;
            eprintln!("Story {id} deleted");
        }
        BacklogAction::TaskDone {
            story,
            index,
            outcome,
        } => {
            let full_id = resolve_story_id(client, &story).await?;
            let resp: serde_json::Value = client.get(&format!("/v1/stories/{full_id}")).await?;

            let mut tasks: Vec<serde_json::Value> =
                resp["tasks"].as_array().cloned().unwrap_or_default();

            if index >= tasks.len() {
                return Err(format!(
                    "Task index {index} out of range (story has {} tasks)",
                    tasks.len()
                )
                .into());
            }

            tasks[index]["done"] = json!(true);
            if let Some(o) = &outcome {
                tasks[index]["outcome"] = json!(o);
            }

            let body = json!({ "tasks": tasks, "updated_at": chrono::Utc::now().to_rfc3339() });
            let _: serde_json::Value = client
                .patch(&format!("/v1/stories/{full_id}"), &body)
                .await?;

            // Update local story file if available (for stop hook)
            update_local_story_file(&body);

            let desc = tasks[index]["description"].as_str().unwrap_or("(unnamed)");
            eprintln!("Task {index} marked done: {desc}");
        }
        BacklogAction::AcVerify {
            story,
            index,
            evidence,
        } => {
            let full_id = resolve_story_id(client, &story).await?;
            let resp: serde_json::Value = client.get(&format!("/v1/stories/{full_id}")).await?;

            let mut acs: Vec<serde_json::Value> = resp["acceptance_criteria"]
                .as_array()
                .cloned()
                .unwrap_or_default();

            if index >= acs.len() {
                return Err(format!(
                    "AC index {index} out of range (story has {} ACs)",
                    acs.len()
                )
                .into());
            }

            acs[index]["verified"] = json!(true);
            if let Some(e) = &evidence {
                acs[index]["evidence"] = json!(e);
            }

            let body = json!({ "acceptance_criteria": acs, "updated_at": chrono::Utc::now().to_rfc3339() });
            let _: serde_json::Value = client
                .patch(&format!("/v1/stories/{full_id}"), &body)
                .await?;

            update_local_story_file(&body);

            let criterion = acs[index]["criterion"]
                .as_str()
                .or_else(|| acs[index]["title"].as_str())
                .unwrap_or("(unnamed)");
            eprintln!("AC {index} verified: {criterion}");
        }
        BacklogAction::Block { story, reason } => {
            let full_id = resolve_story_id(client, &story).await?;
            let body = json!({
                "status": "blocked",
                "blocked_reason": reason,
                "updated_at": chrono::Utc::now().to_rfc3339()
            });
            let _: serde_json::Value = client
                .patch(&format!("/v1/stories/{full_id}"), &body)
                .await?;

            update_local_story_file(&body);

            eprintln!("Story {story} blocked: {reason}");
        }
        BacklogAction::Park { story, reason } => {
            let full_id = resolve_story_id(client, &story).await?;
            let mut body = json!({
                "status": "parked",
                "updated_at": chrono::Utc::now().to_rfc3339()
            });
            if let Some(r) = &reason {
                body["parked_reason"] = json!(r);
            }
            let _: serde_json::Value = client
                .patch(&format!("/v1/stories/{full_id}"), &body)
                .await?;

            update_local_story_file(&body);

            let msg = reason.as_deref().unwrap_or("no reason given");
            eprintln!("Story {story} parked: {msg}");
        }
    }

    Ok(())
}

/// Update the local story JSON file with partial fields from a PATCH.
/// The stop hook reads this file — keeping it in sync lets the hook
/// see task/AC updates without another API call.
fn update_local_story_file(patch: &serde_json::Value) {
    let story_file = match std::env::var("EPIC_RUNNER_STORY_FILE") {
        Ok(f) if !f.is_empty() => f,
        _ => return,
    };
    let path = std::path::Path::new(&story_file);
    if !path.exists() {
        return;
    }
    let Ok(content) = std::fs::read_to_string(path) else {
        return;
    };
    let Ok(mut story) = serde_json::from_str::<serde_json::Value>(&content) else {
        return;
    };
    // Merge patch fields into existing story
    if let Some(obj) = patch.as_object() {
        for (k, v) in obj {
            story[k] = v.clone();
        }
    }
    if let Ok(json) = serde_json::to_string_pretty(&story) {
        let _ = std::fs::write(path, json);
    }
}

/// Resolve a story identifier — accepts either a story code (e.g. "ER-042")
/// or a UUID/UUID prefix. Code lookup is tried first, then falls back to UUID resolution.
pub async fn resolve_story_id(
    client: &ApiClient,
    id_or_code: &str,
) -> Result<String, Box<dyn std::error::Error>> {
    // If it looks like a story code (letters-digits), try code lookup first
    if id_or_code.contains('-') && id_or_code.chars().next().is_some_and(|c| c.is_alphabetic()) {
        let all: DataWrapper<Vec<serde_json::Value>> = client.get("/v1/stories").await?;
        if let Some(story) = all
            .data
            .iter()
            .find(|s| s["code"].as_str() == Some(id_or_code))
        {
            if let Some(id) = story["id"].as_str() {
                return Ok(id.to_string());
            }
        }
    }
    // Fall back to UUID prefix resolution
    Ok(client.resolve_id("stories", id_or_code).await?)
}

/// Format a done/total fraction as a colored cell.
/// Green = all done, Yellow = partial, Red = none done, "-" = empty.
fn colored_fraction(done: usize, total: usize) -> Cell {
    if total == 0 {
        return Cell::new("-");
    }
    let text = format!("{done}/{total}");
    let color = if done == total {
        Color::Green
    } else if done > 0 {
        Color::Yellow
    } else {
        Color::Red
    };
    Cell::new(text).fg(color)
}

fn truncate(s: &str, max: usize) -> &str {
    if s.len() <= max {
        return s;
    }
    let mut end = max;
    while end > 0 && !s.is_char_boundary(end) {
        end -= 1;
    }
    &s[..end]
}
