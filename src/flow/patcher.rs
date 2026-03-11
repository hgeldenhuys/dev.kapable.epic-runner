use serde::{Deserialize, Serialize};

use super::definition::*;

/// A single patch operation that can be applied to a CeremonyFlow.
/// Patches are serializable so they can be logged, persisted, and replayed.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum FlowPatch {
    /// Insert a new node between two existing nodes.
    /// Removes the edge `after_node → before_node` and creates:
    ///   `after_node → new_node → before_node`
    InsertNode {
        node: Box<CeremonyNode>,
        after_node: String,
        before_node: String,
    },

    /// Remove a node and rewire: all incoming edges → all outgoing targets.
    RemoveNode { key: String },

    /// Update specific config fields on an existing node.
    /// Only non-None fields in the update are applied (partial merge).
    UpdateNodeConfig { key: String, config: ConfigUpdate },
}

/// Partial config update — only set fields override the existing config.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct ConfigUpdate {
    pub model: Option<String>,
    pub effort: Option<String>,
    pub budget_usd: Option<f64>,
    pub loop_max: Option<i32>,
    pub rubber_duck_after: Option<i32>,
    pub heartbeat_timeout_secs: Option<u64>,
    pub system_prompt: Option<String>,
    pub prompt: Option<String>,
}

/// Result of applying patches — includes the modified flow and a log of what changed.
#[derive(Debug, Clone)]
pub struct PatchResult {
    pub flow: CeremonyFlow,
    pub applied: Vec<String>,
    pub skipped: Vec<String>,
}

/// Apply a sequence of patches to a ceremony flow.
/// Each patch is applied in order. Invalid patches are skipped with a warning.
/// Returns the modified flow plus a log of applied/skipped patches.
pub fn apply_patches(flow: &CeremonyFlow, patches: &[FlowPatch]) -> PatchResult {
    let mut result = flow.clone();
    let mut applied = Vec::new();
    let mut skipped = Vec::new();

    for (i, patch) in patches.iter().enumerate() {
        match apply_one(&mut result, patch) {
            Ok(desc) => applied.push(format!("[{}] {}", i, desc)),
            Err(reason) => skipped.push(format!("[{}] {}", i, reason)),
        }
    }

    PatchResult {
        flow: result,
        applied,
        skipped,
    }
}

/// Apply a single patch, returning a description on success or a reason on failure.
fn apply_one(flow: &mut CeremonyFlow, patch: &FlowPatch) -> Result<String, String> {
    match patch {
        FlowPatch::InsertNode {
            node,
            after_node,
            before_node,
        } => insert_node(flow, node, after_node, before_node),
        FlowPatch::RemoveNode { key } => remove_node(flow, key),
        FlowPatch::UpdateNodeConfig { key, config } => update_node_config(flow, key, config),
    }
}

/// Insert a node between `after_node` and `before_node`.
fn insert_node(
    flow: &mut CeremonyFlow,
    node: &CeremonyNode,
    after_node: &str,
    before_node: &str,
) -> Result<String, String> {
    // Validate: both anchor nodes must exist
    if flow.node(after_node).is_none() {
        return Err(format!("InsertNode: after_node '{}' not found", after_node));
    }
    if flow.node(before_node).is_none() {
        return Err(format!(
            "InsertNode: before_node '{}' not found",
            before_node
        ));
    }

    // Validate: new node key must not already exist
    if flow.node(&node.key).is_some() {
        return Err(format!("InsertNode: node '{}' already exists", node.key));
    }

    // Find and remove the edge after_node → before_node
    let edge_idx = flow
        .edges
        .iter()
        .position(|e| e.source == after_node && e.target == before_node);

    let removed_handle = match edge_idx {
        Some(idx) => {
            let removed = flow.edges.remove(idx);
            removed.handle
        }
        None => {
            return Err(format!(
                "InsertNode: no edge from '{}' to '{}'",
                after_node, before_node
            ));
        }
    };

    // Add the new node
    // Find the position to insert (after the after_node in the nodes list)
    let insert_pos = flow
        .nodes
        .iter()
        .position(|n| n.key == before_node)
        .unwrap_or(flow.nodes.len());
    flow.nodes.insert(insert_pos, node.clone());

    // Add edges: after_node → new_node → before_node
    // The edge from after_node keeps the original handle (important for gate pass/fail edges)
    flow.edges.push(CeremonyEdge {
        source: after_node.to_string(),
        target: node.key.clone(),
        handle: removed_handle,
    });
    flow.edges.push(CeremonyEdge {
        source: node.key.clone(),
        target: before_node.to_string(),
        handle: None,
    });

    Ok(format!(
        "Inserted '{}' between '{}' and '{}'",
        node.key, after_node, before_node
    ))
}

