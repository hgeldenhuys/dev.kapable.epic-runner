use crate::executor::{self, ExecutorConfig, ExecutorResult};
use crate::types::*;

pub struct SupervisorConfig {
    pub max_stop_hooks: i32,
    pub rubber_duck_after: i32,
    pub auto_abort_on_same_error: bool,
}

pub struct SupervisedResult {
    pub executor_result: ExecutorResult,
    pub decisions: Vec<SupervisorDecision>,
    pub rubber_duck_sessions: Vec<RubberDuckSession>,
    pub impediment_raised: Option<String>,
    pub total_stop_hooks: i32,
}

/// Supervise execution with stop-hook loop, rubber duck dispatch, and decision engine.
///
/// Flow:
/// 1. Execute → if natural exit (code 0, no stop hook) → Complete
/// 2. Stop hook fired → evaluate: resume, rubber duck, or abort
/// 3. After `rubber_duck_after` stop hooks → invoke rubber duck agent
/// 4. After `max_stop_hooks` → abort with impediment
/// 5. If "blocked by" detected in output → raise impediment immediately
pub async fn supervise(
    config: ExecutorConfig,
    sup_config: SupervisorConfig,
    event_callback: &(impl Fn(SprintEvent) + Send + Sync),
) -> Result<SupervisedResult, Box<dyn std::error::Error>> {
    let mut decisions = Vec::new();
    let mut rubber_duck_sessions = Vec::new();
    let mut impediment_raised = None;
    let mut stop_hook_count = 0;
    let mut current_config = config;
    let mut last_error: Option<String> = None;

    loop {
        let result = executor::execute(current_config.clone(), event_callback).await?;

        // Check for blocker text
        if let Some(ref text) = result.result_text {
            if let Some(blocker) = extract_blocker(text) {
                impediment_raised = Some(blocker.clone());
                let decision = SupervisorDecision {
                    sprint_id: result.session_id,
                    stop_hook_count,
                    decision: SupervisorAction::RaiseImpediment,
                    reasoning: format!("Blocked by: {blocker}"),
                    rubber_duck_insights: None,
                    timestamp: chrono::Utc::now(),
                };
                decisions.push(decision);
                return Ok(SupervisedResult {
                    executor_result: result,
                    decisions,
                    rubber_duck_sessions,
                    impediment_raised,
                    total_stop_hooks: stop_hook_count,
                });
            }
        }

        // Natural completion
        if !result.stop_hook_fired && result.exit_code == 0 {
            let decision = SupervisorDecision {
                sprint_id: result.session_id,
                stop_hook_count,
                decision: SupervisorAction::Complete,
                reasoning: "Natural completion — exit code 0, no stop hook".to_string(),
                rubber_duck_insights: None,
                timestamp: chrono::Utc::now(),
            };
            decisions.push(decision);
            return Ok(SupervisedResult {
                executor_result: result,
                decisions,
                rubber_duck_sessions,
                impediment_raised,
                total_stop_hooks: stop_hook_count,
            });
        }

        stop_hook_count += 1;

        // Check for same error (auto-abort)
        if sup_config.auto_abort_on_same_error {
            let current_error = result.last_tool_use.clone();
            if current_error == last_error && last_error.is_some() {
                let decision = SupervisorDecision {
                    sprint_id: result.session_id,
                    stop_hook_count,
                    decision: SupervisorAction::Abort,
                    reasoning: "Same error repeated — aborting to prevent loop".to_string(),
                    rubber_duck_insights: None,
                    timestamp: chrono::Utc::now(),
                };
                decisions.push(decision);
                return Ok(SupervisedResult {
                    executor_result: result,
                    decisions,
                    rubber_duck_sessions,
                    impediment_raised,
                    total_stop_hooks: stop_hook_count,
                });
            }
            last_error = current_error;
        }

        // Max stop hooks reached
        if stop_hook_count >= sup_config.max_stop_hooks {
            let decision = SupervisorDecision {
                sprint_id: result.session_id,
                stop_hook_count,
                decision: SupervisorAction::Abort,
                reasoning: format!(
                    "Max stop hooks reached ({}/{})",
                    stop_hook_count, sup_config.max_stop_hooks
                ),
                rubber_duck_insights: None,
                timestamp: chrono::Utc::now(),
            };
            decisions.push(decision);
            return Ok(SupervisedResult {
                executor_result: result,
                decisions,
                rubber_duck_sessions,
                impediment_raised,
                total_stop_hooks: stop_hook_count,
            });
        }

        // Rubber duck threshold
        if stop_hook_count >= sup_config.rubber_duck_after {
            tracing::warn!(
                stop_hook_count,
                max = sup_config.max_stop_hooks,
                "Stop hook threshold — invoking rubber duck"
            );
            let duck = invoke_rubber_duck(&result).await;
            let insights = duck.as_ref().map(|d| d.insights.join("; "));
            rubber_duck_sessions.extend(duck);

            let decision = SupervisorDecision {
                sprint_id: result.session_id,
                stop_hook_count,
                decision: SupervisorAction::ResumeWithRubberDuck,
                reasoning: format!("Stop hook #{stop_hook_count} — rubber duck invoked"),
                rubber_duck_insights: insights.clone(),
                timestamp: chrono::Utc::now(),
            };
            decisions.push(decision);

            // Resume with rubber duck insights
            current_config.resume_session = true;
            current_config.prompt = format!(
                "Resume. Rubber duck insights: {}",
                insights.unwrap_or_else(|| "none".to_string())
            );
        } else {
            tracing::info!(
                stop_hook_count,
                max = sup_config.max_stop_hooks,
                "Stop hook — resuming"
            );
            let decision = SupervisorDecision {
                sprint_id: result.session_id,
                stop_hook_count,
                decision: SupervisorAction::Resume,
                reasoning: format!("Stop hook #{stop_hook_count} — simple resume"),
                rubber_duck_insights: None,
                timestamp: chrono::Utc::now(),
            };
            decisions.push(decision);

            current_config.resume_session = true;
            current_config.prompt = "Resume. Continue where you left off.".to_string();
        }
    }
}

