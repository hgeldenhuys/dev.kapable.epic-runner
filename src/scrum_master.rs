use serde::{Deserialize, Serialize};

use crate::flow::definition::*;
use crate::flow::patcher::{ConfigUpdate, FlowPatch};

/// SM observation recorded during/after a sprint.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SmObservation {
    pub category: ObservationCategory,
    pub description: String,
    pub severity: Severity,
    pub action_item: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum ObservationCategory {
    Process,
    Technical,
    Communication,
    Quality,
    Velocity,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum Severity {
    Low,
    Medium,
    High,
    Critical,
}

/// Sprint retrospective output.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RetroOutput {
    pub went_well: Vec<String>,
    pub friction_points: Vec<String>,
    pub action_items: Vec<String>,
    pub discovered_work: Vec<String>,
    pub observations: Vec<SmObservation>,
}

/// Historical record of a sprint's ceremony results, used for pattern detection.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SprintHistory {
    pub sprint_number: i32,
    pub node_results: Vec<NodeOutcome>,
    pub retro: Option<RetroOutput>,
}

/// Simplified node outcome for history analysis.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NodeOutcome {
    pub key: String,
    pub status: String,
    pub cost_usd: Option<f64>,
}

/// Parse SM retro output from LLM response.
pub fn parse_retro(text: Option<&str>) -> Option<RetroOutput> {
    let text = text?;
    // Try direct parse
    if let Ok(r) = serde_json::from_str::<RetroOutput>(text) {
        return Some(r);
    }
    // Try stripping markdown fences
    let stripped = text
        .trim()
        .strip_prefix("```json")
        .or_else(|| text.trim().strip_prefix("```"))
        .and_then(|s| s.strip_suffix("```"))
        .unwrap_or(text);
    serde_json::from_str::<RetroOutput>(stripped.trim()).ok()
}

/// Detect friction patterns across multiple retros.
pub fn detect_recurring_friction(retros: &[RetroOutput]) -> Vec<String> {
    use std::collections::HashMap;
    let mut friction_counts: HashMap<String, usize> = HashMap::new();
    for retro in retros {
        for friction in &retro.friction_points {
            let key = friction.to_lowercase();
            *friction_counts.entry(key).or_insert(0) += 1;
        }
    }
    let mut recurring: Vec<String> = friction_counts
        .into_iter()
        .filter(|(_, count)| *count >= 2)
        .map(|(key, count)| format!("{key} (x{count})"))
        .collect();
    recurring.sort();
    recurring
}

/// Threshold for how many consecutive gate failures trigger a patch.
const CONSECUTIVE_FAIL_THRESHOLD: usize = 2;

