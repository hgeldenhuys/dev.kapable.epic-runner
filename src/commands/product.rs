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
        /// Local repository path (DEPRECATED: use --repo-url for portability)
        #[arg(long)]
        repo_path: Option<String>,
        /// Git remote URL for multi-machine portability (e.g. git@github.com:org/repo.git)
        #[arg(long)]
        repo_url: Option<String>,
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
            repo_url,
            description,
            story_prefix,
        } => {
            // Must provide at least one of --repo-url or --repo-path
            if repo_url.is_none() && repo_path.is_none() {
                return Err("Must provide --repo-url (recommended) or --repo-path. \
                     Use --repo-url for multi-machine portability."
                    .into());
            }

            // If only repo_url provided, use CWD as repo_path placeholder
            let effective_repo_path = repo_path.unwrap_or_else(|| {
                std::env::current_dir()
                    .map(|p| p.to_string_lossy().to_string())
                    .unwrap_or_else(|_| ".".to_string())
            });

            if repo_url.is_none() {
                eprintln!(
                    "Warning: --repo-path is machine-specific and deprecated. \
                     Use --repo-url for multi-machine portability."
                );
            }

            // Derive prefix from slug if not provided: "epic-runner" → "ER"
            let prefix = story_prefix.unwrap_or_else(|| derive_prefix(&slug));
            let mut body = json!({
                "name": name,
                "slug": slug,
                "repo_path": effective_repo_path,
                "description": description,
                "story_prefix": prefix,
            });
            if let Some(ref url) = repo_url {
                body["repo_url"] = json!(url);
            }
            let resp: serde_json::Value = client.post("/v1/products", &body).await?;
            if cli.json {
                println!("{}", serde_json::to_string_pretty(&resp)?);
            } else {
                let id = resp["id"].as_str().unwrap_or("?");
                eprintln!("Product created: {id} ({name})");
                eprintln!(
                    "  Story prefix: {prefix} (stories will be {prefix}-001, {prefix}-002, ...)"
                );
                if let Some(ref url) = repo_url {
                    eprintln!("  Repo URL: {url} (portable)");
                } else {
                    eprintln!("  Repo path: {effective_repo_path} (machine-specific)");
                }
            }
        }
        ProductAction::List => {
            let resp: DataWrapper<Vec<serde_json::Value>> = client.get("/v1/products").await?;
            if cli.json {
                println!("{}", serde_json::to_string_pretty(&resp.data)?);
            } else {
                let mut table = Table::new();
                table.set_header(vec!["ID", "Name", "Slug", "Repo", "Portable"]);
                for row in &resp.data {
                    let p: Product = serde_json::from_value(row.clone())?;
                    let repo_display = if let Some(ref url) = p.repo_url {
                        url.clone()
                    } else {
                        p.repo_path.clone()
                    };
                    let portable = if p.repo_url.is_some() { "yes" } else { "no" };
                    table.add_row(vec![
                        Cell::new(&p.id.to_string()[..8]),
                        Cell::new(&p.name),
                        Cell::new(&p.slug),
                        Cell::new(&repo_display),
                        Cell::new(portable),
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
