use std::collections::{HashMap, HashSet, VecDeque};

use clap::Args;
use owo_colors::OwoColorize;

use crate::flow::definition::{CeremonyFlow, CeremonyNodeType};

#[derive(Args)]
pub struct FlowValidateArgs {
    /// Path to the YAML flow file to validate (omit to validate the embedded default)
    pub path: Option<String>,
}

/// Validation finding with severity.
#[derive(Debug)]
struct Finding {
    severity: Severity,
    message: String,
}

#[derive(Debug, PartialEq)]
enum Severity {
    Error,
    Warning,
}

pub fn run(args: FlowValidateArgs) -> Result<(), Box<dyn std::error::Error>> {
    let (flow, source_label) = if let Some(path) = &args.path {
        let yaml = std::fs::read_to_string(path)?;
        let flow = CeremonyFlow::from_yaml(&yaml)?;
        (flow, path.as_str())
    } else {
        (CeremonyFlow::default_flow(), "<embedded default>")
    };

    eprintln!("Validating flow: {} ({})", flow.name, source_label);
    eprintln!("  {} nodes, {} edges", flow.nodes.len(), flow.edges.len());

    let findings = validate(&flow);

    let errors: Vec<_> = findings
        .iter()
        .filter(|f| f.severity == Severity::Error)
        .collect();
    let warnings: Vec<_> = findings
        .iter()
        .filter(|f| f.severity == Severity::Warning)
        .collect();

    for f in &findings {
        match f.severity {
            Severity::Error => eprintln!("  [{}] {}", "ERROR".red().bold(), f.message),
            Severity::Warning => eprintln!("  [{}] {}", "WARN".yellow(), f.message),
        }
    }

    if findings.is_empty() {
        eprintln!("  {}", "All checks passed.".green().bold());
    } else {
        eprintln!(
            "\n  {} error(s), {} warning(s)",
            errors.len().to_string().red().bold(),
            warnings.len().to_string().yellow()
        );
    }

    if !errors.is_empty() {
        std::process::exit(1);
    }
    Ok(())
}

/// Run all structural validations on a ceremony flow.
fn validate(flow: &CeremonyFlow) -> Vec<Finding> {
    let mut findings = Vec::new();
    let node_keys: HashSet<&str> = flow.nodes.iter().map(|n| n.key.as_str()).collect();

    // 1. Check for duplicate node keys
    check_duplicate_keys(flow, &mut findings);

    // 2. Check edges reference existing nodes
    check_edge_references(flow, &node_keys, &mut findings);

    // 3. Check for cycles (Kahn's — if not all nodes consumed, there's a cycle)
    check_cycles(flow, &mut findings);

    // 4. Check for unreachable nodes (no incoming edges and not a source)
    check_unreachable(flow, &mut findings);

    // 5. Check gate nodes have required config
    check_gate_config(flow, &mut findings);

    // 6. Check harness/agent/loop nodes have prompts
    check_prompt_config(flow, &mut findings);

    // 7. Check source and output nodes exist
    check_source_output(flow, &node_keys, &mut findings);

    // 8. Check gate edges have pass/fail handles
    check_gate_edges(flow, &mut findings);

    findings
}

fn check_duplicate_keys(flow: &CeremonyFlow, findings: &mut Vec<Finding>) {
    let mut seen = HashSet::new();
    for node in &flow.nodes {
        if !seen.insert(&node.key) {
            findings.push(Finding {
                severity: Severity::Error,
                message: format!("Duplicate node key: '{}'", node.key),
            });
        }
    }
}

fn check_edge_references(flow: &CeremonyFlow, keys: &HashSet<&str>, findings: &mut Vec<Finding>) {
    for edge in &flow.edges {
        if !keys.contains(edge.source.as_str()) {
            findings.push(Finding {
                severity: Severity::Error,
                message: format!("Edge references non-existent source: '{}'", edge.source),
            });
        }
        if !keys.contains(edge.target.as_str()) {
            findings.push(Finding {
                severity: Severity::Error,
                message: format!("Edge references non-existent target: '{}'", edge.target),
            });
        }
    }
}

