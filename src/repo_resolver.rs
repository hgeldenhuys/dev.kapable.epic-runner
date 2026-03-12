//! Resolves a git remote URL to a local clone path.
//!
//! Resolution order:
//! 1. Config.toml [local_overrides] explicit mapping
//! 2. Scan well-known directories for a git repo whose remote matches
//! 3. Error with helpful message if no match found

use std::path::{Path, PathBuf};

/// Resolve a repo URL to a local filesystem path.
///
/// Checks: config override -> directory scan -> error.
pub fn resolve_repo_url(repo_url: &str) -> Result<String, String> {
    // 1. Check config.toml for explicit local_override
    if let Some(path) = check_config_override(repo_url) {
        let p = Path::new(&path);
        if p.exists() && p.join(".git").exists() {
            tracing::info!(repo_url, path = %path, "Using config.toml local_override");
            return Ok(path);
        }
        tracing::warn!(
            repo_url, path = %path,
            "Config local_override path doesn't exist or isn't a git repo — falling back to scan"
        );
    }

    // 2. Scan well-known directories
    let scan_dirs = get_scan_directories();
    for dir in &scan_dirs {
        let dir_path = Path::new(dir);
        if !dir_path.is_dir() {
            continue;
        }
        // Check each subdirectory
        let entries = match std::fs::read_dir(dir_path) {
            Ok(e) => e,
            Err(_) => continue,
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if !path.is_dir() || !path.join(".git").exists() {
                continue;
            }
            if repo_remote_matches(&path, repo_url) {
                let resolved = path.to_string_lossy().to_string();
                tracing::info!(repo_url, path = %resolved, "Resolved repo URL via directory scan");
                return Ok(resolved);
            }
        }
    }

    // 3. Also check current directory
    let cwd = std::env::current_dir().unwrap_or_default();
    if cwd.join(".git").exists() && repo_remote_matches(&cwd, repo_url) {
        let resolved = cwd.to_string_lossy().to_string();
        tracing::info!(repo_url, path = %resolved, "Resolved repo URL from current directory");
        return Ok(resolved);
    }

    Err(format!(
        "No local clone found for {repo_url}. \
         Clone it first or set local_path in .epic-runner/config.toml:\n\n\
         [local_overrides]\n\
         \"{repo_url}\" = \"/path/to/your/clone\""
    ))
}

/// Resolve a Product's repository to a local path.
/// Handles both new-style repo_url and legacy repo_path.
pub fn resolve_product_repo(repo_url: Option<&str>, repo_path: &str) -> Result<String, String> {
    if let Some(url) = repo_url {
        // New-style: resolve URL to local clone
        resolve_repo_url(url)
    } else if !repo_path.is_empty() {
        // Legacy: absolute path — use as-is with deprecation warning
        let p = Path::new(repo_path);
        if p.is_absolute() {
            tracing::warn!(
                repo_path,
                "Using legacy absolute repo_path — this is machine-specific. \
                 Re-create the product with --repo-url for multi-machine portability, \
                 or add [local_overrides] to .epic-runner/config.toml"
            );
        }
        if p.exists() {
            Ok(repo_path.to_string())
        } else {
            Err(format!(
                "Legacy repo_path '{}' does not exist on this machine. \
                 Set repo_url for portable resolution.",
                repo_path
            ))
        }
    } else {
        // Neither repo_url nor repo_path set — use CWD
        let cwd = std::env::current_dir()
            .map(|p| p.to_string_lossy().to_string())
            .unwrap_or_else(|_| ".".to_string());
        tracing::warn!(
            "No repo_url or repo_path — defaulting to current directory: {}",
            cwd
        );
        Ok(cwd)
    }
}

/// Check config.toml for a [local_overrides] mapping.
fn check_config_override(repo_url: &str) -> Option<String> {
    // Try project config first, then home config
    for config_path in config_paths() {
        if let Ok(content) = std::fs::read_to_string(&config_path) {
            if let Ok(table) = content.parse::<toml::Table>() {
                if let Some(overrides) = table.get("local_overrides").and_then(|v| v.as_table()) {
                    if let Some(path) = overrides.get(repo_url).and_then(|v| v.as_str()) {
                        return Some(path.to_string());
                    }
                    // Also try normalized URL (strip .git suffix, compare)
                    let normalized = normalize_url(repo_url);
                    for (key, value) in overrides {
                        if normalize_url(key) == normalized {
                            if let Some(path) = value.as_str() {
                                return Some(path.to_string());
                            }
                        }
                    }
                }
            }
        }
    }
    None
}

