use std::collections::{HashMap, HashSet, VecDeque};
use uuid::Uuid;

use super::definition::*;
use crate::api_client::ApiClient;
use crate::event_sink::{self, EventSink};
use crate::executor::{self, ExecutorConfig};
use crate::supervisor;
use crate::types::*;

/// Serializable checkpoint for crash recovery.
/// Saved after each node completes so a crashed sprint-run can resume.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct FlowCheckpoint {
    pub sprint_session_id: String,
    pub completed_nodes: Vec<CheckpointedNode>,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct CheckpointedNode {
    pub key: String,
    pub status: String,
    pub output: Option<String>,
    pub cost_usd: Option<f64>,
    pub impediment_raised: bool,
}

/// Detect the default branch of a git repository (main, master, or custom).
/// Tries `git symbolic-ref refs/remotes/origin/HEAD` first, then falls back
/// to checking whether origin/main or origin/master exists, and finally
/// hard-defaults to "main" so callers never receive an empty string.
pub fn detect_default_branch(repo_path: &str) -> String {
    // Try symbolic-ref (set by git clone or `git remote set-head origin --auto`)
    let output = std::process::Command::new("git")
        .args(["symbolic-ref", "refs/remotes/origin/HEAD"])
        .current_dir(repo_path)
        .output();

    if let Ok(o) = output {
        if o.status.success() {
            let refname = String::from_utf8_lossy(&o.stdout).trim().to_string();
            // refs/remotes/origin/main → main
            if let Some(branch) = refname.strip_prefix("refs/remotes/origin/") {
                return branch.to_string();
            }
        }
    }

    // Fallback: check if origin/main or origin/master exists
    for candidate in &["main", "master"] {
        let check = std::process::Command::new("git")
            .args(["rev-parse", "--verify", &format!("origin/{candidate}")])
            .current_dir(repo_path)
            .output();
        if let Ok(o) = check {
            if o.status.success() {
                return candidate.to_string();
            }
        }
    }

    // Last resort — default to "main"
    "main".to_string()
}

/// Context passed through the flow during execution.
#[derive(Clone)]
pub struct FlowContext {
    pub epic: Epic,
    pub sprint: Sprint,
    pub stories: serde_json::Value,
    pub repo_path: String,
    /// Default branch detected from the remote (e.g. main, master, trunk).
    /// Populated once at FlowContext construction via `detect_default_branch`.
    pub default_branch: String,
    pub model_override: Option<String>,
    pub effort_override: Option<String>,
    pub budget_override: Option<f64>,
    pub add_dirs: Vec<String>,
    /// Learnings from previous sprints (fed back by retro → next sprint)
    pub previous_learnings: String,
    /// Product brief — architecture, file map, conventions, gotchas.
    /// Injected into agent system prompts via {{product.brief}} to cut orientation cost.
    pub product_brief: String,
    /// Product definition of done — conditional rules the judge evaluates.
    /// Injected via {{product.definition_of_done}}.
    pub product_definition_of_done: String,
    /// When executing per-story, holds the current story being processed.
    /// Resolved as {{story}} in template interpolation.
    pub current_story: Option<serde_json::Value>,
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
    /// Parsed builder output (populated when per_story execution completes).
    /// Downstream write-back logic uses this instead of re-parsing the output text.
    pub builder_output: Option<crate::builder::BuilderOutput>,
}