/// Remove a node and rewire: connect all sources to all targets.
fn remove_node(flow: &mut CeremonyFlow, key: &str) -> Result<String, String> {
    if flow.node(key).is_none() {
        return Err(format!("RemoveNode: node '{}' not found", key));
    }

    // Don't allow removing source or output nodes
    if let Some(node) = flow.node(key) {
        if node.node_type == CeremonyNodeType::Source || node.node_type == CeremonyNodeType::Output
        {
            return Err(format!(
                "RemoveNode: cannot remove {:?} node '{}'",
                node.node_type, key
            ));
        }
    }

    // Collect incoming and outgoing edges
    let incoming: Vec<(String, Option<String>)> = flow
        .edges
        .iter()
        .filter(|e| e.target == key)
        .map(|e| (e.source.clone(), e.handle.clone()))
        .collect();

    let outgoing: Vec<(String, Option<String>)> = flow
        .edges
        .iter()
        .filter(|e| e.source == key)
        .map(|e| (e.target.clone(), e.handle.clone()))
        .collect();

    // Remove all edges involving this node
    flow.edges.retain(|e| e.source != key && e.target != key);

    // Rewire: each incoming source → each outgoing target
    for (src, src_handle) in &incoming {
        for (tgt, _) in &outgoing {
            flow.edges.push(CeremonyEdge {
                source: src.clone(),
                target: tgt.clone(),
                handle: src_handle.clone(),
            });
        }
    }

    // Remove the node
    flow.nodes.retain(|n| n.key != key);

    Ok(format!("Removed node '{}'", key))
}

/// Update specific config fields on an existing node.
fn update_node_config(
    flow: &mut CeremonyFlow,
    key: &str,
    update: &ConfigUpdate,
) -> Result<String, String> {
    let node = flow
        .nodes
        .iter_mut()
        .find(|n| n.key == key)
        .ok_or_else(|| format!("UpdateNodeConfig: node '{}' not found", key))?;

    let mut changes = Vec::new();

    if let Some(ref model) = update.model {
        node.config.model = Some(model.clone());
        changes.push(format!("model={}", model));
    }
    if let Some(ref effort) = update.effort {
        node.config.effort = Some(effort.clone());
        changes.push(format!("effort={}", effort));
    }
    if let Some(budget) = update.budget_usd {
        node.config.budget_usd = Some(budget);
        changes.push(format!("budget_usd={}", budget));
    }
    if let Some(loop_max) = update.loop_max {
        node.config.loop_max = Some(loop_max);
        changes.push(format!("loop_max={}", loop_max));
    }
    if let Some(rubber_duck_after) = update.rubber_duck_after {
        node.config.rubber_duck_after = Some(rubber_duck_after);
        changes.push(format!("rubber_duck_after={}", rubber_duck_after));
    }
    if let Some(timeout) = update.heartbeat_timeout_secs {
        node.config.heartbeat_timeout_secs = Some(timeout);
        changes.push(format!("heartbeat_timeout_secs={}", timeout));
    }
    if let Some(ref prompt) = update.system_prompt {
        node.config.system_prompt = Some(prompt.clone());
        changes.push("system_prompt=<updated>".to_string());
    }
    if let Some(ref prompt) = update.prompt {
        node.config.prompt = Some(prompt.clone());
        changes.push("prompt=<updated>".to_string());
    }

    if changes.is_empty() {
        return Err(format!(
            "UpdateNodeConfig: no fields to update on '{}'",
            key
        ));
    }

    Ok(format!("Updated '{}': {}", key, changes.join(", ")))
}

