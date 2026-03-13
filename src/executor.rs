use std::collections::HashMap;
use std::process::Stdio;
use std::time::Duration;
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::Command;
use uuid::Uuid;

use crate::agents;
use crate::stream::{self, StreamEvent};
use crate::types::{SprintEvent, SprintEventType};

/// Tools that generate high-frequency events during codebase exploration.
/// These are aggregated into a single summary event per executor run instead
/// of emitting individual CeremonyStarted events (which can produce 500+ events).
const HIGH_FREQUENCY_TOOLS: &[&str] = &["Read", "Grep", "Glob", "Bash"];

#[derive(Clone)]
pub struct ExecutorConfig {
    pub model: String,
    pub effort: String,
    pub worktree_name: String,
    pub session_id: Uuid,
    pub repo_path: String,
    pub add_dirs: Vec<String>,
    pub system_prompt: Option<String>,
    pub prompt: String,
    pub chrome: bool,
    pub max_budget_usd: Option<f64>,
    pub allowed_tools: Option<Vec<String>>,
    pub resume_session: bool,
    pub agent: Option<String>,
    pub heartbeat_timeout_secs: u64,
    /// Enable --brief mode (activates SendUserMessage tool for structured status updates)
    pub brief: bool,
    /// Maximum turns for the Claude CLI session
    pub max_turns: Option<u32>,
    /// Node identity for ceremony event attribution
    pub node_id: Option<String>,
    pub node_label: Option<String>,
    /// Additional environment variables to set on the subprocess
    #[allow(clippy::type_complexity)]
    pub extra_env: Vec<(String, String)>,
}

pub struct ExecutorResult {
    pub session_id: Uuid,
    pub exit_code: i32,
    pub result_text: Option<String>,
    pub cost_usd: Option<f64>,
    pub events: Vec<SprintEvent>,
    pub stop_hook_fired: bool,
    pub last_tool_use: Option<String>,
    /// Structured messages sent by the subprocess via SendUserMessage tool
    pub user_messages: Vec<String>,
    /// All text blocks from every assistant message in the session.
    /// The `result_text` only captures the final result, but structured output
    /// (like JSON arrays from the groomer) often appears in earlier messages.
    /// Consumers should search these when `result_text` fails to parse.
    pub all_assistant_texts: Vec<String>,
    /// Aggregated tool call counts for high-frequency tools (Read, Grep, Glob, Bash).
    /// These are counted instead of emitting individual CeremonyStarted events,
    /// reducing event volume by 90%+ for research-heavy nodes.
    pub tool_event_counts: HashMap<String, u64>,
}

pub fn build_command(config: &ExecutorConfig) -> Command {
    let mut cmd = Command::new("claude");
    cmd.arg("--print");
    cmd.arg("--verbose");
    cmd.arg("--output-format").arg("stream-json");
    cmd.arg("--model").arg(&config.model);
    cmd.arg("--effort").arg(&config.effort);
    cmd.arg("--max-turns")
        .arg(config.max_turns.unwrap_or(50).to_string());
    cmd.arg("--dangerously-skip-permissions");

    // Disable Claude's built-in git commit/PR instructions — ceremony nodes
    // have their own git handling via system prompts and deploy nodes.
    cmd.env("CLAUDE_CODE_DISABLE_GIT_INSTRUCTIONS", "1");

    // Set additional env vars (e.g., EPIC_RUNNER_STORY_FILE for stop hook)
    for (key, val) in &config.extra_env {
        cmd.env(key, val);
    }

    if config.resume_session {
        cmd.arg("--resume").arg(config.session_id.to_string());
    } else {
        cmd.arg("--session-id").arg(config.session_id.to_string());
        cmd.arg("--worktree").arg(&config.worktree_name);
    }

    if let Some(agent) = &config.agent {
        // Resolve agent name to absolute path (checks repo override, then embedded)
        let repo = std::path::Path::new(&config.repo_path);
        if let Some(agent_path) = agents::resolve_agent_path(agent, repo) {
            cmd.arg("--agent").arg(agent_path);
        } else {
            // Fall back to bare name (let Claude Code resolve it)
            cmd.arg("--agent").arg(agent);
        }
    }
    if config.brief {
        cmd.arg("--brief");
    }
    if config.chrome {
        cmd.arg("--chrome");
    }
    for dir in &config.add_dirs {
        cmd.arg("--add-dir").arg(dir);
    }
    if let Some(budget) = config.max_budget_usd {
        cmd.arg("--max-budget-usd").arg(budget.to_string());
    }
    if let Some(tools) = &config.allowed_tools {
        cmd.arg("--allowed-tools").arg(tools.join(","));
    }
    if let Some(sp) = &config.system_prompt {
        cmd.arg("--append-system-prompt").arg(sp);
    }
    cmd.arg(&config.prompt);
    cmd.current_dir(&config.repo_path);
    cmd.stdout(Stdio::piped());
    cmd.stderr(Stdio::inherit());
    cmd
}