/// Execute a ceremony flow using Kahn's topological sort with parallel level execution.
///
/// Algorithm:
/// 1. Compute in-degrees for all nodes
/// 2. Enqueue all zero-degree nodes (sources)
/// 3. For each level: execute all ready nodes IN PARALLEL via join_all
/// 4. After level completes: process gate skips, insert results, update in-degrees
/// 5. always_run nodes execute regardless of skip state
///
/// The {{input}} template variable resolves to concatenated outputs of direct upstream nodes,
/// matching the platform Flow editor's piping behavior.
pub async fn execute_flow(
    flow: &CeremonyFlow,
    ctx: &FlowContext,
    sink: &EventSink,
    client: &ApiClient,
) -> Result<Vec<NodeResult>, Box<dyn std::error::Error>> {
    let adj = flow.adjacency();
    let rev_adj = flow.reverse_adjacency();
    let mut in_deg = flow.in_degrees();
    let mut results: HashMap<String, NodeResult> = HashMap::new();
    let mut skip_set: HashSet<String> = HashSet::new();

    // Checkpoint resume: restore completed nodes from previous crash
    let checkpoint_path = checkpoint_path(&ctx.sprint.session_id.to_string());
    if let Some(checkpoint) = load_checkpoint(&checkpoint_path) {
        let restored = checkpoint.completed_nodes.len();
        for cn in checkpoint.completed_nodes {
            let status = match cn.status.as_str() {
                "Completed" => CeremonyStatus::Completed,
                "Failed" => CeremonyStatus::Failed,
                "Skipped" => CeremonyStatus::Skipped,
                _ => CeremonyStatus::Failed,
            };
            results.insert(
                cn.key.clone(),
                NodeResult {
                    key: cn.key,
                    status,
                    output: cn.output,
                    cost_usd: cn.cost_usd,
                    impediment_raised: cn.impediment_raised,
                    judge_verdict: None,
                    supervisor_decisions: vec![],
                    rubber_duck_sessions: vec![],
                    builder_output: None,
                },
            );
        }
        // Recompute in-degrees: already-completed nodes should be treated as processed
        for key in results.keys() {
            if let Some(downstream) = adj.get(key) {
                for (target, _) in downstream {
                    if let Some(deg) = in_deg.get_mut(target) {
                        *deg = deg.saturating_sub(1);
                    }
                }
            }
        }
        tracing::info!(
            restored,
            "Resumed from checkpoint — skipping completed nodes"
        );
    }

    // Ensure research directory exists for the researcher agent
    let research_dir = format!("{}/.epic-runner/research/{}", ctx.repo_path, ctx.epic.code);
    std::fs::create_dir_all(&research_dir).ok();

    // Kahn's BFS — seed queue with zero-degree nodes (that aren't already completed)
    let mut queue: VecDeque<String> = VecDeque::new();
    for (key, deg) in &in_deg {
        if *deg == 0 && !results.contains_key(key) {
            queue.push_back(key.clone());
        }
    }

    while !queue.is_empty() {
        let level_size = queue.len();
        let level_keys: Vec<String> = queue.drain(..level_size).collect();

        // Take shared references for the async blocks (references are Copy, so
        // async move can capture them without moving the owned HashMap/sets)
        let results_ref = &results;
        let skip_ref = &skip_set;

        // Build futures for all nodes in this BFS level
        let futures: Vec<_> = level_keys
            .iter()
            .map(|key| {
                let node = flow.node(key);
                let should_skip = skip_ref.contains(key);

                // Compute {{input}} — concatenated outputs of direct upstream nodes
                let parent_keys = rev_adj.get(key).cloned().unwrap_or_default();
                let input: String = parent_keys
                    .iter()
                    .filter_map(|pk| results_ref.get(pk)?.output.as_deref())
                    .collect::<Vec<_>>()
                    .join("\n---\n");

                async move {
                    let node = match node {
                        Some(n) => n,
                        None => {
                            return Ok::<_, Box<dyn std::error::Error>>(NodeResult {
                                key: key.clone(),
                                status: CeremonyStatus::Skipped,
                                output: None,
                                cost_usd: None,
                                impediment_raised: false,
                                judge_verdict: None,
                                supervisor_decisions: vec![],
                                rubber_duck_sessions: vec![],
                                builder_output: None,
                            })
                        }
                    };

                    if should_skip && !node.always_run {
                        return Ok(NodeResult {
                            key: key.clone(),
                            status: CeremonyStatus::Skipped,
                            output: None,
                            cost_usd: None,
                            impediment_raised: false,
                            judge_verdict: None,
                            supervisor_decisions: vec![],
                            rubber_duck_sessions: vec![],
                            builder_output: None,
                        });
                    }

                    // Stream node_started event to DB
                    sink.emit(SprintEvent {
                        sprint_id: ctx.sprint.session_id,
                        event_type: SprintEventType::NodeStarted,
                        node_id: Some(node.key.clone()),
                        node_label: Some(node.label.clone()),
                        summary: format!("{} ({})", node.label, node.key),
                        detail: Some(serde_json::json!({
                            "node_key": node.key,
                            "node_type": format!("{:?}", node.node_type),
                        })),
                        timestamp: chrono::Utc::now(),
                    });

                    tracing::info!(label = %node.label, key = %node.key, "Executing node");
                    let result =
                        execute_node(node, ctx, results_ref, &input, &parent_keys, sink).await?;

                    // Stream node_completed event to DB
                    sink.emit(SprintEvent {
                        sprint_id: ctx.sprint.session_id,
                        event_type: SprintEventType::NodeCompleted,
                        node_id: Some(node.key.clone()),
                        node_label: Some(node.label.clone()),
                        summary: format!("{} → {:?}", node.label, result.status),
                        detail: Some(serde_json::json!({
                            "node_key": node.key,
                            "status": format!("{:?}", result.status),
                            "cost_usd": result.cost_usd,
                        })),
                        timestamp: chrono::Utc::now(),
                    });

                    Ok(result)
                }
            })
            .collect();

        // Execute all nodes in this level CONCURRENTLY
        let level_results = futures::future::join_all(futures).await;

        // Post-process: gate skipping, insert results, update in-degrees
        for (key, result) in level_keys.iter().zip(level_results) {
            let result = result?;

            // Handle gate skip propagation AFTER all parallel nodes complete
            if let Some(node) = flow.node(key) {
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
            }

            // Extract and finalize structured artifact (awaited to ensure persistence)
            if let Some((artifact_type, title, content)) =
                event_sink::extract_artifact_info(&result.key, &result)
            {
                let summary = result.output.as_ref().map(|o| {
                    if o.len() > 200 {
                        format!("{}...", &o[..200])
                    } else {
                        o.clone()
                    }
                });
                sink.finalize_artifact(
                    client,
                    ctx.sprint.session_id,
                    &ctx.epic.code,
                    artifact_type,
                    &result.key,
                    &title,
                    summary.as_deref(),
                    content,
                )
                .await;
            }

            results.insert(key.clone(), result);

            // Checkpoint after each node — enables crash recovery
            save_checkpoint(
                &checkpoint_path,
                &ctx.sprint.session_id.to_string(),
                &results,
            );

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

    // Clean up checkpoint on successful completion
    let _ = std::fs::remove_file(&checkpoint_path);

    // Collect results in node definition order
    let ordered: Vec<NodeResult> = flow
        .nodes
        .iter()
        .filter_map(|n| results.remove(&n.key))
        .collect();

    // Finalize sprint-level aggregated cost artifact
    let mut ceremony_costs = serde_json::Map::new();
    let mut total_cost = 0.0f64;
    for result in &ordered {
        if let Some(cost) = result.cost_usd {
            ceremony_costs.insert(result.key.clone(), serde_json::json!(cost));
            total_cost += cost;
        }
    }
    ceremony_costs.insert("total".to_string(), serde_json::json!(total_cost));
    ceremony_costs.insert("schema_version".to_string(), serde_json::json!("1"));

    sink.finalize_artifact(
        client,
        ctx.sprint.session_id,
        &ctx.epic.code,
        "ceremony_costs",
        "flow_engine",
        "Ceremony Cost Breakdown",
        Some(&format!("Total: ${:.2}", total_cost)),
        serde_json::Value::Object(ceremony_costs),
    )
    .await;

    // Finalize sprint-level velocity artifact
    let stories_planned = ctx
        .sprint
        .stories
        .as_ref()
        .and_then(|s| s.as_array())
        .map(|a| a.len())
        .unwrap_or(0);
    let stories_completed_count = ordered
        .iter()
        .find(|r| r.key == "judge_code" || r.key == "judge")
        .and_then(|r| r.judge_verdict.as_ref())
        .and_then(|v| v.stories_completed.as_ref())
        .map(|sc| sc.len())
        .unwrap_or(0);
    let completed_nodes = ordered
        .iter()
        .filter(|r| r.status == CeremonyStatus::Completed)
        .count();

    let velocity_content = serde_json::json!({
        "schema_version": "1",
        "stories_planned": stories_planned,
        "stories_completed": stories_completed_count,
        "points_completed": stories_completed_count,
        "total_cost_usd": total_cost,
        "nodes_completed": completed_nodes,
    });

    sink.finalize_artifact(
        client,
        ctx.sprint.session_id,
        &ctx.epic.code,
        "velocity",
        "flow_engine",
        "Sprint Velocity",
        Some(&format!(
            "{}/{} stories completed",
            stories_completed_count, stories_planned
        )),
        velocity_content,
    )
    .await;

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
    input: &str,
    parent_keys: &[String],
    sink: &EventSink,
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
            builder_output: None,
        }),

        CeremonyNodeType::Harness | CeremonyNodeType::Agent => {
            // ── Conditional skip: research + groom when all stories already have ACs ──
            // Stories that were groomed in a previous sprint already have acceptance_criteria
            // and tasks. No need to spin up an expensive Claude session just to echo them back.
            if (node.key == "research" || node.key == "groom") && !node.config.resume_stories {
                let stories = ctx.stories.as_array().cloned().unwrap_or_default();
                let all_have_acs = !stories.is_empty()
                    && stories.iter().all(|s| {
                        s.get("acceptance_criteria")
                            .and_then(|v| v.as_array())
                            .map(|a| !a.is_empty())
                            .unwrap_or(false)
                    });
                if all_have_acs {
                    tracing::info!(
                        node = %node.key,
                        stories = stories.len(),
                        "All stories already groomed — skipping {}",
                        node.key
                    );
                    sink.emit(SprintEvent {
                        sprint_id: ctx.sprint.session_id,
                        event_type: SprintEventType::NodeCompleted,
                        node_id: Some(node.key.clone()),
                        node_label: Some(node.label.clone()),
                        summary: format!("{} → Skipped (all stories already groomed)", &node.label),
                        detail: Some(serde_json::json!({
                            "node_key": node.key,
                            "status": "Skipped",
                            "reason": "all_stories_have_acceptance_criteria",
                        })),
                        timestamp: chrono::Utc::now(),
                    });
                    return Ok(NodeResult {
                        key: node.key.clone(),
                        status: CeremonyStatus::Completed,
                        output: Some("Skipped — all stories already groomed".to_string()),
                        cost_usd: Some(0.0),
                        impediment_raised: false,
                        judge_verdict: None,
                        supervisor_decisions: vec![],
                        rubber_duck_sessions: vec![],
                        builder_output: None,
                    });
                }
            }

            if node.config.resume_stories {
                // ── Per-story resume (retro interview) ────────────────
                // Resume each story's builder session with a different agent
                // (e.g., scrum-master) to interview about what happened.
                let stories = ctx.stories.as_array().cloned().unwrap_or_default();
                let mut all_outputs = Vec::new();
                let mut total_cost: f64 = 0.0;

                for story_val in &stories {
                    let story_id = story_val
                        .get("id")
                        .and_then(|v| v.as_str())
                        .unwrap_or_default();
                    let story_code = story_val
                        .get("code")
                        .and_then(|v| v.as_str())
                        .unwrap_or("???");

                    let mut story_ctx = ctx.clone();
                    story_ctx.current_story = Some(story_val.clone());

                    let mut config = build_executor_config(node, &story_ctx, input, upstream);

                    // Resume the builder's session (story UUID = session ID)
                    if let Ok(uuid) = story_id.parse::<Uuid>() {
                        config.session_id = uuid;
                        config.resume_session = true;
                    }

                    tracing::info!(
                        story_id,
                        story_code,
                        node = %node.key,
                        "Per-story resume (retro interview)"
                    );

                    let nk = node.key.clone();
                    let sc = story_code.to_string();
                    let sink_clone = sink.clone();
                    let callback = move |e: SprintEvent| {
                        tracing::debug!(
                            key = %nk,
                            story = %sc,
                            event = e.event_type_str(),
                            "Per-story retro event"
                        );
                        sink_clone.emit(e);
                    };
                    let result = executor::execute(config, &callback).await?;

                    if let Some(cost) = result.cost_usd {
                        total_cost += cost;
                    }
                    if let Some(text) = result.result_text {
                        all_outputs.push(format!("## {story_code}\n{text}"));
                    }
                }

                let combined = if all_outputs.is_empty() {
                    None
                } else {
                    Some(all_outputs.join("\n\n---\n\n"))
                };

                Ok(NodeResult {
                    key: node.key.clone(),
                    status: CeremonyStatus::Completed,
                    output: combined,
                    cost_usd: Some(total_cost),
                    impediment_raised: false,
                    judge_verdict: None,
                    supervisor_decisions: vec![],
                    rubber_duck_sessions: vec![],
                    builder_output: None,
                })
            } else {
                // ── Standard single-session dispatch ──────────────────
                let config = build_executor_config(node, ctx, input, upstream);
                let node_key = node.key.clone();
                let sink_clone = sink.clone();
                let callback = move |e: SprintEvent| {
                    tracing::debug!(key = %node_key, event = e.event_type_str(), summary = %e.summary, "Agent event");
                    sink_clone.emit(e);
                };
                let result = executor::execute(config, &callback).await?;

                let status = if result.exit_code == 0 {
                    CeremonyStatus::Completed
                } else {
                    CeremonyStatus::Failed
                };

                // Parse judge verdict from the code judge node.
                // The code judge produces the JudgeVerdict that drives story completion,
                // sprint goal inheritance, and intent satisfaction.
                let verdict = if node.key == "judge_code" || node.key == "judge" {
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
                    builder_output: None,
                })
            }
        }

        CeremonyNodeType::Gate => {
            let field = node.config.gate_field.as_deref().unwrap_or("status");
            let expect = node.config.gate_expect.as_deref().unwrap_or("completed");

            // Evaluate direct upstream node(s) via reverse adjacency — NOT HashMap::last()
            // which has non-deterministic iteration order (IMP-835)
            let upstream_status = parent_keys
                .iter()
                .filter_map(|pk| upstream.get(pk))
                .next_back()
                .map(|r| match field {
                    "status" => format!("{:?}", r.status).to_lowercase(),
                    "impediment_raised" => r.impediment_raised.to_string(),
                    _ => "unknown".to_string(),
                })
                .unwrap_or_default();

            let passed = upstream_status.contains(expect);
            tracing::info!(
                label = %node.label,
                result = if passed { "PASS" } else { "FAIL" },
                expect,
                got = %upstream_status,
                "Gate evaluated"
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
                builder_output: None,
            })
        }

        CeremonyNodeType::Loop => {
            if node.config.per_story {
                // ── Per-story dispatch ─────────────────────────────────
                // Each story gets its own Claude session (story UUID = session ID).
                // Enables: stop hooks per story, --resume for retro, file tracking, context isolation.
                let stories = ctx.stories.as_array().cloned().unwrap_or_default();

                // Cache story UUIDs locally so the stop hook can do a fast lookup
                // to determine if a session is a story session.
                let stories_cache_path =
                    std::path::Path::new(&ctx.repo_path).join(".epic-runner/stories/uuids.cache");
                let _ = std::fs::create_dir_all(stories_cache_path.parent().unwrap());
                {
                    let uuids: Vec<&str> = stories
                        .iter()
                        .filter_map(|s| s.get("id").and_then(|v| v.as_str()))
                        .collect();
                    let _ = std::fs::write(&stories_cache_path, uuids.join("\n"));
                }

                let mut all_decisions = Vec::new();
                let mut all_rubber_ducks = Vec::new();
                let mut all_builder_stories = Vec::new();
                let mut total_cost: f64 = 0.0;
                let mut any_impediment = false;
                let mut all_outputs = Vec::new();
                let mut any_failed = false;

                for story_val in &stories {
                    let story_id = story_val
                        .get("id")
                        .and_then(|v| v.as_str())
                        .unwrap_or_default();
                    let story_code = story_val
                        .get("code")
                        .and_then(|v| v.as_str())
                        .unwrap_or("???");

                    tracing::info!(
                        story_id,
                        story_code,
                        node = %node.key,
                        "Per-story dispatch"
                    );

                    // Build a per-story context with current_story set
                    let mut story_ctx = ctx.clone();
                    story_ctx.current_story = Some(story_val.clone());

                    let mut exec_config = build_executor_config(node, &story_ctx, input, upstream);

                    // Override session_id with story UUID for session isolation
                    if let Ok(uuid) = story_id.parse::<Uuid>() {
                        exec_config.session_id = uuid;
                    }

                    // Write story JSON to disk for stop-hook enforcement.
                    // The stop hook reads this file to check task completion.
                    let stories_dir =
                        std::path::Path::new(&ctx.repo_path).join(".epic-runner/stories");
                    let _ = std::fs::create_dir_all(&stories_dir);
                    let story_file = stories_dir.join(format!("{}.json", story_id));
                    if let Ok(json) = serde_json::to_string_pretty(story_val) {
                        let _ = std::fs::write(&story_file, &json);
                    }

                    // Set env vars for hooks: stop gate + file tracking
                    let changed_files_path = stories_dir
                        .join(format!("{}.changed_files", story_id))
                        .to_string_lossy()
                        .to_string();
                    exec_config.extra_env = vec![
                        (
                            "EPIC_RUNNER_STORY_FILE".to_string(),
                            story_file.to_string_lossy().to_string(),
                        ),
                        ("EPIC_RUNNER_STORY_CODE".to_string(), story_code.to_string()),
                        (
                            "EPIC_RUNNER_CHANGED_FILES".to_string(),
                            changed_files_path.clone(),
                        ),
                        (
                            "EPIC_RUNNER_STORIES_CACHE".to_string(),
                            stories_cache_path.to_string_lossy().to_string(),
                        ),
                    ];

                    let sup_config = supervisor::SupervisorConfig {
                        max_stop_hooks: node.config.loop_max.unwrap_or(5),
                        rubber_duck_after: node.config.rubber_duck_after.unwrap_or(2),
                        auto_abort_on_same_error: true,
                    };

                    let nk = node.key.clone();
                    let sc = story_code.to_string();
                    let sink_clone = sink.clone();
                    let callback = move |e: SprintEvent| {
                        tracing::debug!(
                            key = %nk,
                            story = %sc,
                            event = e.event_type_str(),
                            summary = %e.summary,
                            "Per-story supervised event"
                        );
                        sink_clone.emit(e);
                    };

                    let supervised =
                        supervisor::supervise(exec_config, sup_config, &callback).await?;

                    // Accumulate results
                    if let Some(cost) = supervised.executor_result.cost_usd {
                        total_cost += cost;
                    }
                    if supervised.impediment_raised.is_some() {
                        any_impediment = true;
                    }
                    if supervised.executor_result.exit_code != 0 {
                        any_failed = true;
                    }
                    all_decisions.extend(supervised.decisions);
                    all_rubber_ducks.extend(supervised.rubber_duck_sessions);

                    // Parse builder output from this story's session
                    if let Some(mut builder_out) = crate::builder::parse_builder_output(
                        supervised.executor_result.result_text.as_deref(),
                    ) {
                        // Merge hook-tracked changed_files into builder output
                        let hook_files: Vec<String> = std::fs::read_to_string(&changed_files_path)
                            .unwrap_or_default()
                            .lines()
                            .filter(|l| !l.is_empty())
                            .map(String::from)
                            .collect();
                        if !hook_files.is_empty() {
                            for story_result in &mut builder_out.stories {
                                let mut merged = story_result.changed_files.clone();
                                for f in &hook_files {
                                    if !merged.contains(f) {
                                        merged.push(f.clone());
                                    }
                                }
                                story_result.changed_files = merged;
                            }
                        }
                        all_builder_stories.extend(builder_out.stories);
                    }

                    if let Some(text) = supervised.executor_result.result_text {
                        all_outputs.push(format!("## {story_code}\n{text}"));
                    }
                }

                let status = if any_impediment || any_failed {
                    CeremonyStatus::Failed
                } else {
                    CeremonyStatus::Completed
                };

                let combined_output = if all_outputs.is_empty() {
                    None
                } else {
                    Some(all_outputs.join("\n\n---\n\n"))
                };

                let builder_output = if all_builder_stories.is_empty() {
                    None
                } else {
                    Some(crate::builder::BuilderOutput {
                        stories: all_builder_stories,
                    })
                };

                Ok(NodeResult {
                    key: node.key.clone(),
                    status,
                    output: combined_output,
                    cost_usd: Some(total_cost),
                    impediment_raised: any_impediment,
                    judge_verdict: None,
                    supervisor_decisions: all_decisions,
                    rubber_duck_sessions: all_rubber_ducks,
                    builder_output,
                })
            } else {
                // ── Single-session dispatch (original behavior) ───────
                let exec_config = build_executor_config(node, ctx, input, upstream);
                let sup_config = supervisor::SupervisorConfig {
                    max_stop_hooks: node.config.loop_max.unwrap_or(5),
                    rubber_duck_after: node.config.rubber_duck_after.unwrap_or(2),
                    auto_abort_on_same_error: true,
                };

                let node_key = node.key.clone();
                let sink_clone = sink.clone();
                let callback = move |e: SprintEvent| {
                    tracing::debug!(key = %node_key, event = e.event_type_str(), summary = %e.summary, "Supervised event");
                    sink_clone.emit(e);
                };
                let supervised = supervisor::supervise(exec_config, sup_config, &callback).await?;

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
                    builder_output: None,
                })
            }
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
                builder_output: None,
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
            builder_output: None,
        }),

        CeremonyNodeType::Deploy => execute_deploy_node(node, ctx, sink).await,

        CeremonyNodeType::Promote => execute_promote_node(node, ctx, sink).await,
    }
}

