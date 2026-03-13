//! Integration tests for the write-back pipeline.
//!
//! These tests verify that `write_builder_results_to_stories()` sends the correct
//! PATCH requests to the API, using wiremock to capture and assert on payloads.

use epic_runner::api_client::ApiClient;
use epic_runner::builder::{BuilderACResult, BuilderOutput, BuilderStoryResult, BuilderTaskResult};
use epic_runner::types::StoryLogEntry;
use serde_json::json;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

/// Helper: create an ApiClient pointed at the mock server.
fn test_client(server: &MockServer) -> ApiClient {
    ApiClient::new(&server.uri(), "sk_test_writeback")
}

// ── AC 1: Valid builder output → correct PATCH payload ──────────

#[tokio::test]
async fn write_back_integration_valid_output() {
    let server = MockServer::start().await;
    let story_id = "387778ca-59f3-4e18-8f59-13c464faaf2e";

    Mock::given(method("PATCH"))
        .and(path(format!("/v1/stories/{story_id}")))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(json!({"id": story_id, "status": "done"})),
        )
        .expect(1)
        .mount(&server)
        .await;

    let client = test_client(&server);

    let output = BuilderOutput {
        stories: vec![BuilderStoryResult {
            id: story_id.to_string(),
            code: Some("ER-049".to_string()),
            status: "done".to_string(),
            blocked_reason: None,
            tasks: vec![
                BuilderTaskResult {
                    description: "Add detect_default_branch".to_string(),
                    done: true,
                    outcome: Some("Added to engine.rs".to_string()),
                },
                BuilderTaskResult {
                    description: "Write unit test".to_string(),
                    done: true,
                    outcome: None,
                },
            ],
            acceptance_criteria: vec![BuilderACResult {
                criterion: "No hardcoded main refs".to_string(),
                verified: true,
                evidence: Some("grep returns 0 matches".to_string()),
            }],
            changed_files: vec!["src/flow/engine.rs".to_string(), "src/lib.rs".to_string()],
            log_entries: vec![StoryLogEntry {
                summary: "Implemented branch detection".to_string(),
                session_id: None,
                sprint_id: None,
                created_at: None,
            }],
            action_items: vec![],
            commit_hashes: vec!["119e3af".to_string()],
        }],
    };

    let patched = epic_runner::builder::write_builder_results_to_stories(
        &client,
        &output,
        "sprint-session-1",
    )
    .await;

    assert_eq!(patched, 1, "Expected 1 story to be patched");

    // Verify the PATCH request payload shape
    let requests = server.received_requests().await.unwrap();
    assert_eq!(requests.len(), 1);

    let body: serde_json::Value = serde_json::from_slice(&requests[0].body).unwrap();

    // Status field
    assert_eq!(body["status"], "done");

    // Tasks array with done/outcome
    let tasks = body["tasks"].as_array().expect("tasks should be an array");
    assert_eq!(tasks.len(), 2);
    assert_eq!(tasks[0]["description"], "Add detect_default_branch");
    assert_eq!(tasks[0]["done"], true);
    assert_eq!(tasks[0]["outcome"], "Added to engine.rs");
    assert_eq!(tasks[1]["description"], "Write unit test");
    assert_eq!(tasks[1]["done"], true);

    // Acceptance criteria with verified/evidence
    let acs = body["acceptance_criteria"]
        .as_array()
        .expect("acceptance_criteria should be an array");
    assert_eq!(acs.len(), 1);
    assert_eq!(acs[0]["criterion"], "No hardcoded main refs");
    assert_eq!(acs[0]["verified"], true);
    assert_eq!(acs[0]["evidence"], "grep returns 0 matches");

    // Changed files
    let files = body["changed_files"]
        .as_array()
        .expect("changed_files should be an array");
    assert_eq!(files.len(), 2);
    assert_eq!(files[0], "src/flow/engine.rs");
    assert_eq!(files[1], "src/lib.rs");

    // Log entries with session_id injected
    let logs = body["log_entries"]
        .as_array()
        .expect("log_entries should be an array");
    assert_eq!(logs.len(), 1);
    assert_eq!(logs[0]["summary"], "Implemented branch detection");
    assert_eq!(
        logs[0]["session_id"], "sprint-session-1",
        "session_id should be injected from sprint_session_id arg"
    );

    // blocked_reason should NOT be present for a "done" story
    assert!(
        body.get("blocked_reason").is_none(),
        "blocked_reason should not be in payload for done stories"
    );
}

// ── AC 3: Empty story ID → graceful failure ─────────────────────

#[tokio::test]
async fn write_back_missing_id_graceful_failure() {
    let server = MockServer::start().await;

    let valid_id = "11111111-1111-1111-1111-111111111111";
    let empty_id = "";

    // Only register a mock for the valid story — the empty-ID PATCH will 404
    Mock::given(method("PATCH"))
        .and(path(format!("/v1/stories/{valid_id}")))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(json!({"id": valid_id, "status": "done"})),
        )
        .expect(1)
        .mount(&server)
        .await;

    let client = test_client(&server);

    let output = BuilderOutput {
        stories: vec![
            // Story with empty ID — should fail gracefully
            BuilderStoryResult {
                id: empty_id.to_string(),
                code: Some("ER-BAD".to_string()),
                status: "done".to_string(),
                blocked_reason: None,
                tasks: vec![BuilderTaskResult {
                    description: "Some task".to_string(),
                    done: true,
                    outcome: None,
                }],
                acceptance_criteria: vec![],
                changed_files: vec![],
                log_entries: vec![],
                action_items: vec![],
                commit_hashes: vec![],
            },
            // Valid story — should still succeed
            BuilderStoryResult {
                id: valid_id.to_string(),
                code: Some("ER-OK".to_string()),
                status: "done".to_string(),
                blocked_reason: None,
                tasks: vec![],
                acceptance_criteria: vec![],
                changed_files: vec![],
                log_entries: vec![],
                action_items: vec![],
                commit_hashes: vec![],
            },
        ],
    };

    let patched =
        epic_runner::builder::write_builder_results_to_stories(&client, &output, "session-2").await;

    // Only the valid story should succeed; empty-ID story fails gracefully
    assert_eq!(
        patched, 1,
        "Only 1 of 2 stories should be patched (empty ID fails gracefully)"
    );
}

