//! Builder output parsing.
//!
//! The builder agent produces structured JSON at the end of each session
//! with per-story results: task completion, AC verification, changed files,
//! log entries, and action items. This module parses that output and provides
//! write-back helpers that PATCH story records in the DB.

use serde::{Deserialize, Serialize};

use crate::api_client::ApiClient;
use crate::types::{AcceptanceCriterion, ActionItem, StoryLogEntry, StoryTask};

// ── Builder Output Schema ─────────────────────

/// Top-level builder output — wraps an array of per-story results.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BuilderOutput {
    pub stories: Vec<BuilderStoryResult>,
}

/// Per-story result from the builder session.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BuilderStoryResult {
    /// Original story UUID — must match input story for write-back
    #[serde(default)]
    pub id: String,
    /// Story code (e.g. "ER-049")
    #[serde(default)]
    pub code: Option<String>,
    /// Final status: "done", "blocked", "in_progress"
    #[serde(default)]
    pub status: String,
    /// Reason if status is "blocked"
    #[serde(default)]
    pub blocked_reason: Option<String>,
    /// Task completion states
    #[serde(default)]
    pub tasks: Vec<BuilderTaskResult>,
    /// AC verification states
    #[serde(default)]
    pub acceptance_criteria: Vec<BuilderACResult>,
    /// Files modified during this story
    #[serde(default)]
    pub changed_files: Vec<String>,
    /// Session log entries (summaries suitable for audio playback)
    #[serde(default)]
    pub log_entries: Vec<StoryLogEntry>,
    /// Follow-up action items discovered during implementation
    #[serde(default)]
    pub action_items: Vec<ActionItem>,
    /// Git commit hashes produced for this story
    #[serde(default)]
    pub commit_hashes: Vec<String>,
}

/// Task result from builder — mirrors StoryTask but with completion data.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BuilderTaskResult {
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub done: bool,
    #[serde(default)]
    pub outcome: Option<String>,
}

/// AC result from builder — mirrors AcceptanceCriterion but with verification data.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BuilderACResult {
    #[serde(default)]
    pub criterion: String,
    #[serde(default)]
    pub verified: bool,
    #[serde(default)]
    pub evidence: Option<String>,
}

// ── Parsing ───────────────────────────────────

/// Parse a BuilderOutput from LLM output text.
///
/// Handles: bare JSON, markdown-fenced JSON, JSON with preamble/trailing text.
/// The builder may output a `{"stories": [...]}` object or the inner object
/// may be nested in commentary.
pub fn parse_builder_output(text: Option<&str>) -> Option<BuilderOutput> {
    let text = text?;

    let value = crate::json_extract::extract_json_object(text)?;

    // Try 1: Full BuilderOutput with "stories" array
    if value.get("stories").and_then(|s| s.as_array()).is_some() {
        if let Ok(output) = serde_json::from_value::<BuilderOutput>(value.clone()) {
            return Some(output);
        }
    }

    // Try 2: Single BuilderStoryResult (per-story mode output) — wrap it.
    // Require at least an "id" or "status" field to distinguish from random JSON,
    // since all fields are #[serde(default)].
    if value.get("id").is_some() || value.get("status").is_some() {
        if let Ok(story) = serde_json::from_value::<BuilderStoryResult>(value) {
            return Some(BuilderOutput {
                stories: vec![story],
            });
        }
    }

    None
}

// ── Write-back ────────────────────────────────