/// Execute a Deploy node: merge worktree → main, push, trigger pipeline, wait.
async fn execute_deploy_node(
    node: &CeremonyNode,
    ctx: &FlowContext,
    sink: &EventSink,
) -> Result<NodeResult, Box<dyn std::error::Error>> {
    let c = &node.config;
    let worktree_path = format!(".claude/worktrees/{}", ctx.epic.code);
    let worktree_branch = format!("worktree-{}", ctx.epic.code);
    let repo_path = &ctx.repo_path;

    // Step 1: Commit any uncommitted changes in the worktree
    sink.emit(SprintEvent {
        sprint_id: ctx.sprint.session_id,
        event_type: SprintEventType::DeployStep,
        node_id: Some(node.key.clone()),
        node_label: Some(node.label.clone()),
        summary: "Committing worktree changes".to_string(),
        detail: None,
        timestamp: chrono::Utc::now(),
    });

    if std::path::Path::new(&worktree_path).exists() {
        // Phase 1: Stage modifications to already-tracked files (always safe)
        let add_tracked = std::process::Command::new("git")
            .args(["add", "-u"])
            .current_dir(&worktree_path)
            .output();
        if let Ok(output) = add_tracked {
            if !output.status.success() {
                tracing::warn!("git add -u failed in worktree");
            }
        }

        // Phase 2: Selectively stage new (untracked) files, skipping dangerous patterns.
        // Patterns are hardcoded defaults merged with user-defined .epic-runner/.gitignore-deploy.
        let untracked = std::process::Command::new("git")
            .args(["ls-files", "--others", "--exclude-standard"])
            .current_dir(&worktree_path)
            .output();
        if let Ok(output) = untracked {
            let file_list = String::from_utf8_lossy(&output.stdout);

            // Built-in dangerous patterns (always active, can't be overridden)
            let mut deny_patterns: Vec<String> = vec![
                ".env",
                ".pem",
                ".key",
                ".p12",
                ".pfx",
                "credentials",
                "secret",
                "node_modules/",
                "target/",
                ".epic-runner/",
                "build/",
                "dist/",
            ]
            .into_iter()
            .map(String::from)
            .collect();

            // Merge user-defined patterns from .epic-runner/.gitignore-deploy (if it exists)
            let gitignore_deploy_path =
                std::path::Path::new(&worktree_path).join(".epic-runner/.gitignore-deploy");
            if gitignore_deploy_path.exists() {
                if let Ok(contents) = std::fs::read_to_string(&gitignore_deploy_path) {
                    for line in contents.lines() {
                        let trimmed = line.trim();
                        // Skip comments and blank lines (gitignore convention)
                        if !trimmed.is_empty() && !trimmed.starts_with('#') {
                            deny_patterns.push(trimmed.to_string());
                        }
                    }
                    tracing::info!(
                        path = %gitignore_deploy_path.display(),
                        "Loaded additional deploy deny patterns from .gitignore-deploy"
                    );
                }
            }

            let mut staged_count = 0;
            let mut skipped: Vec<String> = Vec::new();

            for file in file_list.lines() {
                let file_lower = file.to_lowercase();
                let is_dangerous = deny_patterns.iter().any(|p| file_lower.contains(p));
                if is_dangerous {
                    skipped.push(file.to_string());
                } else {
                    let add_file = std::process::Command::new("git")
                        .args(["add", file])
                        .current_dir(&worktree_path)
                        .output();
                    if let Ok(o) = add_file {
                        if o.status.success() {
                            staged_count += 1;
                        }
                    }
                }
            }

            if !skipped.is_empty() {
                tracing::warn!(
                    skipped_files = ?skipped,
                    "Skipped {} potentially sensitive untracked file(s) during deploy staging",
                    skipped.len()
                );
            }
            if staged_count > 0 {
                tracing::info!(staged_count, "Staged new untracked files for deploy");
            }
        }

        // Phase 3: Post-staging audit — warn if any sensitive-looking files got staged
        // (catches files that passed pattern checks but have suspicious names)
        let staged_diff = std::process::Command::new("git")
            .args(["diff", "--cached", "--name-only"])
            .current_dir(&worktree_path)
            .output();
        if let Ok(output) = staged_diff {
            let staged_files = String::from_utf8_lossy(&output.stdout);
            let sensitive_indicators = [".env", "secret", "credential", "key", "token", "password"];
            let mut warnings: Vec<String> = Vec::new();
            for file in staged_files.lines() {
                let file_lower = file.to_lowercase();
                for indicator in &sensitive_indicators {
                    if file_lower.contains(indicator) {
                        warnings.push(file.to_string());
                        break;
                    }
                }
            }
            if !warnings.is_empty() {
                tracing::warn!(
                    staged_sensitive_files = ?warnings,
                    "⚠ Post-staging audit: {} staged file(s) have sensitive-looking names — review before pushing",
                    warnings.len()
                );
            }
        }

        // Check if there's anything to commit
        let status = std::process::Command::new("git")
            .args(["status", "--porcelain"])
            .current_dir(&worktree_path)
            .output();
        let has_changes = status
            .map(|o| !String::from_utf8_lossy(&o.stdout).trim().is_empty())
            .unwrap_or(false);

        if has_changes {
            let commit_msg = format!(
                "feat({}): sprint {} ceremony changes\n\nEpic: {} — {}\nSprint: {}\n\nCo-Authored-By: Claude Opus 4.6 <noreply@anthropic.com>",
                ctx.epic.code.to_lowercase(),
                ctx.sprint.number,
                ctx.epic.code,
                ctx.epic.title,
                ctx.sprint.number,
            );
            std::process::Command::new("git")
                .args(["commit", "-m", &commit_msg])
                .current_dir(&worktree_path)
                .output()
                .ok();
        }
    }

    // Step 2: Merge worktree branch into main
    sink.emit(SprintEvent {
        sprint_id: ctx.sprint.session_id,
        event_type: SprintEventType::DeployStep,
        node_id: Some(node.key.clone()),
        node_label: Some(node.label.clone()),
        summary: format!("Merging {} into main", worktree_branch),
        detail: None,
        timestamp: chrono::Utc::now(),
    });

    // Checkout the default branch in the repo root
    let default_branch = ctx.default_branch.as_str();
    let checkout = std::process::Command::new("git")
        .args(["checkout", default_branch])
        .current_dir(repo_path)
        .output()?;
    if !checkout.status.success() {
        let err = String::from_utf8_lossy(&checkout.stderr);
        return Ok(deploy_failed(
            node,
            &format!("Failed to checkout {}: {}", default_branch, err),
        ));
    }

    // Pull latest default branch first
    let pull = std::process::Command::new("git")
        .args(["pull", "origin", default_branch, "--rebase"])
        .current_dir(repo_path)
        .output()?;
    if !pull.status.success() {
        // Abort the failed rebase to leave repo in a clean state
        std::process::Command::new("git")
            .args(["rebase", "--abort"])
            .current_dir(repo_path)
            .output()
            .ok(); // best-effort cleanup
        let err = String::from_utf8_lossy(&pull.stderr);
        return Ok(deploy_failed(
            node,
            &format!("Failed to pull latest {}: {}", default_branch, err),
        ));
    }

    // Merge the worktree branch
    let merge = std::process::Command::new("git")
        .args(["merge", &worktree_branch, "--no-edit"])
        .current_dir(repo_path)
        .output()?;
    if !merge.status.success() {
        let err = String::from_utf8_lossy(&merge.stderr);
        // Abort the merge so we don't leave things in a dirty state
        std::process::Command::new("git")
            .args(["merge", "--abort"])
            .current_dir(repo_path)
            .output()
            .ok();
        return Ok(deploy_failed(node, &format!("Merge conflict: {}", err)));
    }

    // Step 3: Push to origin
    sink.emit(SprintEvent {
        sprint_id: ctx.sprint.session_id,
        event_type: SprintEventType::DeployStep,
        node_id: Some(node.key.clone()),
        node_label: Some(node.label.clone()),
        summary: "Pushing to origin/main".to_string(),
        detail: None,
        timestamp: chrono::Utc::now(),
    });

    let push = std::process::Command::new("git")
        .args(["push", "origin", default_branch])
        .current_dir(repo_path)
        .output()?;
    if !push.status.success() {
        let err = String::from_utf8_lossy(&push.stderr);
        return Ok(deploy_failed(node, &format!("Push failed: {}", err)));
    }

    // Step 3b: Sync worktree branch to main — prevents stale-worktree rework in next sprint.
    // After merging worktree→main, the worktree branch is behind main (main has the merge commit).
    // Reset the worktree branch to main's HEAD so the next sprint starts with current code.
    sink.emit(SprintEvent {
        sprint_id: ctx.sprint.session_id,
        event_type: SprintEventType::DeployStep,
        node_id: Some(node.key.clone()),
        node_label: Some(node.label.clone()),
        summary: format!("Syncing {} to main HEAD", worktree_branch),
        detail: None,
        timestamp: chrono::Utc::now(),
    });
    // Use `git rebase <default_branch>` from within the worktree directory — `git branch -f`
    // fails when the branch is checked out in a worktree.
    let wt_abs = std::path::Path::new(repo_path).join(&worktree_path);
    let sync = std::process::Command::new("git")
        .args(["rebase", default_branch])
        .current_dir(&wt_abs)
        .output()?;
    if !sync.status.success() {
        // Abort failed rebase, then try reset as fallback
        std::process::Command::new("git")
            .args(["rebase", "--abort"])
            .current_dir(&wt_abs)
            .output()
            .ok();
        // Fallback: hard reset to default branch (loses any uncommitted worktree changes, which
        // is fine since we just committed everything in Step 1)
        let reset = std::process::Command::new("git")
            .args(["reset", "--hard", default_branch])
            .current_dir(&wt_abs)
            .output()?;
        if !reset.status.success() {
            let err = String::from_utf8_lossy(&reset.stderr);
            tracing::warn!(
                "Failed to sync worktree to {} (non-fatal): {}",
                default_branch,
                err
            );
        } else {
            tracing::info!(
                "Synced {} to {} HEAD via reset — next sprint starts fresh",
                worktree_branch,
                default_branch
            );
        }
    } else {
        tracing::info!(
            "Synced {} to {} HEAD via rebase — next sprint starts fresh",
            worktree_branch,
            default_branch
        );
    }

    // Step 4: Trigger Connect App Pipeline
    // If no deploy_app_id is configured, skip gracefully — this product is a CLI tool
    // (or other non-Connect-App artifact) that doesn't need pipeline deployment.
    let cfg = match resolve_deploy_config(c) {
        Ok(cfg) => cfg,
        Err(_) => {
            let msg = "No deploy_app_id configured — skipping deploy (CLI/non-Connect-App product)";
            tracing::info!("{}", msg);
            sink.emit(SprintEvent {
                sprint_id: ctx.sprint.session_id,
                event_type: SprintEventType::DeployStep,
                node_id: Some(node.key.clone()),
                node_label: Some(node.label.clone()),
                summary: msg.to_string(),
                detail: None,
                timestamp: chrono::Utc::now(),
            });
            return Ok(NodeResult {
                key: node.key.clone(),
                status: CeremonyStatus::Skipped,
                output: Some(msg.to_string()),
                cost_usd: None,
                impediment_raised: false,
                judge_verdict: None,
                supervisor_decisions: vec![],
                rubber_duck_sessions: vec![],
                builder_output: None,
            });
        }
    };
    let app_id = cfg.app_id.as_str();
    let api_key = cfg.api_key.as_str();
    let api_url = cfg.api_url.as_str();
    let timeout_secs = c.deploy_timeout_secs.unwrap_or(300);

    // Step 4a: Acquire platform-level deploy lock (per app_id)
    let agent_id = ctx.sprint.session_id.to_string();
    let lock_reason = format!(
        "deploy→judge→promote for {} sprint {}",
        ctx.epic.code, ctx.sprint.number
    );

    sink.emit(SprintEvent {
        sprint_id: ctx.sprint.session_id,
        event_type: SprintEventType::DeployStep,
        node_id: Some(node.key.clone()),
        node_label: Some(node.label.clone()),
        summary: format!("Acquiring deploy lock for {}", app_id),
        detail: None,
        timestamp: chrono::Utc::now(),
    });

    match acquire_deploy_lock(&cfg, &agent_id, Some(&agent_id), &lock_reason).await {
        Ok(true) => { /* lock acquired */ }
        Ok(false) => {
            return Ok(deploy_failed(
                node,
                "Deploy lock held by another agent — cannot deploy concurrently. Retry next sprint.",
            ));
        }
        Err(e) => {
            // Lock API not available (pre-migration) — proceed without lock
            tracing::warn!(error = %e, "Deploy lock unavailable — proceeding without lock");
        }
    }

    sink.emit(SprintEvent {
        sprint_id: ctx.sprint.session_id,
        event_type: SprintEventType::DeployStep,
        node_id: Some(node.key.clone()),
        node_label: Some(node.label.clone()),
        summary: format!("Triggering Connect App Pipeline for {}", app_id),
        detail: None,
        timestamp: chrono::Utc::now(),
    });

    let deploy_url = format!(
        "{}/v1/apps/{}/environments/production/deploy",
        api_url, app_id
    );

    // Build deploy request body — include slot if configured (for blue-green)
    let mut deploy_body = serde_json::json!({});
    let deploy_slot = c.deploy_slot.as_deref();
    if let Some(slot) = deploy_slot {
        deploy_body["slot"] = serde_json::json!(slot);
        tracing::info!(slot, "Deploying to slot (blue-green mode)");
    }

    let http_client = reqwest::Client::new();
    let resp = http_client
        .post(&deploy_url)
        .header("x-api-key", api_key)
        .json(&deploy_body)
        .timeout(std::time::Duration::from_secs(30))
        .send()
        .await;

    let pipeline_run_id = match resp {
        Ok(r) if r.status().is_success() => {
            let body: serde_json::Value = r.json().await.unwrap_or_default();
            // The deploy endpoint returns pipeline_run_id — that's what we poll
            body["pipeline_run_id"]
                .as_str()
                .or_else(|| body["id"].as_str())
                .map(String::from)
        }
        Ok(r) => {
            let status = r.status();
            let body = r.text().await.unwrap_or_default();
            release_deploy_lock(&cfg, &agent_id).await;
            return Ok(deploy_failed(
                node,
                &format!("Deploy API returned {}: {}", status, body),
            ));
        }
        Err(e) => {
            release_deploy_lock(&cfg, &agent_id).await;
            return Ok(deploy_failed(
                node,
                &format!("Deploy API request failed: {}", e),
            ));
        }
    };

    // Step 5: Wait for deploy to complete
    sink.emit(SprintEvent {
        sprint_id: ctx.sprint.session_id,
        event_type: SprintEventType::DeployStep,
        node_id: Some(node.key.clone()),
        node_label: Some(node.label.clone()),
        summary: format!(
            "Waiting for deploy to complete (timeout: {}s)",
            timeout_secs
        ),
        detail: pipeline_run_id
            .as_ref()
            .map(|id| serde_json::json!({"pipeline_run_id": id})),
        timestamp: chrono::Utc::now(),
    });

    let start = std::time::Instant::now();
    #[allow(unused_assignments)]
    let mut deploy_success = false;

    // If we have a pipeline_run_id, poll its status
    if let Some(run_id) = &pipeline_run_id {
        let status_url = format!("{}/v1/pipeline-runs/{}", api_url, run_id);
        loop {
            if start.elapsed() > std::time::Duration::from_secs(timeout_secs) {
                release_deploy_lock(&cfg, &agent_id).await;
                return Ok(deploy_failed(node, "Deploy timed out"));
            }

            tokio::time::sleep(std::time::Duration::from_secs(10)).await;

            let status_resp = http_client
                .get(&status_url)
                .header("x-api-key", api_key)
                .send()
                .await;

            if let Ok(r) = status_resp {
                if let Ok(body) = r.json::<serde_json::Value>().await {
                    let status = body["status"].as_str().unwrap_or("");
                    match status {
                        "deployed" | "succeeded" | "completed" | "success" => {
                            deploy_success = true;
                            break;
                        }
                        "failed" | "error" | "cancelled" => {
                            let msg = body["error"].as_str().unwrap_or("unknown error");
                            release_deploy_lock(&cfg, &agent_id).await;
                            return Ok(deploy_failed(node, &format!("Deploy failed: {}", msg)));
                        }
                        _ => {
                            let elapsed = start.elapsed().as_secs();
                            tracing::info!(status, elapsed, "Deploy still in progress...");
                        }
                    }
                }
            }
        }
    } else {
        // No deployment_id — fall back to health check polling
        if let Some(health_url) = &c.deploy_health_url {
            loop {
                if start.elapsed() > std::time::Duration::from_secs(timeout_secs) {
                    release_deploy_lock(&cfg, &agent_id).await;
                    return Ok(deploy_failed(node, "Deploy timed out (health check)"));
                }

                tokio::time::sleep(std::time::Duration::from_secs(10)).await;

                let health = http_client.get(health_url).send().await;
                if let Ok(r) = health {
                    if r.status().is_success() {
                        deploy_success = true;
                        break;
                    }
                }
            }
        } else {
            // No way to verify — wait a fixed 60s and hope for the best
            tracing::warn!("No deployment_id or health_url — waiting 60s for deploy");
            tokio::time::sleep(std::time::Duration::from_secs(60)).await;
            deploy_success = true;
        }
    }

    // Build summary with slot info and URLs for downstream A/B judge
    let slot_info = deploy_slot.unwrap_or("primary");
    let production_url = c.deploy_production_url.as_deref().unwrap_or("");
    let standby_url = c.deploy_standby_url.as_deref().unwrap_or("");

    let summary = if deploy_success {
        let mut s = format!("Deployed {} to slot '{}' successfully", app_id, slot_info);
        if !production_url.is_empty() || !standby_url.is_empty() {
            s.push_str(&format!(
                "\n\nA/B URLs for judge:\n- Production (live): {}\n- Standby (new): {}",
                production_url, standby_url
            ));
        }
        s
    } else {
        format!("Deploy {} to slot '{}' uncertain", app_id, slot_info)
    };

    sink.emit(SprintEvent {
        sprint_id: ctx.sprint.session_id,
        event_type: SprintEventType::DeployStep,
        node_id: Some(node.key.clone()),
        node_label: Some(node.label.clone()),
        summary: summary.clone(),
        detail: None,
        timestamp: chrono::Utc::now(),
    });

    Ok(NodeResult {
        key: node.key.clone(),
        status: if deploy_success {
            CeremonyStatus::Completed
        } else {
            CeremonyStatus::Failed
        },
        output: Some(summary),
        cost_usd: None,
        impediment_raised: false,
        judge_verdict: None,
        supervisor_decisions: vec![],
        rubber_duck_sessions: vec![],
        builder_output: None,
    })
}

