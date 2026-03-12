use std::process::Stdio;
use std::time::Duration;
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::Command;
use uuid::Uuid;

use crate::agents;
use crate::stream::{self, StreamEvent};
use crate::types::{SprintEvent, SprintEventType};

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
        cmd.arg("--system-prompt").arg(sp);
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
                                        timestamp: chrono::Utc::now(),
                                    };
                                    event_callback(se.clone());
                                    events.push(se);
                                }
                                StreamEvent::Assistant { message } => {
                                    for block in &message.content {
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
                                                timestamp: chrono::Utc::now(),
                                            };
                                            event_callback(se.clone());
                                            events.push(se);
                                        }
                                        if let stream::ContentBlock::ToolUse { name, .. } = block {
                                            last_tool_use = Some(name.clone());
                                            let se = SprintEvent {
                                                sprint_id: config.session_id,
                                                event_type: SprintEventType::CeremonyStarted,
                                                node_id: config.node_id.clone(),
                                                node_label: config.node_label.clone(),
                                                summary: format!("Tool: {name}"),
                                                detail: None,
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

    Ok(ExecutorResult {
        session_id: config.session_id,
        exit_code: status.code().unwrap_or(-1),
        result_text,
        cost_usd,
        events,
        stop_hook_fired,
        last_tool_use,
        user_messages,
    })
}