/// Get directories to scan for git clones.
fn get_scan_directories() -> Vec<String> {
    let home = std::env::var("HOME").unwrap_or_default();
    let mut dirs = vec![".".to_string()]; // Current directory first

    if !home.is_empty() {
        // Common development directories
        for subdir in &[
            "Projects",
            "projects",
            "src",
            "code",
            "repos",
            "dev",
            "workspace",
            "WebstormProjects",
            "IdeaProjects",
            "Developer",
            "Code",
            "git",
        ] {
            dirs.push(format!("{home}/{subdir}"));
        }
        // Home directory itself (for top-level clones)
        dirs.push(home);
    }

    dirs
}

/// Check if a git repo at `path` has a remote matching `repo_url`.
fn repo_remote_matches(path: &Path, repo_url: &str) -> bool {
    let output = std::process::Command::new("git")
        .args(["remote", "-v"])
        .current_dir(path)
        .output();

    let output = match output {
        Ok(o) if o.status.success() => o,
        _ => return false,
    };

    let remotes = String::from_utf8_lossy(&output.stdout);
    let target_normalized = normalize_url(repo_url);

    for line in remotes.lines() {
        // Format: "origin\tgit@github.com:org/repo.git (fetch)"
        let parts: Vec<&str> = line.split_whitespace().collect();
        if parts.len() >= 2 {
            let remote_url = parts[1];
            if normalize_url(remote_url) == target_normalized {
                return true;
            }
        }
    }
    false
}

/// Normalize a git URL for comparison.
/// Handles: HTTPS vs SSH, trailing .git, case differences.
/// `git@github.com:org/repo.git` == `https://github.com/org/repo`
fn normalize_url(url: &str) -> String {
    let mut s = url.trim().to_lowercase();

    // Strip trailing .git
    if s.ends_with(".git") {
        s = s[..s.len() - 4].to_string();
    }
    // Strip trailing /
    if s.ends_with('/') {
        s = s[..s.len() - 1].to_string();
    }

    // Convert SSH to canonical form: git@host:org/repo -> host/org/repo
    if let Some(rest) = s.strip_prefix("git@") {
        s = rest.replacen(':', "/", 1);
    }
    // Convert HTTPS: https://host/org/repo -> host/org/repo
    if let Some(rest) = s.strip_prefix("https://") {
        s = rest.to_string();
    }
    if let Some(rest) = s.strip_prefix("http://") {
        s = rest.to_string();
    }
    // Strip www. prefix
    if let Some(rest) = s.strip_prefix("www.") {
        s = rest.to_string();
    }

    s
}

/// Config file paths to check (project first, then home).
fn config_paths() -> Vec<PathBuf> {
    let mut paths = Vec::new();

    // Walk up from CWD looking for .epic-runner/config.toml
    if let Ok(mut dir) = std::env::current_dir() {
        loop {
            let config = dir.join(".epic-runner/config.toml");
            if config.exists() {
                paths.push(config);
                break;
            }
            if dir.join(".git").exists() || !dir.pop() {
                break;
            }
        }
    }

    // Home config
    if let Ok(home) = std::env::var("HOME") {
        let path = PathBuf::from(format!("{home}/.epic-runner/config.toml"));
        if path.exists() {
            paths.push(path);
        }
    }

    paths
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalize_ssh_and_https() {
        assert_eq!(
            normalize_url("git@github.com:org/repo.git"),
            normalize_url("https://github.com/org/repo")
        );
    }

    #[test]
    fn normalize_strips_trailing_git() {
        assert_eq!(
            normalize_url("https://github.com/org/repo.git"),
            "github.com/org/repo"
        );
    }

    #[test]
    fn normalize_case_insensitive() {
        assert_eq!(
            normalize_url("git@GitHub.com:Org/Repo.git"),
            normalize_url("https://github.com/org/repo")
        );
    }

    #[test]
    fn normalize_strips_trailing_slash() {
        assert_eq!(
            normalize_url("https://github.com/org/repo/"),
            "github.com/org/repo"
        );
    }
}