pub async fn execute(
    config: ExecutorConfig,
    event_callback: &(impl Fn(SprintEvent) + Send + Sync),
) -> Result<ExecutorResult, Box<dyn std::error::Error>> {
    let mut cmd = build_command(&config);
    cmd.kill_on_drop(true);
    let mut child = cmd.spawn()?;

    let stdout = child.stdout.take().ok_or("Failed to capture stdout")?;
    let reader = BufReader::new(stdout);
    let mut lines = reader.lines();

    let mut events = Vec::new();
    let mut result_text = None;
    let mut cost_usd = None;
    let mut stop_hook_fired = false;
    let mut last_tool_use = None;
    let mut user_messages: Vec<String> = Vec::new();
    let mut all_assistant_texts: Vec<String> = Vec::new();
    let mut tool_event_counts: HashMap<String, u64> = HashMap::new();
    let heartbeat = Duration::from_secs(config.heartbeat_timeout_secs);

    loop {
        tokio::select! {
            line_result = tokio::time::timeout(heartbeat, lines.next_line()) => {
                match line_result {
                    Ok(Ok(Some(line))) => {
                        if let Some(event) = stream::parse_line(&line) {
                            match &event {
                                StreamEvent::Result { result, total_cost_usd: cost, .. } => {
                                    result_text = Some(result.clone());
                                    cost_usd = *cost;
                                }
                                StreamEvent::System { subtype, .. } => {
                                    if subtype == "stop_hook" { stop_hook_fired = true; }
                                    let se = SprintEvent {
                                        sprint_id: config.session_id,
                                        event_type: if subtype == "stop_hook" {
                                            SprintEventType::StopHookFired
                                        } else {
                                            SprintEventType::Started
                                        },
                                        node_id: config.node_id.clone(),
                                        node_label: config.node_label.clone(),
                                        summary: format!("System: {subtype}"),
                                        detail: None,
                                        cost_usd: None,
                                        timestamp: chrono::Utc::now(),
                                    };
                                    event_callback(se.clone());
                                    events.push(se);
                                }
                                StreamEvent::Assistant { message } => {
                                    for block in &message.content {
                                        // Collect all text blocks for downstream JSON extraction.
                                        // Structured output (JSON arrays/objects) from agents often
                                        // appears in mid-conversation messages, not the final result.
                                        if let stream::ContentBlock::Text { text } = block {
                                            if !text.trim().is_empty() {
                                                all_assistant_texts.push(text.clone());
                                            }
                                        }
                                        // Check for SendUserMessage tool (--brief mode)
                                        if let Some(msg) = stream::extract_user_message(block) {
                                            tracing::info!(message = %msg, "Agent status update (SendUserMessage)");
                                            user_messages.push(msg.clone());
                                            let se = SprintEvent {
                                                sprint_id: config.session_id,
                                                event_type: SprintEventType::AgentMessage,
                                                node_id: config.node_id.clone(),
                                                node_label: config.node_label.clone(),
                                                summary: msg,
                                                detail: None,
                                                cost_usd: None,
                                                timestamp: chrono::Utc::now(),
                                            };
                                            event_callback(se.clone());
                                            events.push(se);
                                        }
                                        if let stream::ContentBlock::ToolUse { name, .. } = block {
                                            last_tool_use = Some(name.clone());
                                            if is_high_frequency_tool(name) {
                                                // Aggregate instead of emitting individually
                                                *tool_event_counts.entry(name.clone()).or_insert(0) += 1;
                                            } else {
                                                let se = SprintEvent {
                                                    sprint_id: config.session_id,
                                                    event_type: SprintEventType::CeremonyStarted,
                                                    node_id: config.node_id.clone(),
                                                    node_label: config.node_label.clone(),
                                                    summary: format!("Tool: {name}"),
                                                    detail: None,
                                                    cost_usd: None,
                                                    timestamp: chrono::Utc::now(),
                                                };
                                                event_callback(se.clone());
                                                events.push(se);
                                            }
                                        }
                                    }
                                }
                            }
                        }
                    }
                    Ok(Ok(None)) => break,
                    Ok(Err(e)) => return Err(e.into()),
                    Err(_) => {
                        tracing::warn!(
                            timeout_secs = config.heartbeat_timeout_secs,
                            "Heartbeat timeout — killing stuck Claude process"
                        );
                        child.kill().await.ok();
                        return Err(format!(
                            "Heartbeat timeout: no output for {}s", config.heartbeat_timeout_secs
                        ).into());
                    }
                }
            }
            _ = tokio::signal::ctrl_c() => {
                tracing::warn!("SIGINT received — killing child process");
                child.kill().await.ok();
                return Err("Interrupted by SIGINT".into());
            }
        }
    }

    let status = child.wait().await?;

    // Emit a single summary event for aggregated high-frequency tool calls
    if !tool_event_counts.is_empty() {
        let mut counts: Vec<_> = tool_event_counts.iter().collect();
        counts.sort_by(|a, b| b.1.cmp(a.1));
        let summary_parts: Vec<String> = counts
            .iter()
            .map(|(tool, count)| format!("{tool}:{count}"))
            .collect();
        let se = SprintEvent {
            sprint_id: config.session_id,
            event_type: SprintEventType::ToolUseSummary,
            node_id: config.node_id.clone(),
            node_label: config.node_label.clone(),
            summary: format!("Tool use: {}", summary_parts.join(", ")),
            detail: Some(serde_json::json!({"tool_counts": &tool_event_counts})),
            cost_usd: None,
            timestamp: chrono::Utc::now(),
        };
        event_callback(se.clone());
        events.push(se);
    }

    Ok(ExecutorResult {
        session_id: config.session_id,
        exit_code: status.code().unwrap_or(-1),
        result_text,
        cost_usd,
        events,
        stop_hook_fired,
        last_tool_use,
        user_messages,
        all_assistant_texts,
        tool_event_counts,
    })
}

