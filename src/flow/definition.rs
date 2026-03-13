use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// A ceremony flow — a directed acyclic graph of ceremony nodes.
/// Compatible with Kapable Flow format but simplified for local execution.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CeremonyFlow {
    pub name: String,
    pub version: String,
    pub description: Option<String>,
    pub nodes: Vec<CeremonyNode>,
    pub edges: Vec<CeremonyEdge>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CeremonyNode {
    pub key: String,
    pub node_type: CeremonyNodeType,
    pub label: String,
    pub config: CeremonyNodeConfig,
    /// If true, this node always runs even if upstream gates fail
    #[serde(default)]
    pub always_run: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum CeremonyNodeType {
    Source,
    Harness,
    Agent,
    Gate,
    Loop,
    Merge,
    Output,
    /// Merge worktree to main, push, trigger Connect App Pipeline, wait for deploy.
    Deploy,
    /// Promote a deployment slot to primary (100% traffic).
    Promote,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct CeremonyNodeConfig {
    pub model: Option<String>,
    pub effort: Option<String>,
    pub budget_usd: Option<f64>,
    pub system_prompt: Option<String>,
    pub prompt: Option<String>,
    #[serde(default)]
    pub chrome: bool,
    /// Enable --brief mode for this node (activates SendUserMessage tool)
    #[serde(default)]
    pub brief: bool,
    pub allowed_tools: Option<Vec<String>>,
    pub heartbeat_timeout_secs: Option<u64>,
    pub gate_field: Option<String>,
    pub gate_expect: Option<String>,
    pub loop_max: Option<i32>,
    pub rubber_duck_after: Option<i32>,
    /// Execute one Claude session per story (story UUID = session ID).
    /// When true, the Loop node iterates over stories individually rather than
    /// dispatching all stories to a single session. Enables: stop hooks per story,
    /// per-story retro via --resume, file tracking per story, context isolation.
    #[serde(default)]
    pub per_story: bool,
    /// Resume each story's previous Claude session instead of creating a new one.
    /// Used for per-story retro: resumes the builder session (story UUID) with
    /// the scrum-master agent to interview about what happened during execution.
    #[serde(default)]
    pub resume_stories: bool,
    /// Maximum turns for the Claude CLI session (defaults to 50 if not set)
    pub max_turns: Option<u32>,
    pub agent: Option<String>,
    /// Deploy node: Connect App Pipeline app ID
    pub deploy_app_id: Option<String>,
    /// Deploy node: admin API key for triggering pipeline
    pub deploy_api_key: Option<String>,
    /// Deploy node: API base URL (defaults to https://api.kapable.dev)
    pub deploy_api_url: Option<String>,
    /// Deploy node: max seconds to wait for deploy (default 300)
    pub deploy_timeout_secs: Option<u64>,
    /// Deploy node: production URL to verify health after deploy
    pub deploy_health_url: Option<String>,
    /// Deploy node: deployment slot name (e.g., "standby" for blue-green)
    pub deploy_slot: Option<String>,
    /// Deploy/Promote node: production URL for live comparison
    pub deploy_production_url: Option<String>,
    /// Deploy node: standby URL for A/B judge comparison
    pub deploy_standby_url: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CeremonyEdge {
    pub source: String,
    pub target: String,
    pub handle: Option<String>,
}

impl CeremonyFlow {
    /// Load from YAML string
    pub fn from_yaml(yaml: &str) -> Result<Self, serde_yaml::Error> {
        serde_yaml::from_str(yaml)
    }

    /// Get the default ceremony flow (embedded in binary)
    pub fn default_flow() -> Self {
        let yaml = include_str!("default_flow.yaml");
        Self::from_yaml(yaml).expect("Default flow YAML must be valid")
    }

    /// Build adjacency map: node_key → [(downstream_key, handle)]
    pub fn adjacency(&self) -> HashMap<String, Vec<(String, Option<String>)>> {
        let mut adj: HashMap<String, Vec<(String, Option<String>)>> = HashMap::new();
        for edge in &self.edges {
            adj.entry(edge.source.clone())
                .or_default()
                .push((edge.target.clone(), edge.handle.clone()));
        }
        adj
    }

    /// Build in-degree map for Kahn's algorithm
    pub fn in_degrees(&self) -> HashMap<String, usize> {
        let mut deg: HashMap<String, usize> = HashMap::new();
        for node in &self.nodes {
            deg.entry(node.key.clone()).or_insert(0);
        }
        for edge in &self.edges {
            *deg.entry(edge.target.clone()).or_insert(0) += 1;
        }
        deg
    }

    /// Build reverse adjacency: node_key → [source_keys that feed into it]
    /// Used to compute {{input}} — the outputs of a node's direct parents.
    pub fn reverse_adjacency(&self) -> HashMap<String, Vec<String>> {
        let mut rev: HashMap<String, Vec<String>> = HashMap::new();
        for edge in &self.edges {
            rev.entry(edge.target.clone())
                .or_default()
                .push(edge.source.clone());
        }
        rev
    }

    /// Find a node by key
    pub fn node(&self, key: &str) -> Option<&CeremonyNode> {
        self.nodes.iter().find(|n| n.key == key)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_flow_parses() {
        let flow = CeremonyFlow::default_flow();
        assert_eq!(flow.nodes.len(), 12); // v4: source → execute → judge → deploy chain → retro → output (no research/groom)
        assert_eq!(flow.edges.len(), 14); // v4: direct execution + deploy chain gates
        assert!(flow.node("source").is_some());
        assert!(flow.node("execute").is_some());
        assert!(flow.node("judge_code").is_some());
        assert!(flow.node("deploy_standby").is_some());
        assert!(flow.node("gate_deploy").is_some());
        assert!(flow.node("judge_ab").is_some());
        assert!(flow.node("gate_ab").is_some());
        assert!(flow.node("promote").is_some());
        assert!(flow.node("sm_retro").is_some());
        // v4: no research or groom nodes
        assert!(flow.node("research").is_none());
        assert!(flow.node("groom").is_none());
    }

    #[test]
    fn topological_order_has_source_first() {
        let flow = CeremonyFlow::default_flow();
        let degrees = flow.in_degrees();
        assert_eq!(degrees["source"], 0);
        assert!(degrees["execute"] > 0);
    }
}