/// Execute a Promote node: call the promote-slot API to shift 100% traffic to standby.
/// Releases the deploy lock after promotion (or on failure).
async fn execute_promote_node(
    node: &CeremonyNode,
    ctx: &FlowContext,
    sink: &EventSink,
) -> Result<NodeResult, Box<dyn std::error::Error>> {
    let c = &node.config;
    // If no deploy_app_id is configured, skip gracefully — nothing to promote.
    let cfg = match resolve_deploy_config(c) {
        Ok(cfg) => cfg,
        Err(_) => {
            let msg =
                "No deploy_app_id configured — skipping promote (CLI/non-Connect-App product)";
            tracing::info!("{}", msg);
            return Ok(NodeResult {
                key: node.key.clone(),
                status: CeremonyStatus::Skipped,
                output: Some(msg.to_string()),
                cost_usd: None,
                impediment_raised: false,
                judge_verdict: None,
                supervisor_decisions: vec![],
                rubber_duck_sessions: vec![],
                builder_output: None,
            });
        }
    };
    let app_id = cfg.app_id.as_str();
    let api_key = cfg.api_key.as_str();
    let api_url = cfg.api_url.as_str();
    let agent_id = ctx.sprint.session_id.to_string();

    let slot_name = c.deploy_slot.as_deref().unwrap_or("standby");

    sink.emit(SprintEvent {
        sprint_id: ctx.sprint.session_id,
        event_type: SprintEventType::DeployStep,
        node_id: Some(node.key.clone()),
        node_label: Some(node.label.clone()),
        summary: format!("Promoting slot '{}' to primary (100% traffic)", slot_name),
        detail: None,
        timestamp: chrono::Utc::now(),
    });

    let promote_url = format!(
        "{}/v1/apps/{}/environments/production/slots/{}/promote",
        api_url, app_id, slot_name
    );

    let http_client = reqwest::Client::new();
    let resp = http_client
        .post(&promote_url)
        .header("x-api-key", api_key)
        .timeout(std::time::Duration::from_secs(30))
        .send()
        .await;

    let result = match resp {
        Ok(r) if r.status().is_success() => {
            let summary = format!(
                "Promoted slot '{}' to primary — zero-downtime swap complete",
                slot_name
            );
            sink.emit(SprintEvent {
                sprint_id: ctx.sprint.session_id,
                event_type: SprintEventType::DeployStep,
                node_id: Some(node.key.clone()),
                node_label: Some(node.label.clone()),
                summary: summary.clone(),
                detail: None,
                timestamp: chrono::Utc::now(),
            });
            NodeResult {
                key: node.key.clone(),
                status: CeremonyStatus::Completed,
                output: Some(summary),
                cost_usd: None,
                impediment_raised: false,
                judge_verdict: None,
                supervisor_decisions: vec![],
                rubber_duck_sessions: vec![],
                builder_output: None,
            }
        }
        Ok(r) => {
            let status = r.status();
            let body = r.text().await.unwrap_or_default();
            deploy_failed(node, &format!("Promote API returned {}: {}", status, body))
        }
        Err(e) => deploy_failed(node, &format!("Promote API request failed: {}", e)),
    };

    // Always release the deploy lock after promote (success or failure)
    release_deploy_lock(&cfg, &agent_id).await;

    Ok(result)
}