/// Extract blocker epic codes from output text.
/// Recognizes multiple patterns LLMs commonly use:
///   - "blocked by AUTH-001"
///   - "depends on DATA-003"
///   - "waiting for INFRA-002"
///
/// Returns the first match (primary blocker for impediment tracking).
fn extract_blocker(text: &str) -> Option<String> {
    let blockers = extract_all_blockers(text);
    blockers.into_iter().next()
}

/// Extract ALL blocker epic codes from output text.
/// Matches patterns: "blocked by X", "depends on X", "waiting for X"
/// where X looks like an epic code (uppercase letters + digits + hyphens).
fn extract_all_blockers(text: &str) -> Vec<String> {
    let lower = text.to_lowercase();
    let patterns = ["blocked by ", "depends on ", "waiting for "];
    let mut found = Vec::new();
    let mut seen = std::collections::HashSet::new();

    for pattern in &patterns {
        let mut search_from = 0;
        while let Some(pos) = lower[search_from..].find(pattern) {
            let abs_pos = search_from + pos + pattern.len();
            let rest = &text[abs_pos..];
            let code: String = rest
                .chars()
                .take_while(|c| c.is_alphanumeric() || *c == '-' || *c == '_')
                .collect();
            // Only accept codes that look like epic codes (contain a hyphen, start with letters)
            if !code.is_empty()
                && code.contains('-')
                && code.chars().next().is_some_and(|c| c.is_alphabetic())
                && seen.insert(code.clone())
            {
                found.push(code);
            }
            search_from = abs_pos;
        }
    }
    found
}

/// Invoke rubber duck agent to diagnose stuck state.
async fn invoke_rubber_duck(result: &ExecutorResult) -> Option<RubberDuckSession> {
    let duck_config = ExecutorConfig {
        model: "haiku".to_string(),
        effort: "low".to_string(),
        worktree_name: String::new(),
        session_id: uuid::Uuid::new_v4(),
        repo_path: ".".to_string(),
        add_dirs: vec![],
        system_prompt: None,
        prompt: format!(
            "The build agent stopped. Last tool: {:?}. Exit code: {}. Diagnose.",
            result.last_tool_use, result.exit_code
        ),
        chrome: false,
        max_budget_usd: Some(0.5),
        allowed_tools: Some(vec![
            "Read".to_string(),
            "Glob".to_string(),
            "Grep".to_string(),
            "Bash".to_string(),
        ]),
        resume_session: false,
        agent: Some("rubber-duck".to_string()),
        heartbeat_timeout_secs: 120,
    };

    match executor::execute(duck_config, &|_| {}).await {
        Ok(duck_result) => Some(RubberDuckSession {
            sprint_id: result.session_id,
            trigger_reason: "Stop hook threshold reached".to_string(),
            stuck_state_summary: format!(
                "Exit: {}, Last tool: {:?}",
                result.exit_code, result.last_tool_use
            ),
            insights: duck_result
                .result_text
                .map(|t| {
                    t.lines()
                        .filter(|l| l.starts_with("- ") || l.starts_with("* "))
                        .map(String::from)
                        .collect()
                })
                .unwrap_or_default(),
            recommended_action: SupervisorAction::Resume,
            cost_usd: duck_result.cost_usd,
            timestamp: chrono::Utc::now(),
        }),
        Err(e) => {
            tracing::error!(error = %e, "Rubber duck invocation failed");
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extract_blocker_finds_epic_code() {
        assert_eq!(
            extract_blocker("I'm blocked by AUTH-001 and can't proceed"),
            Some("AUTH-001".to_string())
        );
    }

    #[test]
    fn extract_blocker_returns_none_for_no_match() {
        assert_eq!(extract_blocker("Everything is fine"), None);
    }

    #[test]
    fn extract_blocker_case_insensitive() {
        assert_eq!(
            extract_blocker("Blocked By DATA-003"),
            Some("DATA-003".to_string())
        );
    }

    #[test]
    fn extract_blocker_depends_on_pattern() {
        assert_eq!(
            extract_blocker("This depends on INFRA-002 being deployed first"),
            Some("INFRA-002".to_string())
        );
    }

    #[test]
    fn extract_blocker_waiting_for_pattern() {
        assert_eq!(
            extract_blocker("I'm waiting for SEC-005 to land"),
            Some("SEC-005".to_string())
        );
    }

    #[test]
    fn extract_blocker_ignores_non_epic_codes() {
        // "blocked by bugs" shouldn't match — no hyphen, doesn't look like an epic code
        assert_eq!(extract_blocker("I'm blocked by bugs in the code"), None);
    }

    #[test]
    fn extract_all_blockers_finds_multiple() {
        let text = "blocked by AUTH-001, also depends on DATA-003 and waiting for SEC-005";
        let blockers = extract_all_blockers(text);
        assert_eq!(blockers.len(), 3);
        assert!(blockers.contains(&"AUTH-001".to_string()));
        assert!(blockers.contains(&"DATA-003".to_string()));
        assert!(blockers.contains(&"SEC-005".to_string()));
    }

    #[test]
    fn extract_all_blockers_deduplicates() {
        let text = "blocked by AUTH-001 and also blocked by AUTH-001 again";
        let blockers = extract_all_blockers(text);
        assert_eq!(blockers.len(), 1);
    }
}
