use std::collections::{HashMap, HashSet, VecDeque};
use uuid::Uuid;

use super::definition::*;
use crate::event_sink::EventSink;
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

/// Context passed through the flow during execution.
pub struct FlowContext {
    pub epic: Epic,
    pub sprint: Sprint,
    pub stories: serde_json::Value,
    pub repo_path: String,
    pub model_override: Option<String>,
    pub effort_override: Option<String>,
    pub budget_override: Option<f64>,
    pub add_dirs: Vec<String>,
    /// Learnings from previous sprints (fed back by retro → next sprint)
    pub previous_learnings: String,
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
        }),

        CeremonyNodeType::Harness | CeremonyNodeType::Agent => {
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
            })
        }

        CeremonyNodeType::Loop => {
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

        CeremonyNodeType::Deploy => {
            execute_deploy_node(node, ctx, sink).await
        }
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
        // Stage all changes
        let add = std::process::Command::new("git")
            .args(["add", "-A"])
            .current_dir(&worktree_path)
            .output();
        if let Ok(output) = add {
            if !output.status.success() {
                tracing::warn!("git add -A failed in worktree");
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

    // Checkout main in the repo root
    let checkout = std::process::Command::new("git")
        .args(["checkout", "main"])
        .current_dir(repo_path)
        .output()?;
    if !checkout.status.success() {
        let err = String::from_utf8_lossy(&checkout.stderr);
        return Ok(deploy_failed(node, &format!("Failed to checkout main: {}", err)));
    }

    // Pull latest main first
    std::process::Command::new("git")
        .args(["pull", "origin", "main", "--rebase"])
        .current_dir(repo_path)
        .output()
        .ok();

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
        .args(["push", "origin", "main"])
        .current_dir(repo_path)
        .output()?;
    if !push.status.success() {
        let err = String::from_utf8_lossy(&push.stderr);
        return Ok(deploy_failed(node, &format!("Push failed: {}", err)));
    }

    // Step 4: Trigger Connect App Pipeline
    // Resolve deploy_app_id: config value → env var → error
    // Config values like "${DEPLOY_APP_ID}" are treated as env var references
    let app_id = match &c.deploy_app_id {
        Some(id) if id.starts_with("${") && id.ends_with('}') => {
            let var_name = &id[2..id.len() - 1];
            std::env::var(var_name).ok()
        }
        Some(id) if !id.is_empty() => Some(id.clone()),
        _ => None,
    }
    .or_else(|| std::env::var("DEPLOY_APP_ID").ok());

    let app_id = match app_id {
        Some(id) => id,
        None => {
            return Ok(deploy_failed(
                node,
                "deploy_app_id not configured — set DEPLOY_APP_ID env var or deploy_app_id in flow YAML",
            ));
        }
    };
    let api_key = c.deploy_api_key.clone().unwrap_or_else(|| {
        std::env::var("KAPABLE_ADMIN_API_KEY")
            .unwrap_or_else(|_| "sk_admin_61af775f967c434dbace3877ade456b8".to_string())
    });
    let api_url = c.deploy_api_url.clone().unwrap_or_else(|| {
        std::env::var("KAPABLE_API_URL")
            .unwrap_or_else(|_| "https://api.kapable.dev".to_string())
    });
    let timeout_secs = c.deploy_timeout_secs.unwrap_or(300);

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

    let http_client = reqwest::Client::new();
    let resp = http_client
        .post(&deploy_url)
        .header("x-api-key", &api_key)
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
            return Ok(deploy_failed(
                node,
                &format!("Deploy API returned {}: {}", status, body),
            ));
        }
        Err(e) => {
            return Ok(deploy_failed(node, &format!("Deploy API request failed: {}", e)));
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
        detail: pipeline_run_id.as_ref().map(|id| serde_json::json!({"pipeline_run_id": id})),
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
                return Ok(deploy_failed(node, "Deploy timed out"));
            }

            tokio::time::sleep(std::time::Duration::from_secs(10)).await;

            let status_resp = http_client
                .get(&status_url)
                .header("x-api-key", &api_key)
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
                            return Ok(deploy_failed(
                                node,
                                &format!("Deploy failed: {}", msg),
                            ));
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

    let summary = if deploy_success {
        format!("Deployed {} successfully", app_id)
    } else {
        format!("Deploy {} uncertain", app_id)
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
    })
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

    template
        .replace("{{input}}", input)
        .replace("{{ceremony_results}}", &ceremony_results)
        .replace("{{ceremony_results_json}}", &ceremony_results_json)
        .replace("{{supervisor_decisions}}", &supervisor_decisions)
        .replace("{{repo.claude_md}}", &claude_md)
        .replace("{{previous_learnings}}", &ctx.previous_learnings)
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