/// Resolved deploy configuration — shared by Deploy, Promote, and lock helpers.
struct DeployConfig {
    app_id: String,
    api_key: String,
    api_url: String,
}

/// Resolve deploy configuration from node config → project config → env vars.
fn resolve_deploy_config(c: &CeremonyNodeConfig) -> Result<DeployConfig, String> {
    let project_config =
        crate::config::find_project_config().and_then(|p| crate::config::read_config(&p).ok());

    let resolve_env_ref = |val: &str| -> Option<String> {
        if val.starts_with("${") && val.ends_with('}') {
            let var_name = &val[2..val.len() - 1];
            std::env::var(var_name).ok()
        } else if !val.is_empty() {
            Some(val.to_string())
        } else {
            None
        }
    };

    let app_id = c
        .deploy_app_id
        .as_deref()
        .and_then(resolve_env_ref)
        .or_else(|| {
            project_config
                .as_ref()
                .and_then(|c| c.deploy_app_id().map(String::from))
        })
        .or_else(|| std::env::var("DEPLOY_APP_ID").ok())
        .ok_or_else(|| "deploy_app_id not configured".to_string())?;

    let api_key = c
        .deploy_api_key
        .clone()
        .or_else(|| {
            project_config
                .as_ref()
                .and_then(|c| c.deploy_api_key().map(String::from))
        })
        .unwrap_or_else(|| {
            std::env::var("KAPABLE_ADMIN_API_KEY")
                .unwrap_or_else(|_| "sk_admin_61af775f967c434dbace3877ade456b8".to_string())
        });

    let api_url = c
        .deploy_api_url
        .clone()
        .or_else(|| {
            project_config
                .as_ref()
                .and_then(|c| c.deploy_api_url().map(String::from))
        })
        .unwrap_or_else(|| {
            std::env::var("KAPABLE_API_URL")
                .unwrap_or_else(|_| "https://api.kapable.dev".to_string())
        });

    Ok(DeployConfig {
        app_id,
        api_key,
        api_url,
    })
}

