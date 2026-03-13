//! Embedded agent definitions for ceremony nodes.
//!
//! Agent `.md` files are embedded in the binary via `include_str!` and written
//! to a temp directory at runtime so Claude Code's `--agent` flag can reference them.
//! This avoids requiring agent files to exist in the target repo.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

/// Embedded agent definitions (name → content).
static AGENTS: OnceLock<HashMap<&'static str, &'static str>> = OnceLock::new();

fn embedded_agents() -> &'static HashMap<&'static str, &'static str> {
    AGENTS.get_or_init(|| {
        let mut m = HashMap::new();
        m.insert("researcher", include_str!("../agents/researcher.md"));
        m.insert("groomer", include_str!("../agents/groomer.md"));
        m.insert("builder", include_str!("../agents/builder.md"));
        m.insert("code-judge", include_str!("../agents/code-judge.md"));
        m.insert("ab-judge", include_str!("../agents/ab-judge.md"));
        m.insert("scrum-master", include_str!("../agents/scrum-master.md"));
        m.insert("rubber-duck", include_str!("../agents/rubber-duck.md"));
        m
    })
}

/// Resolve an agent name to an absolute path usable with `--agent`.
///
/// Resolution order:
/// 1. Check target repo's `.claude/agents/{name}.md` — user overrides win
/// 2. Write embedded agent to temp dir, return that path
///
/// Returns `None` if the agent name is unknown and not found on disk.
pub fn resolve_agent_path(name: &str, repo_path: &Path) -> Option<PathBuf> {
    // 1. Check for user override in the target repo
    let repo_agent = repo_path.join(".claude/agents").join(format!("{name}.md"));
    if repo_agent.exists() {
        tracing::debug!(agent = name, path = %repo_agent.display(), "Using repo-local agent override");
        return Some(repo_agent);
    }

    // 2. Fall back to embedded agent
    let content = embedded_agents().get(name)?;
    let agent_dir = std::env::temp_dir().join("epic-runner-agents");
    std::fs::create_dir_all(&agent_dir).ok()?;
    let agent_path = agent_dir.join(format!("{name}.md"));
    std::fs::write(&agent_path, content).ok()?;
    tracing::debug!(agent = name, path = %agent_path.display(), "Wrote embedded agent to temp dir");
    Some(agent_path)
}

/// Resolve an agent name to an absolute path, replacing template variables.
///
/// Same resolution order as `resolve_agent_path`, but substitutes `{{key}}` → `value`
/// in the agent content before writing to disk. Used by the groomer to inject
/// linked research notes into the prompt.
///
/// Variables map: e.g. `{"research_notes": "## Vector DB\n..."}`.
pub fn resolve_agent_path_with_vars(
    name: &str,
    repo_path: &Path,
    vars: &HashMap<String, String>,
) -> Option<PathBuf> {
    // 1. Check for user override in the target repo
    let repo_agent = repo_path.join(".claude/agents").join(format!("{name}.md"));
    let raw_content = if repo_agent.exists() {
        tracing::debug!(agent = name, path = %repo_agent.display(), "Using repo-local agent override (with vars)");
        std::fs::read_to_string(&repo_agent).ok()?
    } else {
        // 2. Fall back to embedded agent
        let content = embedded_agents().get(name)?;
        (*content).to_string()
    };

    // Replace template variables
    let mut content = raw_content;
    for (key, value) in vars {
        let placeholder = format!("{{{{{key}}}}}");
        content = content.replace(&placeholder, value);
    }

    let agent_dir = std::env::temp_dir().join("epic-runner-agents");
    std::fs::create_dir_all(&agent_dir).ok()?;
    let agent_path = agent_dir.join(format!("{name}.md"));
    std::fs::write(&agent_path, &content).ok()?;
    tracing::debug!(agent = name, path = %agent_path.display(), vars_count = vars.len(), "Wrote agent with vars to temp dir");
    Some(agent_path)
}

/// List all embedded agent names.
pub fn list_embedded() -> Vec<&'static str> {
    let mut names: Vec<_> = embedded_agents().keys().copied().collect();
    names.sort();
    names
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[test]
    fn all_agents_embedded() {
        let agents = embedded_agents();
        assert!(agents.contains_key("researcher"));
        assert!(agents.contains_key("groomer"));
        assert!(agents.contains_key("builder"));
        assert!(agents.contains_key("code-judge"));
        assert!(agents.contains_key("ab-judge"));
        assert!(agents.contains_key("scrum-master"));
        assert!(agents.contains_key("rubber-duck"));
        assert_eq!(agents.len(), 7);
    }

    #[test]
    fn resolve_writes_to_temp() {
        let fake_repo = PathBuf::from("/tmp/nonexistent-repo-for-test");
        let path = resolve_agent_path("researcher", &fake_repo).unwrap();
        assert!(path.exists());
        let content = std::fs::read_to_string(&path).unwrap();
        assert!(content.contains("research agent"));
    }

    #[test]
    fn resolve_unknown_returns_none() {
        let fake_repo = PathBuf::from("/tmp/nonexistent-repo-for-test");
        assert!(resolve_agent_path("nonexistent-agent", &fake_repo).is_none());
    }

    #[test]
    fn list_embedded_returns_all() {
        let names = list_embedded();
        assert_eq!(names.len(), 7);
        assert!(names.contains(&"researcher"));
        assert!(names.contains(&"builder"));
    }

    #[test]
    fn resolve_with_vars_replaces_placeholders() {
        let fake_repo = PathBuf::from("/tmp/nonexistent-repo-for-test");
        let mut vars = HashMap::new();
        vars.insert(
            "research_notes".to_string(),
            "## Vector DB Benchmarks\nPgvector outperforms FAISS for < 1M rows.".to_string(),
        );
        let path = resolve_agent_path_with_vars("groomer", &fake_repo, &vars).unwrap();
        assert!(path.exists());
        let content = std::fs::read_to_string(&path).unwrap();
        assert!(content.contains("## Vector DB Benchmarks"));
        assert!(content.contains("Pgvector outperforms FAISS"));
        // The placeholder should be gone
        assert!(!content.contains("{{research_notes}}"));
    }

    #[test]
    fn resolve_with_empty_vars_still_resolves() {
        let fake_repo = PathBuf::from("/tmp/nonexistent-repo-for-test");
        let vars = HashMap::new();
        let path = resolve_agent_path_with_vars("groomer", &fake_repo, &vars).unwrap();
        assert!(path.exists());
        // File should contain groomer content (no assertions on specific placeholders
        // since parallel tests write to the same temp path)
        let content = std::fs::read_to_string(&path).unwrap();
        assert!(content.contains("sprint planner"));
    }
}