fn check_cycles(flow: &CeremonyFlow, findings: &mut Vec<Finding>) {
    let mut in_deg = flow.in_degrees();
    let adj = flow.adjacency();
    let mut queue: VecDeque<String> = VecDeque::new();
    let mut visited = 0;

    for (key, deg) in &in_deg {
        if *deg == 0 {
            queue.push_back(key.clone());
        }
    }

    while let Some(key) = queue.pop_front() {
        visited += 1;
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

    if visited < flow.nodes.len() {
        let stuck: Vec<_> = in_deg
            .iter()
            .filter(|(_, deg)| **deg > 0)
            .map(|(key, _)| key.clone())
            .collect();
        findings.push(Finding {
            severity: Severity::Error,
            message: format!("Cycle detected involving nodes: {}", stuck.join(", ")),
        });
    }
}

fn check_unreachable(flow: &CeremonyFlow, findings: &mut Vec<Finding>) {
    let targets: HashSet<&str> = flow.edges.iter().map(|e| e.target.as_str()).collect();
    let sources: HashSet<&str> = flow.edges.iter().map(|e| e.source.as_str()).collect();

    for node in &flow.nodes {
        // A node is unreachable if it has no incoming edges AND is not connected as a source
        if !targets.contains(node.key.as_str())
            && !sources.contains(node.key.as_str())
            && flow.nodes.len() > 1
        {
            findings.push(Finding {
                severity: Severity::Warning,
                message: format!("Node '{}' is isolated (no edges)", node.key),
            });
        }
    }
}

fn check_gate_config(flow: &CeremonyFlow, findings: &mut Vec<Finding>) {
    for node in &flow.nodes {
        if node.node_type == CeremonyNodeType::Gate {
            if node.config.gate_field.is_none() {
                findings.push(Finding {
                    severity: Severity::Warning,
                    message: format!(
                        "Gate '{}' has no gate_field — will default to 'status'",
                        node.key
                    ),
                });
            }
            if node.config.gate_expect.is_none() {
                findings.push(Finding {
                    severity: Severity::Warning,
                    message: format!(
                        "Gate '{}' has no gate_expect — will default to 'completed'",
                        node.key
                    ),
                });
            }
        }
    }
}

fn check_prompt_config(flow: &CeremonyFlow, findings: &mut Vec<Finding>) {
    for node in &flow.nodes {
        if matches!(
            node.node_type,
            CeremonyNodeType::Harness | CeremonyNodeType::Agent | CeremonyNodeType::Loop
        ) && node.config.prompt.is_none()
        {
            findings.push(Finding {
                severity: Severity::Error,
                message: format!(
                    "Node '{}' ({:?}) has no prompt configured",
                    node.key, node.node_type
                ),
            });
        }
    }
}

fn check_source_output(flow: &CeremonyFlow, keys: &HashSet<&str>, findings: &mut Vec<Finding>) {
    let has_source = flow
        .nodes
        .iter()
        .any(|n| n.node_type == CeremonyNodeType::Source);
    let has_output = flow
        .nodes
        .iter()
        .any(|n| n.node_type == CeremonyNodeType::Output);

    if !has_source && !keys.is_empty() {
        findings.push(Finding {
            severity: Severity::Warning,
            message: "No source node — flow has no entry point".to_string(),
        });
    }
    if !has_output && !keys.is_empty() {
        findings.push(Finding {
            severity: Severity::Warning,
            message: "No output node — flow has no terminal node".to_string(),
        });
    }
}

fn check_gate_edges(flow: &CeremonyFlow, findings: &mut Vec<Finding>) {
    let gate_keys: HashSet<&str> = flow
        .nodes
        .iter()
        .filter(|n| n.node_type == CeremonyNodeType::Gate)
        .map(|n| n.key.as_str())
        .collect();

    let adj: HashMap<&str, Vec<(&str, Option<&str>)>> = {
        let mut m: HashMap<&str, Vec<(&str, Option<&str>)>> = HashMap::new();
        for edge in &flow.edges {
            m.entry(edge.source.as_str())
                .or_default()
                .push((edge.target.as_str(), edge.handle.as_deref()));
        }
        m
    };

    for gate_key in &gate_keys {
        if let Some(edges) = adj.get(gate_key) {
            let has_pass = edges.iter().any(|(_, h)| *h == Some("pass"));
            let has_fail = edges.iter().any(|(_, h)| *h == Some("fail"));
            if !has_pass {
                findings.push(Finding {
                    severity: Severity::Warning,
                    message: format!(
                        "Gate '{}' has no 'pass' edge — gate has no effect",
                        gate_key
                    ),
                });
            }
            if !has_fail {
                findings.push(Finding {
                    severity: Severity::Warning,
                    message: format!(
                        "Gate '{}' has no 'fail' edge — failures won't short-circuit",
                        gate_key
                    ),
                });
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::flow::definition::*;

    #[test]
    fn default_flow_validates_clean() {
        let flow = CeremonyFlow::default_flow();
        let findings = validate(&flow);
        let errors: Vec<_> = findings
            .iter()
            .filter(|f| f.severity == Severity::Error)
            .collect();
        assert!(errors.is_empty(), "Default flow has errors: {:?}", errors);
    }

    #[test]
    fn detects_missing_edge_target() {
        let flow = CeremonyFlow {
            name: "test".into(),
            version: "1.0".into(),
            description: None,
            nodes: vec![CeremonyNode {
                key: "a".into(),
                node_type: CeremonyNodeType::Source,
                label: "A".into(),
                config: CeremonyNodeConfig::default(),
                always_run: false,
            }],
            edges: vec![CeremonyEdge {
                source: "a".into(),
                target: "nonexistent".into(),
                handle: None,
            }],
        };
        let findings = validate(&flow);
        assert!(findings
            .iter()
            .any(|f| f.severity == Severity::Error && f.message.contains("nonexistent")));
    }

    #[test]
    fn detects_duplicate_keys() {
        let flow = CeremonyFlow {
            name: "test".into(),
            version: "1.0".into(),
            description: None,
            nodes: vec![
                CeremonyNode {
                    key: "dup".into(),
                    node_type: CeremonyNodeType::Source,
                    label: "A".into(),
                    config: CeremonyNodeConfig::default(),
                    always_run: false,
                },
                CeremonyNode {
                    key: "dup".into(),
                    node_type: CeremonyNodeType::Output,
                    label: "B".into(),
                    config: CeremonyNodeConfig::default(),
                    always_run: false,
                },
            ],
            edges: vec![],
        };
        let findings = validate(&flow);
        assert!(findings
            .iter()
            .any(|f| f.severity == Severity::Error && f.message.contains("Duplicate")));
    }

    #[test]
    fn detects_missing_prompt() {
        let flow = CeremonyFlow {
            name: "test".into(),
            version: "1.0".into(),
            description: None,
            nodes: vec![CeremonyNode {
                key: "bad_harness".into(),
                node_type: CeremonyNodeType::Harness,
                label: "Missing prompt".into(),
                config: CeremonyNodeConfig::default(),
                always_run: false,
            }],
            edges: vec![],
        };
        let findings = validate(&flow);
        assert!(findings
            .iter()
            .any(|f| f.severity == Severity::Error && f.message.contains("no prompt")));
    }

    #[test]
    fn detects_cycle() {
        let flow = CeremonyFlow {
            name: "test".into(),
            version: "1.0".into(),
            description: None,
            nodes: vec![
                CeremonyNode {
                    key: "a".into(),
                    node_type: CeremonyNodeType::Merge,
                    label: "A".into(),
                    config: CeremonyNodeConfig::default(),
                    always_run: false,
                },
                CeremonyNode {
                    key: "b".into(),
                    node_type: CeremonyNodeType::Merge,
                    label: "B".into(),
                    config: CeremonyNodeConfig::default(),
                    always_run: false,
                },
            ],
            edges: vec![
                CeremonyEdge {
                    source: "a".into(),
                    target: "b".into(),
                    handle: None,
                },
                CeremonyEdge {
                    source: "b".into(),
                    target: "a".into(),
                    handle: None,
                },
            ],
        };
        let findings = validate(&flow);
        assert!(findings
            .iter()
            .any(|f| f.severity == Severity::Error && f.message.contains("Cycle")));
    }
}
