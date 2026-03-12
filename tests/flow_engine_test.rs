use epic_runner::flow::definition::*;

#[test]
fn default_flow_has_valid_topology() {
    let flow = CeremonyFlow::default_flow();
    let degrees = flow.in_degrees();

    // Source has zero in-degree
    assert_eq!(degrees["source"], 0);

    // Output has no outgoing edges
    let adj = flow.adjacency();
    assert!(adj.get("output").is_none() || adj["output"].is_empty());

    // All edge targets exist as nodes
    let node_keys: std::collections::HashSet<_> =
        flow.nodes.iter().map(|n| n.key.clone()).collect();
    for edge in &flow.edges {
        assert!(
            node_keys.contains(&edge.target),
            "Edge target {} not found",
            edge.target
        );
        assert!(
            node_keys.contains(&edge.source),
            "Edge source {} not found",
            edge.source
        );
    }
}

#[test]
fn gate_nodes_have_pass_and_fail_edges() {
    let flow = CeremonyFlow::default_flow();
    for node in &flow.nodes {
        if node.node_type == CeremonyNodeType::Gate {
            let outgoing: Vec<_> = flow.edges.iter().filter(|e| e.source == node.key).collect();
            let handles: Vec<_> = outgoing
                .iter()
                .filter_map(|e| e.handle.as_deref())
                .collect();
            assert!(
                handles.contains(&"pass"),
                "Gate {} missing pass edge",
                node.key
            );
            assert!(
                handles.contains(&"fail"),
                "Gate {} missing fail edge",
                node.key
            );
        }
    }
}

#[test]
fn always_run_nodes_exist() {
    let flow = CeremonyFlow::default_flow();
    let always_run: Vec<_> = flow
        .nodes
        .iter()
        .filter(|n| n.always_run)
        .map(|n| n.key.as_str())
        .collect();
    assert!(always_run.contains(&"merge_results"));
    assert!(always_run.contains(&"sm_retro"));
}

#[test]
fn flow_has_exactly_one_source() {
    let flow = CeremonyFlow::default_flow();
    let sources: Vec<_> = flow
        .nodes
        .iter()
        .filter(|n| n.node_type == CeremonyNodeType::Source)
        .collect();
    assert_eq!(sources.len(), 1);
    assert_eq!(sources[0].key, "source");
}

#[test]
fn flow_has_exactly_one_output() {
    let flow = CeremonyFlow::default_flow();
    let outputs: Vec<_> = flow
        .nodes
        .iter()
        .filter(|n| n.node_type == CeremonyNodeType::Output)
        .collect();
    assert_eq!(outputs.len(), 1);
    assert_eq!(outputs[0].key, "output");
}

#[test]
fn adjacency_map_is_consistent() {
    let flow = CeremonyFlow::default_flow();
    let adj = flow.adjacency();
    let mut edge_count = 0;
    for targets in adj.values() {
        edge_count += targets.len();
    }
    assert_eq!(edge_count, flow.edges.len());
}

#[test]
fn custom_flow_from_yaml() {
    let yaml = r#"
name: "Minimal"
version: "1.0.0"
nodes:
  - key: source
    node_type: source
    label: "Start"
    config: {}
  - key: output
    node_type: output
    label: "End"
    config: {}
edges:
  - source: source
    target: output
"#;
    let flow = CeremonyFlow::from_yaml(yaml).unwrap();
    assert_eq!(flow.nodes.len(), 2);
    assert_eq!(flow.edges.len(), 1);
}

// ── Reverse Adjacency Tests ────────────────────────

#[test]
fn reverse_adjacency_maps_targets_to_sources() {
    let flow = CeremonyFlow::default_flow();
    let rev = flow.reverse_adjacency();

    // source has no parents (zero in-degree)
    assert!(rev.get("source").is_none());

    // research's parent is source
    assert!(rev["research"].contains(&"source".to_string()));

    // groom's parent is research (v3 — no inter-step gates)
    assert!(rev["groom"].contains(&"research".to_string()));

    // merge_results has multiple parents (deploy gate failures + promote)
    assert!(rev["merge_results"].len() >= 2);
}

#[test]
fn reverse_adjacency_consistent_with_forward() {
    let flow = CeremonyFlow::default_flow();
    let adj = flow.adjacency();
    let rev = flow.reverse_adjacency();

    // Every forward edge A→B should appear as B←A in reverse
    for (source, targets) in &adj {
        for (target, _handle) in targets {
            let parents = rev
                .get(target)
                .expect("target should have parents in rev_adj");
            assert!(
                parents.contains(source),
                "Forward edge {}→{} not found in reverse adjacency",
                source,
                target
            );
        }
    }
}

// ── Kahn's BFS Level Tests ─────────────────────────

