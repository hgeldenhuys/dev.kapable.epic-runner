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