/// Analyze sprint history and recommend flow patches for the next sprint.
///
/// Pattern detection rules (v3 — no inter-step gates):
/// 1. If `research` fails >= 2 consecutive sprints → insert a `research_review` harness
///    between `research` and `groom` to improve research quality.
/// 2. If `execute` fails (impediment) >= 2 consecutive sprints → increase `execute` loop_max.
/// 3. If judge consistently reports low confidence → increase judge budget.
/// 4. If recurring friction mentions "research" → boost research budget.
pub fn recommend_flow_patches(
    history: &[SprintHistory],
    current_flow: &CeremonyFlow,
) -> Vec<FlowPatch> {
    let mut patches = Vec::new();

    // Rule 1: Consecutive research failures → insert research-review node
    if consecutive_node_failures(history, "research") >= CONSECUTIVE_FAIL_THRESHOLD {
        // Only insert if the node doesn't already exist (idempotent)
        if current_flow.node("research_review").is_none() {
            patches.push(FlowPatch::InsertNode {
                node: Box::new(CeremonyNode {
                    key: "research_review".to_string(),
                    node_type: CeremonyNodeType::Harness,
                    label: "Research Quality Review".to_string(),
                    config: CeremonyNodeConfig {
                        model: Some("sonnet".to_string()),
                        effort: Some("high".to_string()),
                        budget_usd: Some(1.0),
                        heartbeat_timeout_secs: Some(120),
                        system_prompt: Some(
                            "You are a research quality reviewer. Evaluate the research output \
                             for completeness, accuracy, and actionability.\n\
                             Check:\n\
                             1. Are all relevant files identified with line numbers?\n\
                             2. Are dependencies and blockers realistic (not hallucinated)?\n\
                             3. Are conventions from CLAUDE.md correctly cited?\n\
                             4. Is the output valid JSON matching the required schema?\n\n\
                             If the research is insufficient, RE-DO the research with corrections.\n\
                             Output the corrected research JSON."
                                .to_string(),
                        ),
                        prompt: Some(
                            "Review and improve this research output:\n{{input}}"
                                .to_string(),
                        ),
                        ..Default::default()
                    },
                    always_run: false,
                }),
                after_node: "research".to_string(),
                before_node: "groom".to_string(),
            });
        }
    }

    // Rule 2: Consecutive execute failures → increase loop_max
    if consecutive_node_failures(history, "execute") >= CONSECUTIVE_FAIL_THRESHOLD {
        if let Some(node) = current_flow.node("execute") {
            let current_max = node.config.loop_max.unwrap_or(5);
            let new_max = (current_max + 2).min(10); // cap at 10
            if new_max > current_max {
                patches.push(FlowPatch::UpdateNodeConfig {
                    key: "execute".to_string(),
                    config: ConfigUpdate {
                        loop_max: Some(new_max),
                        ..Default::default()
                    },
                });
            }
        }
    }

    // Rule 3: Recurring "research" friction → boost research budget
    let retros: Vec<&RetroOutput> = history.iter().filter_map(|h| h.retro.as_ref()).collect();
    let friction =
        detect_recurring_friction(&retros.iter().map(|r| (*r).clone()).collect::<Vec<_>>());
    let has_research_friction = friction.iter().any(|f| f.contains("research"));
    if has_research_friction {
        if let Some(node) = current_flow.node("research") {
            let current_budget = node.config.budget_usd.unwrap_or(1.0);
            let new_budget = (current_budget * 1.5).min(5.0); // cap at 5.0
            if new_budget > current_budget {
                patches.push(FlowPatch::UpdateNodeConfig {
                    key: "research".to_string(),
                    config: ConfigUpdate {
                        budget_usd: Some(new_budget),
                        ..Default::default()
                    },
                });
            }
        }
    }

    // Rule 4: Consecutive groom failures → boost groom budget + timeout
    if consecutive_node_failures(history, "groom") >= CONSECUTIVE_FAIL_THRESHOLD {
        if let Some(node) = current_flow.node("groom") {
            let current_budget = node.config.budget_usd.unwrap_or(1.0);
            let new_budget = (current_budget * 1.5).min(5.0);
            if new_budget > current_budget {
                patches.push(FlowPatch::UpdateNodeConfig {
                    key: "groom".to_string(),
                    config: ConfigUpdate {
                        budget_usd: Some(new_budget),
                        heartbeat_timeout_secs: Some(240),
                        ..Default::default()
                    },
                });
            }
        }
    }

    patches
}