/// Write builder results back to story records in the DB.
///
/// For each story in the builder output:
/// - Updates tasks with done/outcome
/// - Updates ACs with verified/evidence
/// - Sets changed_files, log_entries, action_items
/// - Transitions status (done, blocked, in_progress)
///
/// Returns the number of stories successfully patched.
pub async fn write_builder_results_to_stories(
    client: &ApiClient,
    builder_output: &BuilderOutput,
    sprint_session_id: &str,
) -> usize {
    let mut patched = 0usize;

    for story_result in &builder_output.stories {
        let story_id = &story_result.id;

        // Build task array with completion data
        let tasks: Vec<serde_json::Value> = story_result
            .tasks
            .iter()
            .map(|t| {
                serde_json::json!({
                    "description": t.description,
                    "done": t.done,
                    "outcome": t.outcome,
                })
            })
            .collect();

        // Build AC array with verification data
        let acs: Vec<serde_json::Value> = story_result
            .acceptance_criteria
            .iter()
            .map(|ac| {
                serde_json::json!({
                    "criterion": ac.criterion,
                    "verified": ac.verified,
                    "evidence": ac.evidence,
                })
            })
            .collect();

        // Build log entries with session_id injected
        let log_entries: Vec<serde_json::Value> = story_result
            .log_entries
            .iter()
            .map(|le| {
                serde_json::json!({
                    "summary": le.summary,
                    "session_id": le.session_id.as_deref().unwrap_or(sprint_session_id),
                    "created_at": le.created_at.as_deref()
                        .unwrap_or(&chrono::Utc::now().to_rfc3339()),
                })
            })
            .collect();

        // Build PATCH payload
        let mut patch = serde_json::json!({
            "status": &story_result.status,
        });

        let patch_obj = patch.as_object_mut().unwrap();

        if !tasks.is_empty() {
            patch_obj.insert("tasks".to_string(), serde_json::Value::Array(tasks));
        }
        if !acs.is_empty() {
            patch_obj.insert(
                "acceptance_criteria".to_string(),
                serde_json::Value::Array(acs),
            );
        }
        if !story_result.changed_files.is_empty() {
            patch_obj.insert(
                "changed_files".to_string(),
                serde_json::to_value(&story_result.changed_files).unwrap_or_default(),
            );
        }
        if !log_entries.is_empty() {
            patch_obj.insert(
                "log_entries".to_string(),
                serde_json::Value::Array(log_entries),
            );
        }
        if !story_result.action_items.is_empty() {
            patch_obj.insert(
                "action_items".to_string(),
                serde_json::to_value(&story_result.action_items).unwrap_or_default(),
            );
        }
        if let Some(reason) = &story_result.blocked_reason {
            patch_obj.insert(
                "blocked_reason".to_string(),
                serde_json::Value::String(reason.clone()),
            );
        }

        match client
            .patch::<_, serde_json::Value>(&format!("/v1/stories/{}", story_id), &patch)
            .await
        {
            Ok(_) => {
                patched += 1;
                tracing::info!(
                    story_id,
                    status = %story_result.status,
                    tasks_done = story_result.tasks.iter().filter(|t| t.done).count(),
                    acs_verified = story_result.acceptance_criteria.iter().filter(|a| a.verified).count(),
                    "Wrote builder results to story"
                );
            }
            Err(e) => {
                tracing::warn!(story_id, error = %e, "Failed to write builder results to story");
            }
        }
    }

    patched
}

/// Merge builder task results back into story tasks, preserving groomer-provided
/// fields (persona, file, line_hint) while updating builder-provided fields (done, outcome).
pub fn merge_tasks(
    groomed_tasks: &[StoryTask],
    builder_tasks: &[BuilderTaskResult],
) -> Vec<StoryTask> {
    groomed_tasks
        .iter()
        .map(|gt| {
            // Match by description (tasks are ordered, but description is more reliable)
            let builder_match = builder_tasks
                .iter()
                .find(|bt| bt.description == gt.description);

            StoryTask {
                description: gt.description.clone(),
                persona: gt.persona.clone(),
                file: gt.file.clone(),
                line_hint: gt.line_hint.clone(),
                done: builder_match.map(|bt| bt.done).unwrap_or(gt.done),
                outcome: builder_match
                    .and_then(|bt| bt.outcome.clone())
                    .or_else(|| gt.outcome.clone()),
            }
        })
        .collect()
}

/// Merge builder AC results back into story ACs, preserving groomer-provided
/// fields (testable_by, file, line_hint) while updating builder-provided fields (verified, evidence).
pub fn merge_acceptance_criteria(
    groomed_acs: &[AcceptanceCriterion],
    builder_acs: &[BuilderACResult],
) -> Vec<AcceptanceCriterion> {
    groomed_acs
        .iter()
        .map(|gac| {
            let builder_match = builder_acs
                .iter()
                .find(|bac| bac.criterion == gac.display_text());

            AcceptanceCriterion {
                criterion: gac.criterion.clone(),
                title: gac.title.clone(),
                given: gac.given.clone(),
                when: gac.when.clone(),
                then: gac.then.clone(),
                testable_by: gac.testable_by.clone(),
                file: gac.file.clone(),
                line_hint: gac.line_hint.clone(),
                verified: builder_match
                    .map(|bac| bac.verified)
                    .unwrap_or(gac.verified),
                evidence: builder_match
                    .and_then(|bac| bac.evidence.clone())
                    .or_else(|| gac.evidence.clone()),
            }
        })
        .collect()
}