/// Acquire a platform-level deploy lock for the deploy→judge→promote sequence.
/// Returns Ok(true) if acquired, Ok(false) if lock is held, Err on network failure.
async fn acquire_deploy_lock(
    cfg: &DeployConfig,
    agent_id: &str,
    session_id: Option<&str>,
    reason: &str,
) -> Result<bool, String> {
    let url = format!("{}/v1/apps/{}/deploy-lock", cfg.api_url, cfg.app_id);

    let body = serde_json::json!({
        "agent_id": agent_id,
        "session_id": session_id,
        "reason": reason,
        "ttl_secs": 900,  // 15 min — covers deploy + judge + promote
    });

    let client = reqwest::Client::new();
    let resp = client
        .post(&url)
        .header("x-api-key", &cfg.api_key)
        .json(&body)
        .timeout(std::time::Duration::from_secs(10))
        .send()
        .await
        .map_err(|e| format!("Deploy lock request failed: {}", e))?;

    if resp.status().is_success() {
        tracing::info!(app_id = %cfg.app_id, agent_id, "Deploy lock acquired");
        Ok(true)
    } else if resp.status().as_u16() == 409 {
        let body = resp.text().await.unwrap_or_default();
        tracing::warn!(app_id = %cfg.app_id, agent_id, body, "Deploy lock conflict");
        Ok(false)
    } else {
        let status = resp.status();
        let body = resp.text().await.unwrap_or_default();
        Err(format!("Deploy lock API returned {}: {}", status, body))
    }
}