#[test]
fn kahns_bfs_produces_correct_levels() {
    // Build a diamond DAG: source → A, B (parallel) → merge → output
    let yaml = r#"
name: "Diamond"
version: "1.0.0"
nodes:
  - key: source
    node_type: source
    label: "S"
    config: {}
  - key: a
    node_type: gate
    label: "A"
    config:
      gate_field: status
      gate_expect: completed
  - key: b
    node_type: gate
    label: "B"
    config:
      gate_field: status
      gate_expect: completed
  - key: merge
    node_type: merge
    label: "M"
    config: {}
  - key: output
    node_type: output
    label: "O"
    config: {}
edges:
  - source: source
    target: a
  - source: source
    target: b
  - source: a
    target: merge
  - source: b
    target: merge
  - source: merge
    target: output
"#;
    let flow = CeremonyFlow::from_yaml(yaml).unwrap();
    let degrees = flow.in_degrees();

    // source: 0 in-degree (level 0)
    assert_eq!(degrees["source"], 0);

    // a and b: 1 in-degree each (level 1 — should run in parallel)
    assert_eq!(degrees["a"], 1);
    assert_eq!(degrees["b"], 1);

    // merge: 2 in-degree (level 2 — waits for both a and b)
    assert_eq!(degrees["merge"], 2);

    // output: 1 in-degree (level 3)
    assert_eq!(degrees["output"], 1);
}

// ── Gate Skip Propagation Tests ────────────────────

#[test]
fn gate_skip_propagation_reaches_all_downstream() {
    let flow = CeremonyFlow::default_flow();
    let adj = flow.adjacency();

    // Simulate: gate_deploy fails → should skip deploy_standby + downstream deploy chain
    let mut skip_set = std::collections::HashSet::new();

    // Find the "pass" edge target for gate_deploy
    if let Some(downstream) = adj.get("gate_deploy") {
        for (target, handle) in downstream {
            if handle.as_deref() == Some("pass") {
                propagate_skip_test(&adj, target, &mut skip_set);
            }
        }
    }

    // deploy_standby should be skipped (pass edge from gate_deploy)
    assert!(
        skip_set.contains("deploy_standby"),
        "deploy_standby should be skipped"
    );
    // gate_deploy_ok should be skipped (downstream of deploy_standby)
    assert!(
        skip_set.contains("gate_deploy_ok"),
        "gate_deploy_ok should be skipped"
    );
    // judge_ab should be skipped (downstream of gate_deploy_ok)
    assert!(skip_set.contains("judge_ab"), "judge_ab should be skipped");
}

/// Test helper — mirrors propagate_skip from engine.rs
fn propagate_skip_test(
    adj: &std::collections::HashMap<String, Vec<(String, Option<String>)>>,
    start: &str,
    skip_set: &mut std::collections::HashSet<String>,
) {
    let mut stack = vec![start.to_string()];
    while let Some(key) = stack.pop() {
        if skip_set.insert(key.clone()) {
            if let Some(downstream) = adj.get(&key) {
                for (target, _) in downstream {
                    stack.push(target.clone());
                }
            }
        }
    }
}

// ── Flow YAML Validation Tests ─────────────────────

#[test]
fn dogfood_minimal_flow_parses() {
    let yaml = include_str!("../flows/dogfood-minimal.yaml");
    let flow = CeremonyFlow::from_yaml(yaml).unwrap();
    assert_eq!(flow.nodes.len(), 3);
    assert_eq!(flow.edges.len(), 2);
    assert!(flow.node("source").is_some());
    assert!(flow.node("execute").is_some());
    assert!(flow.node("output").is_some());
}

#[test]
fn dogfood_minimal_has_valid_topology() {
    let yaml = include_str!("../flows/dogfood-minimal.yaml");
    let flow = CeremonyFlow::from_yaml(yaml).unwrap();
    let degrees = flow.in_degrees();

    assert_eq!(degrees["source"], 0);
    assert_eq!(degrees["execute"], 1);
    assert_eq!(degrees["output"], 1);
}

#[test]
fn all_node_types_have_valid_configs() {
    let flow = CeremonyFlow::default_flow();
    for node in &flow.nodes {
        match node.node_type {
            CeremonyNodeType::Gate => {
                assert!(
                    node.config.gate_field.is_some(),
                    "Gate {} missing gate_field",
                    node.key
                );
                assert!(
                    node.config.gate_expect.is_some(),
                    "Gate {} missing gate_expect",
                    node.key
                );
            }
            CeremonyNodeType::Loop => {
                assert!(
                    node.config.loop_max.is_some(),
                    "Loop {} missing loop_max",
                    node.key
                );
            }
            CeremonyNodeType::Harness | CeremonyNodeType::Agent => {
                assert!(
                    node.config.prompt.is_some(),
                    "Harness/Agent {} missing prompt",
                    node.key
                );
            }
            _ => {}
        }
    }
}

#[test]
fn flow_has_no_cycles() {
    // If Kahn's algorithm processes all nodes, there are no cycles.
    let flow = CeremonyFlow::default_flow();
    let mut in_deg = flow.in_degrees();
    let adj = flow.adjacency();
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
        flow.nodes.len(),
        "Not all nodes processed — cycle detected"
    );
}