// ── Tests ─────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_builder_output_bare_json() {
        let json = r#"{
            "stories": [{
                "id": "387778ca-59f3-4e18-8f59-13c464faaf2e",
                "code": "ER-049",
                "status": "done",
                "tasks": [
                    {"description": "Add detect_default_branch", "done": true, "outcome": "Added to engine.rs"}
                ],
                "acceptance_criteria": [
                    {"criterion": "No hardcoded main refs", "verified": true, "evidence": "grep returns 0 matches"}
                ],
                "changed_files": ["src/flow/engine.rs"],
                "log_entries": [{"summary": "Implemented branch detection"}],
                "action_items": [],
                "commit_hashes": ["119e3af"]
            }]
        }"#;

        let output = parse_builder_output(Some(json)).unwrap();
        assert_eq!(output.stories.len(), 1);
        assert_eq!(output.stories[0].status, "done");
        assert!(output.stories[0].tasks[0].done);
        assert!(output.stories[0].acceptance_criteria[0].verified);
        assert_eq!(output.stories[0].changed_files, vec!["src/flow/engine.rs"]);
    }

    #[test]
    fn parse_builder_output_fenced() {
        let text = "Here's my work:\n\n```json\n{\"stories\":[{\"id\":\"abc\",\"status\":\"done\",\"tasks\":[],\"acceptance_criteria\":[]}]}\n```\n\nAll done!";
        let output = parse_builder_output(Some(text)).unwrap();
        assert_eq!(output.stories[0].id, "abc");
    }

    #[test]
    fn parse_builder_output_blocked_story() {
        let json = r#"{"stories":[{
            "id": "test-uuid",
            "status": "blocked",
            "blocked_reason": "Needs auth endpoint deployed first",
            "tasks": [{"description": "Wire up auth", "done": false}],
            "acceptance_criteria": []
        }]}"#;

        let output = parse_builder_output(Some(json)).unwrap();
        assert_eq!(output.stories[0].status, "blocked");
        assert_eq!(
            output.stories[0].blocked_reason.as_deref(),
            Some("Needs auth endpoint deployed first")
        );
        assert!(!output.stories[0].tasks[0].done);
    }

    #[test]
    fn parse_returns_none_for_no_stories_key() {
        let json = r#"{"went_well": ["fast"]}"#;
        assert!(parse_builder_output(Some(json)).is_none());
    }

    #[test]
    fn parse_returns_none_for_garbage() {
        assert!(parse_builder_output(Some("not json")).is_none());
    }

    #[test]
    fn parse_returns_none_for_none() {
        assert!(parse_builder_output(None).is_none());
    }

    #[test]
    fn merge_tasks_preserves_groomer_fields() {
        let groomed = vec![StoryTask {
            description: "Add function".to_string(),
            persona: "backend-engineer".to_string(),
            file: Some("src/lib.rs".to_string()),
            line_hint: Some("42".to_string()),
            done: false,
            outcome: None,
        }];
        let builder = vec![BuilderTaskResult {
            description: "Add function".to_string(),
            done: true,
            outcome: Some("Added to lib.rs line 42".to_string()),
        }];

        let merged = merge_tasks(&groomed, &builder);
        assert_eq!(merged.len(), 1);
        assert!(merged[0].done);
        assert_eq!(merged[0].persona, "backend-engineer");
        assert_eq!(merged[0].file.as_deref(), Some("src/lib.rs"));
        assert_eq!(
            merged[0].outcome.as_deref(),
            Some("Added to lib.rs line 42")
        );
    }

    #[test]
    fn merge_acs_preserves_groomer_fields() {
        let groomed = vec![AcceptanceCriterion {
            criterion: "Tests pass".to_string(),
            title: None,
            given: None,
            when: None,
            then: None,
            testable_by: Some("cargo test".to_string()),
            file: Some("src/lib.rs".to_string()),
            line_hint: None,
            verified: false,
            evidence: None,
        }];
        let builder = vec![BuilderACResult {
            criterion: "Tests pass".to_string(),
            verified: true,
            evidence: Some("86 tests passing".to_string()),
        }];

        let merged = merge_acceptance_criteria(&groomed, &builder);
        assert_eq!(merged.len(), 1);
        assert!(merged[0].verified);
        assert_eq!(merged[0].testable_by.as_deref(), Some("cargo test"));
        assert_eq!(merged[0].evidence.as_deref(), Some("86 tests passing"));
    }

    #[test]
    fn merge_tasks_unmatched_keeps_groomer_state() {
        let groomed = vec![StoryTask {
            description: "Original task".to_string(),
            persona: "backend-engineer".to_string(),
            file: None,
            line_hint: None,
            done: false,
            outcome: None,
        }];
        // Builder reported a different description — no match
        let builder = vec![BuilderTaskResult {
            description: "Different task".to_string(),
            done: true,
            outcome: Some("Done".to_string()),
        }];

        let merged = merge_tasks(&groomed, &builder);
        assert!(!merged[0].done); // Stays false — no match
    }

    #[test]
    fn multi_story_output() {
        let json = r#"{"stories":[
            {"id":"a","status":"done","tasks":[],"acceptance_criteria":[],"changed_files":["a.rs"]},
            {"id":"b","status":"blocked","blocked_reason":"needs a","tasks":[],"acceptance_criteria":[]}
        ]}"#;

        let output = parse_builder_output(Some(json)).unwrap();
        assert_eq!(output.stories.len(), 2);
        assert_eq!(output.stories[0].status, "done");
        assert_eq!(output.stories[1].status, "blocked");
    }
}
