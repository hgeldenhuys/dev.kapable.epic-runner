use clap::{Args, Subcommand};
use comfy_table::{Cell, Table};
use serde_json::json;

use super::CliConfig;
use crate::api_client::{ApiClient, DataWrapper};
use crate::types::Product;

#[derive(Args)]
pub struct ProductArgs {
    #[command(subcommand)]
    pub action: ProductAction,
}

#[derive(Subcommand)]
pub enum ProductAction {
    /// Create a new product
    Create {
        #[arg(long)]
        name: String,
        #[arg(long)]
        slug: String,
        #[arg(long)]
        repo_path: String,
        #[arg(long)]
        description: Option<String>,
        /// Short prefix for story codes (e.g. "ER" → ER-001). Defaults to uppercase slug initials.
        #[arg(long)]
        story_prefix: Option<String>,
    },
    /// List all products
    List,
    /// Show product details
    Show { id: String },
}

pub async fn run(
    args: ProductArgs,
    client: &ApiClient,
    cli: &CliConfig,
) -> Result<(), Box<dyn std::error::Error>> {
    match args.action {
        ProductAction::Create {
            name,
            slug,
            repo_path,
            description,
            story_prefix,
        } => {
            // Derive prefix from slug if not provided: "epic-runner" → "ER"
            let prefix = story_prefix.unwrap_or_else(|| derive_prefix(&slug));
            let body = json!({
                "name": name,
                "slug": slug,
                "repo_path": repo_path,
                "description": description,
                "story_prefix": prefix,
            });
            let resp: serde_json::Value = client.post("/v1/products", &body).await?;
            if cli.json {
                println!("{}", serde_json::to_string_pretty(&resp)?);
            } else {
                let id = resp["id"].as_str().unwrap_or("?");
                eprintln!("Product created: {id} ({name})");
                eprintln!(
                    "  Story prefix: {prefix} (stories will be {prefix}-001, {prefix}-002, ...)"
                );
            }
        }
        ProductAction::List => {
            let resp: DataWrapper<Vec<serde_json::Value>> = client.get("/v1/products").await?;
            if cli.json {
                println!("{}", serde_json::to_string_pretty(&resp.data)?);
            } else {
                let mut table = Table::new();
                table.set_header(vec!["ID", "Name", "Slug", "Repo Path"]);
                for row in &resp.data {
                    let p: Product = serde_json::from_value(row.clone())?;
                    table.add_row(vec![
                        Cell::new(&p.id.to_string()[..8]),
                        Cell::new(&p.name),
                        Cell::new(&p.slug),
                        Cell::new(&p.repo_path),
                    ]);
                }
                println!("{table}");
            }
        }
        ProductAction::Show { id } => {
            let resp: serde_json::Value = client.get(&format!("/v1/products/{id}")).await?;
            println!("{}", serde_json::to_string_pretty(&resp)?);
        }
    }

    Ok(())
}

/// Derives a short story-code prefix from a product slug.
///
/// Splits the slug on hyphens, takes the first character of each word,
/// and uppercases the result. This mirrors common acronym conventions:
/// `"epic-runner"` → `"ER"`, `"kapable"` → `"K"`, `"my-cool-app"` → `"MCA"`.
///
/// An empty or all-separator slug falls back to `"S"` so that story codes
/// are always valid (e.g. `"S-001"`).
///
/// # Algorithm
///
/// 1. Split `slug` on `'-'`.
/// 2. Take the first [`char`] of each non-empty word.
/// 3. Collect into a [`String`] and call `.to_uppercase()`.
/// 4. If the result is empty, return `"S"` as a safe fallback.
///
/// # Examples
///
/// ```
/// // Two-word slug → two-letter prefix
/// // derive_prefix("epic-runner") == "ER"
///
/// // Single-word slug → single-letter prefix
/// // derive_prefix("kapable") == "K"
///
/// // Three-word slug → three-letter prefix
/// // derive_prefix("my-cool-app") == "MCA"
///
/// // Empty slug → fallback prefix
/// // derive_prefix("") == "S"
/// ```
fn derive_prefix(slug: &str) -> String {
    let prefix: String = slug
        .split('-')
        .filter_map(|word| word.chars().next())
        .collect::<String>()
        .to_uppercase();
    if prefix.is_empty() {
        "S".to_string() // fallback
    } else {
        prefix
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn derive_prefix_from_slug() {
        assert_eq!(derive_prefix("epic-runner"), "ER");
        assert_eq!(derive_prefix("kapable"), "K");
        assert_eq!(derive_prefix("my-cool-app"), "MCA");
    }

    #[test]
    fn derive_prefix_empty_fallback() {
        assert_eq!(derive_prefix(""), "S");
    }
}
