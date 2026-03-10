use std::collections::{HashMap, HashSet, VecDeque};
use uuid::Uuid;

use super::definition::*;
use crate::executor::{self, ExecutorConfig};
use crate::supervisor;
use crate::types::*;

/// Context passed through the flow during execution.
pub struct FlowContext {
    pub epic: Epic,
    pub sprint: Sprint,
    pub stories: serde_json::Value,
    pub repo_path: String,
    pub model_override: Option<String>,
    pub effort_override: Option<String>,
    pub add_dirs: Vec<String>,
}

/// Result of executing one node.
#[derive(Debug, Clone)]
pub struct NodeResult {
    pub key: String,
    pub status: CeremonyStatus,
    pub output: Option<String>,
    pub cost_usd: Option<f64>,
    pub impediment_raised: bool,
    pub judge_verdict: Option<JudgeVerdict>,
    pub supervisor_decisions: Vec<SupervisorDecision>,
    pub rubber_duck_sessions: Vec<RubberDuckSession>,
}

/// Execute a ceremony flow locally using Kahn's topological sort.
///
/// Algorithm:
/// 1. Compute in-degrees for all nodes
/// 2. Enqueue all zero-degree nodes (sources)
/// 3. For each level: execute all ready nodes, collect results
/// 4. Decrement in-degrees of downstream nodes
/// 5. Handle gate skipping (fail → skip downstream on "pass" handle)
/// 6. always_run nodes execute regardless of skip state
pub async fn execute_flow(
    flow: &CeremonyFlow,
    ctx: &FlowContext,
) -> Result<Vec<NodeResult>, Box<dyn std::error::Error>> {
    let adj = flow.adjacency();
    let mut in_deg = flow.in_degrees();
    let mut results: HashMap<String, NodeResult> = HashMap::new();
    let mut skip_set: HashSet<String> = HashSet::new();

    // Kahn's BFS
    let mut queue: VecDeque<String> = VecDeque::new();
    for (key, deg) in &in_deg {
        if *deg == 0 {
            queue.push_back(key.clone());
        }
    }

    while !queue.is_empty() {
        let level_size = queue.len();
        let level_keys: Vec<String> = queue.drain(..level_size).collect();

        for key in &level_keys {
            let node = match flow.node(key) {
                Some(n) => n,
                None => continue,
            };

            let should_skip = skip_set.contains(key) && !node.always_run;
            let result = if should_skip {
                NodeResult {
                    key: key.clone(),
                    status: CeremonyStatus::Skipped,
                    output: None,
                    cost_usd: None,
                    impediment_raised: false,
                    judge_verdict: None,
                    supervisor_decisions: vec![],
                    rubber_duck_sessions: vec![],
                }
            } else {
                eprintln!("[flow] Executing: {} ({})", node.label, node.key);
                execute_node(node, ctx, &results).await?
            };

            // Handle gate skipping
            if node.node_type == CeremonyNodeType::Gate {
                let gate_passed = result.status == CeremonyStatus::Completed;
                if let Some(downstream) = adj.get(key) {
                    for (target, handle) in downstream {
                        let is_pass_edge = handle.as_deref() == Some("pass");
                        let is_fail_edge = handle.as_deref() == Some("fail");
                        if !gate_passed && is_pass_edge {
                            propagate_skip(&adj, target, &mut skip_set);
                        }
                        if gate_passed && is_fail_edge {
                            propagate_skip(&adj, target, &mut skip_set);
                        }
                    }
                }
            }

            results.insert(key.clone(), result);

            // Decrement in-degrees of downstream nodes
            if let Some(downstream) = adj.get(key) {
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
    }

    // Collect results in node order
    let ordered: Vec<NodeResult> = flow
        .nodes
        .iter()
        .filter_map(|n| results.remove(&n.key))
        .collect();
    Ok(ordered)
}

/// Propagate skip through all reachable downstream nodes.
fn propagate_skip(
    adj: &HashMap<String, Vec<(String, Option<String>)>>,
    start: &str,
    skip_set: &mut HashSet<String>,
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

/// Execute a single ceremony node based on its type.
async fn execute_node(
    node: &CeremonyNode,
    ctx: &FlowContext,
    upstream: &HashMap<String, NodeResult>,
) -> Result<NodeResult, Box<dyn std::error::Error>> {
    match node.node_type {
        CeremonyNodeType::Source => Ok(NodeResult {
            key: node.key.clone(),
            status: CeremonyStatus::Completed,
            output: Some(serde_json::to_string(&ctx.stories)?),
            cost_usd: None,
            impediment_raised: false,
            judge_verdict: None,
            supervisor_decisions: vec![],
            rubber_duck_sessions: vec![],
        }),

        CeremonyNodeType::Harness | CeremonyNodeType::Agent => {
            let config = build_executor_config(node, ctx);
            let result = executor::execute(config, |e| {
                eprintln!("  [{}/{}] {}", node.key, e.event_type_str(), e.summary);
            })
            .await?;

            let status = if result.exit_code == 0 {
                CeremonyStatus::Completed
            } else {
                CeremonyStatus::Failed
            };

            // Parse judge verdict if this is the judge node
            let verdict = if node.key == "judge" {
                crate::judge::parse_verdict(result.result_text.as_deref())
            } else {
                None
            };

            Ok(NodeResult {
                key: node.key.clone(),
                status,
                output: result.result_text,
                cost_usd: result.cost_usd,
                impediment_raised: false,
                judge_verdict: verdict,
                supervisor_decisions: vec![],
                rubber_duck_sessions: vec![],
            })
        }

        CeremonyNodeType::Gate => {
            let field = node.config.gate_field.as_deref().unwrap_or("status");
            let expect = node.config.gate_expect.as_deref().unwrap_or("completed");

            // Find the most recent upstream result
            let upstream_status = upstream
                .values()
                .filter(|r| r.key != node.key)
                .last()
                .map(|r| match field {
                    "status" => format!("{:?}", r.status).to_lowercase(),
                    "impediment_raised" => r.impediment_raised.to_string(),
                    _ => "unknown".to_string(),
                })
                .unwrap_or_default();

            let passed = upstream_status.contains(expect);
            eprintln!(
                "  [gate] {}: {} (expect: {}, got: {})",
                node.label,
                if passed { "PASS" } else { "FAIL" },
                expect,
                upstream_status
            );

            Ok(NodeResult {
                key: node.key.clone(),
                status: if passed {
                    CeremonyStatus::Completed
                } else {
                    CeremonyStatus::Failed
                },
                output: Some(format!("gate: {} = {}", field, upstream_status)),
                cost_usd: None,
                impediment_raised: false,
                judge_verdict: None,
                supervisor_decisions: vec![],
                rubber_duck_sessions: vec![],
            })
        }

        CeremonyNodeType::Loop => {
            let exec_config = build_executor_config(node, ctx);
            let sup_config = supervisor::SupervisorConfig {
                max_stop_hooks: node.config.loop_max.unwrap_or(5),
                rubber_duck_after: node.config.rubber_duck_after.unwrap_or(2),
                auto_abort_on_same_error: true,
            };

            let supervised = supervisor::supervise(exec_config, sup_config, |e| {
                eprintln!("  [execute/{}] {}", e.event_type_str(), e.summary);
            })
            .await?;

            let impediment = supervised.impediment_raised.is_some();
            let status = if impediment {
                CeremonyStatus::Failed
            } else if supervised.executor_result.exit_code == 0 {
                CeremonyStatus::Completed
            } else {
                CeremonyStatus::Failed
            };

            Ok(NodeResult {
                key: node.key.clone(),
                status,
                output: supervised.executor_result.result_text,
                cost_usd: supervised.executor_result.cost_usd,
                impediment_raised: impediment,
                judge_verdict: None,
                supervisor_decisions: supervised.decisions,
                rubber_duck_sessions: supervised.rubber_duck_sessions,
            })
        }

        CeremonyNodeType::Merge => {
            let merged: Vec<String> = upstream.values().filter_map(|r| r.output.clone()).collect();
            Ok(NodeResult {
                key: node.key.clone(),
                status: CeremonyStatus::Completed,
                output: Some(merged.join("\n---\n")),
                cost_usd: None,
                impediment_raised: false,
                judge_verdict: None,
                supervisor_decisions: vec![],
                rubber_duck_sessions: vec![],
            })
        }

        CeremonyNodeType::Output => Ok(NodeResult {
            key: node.key.clone(),
            status: CeremonyStatus::Completed,
            output: upstream.values().last().and_then(|r| r.output.clone()),
            cost_usd: None,
            impediment_raised: false,
            judge_verdict: None,
            supervisor_decisions: vec![],
            rubber_duck_sessions: vec![],
        }),
    }
}

/// Build an ExecutorConfig from a ceremony node's config + flow context.
fn build_executor_config(node: &CeremonyNode, ctx: &FlowContext) -> ExecutorConfig {
    let c = &node.config;
    ExecutorConfig {
        model: ctx
            .model_override
            .clone()
            .or_else(|| c.model.clone())
            .unwrap_or_else(|| "sonnet".to_string()),
        effort: ctx
            .effort_override
            .clone()
            .or_else(|| c.effort.clone())
            .unwrap_or_else(|| "high".to_string()),
        worktree_name: ctx.epic.code.clone(),
        session_id: if node.node_type == CeremonyNodeType::Loop {
            ctx.sprint.session_id
        } else {
            Uuid::new_v4()
        },
        repo_path: ctx.repo_path.clone(),
        add_dirs: ctx.add_dirs.clone(),
        system_prompt: c.system_prompt.as_ref().map(|s| interpolate(s, ctx)),
        prompt: interpolate(c.prompt.as_deref().unwrap_or(""), ctx),
        chrome: c.chrome,
        max_budget_usd: c.budget_usd,
        allowed_tools: c.allowed_tools.clone(),
        resume_session: false,
        agent: c.agent.clone(),
        heartbeat_timeout_secs: c.heartbeat_timeout_secs.unwrap_or(300),
    }
}

/// Simple template interpolation for {{epic.code}}, {{sprint.number}}, etc.
fn interpolate(template: &str, ctx: &FlowContext) -> String {
    template
        .replace("{{epic.code}}", &ctx.epic.code)
        .replace("{{epic.title}}", &ctx.epic.title)
        .replace("{{epic.intent}}", &ctx.epic.intent)
        .replace(
            "{{epic.success_criteria}}",
            &serde_json::to_string_pretty(
                ctx.epic
                    .success_criteria
                    .as_ref()
                    .unwrap_or(&serde_json::json!([])),
            )
            .unwrap_or_default(),
        )
        .replace("{{sprint.number}}", &ctx.sprint.number.to_string())
        .replace(
            "{{stories}}",
            &serde_json::to_string_pretty(&ctx.stories).unwrap_or_default(),
        )
}