/// Count how many consecutive recent sprints had a node fail.
/// Counts backwards from the most recent sprint.
fn consecutive_node_failures(history: &[SprintHistory], node_key: &str) -> usize {
    let mut count = 0;
    // Iterate from most recent to oldest
    for sprint in history.iter().rev() {
        let node_failed = sprint
            .node_results
            .iter()
            .any(|n| n.key == node_key && n.status == "Failed");
        if node_failed {
            count += 1;
        } else {
            break; // Stop at first non-failure
        }
    }
    count
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_retro_from_json() {
        let json = r#"{"went_well":["fast"],"friction_points":["slow tests"],"action_items":["fix CI"],"discovered_work":["new bug"],"observations":[]}"#;
        let r = parse_retro(Some(json)).unwrap();
        assert_eq!(r.went_well, vec!["fast"]);
        assert_eq!(r.friction_points, vec!["slow tests"]);
    }

    #[test]
    fn detect_recurring_friction_works() {
        let retros = vec![
            RetroOutput {
                went_well: vec![],
                friction_points: vec!["slow tests".to_string(), "flaky CI".to_string()],
                action_items: vec![],
                discovered_work: vec![],
                observations: vec![],
            },
            RetroOutput {
                went_well: vec![],
                friction_points: vec!["slow tests".to_string()],
                action_items: vec![],
                discovered_work: vec![],
                observations: vec![],
            },
        ];
        let recurring = detect_recurring_friction(&retros);
        assert_eq!(recurring.len(), 1);
        assert!(recurring[0].contains("slow tests"));
    }

    fn make_history(gate_key: &str, statuses: &[&str]) -> Vec<SprintHistory> {
        statuses
            .iter()
            .enumerate()
            .map(|(i, status)| SprintHistory {
                sprint_number: (i + 1) as i32,
                node_results: vec![NodeOutcome {
                    key: gate_key.to_string(),
                    status: status.to_string(),
                    cost_usd: None,
                }],
                retro: None,
            })
            .collect()
    }

    #[test]
    fn consecutive_failures_counted_from_end() {
        let history = make_history("research", &["Completed", "Failed", "Failed"]);
        assert_eq!(consecutive_node_failures(&history, "research"), 2);
    }

    #[test]
    fn consecutive_failures_stops_at_success() {
        let history = make_history("research", &["Failed", "Completed", "Failed"]);
        assert_eq!(consecutive_node_failures(&history, "research"), 1);
    }

    #[test]
    fn no_failures_returns_zero() {
        let history = make_history("research", &["Completed", "Completed"]);
        assert_eq!(consecutive_node_failures(&history, "research"), 0);
    }

    #[test]
    fn recommend_research_review_on_research_failures() {
        let history = make_history("research", &["Failed", "Failed"]);
        let flow = CeremonyFlow::default_flow();
        let patches = recommend_flow_patches(&history, &flow);

        // Should recommend inserting research_review node
        assert!(
            patches.iter().any(
                |p| matches!(p, FlowPatch::InsertNode { node, .. } if node.key == "research_review")
            ),
            "Expected research_review insert patch, got: {:?}",
            patches
        );
    }

    #[test]
    fn no_patch_when_single_failure() {
        let history = make_history("research", &["Completed", "Failed"]);
        let flow = CeremonyFlow::default_flow();
        let patches = recommend_flow_patches(&history, &flow);

        assert!(
            !patches.iter().any(
                |p| matches!(p, FlowPatch::InsertNode { node, .. } if node.key == "research_review")
            ),
            "Should not recommend research_review after single failure"
        );
    }

    #[test]
    fn idempotent_research_review_not_duplicated() {
        let history = make_history("research", &["Failed", "Failed"]);
        let flow = CeremonyFlow::default_flow();

        // Apply patches once
        let patches = recommend_flow_patches(&history, &flow);
        let result = crate::flow::patcher::apply_patches(&flow, &patches);

        // Recommend again on the patched flow — should NOT insert again
        let patches2 = recommend_flow_patches(&history, &result.flow);
        assert!(
            !patches2.iter().any(
                |p| matches!(p, FlowPatch::InsertNode { node, .. } if node.key == "research_review")
            ),
            "Should not duplicate research_review node"
        );
    }

    #[test]
    fn recommend_loop_max_increase_on_execute_failures() {
        let history = make_history("execute", &["Failed", "Failed"]);
        let flow = CeremonyFlow::default_flow();
        let patches = recommend_flow_patches(&history, &flow);

        assert!(
            patches.iter().any(|p| matches!(p, FlowPatch::UpdateNodeConfig { key, config } if key == "execute" && config.loop_max.is_some())),
            "Expected execute loop_max increase, got: {:?}",
            patches
        );
    }

    #[test]
    fn recommend_research_budget_boost_on_friction() {
        let history = vec![
            SprintHistory {
                sprint_number: 1,
                node_results: vec![],
                retro: Some(RetroOutput {
                    went_well: vec![],
                    friction_points: vec!["research quality poor".to_string()],
                    action_items: vec![],
                    discovered_work: vec![],
                    observations: vec![],
                }),
            },
            SprintHistory {
                sprint_number: 2,
                node_results: vec![],
                retro: Some(RetroOutput {
                    went_well: vec![],
                    friction_points: vec!["research quality poor".to_string()],
                    action_items: vec![],
                    discovered_work: vec![],
                    observations: vec![],
                }),
            },
        ];
        let flow = CeremonyFlow::default_flow();
        let patches = recommend_flow_patches(&history, &flow);

        assert!(
            patches.iter().any(|p| matches!(p, FlowPatch::UpdateNodeConfig { key, config } if key == "research" && config.budget_usd.is_some())),
            "Expected research budget boost, got: {:?}",
            patches
        );
    }
}
