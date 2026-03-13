use clap::{Args, Subcommand};
use serde_json::json;

use super::CliConfig;
use crate::api_client::{ApiClient, DataWrapper};
use crate::commands::backlog::resolve_story_id;
use crate::types::ResearchNote;

#[derive(Args)]
pub struct ResearchArgs {
    #[command(subcommand)]
    pub action: ResearchAction,
}

#[derive(Subcommand)]
pub enum ResearchAction {
    /// Add a research note from a file and link it to a story
    Add {
        /// Story code or ID to link (e.g. "ER-059")
        story: String,
        /// Path to the research document (markdown)
        #[arg(long)]
        file: String,
        /// Title for the research note (defaults to filename)
        #[arg(long)]
        title: Option<String>,
        /// Tags (comma-separated)
        #[arg(long, value_delimiter = ',')]
        tags: Option<Vec<String>>,
    },
    /// List research notes linked to a story
    List {
        /// Story code or ID
        story: String,
    },
    /// Show a research note by ID
    Show {
        /// Research note ID (UUID or prefix)
        id: String,
    },
    /// Link an existing research note to a story
    Link {
        /// Story code or ID
        story: String,
        /// Research note ID
        #[arg(long)]
        note: String,
    },
    /// Unlink a research note from a story
    Unlink {
        /// Story code or ID
        story: String,
        /// Research note ID
        #[arg(long)]
        note: String,
    },
}