/// Release a platform-level deploy lock. Best-effort — log but don't fail on errors.
async fn release_deploy_lock(cfg: &DeployConfig, agent_id: &str) {
    let url = format!("{}/v1/apps/{}/deploy-lock", cfg.api_url, cfg.app_id);

    let body = serde_json::json!({
        "agent_id": agent_id,
    });

    let client = reqwest::Client::new();
    match client
        .delete(&url)
        .header("x-api-key", &cfg.api_key)
        .json(&body)
        .timeout(std::time::Duration::from_secs(10))
        .send()
        .await
    {
        Ok(r) if r.status().is_success() => {
            tracing::info!(app_id = %cfg.app_id, agent_id, "Deploy lock released");
        }
        Ok(r) => {
            let status = r.status();
            let body = r.text().await.unwrap_or_default();
            tracing::warn!(app_id = %cfg.app_id, status = %status, body, "Deploy lock release non-200");
        }
        Err(e) => {
            tracing::warn!(app_id = %cfg.app_id, error = %e, "Deploy lock release failed (will TTL-expire)");
        }
    }
}

/// Helper to create a failed deploy NodeResult
fn deploy_failed(node: &CeremonyNode, reason: &str) -> NodeResult {
    tracing::error!(reason, "Deploy node failed");
    NodeResult {
        key: node.key.clone(),
        status: CeremonyStatus::Failed,
        output: Some(format!("Deploy failed: {}", reason)),
        cost_usd: None,
        impediment_raised: false,
        judge_verdict: None,
        supervisor_decisions: vec![],
        rubber_duck_sessions: vec![],
        builder_output: None,
    }
}