/// Returns true if this tool generates high-frequency events that should be
/// aggregated into summaries rather than emitted individually.
pub fn is_high_frequency_tool(name: &str) -> bool {
    HIGH_FREQUENCY_TOOLS.contains(&name)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn event_sink_aggregation() {
        // High-frequency tools should be aggregated (not emitted individually)
        assert!(is_high_frequency_tool("Read"));
        assert!(is_high_frequency_tool("Grep"));
        assert!(is_high_frequency_tool("Glob"));
        assert!(is_high_frequency_tool("Bash"));

        // Low-frequency/high-signal tools should NOT be aggregated
        assert!(!is_high_frequency_tool("Write"));
        assert!(!is_high_frequency_tool("Edit"));
        assert!(!is_high_frequency_tool("Agent"));
        assert!(!is_high_frequency_tool("TodoWrite"));
        assert!(!is_high_frequency_tool("Skill"));

        // Verify counting logic produces correct aggregation
        let mut counts: HashMap<String, u64> = HashMap::new();
        let tool_calls = vec![
            "Read", "Read", "Grep", "Read", "Bash", "Grep", "Read", "Glob", "Read", "Grep", "Bash",
            "Read", "Read", "Glob",
        ];

        for tool in &tool_calls {
            if is_high_frequency_tool(tool) {
                *counts.entry(tool.to_string()).or_insert(0) += 1;
            }
        }

        // All 14 tool calls should be counted (all are high-frequency)
        assert_eq!(counts.get("Read"), Some(&7));
        assert_eq!(counts.get("Grep"), Some(&3));
        assert_eq!(counts.get("Bash"), Some(&2));
        assert_eq!(counts.get("Glob"), Some(&2));

        // Total aggregated count matches total tool calls
        let total: u64 = counts.values().sum();
        assert_eq!(total, tool_calls.len() as u64);

        // Summary would be 1 event instead of 14 individual events
        assert!(counts.len() <= 4); // At most 4 tool categories
    }

    #[test]
    fn event_sink_aggregation_mixed_tools() {
        // Simulate a real session with mixed high/low-frequency tools
        let tool_calls = vec![
            "Read",
            "Read",
            "Grep",
            "Write",
            "Read",
            "Bash",
            "Edit",
            "Glob",
            "Read",
            "Agent",
            "Grep",
            "TodoWrite",
        ];

        let mut aggregated: HashMap<String, u64> = HashMap::new();
        let mut emitted_count = 0u64;

        for tool in &tool_calls {
            if is_high_frequency_tool(tool) {
                *aggregated.entry(tool.to_string()).or_insert(0) += 1;
            } else {
                emitted_count += 1;
            }
        }

        // 8 high-frequency calls aggregated into counts
        let aggregated_total: u64 = aggregated.values().sum();
        assert_eq!(aggregated_total, 8);

        // 4 low-frequency calls emitted individually
        assert_eq!(emitted_count, 4);

        // Event reduction: 1 summary + 4 individual = 5 events instead of 12
        // That's 58% reduction even with mixed tools. In practice, research nodes
        // have >95% high-frequency tools, yielding 90%+ reduction.
        let events_before = tool_calls.len() as u64;
        let events_after = 1 + emitted_count; // 1 summary + individual low-freq
        assert!(events_after < events_before);
    }
}