pub async fn run(
    args: ResearchArgs,
    client: &ApiClient,
    cli: &CliConfig,
) -> Result<(), Box<dyn std::error::Error>> {
    match args.action {
        ResearchAction::Add {
            story,
            file,
            title,
            tags,
        } => {
            // Read the research document from disk
            let content = std::fs::read_to_string(&file)
                .map_err(|e| format!("Cannot read file '{file}': {e}"))?;

            // Derive title from filename if not provided
            let title = title.unwrap_or_else(|| {
                std::path::Path::new(&file)
                    .file_stem()
                    .and_then(|s| s.to_str())
                    .unwrap_or("untitled")
                    .to_string()
            });

            // Resolve story to get product_id
            let story_id = resolve_story_id(client, &story).await?;
            let story_data: serde_json::Value =
                client.get(&format!("/v1/stories/{story_id}")).await?;
            let product_id = story_data["product_id"]
                .as_str()
                .ok_or("Story has no product_id")?;

            // Create the research note
            let now = chrono::Utc::now().to_rfc3339();
            let note_body = json!({
                "product_id": product_id,
                "title": title,
                "content": content,
                "source_path": file,
                "tags": tags,
                "created_at": now,
                "updated_at": now,
            });
            let note_resp: serde_json::Value =
                client.post("/v1/research_notes", &note_body).await?;
            let note_id = note_resp["id"]
                .as_str()
                .ok_or("research_notes POST returned no id")?;

            // Create the link
            let link_body = json!({
                "story_id": story_id,
                "research_note_id": note_id,
                "created_at": now,
            });
            let _: serde_json::Value = client.post("/v1/story_research_links", &link_body).await?;

            if cli.json {
                println!("{}", serde_json::to_string_pretty(&note_resp)?);
            } else {
                eprintln!("Research note created: {note_id}");
                eprintln!("  Title: {title}");
                eprintln!("  Source: {file}");
                eprintln!("  Linked to story: {story}");
            }
        }
        ResearchAction::List { story } => {
            let story_id = resolve_story_id(client, &story).await?;

            // Get all links for this story
            let links: DataWrapper<Vec<serde_json::Value>> =
                client.get("/v1/story_research_links").await?;
            let story_links: Vec<&serde_json::Value> = links
                .data
                .iter()
                .filter(|l| l["story_id"].as_str() == Some(&story_id))
                .collect();

            if story_links.is_empty() {
                if cli.json {
                    println!("[]");
                } else {
                    eprintln!("No research notes linked to {story}");
                }
                return Ok(());
            }

            // Fetch all research notes and filter to linked ones
            let notes: DataWrapper<Vec<serde_json::Value>> =
                client.get("/v1/research_notes").await?;
            let note_ids: Vec<&str> = story_links
                .iter()
                .filter_map(|l| l["research_note_id"].as_str())
                .collect();

            let linked_notes: Vec<&serde_json::Value> = notes
                .data
                .iter()
                .filter(|n| n["id"].as_str().is_some_and(|id| note_ids.contains(&id)))
                .collect();

            if cli.json {
                println!("{}", serde_json::to_string_pretty(&linked_notes)?);
            } else {
                eprintln!("Research notes for {story}:");
                for note in &linked_notes {
                    let id = note["id"].as_str().unwrap_or("?");
                    let title = note["title"].as_str().unwrap_or("(untitled)");
                    let source = note["source_path"].as_str().unwrap_or("-");
                    let id_short = if id.len() > 8 { &id[..8] } else { id };
                    eprintln!("  {id_short}  {title}  ({source})");
                }
                eprintln!("{} notes", linked_notes.len());
            }
        }
        ResearchAction::Show { id } => {
            let full_id = client.resolve_id("research_notes", &id).await?;
            let resp: serde_json::Value =
                client.get(&format!("/v1/research_notes/{full_id}")).await?;

            if cli.json {
                println!("{}", serde_json::to_string_pretty(&resp)?);
            } else {
                let note: ResearchNote = serde_json::from_value(resp)?;
                println!("╭─ {} ─ {}", &note.id.to_string()[..8], note.title);
                if let Some(source) = &note.source_path {
                    println!("│ Source: {source}");
                }
                if let Some(tags) = &note.tags {
                    if !tags.is_empty() {
                        println!("│ Tags: {}", tags.join(", "));
                    }
                }
                println!("│");
                // Print content (truncated for terminal)
                for line in note.content.lines().take(50) {
                    println!("│ {line}");
                }
                if note.content.lines().count() > 50 {
                    println!("│ ... ({} more lines)", note.content.lines().count() - 50);
                }
                println!("╰─");
            }
        }
        ResearchAction::Link { story, note } => {
            let story_id = resolve_story_id(client, &story).await?;
            let note_id = client.resolve_id("research_notes", &note).await?;

            // Check for existing link
            let links: DataWrapper<Vec<serde_json::Value>> =
                client.get("/v1/story_research_links").await?;
            let already_linked = links.data.iter().any(|l| {
                l["story_id"].as_str() == Some(&story_id)
                    && l["research_note_id"].as_str() == Some(&note_id)
            });

            if already_linked {
                eprintln!("Already linked: {story} ↔ {note}");
                return Ok(());
            }

            let now = chrono::Utc::now().to_rfc3339();
            let body = json!({
                "story_id": story_id,
                "research_note_id": note_id,
                "created_at": now,
            });
            let _: serde_json::Value = client.post("/v1/story_research_links", &body).await?;

            eprintln!("Linked: {story} ↔ {note}");
        }
        ResearchAction::Unlink { story, note } => {
            let story_id = resolve_story_id(client, &story).await?;
            let note_id = client.resolve_id("research_notes", &note).await?;

            let links: DataWrapper<Vec<serde_json::Value>> =
                client.get("/v1/story_research_links").await?;
            let link = links
                .data
                .iter()
                .find(|l| {
                    l["story_id"].as_str() == Some(&story_id)
                        && l["research_note_id"].as_str() == Some(&note_id)
                })
                .ok_or(format!("No link found between {story} and {note}"))?;

            let link_id = link["id"].as_str().ok_or("Link has no id")?;
            client
                .delete(&format!("/v1/story_research_links/{link_id}"))
                .await?;

            eprintln!("Unlinked: {story} ↔ {note}");
        }
    }

    Ok(())
}

/// Fetch all research notes linked to a story by its ID.
/// Used by the groomer to inject research context into prompts.
pub async fn fetch_research_for_story(
    client: &ApiClient,
    story_id: &str,
) -> Result<Vec<ResearchNote>, Box<dyn std::error::Error>> {
    let links: DataWrapper<Vec<serde_json::Value>> = client.get("/v1/story_research_links").await?;
    let note_ids: Vec<String> = links
        .data
        .iter()
        .filter(|l| l["story_id"].as_str() == Some(story_id))
        .filter_map(|l| l["research_note_id"].as_str().map(String::from))
        .collect();

    if note_ids.is_empty() {
        return Ok(vec![]);
    }

    let all_notes: DataWrapper<Vec<serde_json::Value>> = client.get("/v1/research_notes").await?;
    let mut result = Vec::new();
    for note_val in &all_notes.data {
        if let Some(id) = note_val["id"].as_str() {
            if note_ids.iter().any(|nid| nid == id) {
                if let Ok(note) = serde_json::from_value::<ResearchNote>(note_val.clone()) {
                    result.push(note);
                }
            }
        }
    }

    Ok(result)
}