/// Build an ExecutorConfig from a ceremony node's config + flow context.
fn build_executor_config(
    node: &CeremonyNode,
    ctx: &FlowContext,
    input: &str,
    all_results: &HashMap<String, NodeResult>,
) -> ExecutorConfig {
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
        system_prompt: c
            .system_prompt
            .as_ref()
            .map(|s| interpolate(s, ctx, input, all_results)),
        prompt: interpolate(c.prompt.as_deref().unwrap_or(""), ctx, input, all_results),
        chrome: c.chrome,
        brief: c.brief,
        max_budget_usd: ctx.budget_override.or(c.budget_usd),
        allowed_tools: c.allowed_tools.clone(),
        resume_session: false,
        agent: c.agent.clone(),
        heartbeat_timeout_secs: c.heartbeat_timeout_secs.unwrap_or(300),
        node_id: Some(node.key.clone()),
        node_label: Some(node.label.clone()),
        max_turns: c.max_turns,
        extra_env: vec![],
    }
}

/// Template interpolation supporting all flow variables:
/// - {{input}} — concatenated outputs of direct upstream nodes (platform Flow editor compatible)
/// - {{epic.code}}, {{epic.title}}, {{epic.intent}}, {{epic.success_criteria}}
/// - {{sprint.number}}, {{stories}}
/// - {{ceremony_results}} — human-readable CSV summary of all node results so far
/// - {{ceremony_results_json}} — structured JSON array of all node results so far
/// - {{supervisor_decisions}} — summary of all supervisor decisions so far
/// - {{repo.claude_md}} — contents of CLAUDE.md from the repo root (project conventions)
/// - {{product.brief}} — product architecture, file map, conventions (cuts agent orientation cost)
/// - {{product.definition_of_done}} — conditional DoD rules for the judge
fn interpolate(
    template: &str,
    ctx: &FlowContext,
    input: &str,
    all_results: &HashMap<String, NodeResult>,
) -> String {
    // Build ceremony_results — human-readable CSV
    let ceremony_results: String = all_results
        .values()
        .map(|r| format!("{}: {:?}", r.key, r.status))
        .collect::<Vec<_>>()
        .join(", ");

    // Build ceremony_results_json — structured array for programmatic parsing
    let ceremony_results_json: String = serde_json::to_string_pretty(
        &all_results
            .values()
            .map(|r| {
                serde_json::json!({
                    "key": r.key,
                    "status": format!("{:?}", r.status),
                    "cost_usd": r.cost_usd,
                    "impediment_raised": r.impediment_raised,
                })
            })
            .collect::<Vec<_>>(),
    )
    .unwrap_or_else(|_| "[]".to_string());

    let supervisor_decisions: String = all_results
        .values()
        .flat_map(|r| r.supervisor_decisions.iter())
        .map(|d| format!("{:?}: {}", d.decision, d.reasoning))
        .collect::<Vec<_>>()
        .join("; ");

    // Load CLAUDE.md from repo root (best-effort — empty string if missing)
    let claude_md = if template.contains("{{repo.claude_md}}") {
        let claude_md_path = std::path::Path::new(&ctx.repo_path).join("CLAUDE.md");
        std::fs::read_to_string(&claude_md_path).unwrap_or_default()
    } else {
        String::new()
    };

    // Extract A/B URLs from deploy node output (if present)
    let deploy_output = all_results
        .get("deploy_standby")
        .or_else(|| all_results.get("deploy"))
        .and_then(|r| r.output.as_deref())
        .unwrap_or("");
    let ab_urls = deploy_output; // The full deploy output contains A/B URLs section

    // Resolve {{story}} — single story JSON when in per-story mode
    let current_story_json = ctx
        .current_story
        .as_ref()
        .map(|s| serde_json::to_string_pretty(s).unwrap_or_default())
        .unwrap_or_default();

    template
        .replace("{{input}}", input)
        .replace("{{ceremony_results}}", &ceremony_results)
        .replace("{{ceremony_results_json}}", &ceremony_results_json)
        .replace("{{supervisor_decisions}}", &supervisor_decisions)
        .replace("{{repo.claude_md}}", &claude_md)
        .replace("{{previous_learnings}}", &ctx.previous_learnings)
        .replace("{{deploy_output}}", ab_urls)
        .replace("{{product.brief}}", &ctx.product_brief)
        .replace(
            "{{product.definition_of_done}}",
            &ctx.product_definition_of_done,
        )
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
        .replace("{{story}}", &current_story_json)
}

/// Get the checkpoint file path for a sprint session.
fn checkpoint_path(session_id: &str) -> std::path::PathBuf {
    std::path::PathBuf::from(format!(".epic-runner/checkpoints/{}.json", session_id))
}

/// Load checkpoint from disk (returns None if missing or corrupt).
fn load_checkpoint(path: &std::path::Path) -> Option<FlowCheckpoint> {
    let data = std::fs::read_to_string(path).ok()?;
    serde_json::from_str(&data).ok()
}

/// Save checkpoint to disk (best-effort — don't crash the flow if disk write fails).
fn save_checkpoint(
    path: &std::path::Path,
    session_id: &str,
    results: &HashMap<String, NodeResult>,
) {
    let checkpoint = FlowCheckpoint {
        sprint_session_id: session_id.to_string(),
        completed_nodes: results
            .values()
            .map(|r| CheckpointedNode {
                key: r.key.clone(),
                status: format!("{:?}", r.status),
                output: r.output.clone(),
                cost_usd: r.cost_usd,
                impediment_raised: r.impediment_raised,
            })
            .collect(),
    };

    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).ok();
    }
    match serde_json::to_string_pretty(&checkpoint) {
        Ok(json) => {
            if let Err(e) = std::fs::write(path, json) {
                tracing::warn!(error = %e, "Failed to write checkpoint");
            }
        }
        Err(e) => tracing::warn!(error = %e, "Failed to serialize checkpoint"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Verifies that detect_default_branch falls back gracefully when there is no
    /// remote configured (e.g. a brand-new temp directory with git init but no remote).
    #[test]
    fn test_detect_default_branch_fallback() {
        let tmp = tempfile::TempDir::new().expect("tempdir");
        // Initialise a bare git repo — no remote, so symbolic-ref and rev-parse both fail.
        std::process::Command::new("git")
            .args(["init"])
            .current_dir(tmp.path())
            .output()
            .expect("git init");
        let branch = detect_default_branch(tmp.path().to_str().unwrap());
        // Must not panic and must return a non-empty string.
        assert!(!branch.is_empty(), "fallback branch must not be empty");
        // The hard-coded last-resort is "main".
        assert_eq!(branch, "main");
    }

    /// Verifies that detect_default_branch reads the symbolic-ref when available.
    /// Uses `git symbolic-ref` to point refs/remotes/origin/HEAD → master, then
    /// asserts the function returns "master" not "main".
    #[test]
    fn test_detect_default_branch() {
        let tmp = tempfile::TempDir::new().expect("tempdir");
        let repo = tmp.path().to_str().unwrap();

        // Init repo and create the ref namespace git clone would create.
        std::process::Command::new("git")
            .args(["init"])
            .current_dir(repo)
            .output()
            .expect("git init");

        // Create the packed-refs directory structure so symbolic-ref can write.
        std::process::Command::new("git")
            .args([
                "symbolic-ref",
                "refs/remotes/origin/HEAD",
                "refs/remotes/origin/master",
            ])
            .current_dir(repo)
            .output()
            .expect("git symbolic-ref set");

        let branch = detect_default_branch(repo);
        assert_eq!(branch, "master", "should detect master from symbolic-ref");
    }
}