/// Serialize a CeremonyFlow back to YAML.
pub fn flow_to_yaml(flow: &CeremonyFlow) -> Result<String, serde_yaml::Error> {
    serde_yaml::to_string(flow)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn minimal_flow() -> CeremonyFlow {
        CeremonyFlow::from_yaml(
            r#"
name: "Test"
version: "1.0.0"
nodes:
  - key: source
    node_type: source
    label: "S"
    config: {}
  - key: research
    node_type: harness
    label: "Research"
    config:
      model: sonnet
      prompt: "Do research"
  - key: output
    node_type: output
    label: "O"
    config: {}
edges:
  - source: source
    target: research
  - source: research
    target: output
"#,
        )
        .unwrap()
    }

    #[test]
    fn insert_node_between_existing() {
        let flow = minimal_flow();
        let new_node = CeremonyNode {
            key: "review".to_string(),
            node_type: CeremonyNodeType::Harness,
            label: "Review".to_string(),
            config: CeremonyNodeConfig {
                model: Some("sonnet".to_string()),
                prompt: Some("Review the research".to_string()),
                ..Default::default()
            },
            always_run: false,
        };

        let patches = vec![FlowPatch::InsertNode {
            node: Box::new(new_node),
            after_node: "research".to_string(),
            before_node: "output".to_string(),
        }];

        let result = apply_patches(&flow, &patches);
        assert_eq!(result.applied.len(), 1);
        assert_eq!(result.skipped.len(), 0);
        assert_eq!(result.flow.nodes.len(), 4);

        // Verify edges: research → review → output
        let adj = result.flow.adjacency();
        let research_targets: Vec<&str> = adj["research"].iter().map(|(t, _)| t.as_str()).collect();
        assert!(research_targets.contains(&"review"));
        assert!(!research_targets.contains(&"output"));

        let review_targets: Vec<&str> = adj["review"].iter().map(|(t, _)| t.as_str()).collect();
        assert!(review_targets.contains(&"output"));
    }

    #[test]
    fn insert_node_preserves_gate_handle() {
        let flow = CeremonyFlow::default_flow();
        let new_node = CeremonyNode {
            key: "research_review".to_string(),
            node_type: CeremonyNodeType::Harness,
            label: "Research Review".to_string(),
            config: CeremonyNodeConfig {
                model: Some("sonnet".to_string()),
                prompt: Some("Review research quality".to_string()),
                ..Default::default()
            },
            always_run: false,
        };

        // Insert between gate_research (pass) → groom
        let patches = vec![FlowPatch::InsertNode {
            node: Box::new(new_node),
            after_node: "gate_research".to_string(),
            before_node: "groom".to_string(),
        }];

        let result = apply_patches(&flow, &patches);
        assert_eq!(result.applied.len(), 1);

        // Verify the pass handle is preserved on gate_research → research_review edge
        let pass_edge = result
            .flow
            .edges
            .iter()
            .find(|e| e.source == "gate_research" && e.target == "research_review");
        assert!(pass_edge.is_some());
        assert_eq!(pass_edge.unwrap().handle.as_deref(), Some("pass"));
    }

    #[test]
    fn remove_node_rewires_edges() {
        let flow = minimal_flow();
        let patches = vec![FlowPatch::RemoveNode {
            key: "research".to_string(),
        }];

        let result = apply_patches(&flow, &patches);
        assert_eq!(result.applied.len(), 1);
        assert_eq!(result.flow.nodes.len(), 2);

        // source → output (rewired)
        let adj = result.flow.adjacency();
        let source_targets: Vec<&str> = adj["source"].iter().map(|(t, _)| t.as_str()).collect();
        assert!(source_targets.contains(&"output"));
    }

    #[test]
    fn cannot_remove_source_or_output() {
        let flow = minimal_flow();

        let result = apply_patches(
            &flow,
            &[FlowPatch::RemoveNode {
                key: "source".to_string(),
            }],
        );
        assert_eq!(result.skipped.len(), 1);
        assert!(result.skipped[0].contains("cannot remove"));

        let result = apply_patches(
            &flow,
            &[FlowPatch::RemoveNode {
                key: "output".to_string(),
            }],
        );
        assert_eq!(result.skipped.len(), 1);
    }

    #[test]
    fn update_node_config_partial() {
        let flow = minimal_flow();
        let patches = vec![FlowPatch::UpdateNodeConfig {
            key: "research".to_string(),
            config: ConfigUpdate {
                budget_usd: Some(5.0),
                model: Some("opus".to_string()),
                ..Default::default()
            },
        }];

        let result = apply_patches(&flow, &patches);
        assert_eq!(result.applied.len(), 1);

        let node = result.flow.node("research").unwrap();
        assert_eq!(node.config.budget_usd, Some(5.0));
        assert_eq!(node.config.model.as_deref(), Some("opus"));
        // Original prompt should be preserved
        assert_eq!(node.config.prompt.as_deref(), Some("Do research"));
    }

    #[test]
    fn insert_nonexistent_after_node_skipped() {
        let flow = minimal_flow();
        let patches = vec![FlowPatch::InsertNode {
            node: Box::new(CeremonyNode {
                key: "x".to_string(),
                node_type: CeremonyNodeType::Harness,
                label: "X".to_string(),
                config: CeremonyNodeConfig::default(),
                always_run: false,
            }),
            after_node: "nonexistent".to_string(),
            before_node: "output".to_string(),
        }];

        let result = apply_patches(&flow, &patches);
        assert_eq!(result.skipped.len(), 1);
        assert!(result.skipped[0].contains("not found"));
    }

    #[test]
    fn insert_duplicate_key_skipped() {
        let flow = minimal_flow();
        let patches = vec![FlowPatch::InsertNode {
            node: Box::new(CeremonyNode {
                key: "research".to_string(), // already exists
                node_type: CeremonyNodeType::Harness,
                label: "Dup".to_string(),
                config: CeremonyNodeConfig::default(),
                always_run: false,
            }),
            after_node: "source".to_string(),
            before_node: "research".to_string(),
        }];

        let result = apply_patches(&flow, &patches);
        assert_eq!(result.skipped.len(), 1);
        assert!(result.skipped[0].contains("already exists"));
    }

    #[test]
    fn multiple_patches_applied_in_order() {
        let flow = minimal_flow();
        let patches = vec![
            FlowPatch::UpdateNodeConfig {
                key: "research".to_string(),
                config: ConfigUpdate {
                    budget_usd: Some(3.0),
                    ..Default::default()
                },
            },
            FlowPatch::InsertNode {
                node: Box::new(CeremonyNode {
                    key: "review".to_string(),
                    node_type: CeremonyNodeType::Harness,
                    label: "Review".to_string(),
                    config: CeremonyNodeConfig {
                        prompt: Some("Review".to_string()),
                        ..Default::default()
                    },
                    always_run: false,
                }),
                after_node: "research".to_string(),
                before_node: "output".to_string(),
            },
        ];

        let result = apply_patches(&flow, &patches);
        assert_eq!(result.applied.len(), 2);
        assert_eq!(result.flow.nodes.len(), 4);
        assert_eq!(
            result.flow.node("research").unwrap().config.budget_usd,
            Some(3.0)
        );
    }

    #[test]
    fn flow_to_yaml_roundtrips() {
        let flow = minimal_flow();
        let yaml = flow_to_yaml(&flow).unwrap();
        let parsed = CeremonyFlow::from_yaml(&yaml).unwrap();
        assert_eq!(parsed.nodes.len(), flow.nodes.len());
        assert_eq!(parsed.edges.len(), flow.edges.len());
    }

    #[test]
    fn patched_flow_has_no_cycles() {
        let flow = CeremonyFlow::default_flow();
        let patches = vec![FlowPatch::InsertNode {
            node: Box::new(CeremonyNode {
                key: "research_review".to_string(),
                node_type: CeremonyNodeType::Harness,
                label: "Research Review".to_string(),
                config: CeremonyNodeConfig {
                    model: Some("sonnet".to_string()),
                    prompt: Some("Review".to_string()),
                    ..Default::default()
                },
                always_run: false,
            }),
            after_node: "gate_research".to_string(),
            before_node: "groom".to_string(),
        }];

        let result = apply_patches(&flow, &patches);
        let patched = &result.flow;

        // Kahn's cycle check
        let mut in_deg = patched.in_degrees();
        let adj = patched.adjacency();
        let mut queue: std::collections::VecDeque<String> = in_deg
            .iter()
            .filter(|(_, d)| **d == 0)
            .map(|(k, _)| k.clone())
            .collect();

        let mut processed = 0;
        while let Some(key) = queue.pop_front() {
            processed += 1;
            if let Some(downstream) = adj.get(&key) {
                for (target, _) in downstream {
                    if let Some(deg) = in_deg.get_mut(target) {
                        *deg -= 1;
                        if *deg == 0 {
                            queue.push_back(target.clone());
                        }
                    }
                }
            }
        }
        assert_eq!(
            processed,
            patched.nodes.len(),
            "Cycle detected in patched flow"
        );
    }
}