// ── AC 4: Blocked story includes blocked_reason ─────────────────

#[tokio::test]
async fn write_back_blocked_story_includes_reason() {
    let server = MockServer::start().await;
    let story_id = "22222222-2222-2222-2222-222222222222";

    Mock::given(method("PATCH"))
        .and(path(format!("/v1/stories/{story_id}")))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(json!({"id": story_id, "status": "blocked"})),
        )
        .expect(1)
        .mount(&server)
        .await;

    let client = test_client(&server);

    let output = BuilderOutput {
        stories: vec![BuilderStoryResult {
            id: story_id.to_string(),
            code: Some("ER-BLOCKED".to_string()),
            status: "blocked".to_string(),
            blocked_reason: Some("Needs auth endpoint deployed first".to_string()),
            tasks: vec![BuilderTaskResult {
                description: "Wire up auth".to_string(),
                done: false,
                outcome: None,
            }],
            acceptance_criteria: vec![BuilderACResult {
                criterion: "Auth flow works end to end".to_string(),
                verified: false,
                evidence: None,
            }],
            changed_files: vec![],
            log_entries: vec![],
            action_items: vec![],
            commit_hashes: vec![],
        }],
    };

    let patched =
        epic_runner::builder::write_builder_results_to_stories(&client, &output, "session-3").await;

    assert_eq!(patched, 1);

    let requests = server.received_requests().await.unwrap();
    let body: serde_json::Value = serde_json::from_slice(&requests[0].body).unwrap();

    // Status should be blocked
    assert_eq!(body["status"], "blocked");

    // blocked_reason MUST be present
    assert_eq!(
        body["blocked_reason"], "Needs auth endpoint deployed first",
        "blocked_reason must be persisted in PATCH payload"
    );

    // Tasks should reflect incomplete state
    let tasks = body["tasks"].as_array().expect("tasks should be an array");
    assert_eq!(tasks[0]["done"], false);

    // ACs should reflect unverified state
    let acs = body["acceptance_criteria"]
        .as_array()
        .expect("acceptance_criteria should be an array");
    assert_eq!(acs[0]["verified"], false);
    assert!(
        acs[0]["evidence"].is_null(),
        "evidence should be null for unverified ACs"
    );
}

// ── Multi-story batch: mix of done, blocked, and empty-ID ───────

#[tokio::test]
async fn write_back_integration_batch_mixed_statuses() {
    let server = MockServer::start().await;

    let done_id = "aaaa1111-1111-1111-1111-111111111111";
    let blocked_id = "bbbb2222-2222-2222-2222-222222222222";

    Mock::given(method("PATCH"))
        .and(path(format!("/v1/stories/{done_id}")))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(json!({"id": done_id, "status": "done"})),
        )
        .expect(1)
        .mount(&server)
        .await;

    Mock::given(method("PATCH"))
        .and(path(format!("/v1/stories/{blocked_id}")))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(json!({"id": blocked_id, "status": "blocked"})),
        )
        .expect(1)
        .mount(&server)
        .await;

    let client = test_client(&server);

    let output = BuilderOutput {
        stories: vec![
            BuilderStoryResult {
                id: done_id.to_string(),
                code: Some("ER-001".to_string()),
                status: "done".to_string(),
                blocked_reason: None,
                tasks: vec![BuilderTaskResult {
                    description: "Task A".to_string(),
                    done: true,
                    outcome: Some("Completed".to_string()),
                }],
                acceptance_criteria: vec![],
                changed_files: vec!["a.rs".to_string()],
                log_entries: vec![],
                action_items: vec![],
                commit_hashes: vec![],
            },
            // Empty-ID story — fails gracefully
            BuilderStoryResult {
                id: String::new(),
                code: None,
                status: "done".to_string(),
                blocked_reason: None,
                tasks: vec![],
                acceptance_criteria: vec![],
                changed_files: vec![],
                log_entries: vec![],
                action_items: vec![],
                commit_hashes: vec![],
            },
            BuilderStoryResult {
                id: blocked_id.to_string(),
                code: Some("ER-003".to_string()),
                status: "blocked".to_string(),
                blocked_reason: Some("Dependency not ready".to_string()),
                tasks: vec![],
                acceptance_criteria: vec![],
                changed_files: vec![],
                log_entries: vec![],
                action_items: vec![],
                commit_hashes: vec![],
            },
        ],
    };

    let patched =
        epic_runner::builder::write_builder_results_to_stories(&client, &output, "session-batch")
            .await;

    // 2 out of 3 should succeed (empty-ID fails)
    assert_eq!(patched, 2, "2 of 3 stories should be patched successfully");
}
