use std::io::Write;

#[test]
fn read_config_parses_full_toml() {
    let mut tmp = tempfile::NamedTempFile::new().unwrap();
    writeln!(
        tmp,
        r#"
[api]
base_url = "https://example.com"
api_key = "sk_test_123"

[project]
project_id = "abc-123"
product = "clowder"
ceremony_flow_id = "flow-uuid-here"
"#
    )
    .unwrap();
    let content = std::fs::read_to_string(tmp.path()).unwrap();
    let config: epic_runner::config::EpicRunnerConfig = toml::from_str(&content).unwrap();
    assert_eq!(config.base_url(), Some("https://example.com"));
    assert_eq!(config.api_key(), Some("sk_test_123"));
    assert_eq!(config.project_id(), Some("abc-123"));
    assert_eq!(config.ceremony_flow_id(), Some("flow-uuid-here"));
}

#[test]
fn read_config_handles_minimal_toml() {
    let mut tmp = tempfile::NamedTempFile::new().unwrap();
    writeln!(
        tmp,
        r#"
[api]
api_key = "sk_min"
"#
    )
    .unwrap();
    let content = std::fs::read_to_string(tmp.path()).unwrap();
    let config: epic_runner::config::EpicRunnerConfig = toml::from_str(&content).unwrap();
    assert_eq!(config.api_key(), Some("sk_min"));
    assert_eq!(config.base_url(), None);
    assert_eq!(config.project_id(), None);
}

#[test]
fn empty_config_has_none_values() {
    let config = epic_runner::config::EpicRunnerConfig::default();
    assert_eq!(config.api_key(), None);
    assert_eq!(config.base_url(), None);
    assert_eq!(config.project_id(), None);
    assert_eq!(config.ceremony_flow_id(), None);
}

// ── Mock HTTP Server Tests ───────────────────────────

use epic_runner::api_client::{ApiClient, ApiError, DataWrapper};
use serde_json::json;
use wiremock::matchers::{header, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

/// Helper: create an ApiClient pointed at the mock server.
fn test_client(server: &MockServer) -> ApiClient {
    ApiClient::new(&server.uri(), "sk_test_abc123")
}

// ── GET tests ─────────────────────────────────────────

#[tokio::test]
async fn get_returns_parsed_json() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/v1/epics"))
        .and(header("x-api-key", "sk_test_abc123"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "data": [{"id": "abc-123", "code": "TEST-001", "title": "Test Epic"}]
        })))
        .mount(&server)
        .await;

    let client = test_client(&server);
    let resp: DataWrapper<Vec<serde_json::Value>> = client.get("/v1/epics").await.unwrap();
    assert_eq!(resp.data.len(), 1);
    assert_eq!(resp.data[0]["code"], "TEST-001");
}

#[tokio::test]
async fn get_404_returns_not_found_error() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/v1/missing"))
        .respond_with(ResponseTemplate::new(404).set_body_string("table not found"))
        .mount(&server)
        .await;

    let client = test_client(&server);
    let result: Result<serde_json::Value, ApiError> = client.get("/v1/missing").await;
    assert!(matches!(result, Err(ApiError::NotFound(_))));
}

#[tokio::test]
async fn get_401_returns_auth_error() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/v1/epics"))
        .respond_with(ResponseTemplate::new(401).set_body_string("invalid key"))
        .mount(&server)
        .await;

    let client = test_client(&server);
    let result: Result<serde_json::Value, ApiError> = client.get("/v1/epics").await;
    assert!(matches!(result, Err(ApiError::Auth(_))));
}

#[tokio::test]
async fn get_403_returns_auth_error() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/v1/epics"))
        .respond_with(ResponseTemplate::new(403).set_body_string("forbidden"))
        .mount(&server)
        .await;

    let client = test_client(&server);
    let result: Result<serde_json::Value, ApiError> = client.get("/v1/epics").await;
    assert!(matches!(result, Err(ApiError::Auth(_))));
}

#[tokio::test]
async fn get_422_returns_validation_error() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/v1/epics"))
        .respond_with(ResponseTemplate::new(422).set_body_string("bad input"))
        .mount(&server)
        .await;

    let client = test_client(&server);
    let result: Result<serde_json::Value, ApiError> = client.get("/v1/epics").await;
    assert!(matches!(result, Err(ApiError::Validation(_))));
}

// ── POST tests ────────────────────────────────────────

#[tokio::test]
async fn post_sends_json_body() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/stories"))
        .and(header("content-type", "application/json"))
        .and(header("x-api-key", "sk_test_abc123"))
        .respond_with(ResponseTemplate::new(201).set_body_json(json!({
            "id": "new-123",
            "title": "New Story"
        })))
        .mount(&server)
        .await;

    let client = test_client(&server);
    let body = json!({"title": "New Story", "epic_code": "TEST-001"});
    let resp: serde_json::Value = client.post("/v1/stories", &body).await.unwrap();
    assert_eq!(resp["id"], "new-123");
}

// ── PATCH tests ───────────────────────────────────────

#[tokio::test]
async fn patch_sends_json_body() {
    let server = MockServer::start().await;
    Mock::given(method("PATCH"))
        .and(path("/v1/stories/abc-123"))
        .and(header("content-type", "application/json"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "id": "abc-123",
            "status": "done"
        })))
        .mount(&server)
        .await;

    let client = test_client(&server);
    let body = json!({"status": "done"});
    let resp: serde_json::Value = client.patch("/v1/stories/abc-123", &body).await.unwrap();
    assert_eq!(resp["status"], "done");
}

// ── DELETE tests ──────────────────────────────────────

#[tokio::test]
async fn delete_returns_ok_on_success() {
    let server = MockServer::start().await;
    Mock::given(method("DELETE"))
        .and(path("/v1/stories/abc-123"))
        .respond_with(ResponseTemplate::new(204))
        .mount(&server)
        .await;

    let client = test_client(&server);
    client.delete("/v1/stories/abc-123").await.unwrap();
}

#[tokio::test]
async fn delete_returns_error_on_404() {
    let server = MockServer::start().await;
    Mock::given(method("DELETE"))
        .and(path("/v1/stories/missing"))
        .respond_with(ResponseTemplate::new(404).set_body_string("not found"))
        .mount(&server)
        .await;

    let client = test_client(&server);
    let result = client.delete("/v1/stories/missing").await;
    assert!(matches!(result, Err(ApiError::NotFound(_))));
}

// ── resolve_id tests ──────────────────────────────────

#[tokio::test]
async fn resolve_id_passes_through_full_uuid() {
    let server = MockServer::start().await;
    let client = test_client(&server);

    let result = client
        .resolve_id("stories", "472bddf1-9b9d-41ab-9b23-c8dade2ff26b")
        .await
        .unwrap();
    assert_eq!(result, "472bddf1-9b9d-41ab-9b23-c8dade2ff26b");
}

#[tokio::test]
async fn resolve_id_matches_short_prefix() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/v1/stories"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "data": [
                {"id": "472bddf1-9b9d-41ab-9b23-c8dade2ff26b", "title": "Story A"},
                {"id": "86ec8ded-5ec9-47b9-9761-ea0f66b09a66", "title": "Story B"}
            ]
        })))
        .mount(&server)
        .await;

    let client = test_client(&server);
    let result = client.resolve_id("stories", "472bddf1").await.unwrap();
    assert_eq!(result, "472bddf1-9b9d-41ab-9b23-c8dade2ff26b");
}

#[tokio::test]
async fn resolve_id_errors_on_no_match() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/v1/stories"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "data": [{"id": "abc-123"}]
        })))
        .mount(&server)
        .await;

    let client = test_client(&server);
    let result = client.resolve_id("stories", "zzz").await;
    assert!(matches!(result, Err(ApiError::NotFound(_))));
}

#[tokio::test]
async fn resolve_id_errors_on_ambiguous_prefix() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/v1/stories"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "data": [
                {"id": "aaa-111-xxx"},
                {"id": "aaa-222-yyy"}
            ]
        })))
        .mount(&server)
        .await;

    let client = test_client(&server);
    let result = client.resolve_id("stories", "aaa").await;
    assert!(matches!(result, Err(ApiError::Validation(_))));
}

// ── Retry behavior tests ─────────────────────────────

#[tokio::test]
async fn retries_on_500_then_succeeds() {
    let server = MockServer::start().await;

    Mock::given(method("GET"))
        .and(path("/v1/epics"))
        .respond_with(ResponseTemplate::new(500).set_body_string("internal error"))
        .up_to_n_times(1)
        .mount(&server)
        .await;

    Mock::given(method("GET"))
        .and(path("/v1/epics"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "data": [{"id": "ok"}]
        })))
        .mount(&server)
        .await;

    let client = test_client(&server);
    let resp: DataWrapper<Vec<serde_json::Value>> = client.get("/v1/epics").await.unwrap();
    assert_eq!(resp.data.len(), 1);
}

#[tokio::test]
async fn retries_on_429_rate_limit() {
    let server = MockServer::start().await;

    Mock::given(method("GET"))
        .and(path("/v1/epics"))
        .respond_with(ResponseTemplate::new(429).set_body_string("rate limited"))
        .up_to_n_times(1)
        .mount(&server)
        .await;

    Mock::given(method("GET"))
        .and(path("/v1/epics"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "data": []
        })))
        .mount(&server)
        .await;

    let client = test_client(&server);
    let resp: DataWrapper<Vec<serde_json::Value>> = client.get("/v1/epics").await.unwrap();
    assert!(resp.data.is_empty());
}

#[tokio::test]
async fn gives_up_after_max_retries() {
    let server = MockServer::start().await;

    Mock::given(method("GET"))
        .and(path("/v1/epics"))
        .respond_with(ResponseTemplate::new(500).set_body_string("permanent failure"))
        .mount(&server)
        .await;

    let client = test_client(&server);
    let result: Result<serde_json::Value, ApiError> = client.get("/v1/epics").await;
    assert!(matches!(result, Err(ApiError::Server(_))));
}

// ── Error message quality tests ──────────────────────

#[tokio::test]
async fn error_messages_include_actionable_guidance() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/v1/epics"))
        .respond_with(ResponseTemplate::new(401).set_body_string("bad key"))
        .mount(&server)
        .await;

    let client = test_client(&server);
    let err = client
        .get::<serde_json::Value>("/v1/epics")
        .await
        .unwrap_err();
    let msg = err.to_string();
    assert!(msg.contains("sk_admin_") || msg.contains("sk_live_"));
    assert!(msg.contains("API key"));
}

#[tokio::test]
async fn payment_required_mentions_usage() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/v1/epics"))
        .respond_with(ResponseTemplate::new(402).set_body_string("quota exceeded"))
        .mount(&server)
        .await;

    let client = test_client(&server);
    let err = client
        .get::<serde_json::Value>("/v1/epics")
        .await
        .unwrap_err();
    let msg = err.to_string();
    assert!(msg.contains("quota") || msg.contains("usage"));
}

// ── Pre-flight auth check (verify_auth) ─────────

#[tokio::test]
async fn verify_auth_succeeds_with_valid_key() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/v1/epics"))
        .and(header("x-api-key", "sk_test_abc123"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "data": []
        })))
        .mount(&server)
        .await;

    let client = test_client(&server);
    // Should succeed without error
    client.verify_auth().await.unwrap();
}

#[tokio::test]
async fn verify_auth_fails_on_401_invalid_key() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/v1/epics"))
        .respond_with(ResponseTemplate::new(401).set_body_string("invalid api key"))
        .mount(&server)
        .await;

    let client = test_client(&server);
    let result = client.verify_auth().await;
    assert!(matches!(result, Err(ApiError::Auth(_))));
    let err_msg = result.unwrap_err().to_string();
    assert!(err_msg.contains("401"), "Error should mention 401 status");
}

#[tokio::test]
async fn verify_auth_fails_on_403_forbidden() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/v1/epics"))
        .respond_with(ResponseTemplate::new(403).set_body_string("key lacks permission"))
        .mount(&server)
        .await;

    let client = test_client(&server);
    let result = client.verify_auth().await;
    assert!(matches!(result, Err(ApiError::Auth(_))));
}

#[tokio::test]
async fn verify_auth_sends_correct_api_key_header() {
    let server = MockServer::start().await;
    // Only match if the correct x-api-key header is present
    Mock::given(method("GET"))
        .and(path("/v1/epics"))
        .and(header("x-api-key", "sk_test_abc123"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({ "data": [] })))
        .mount(&server)
        .await;

    let client = test_client(&server);
    // If the header is wrong, wiremock returns 404 (no matching mock),
    // which would cause verify_auth to return an error
    client.verify_auth().await.unwrap();
}

#[tokio::test]
async fn verify_auth_retries_on_transient_500() {
    let server = MockServer::start().await;

    // First request: 500 (transient)
    Mock::given(method("GET"))
        .and(path("/v1/epics"))
        .respond_with(ResponseTemplate::new(500).set_body_string("temporary failure"))
        .up_to_n_times(1)
        .mount(&server)
        .await;

    // Second request: success
    Mock::given(method("GET"))
        .and(path("/v1/epics"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({ "data": [] })))
        .mount(&server)
        .await;

    let client = test_client(&server);
    // Should succeed after retry
    client.verify_auth().await.unwrap();
}

#[tokio::test]
async fn verify_auth_child_process_gets_same_credentials() {
    // Simulates the orchestrator creating a client, extracting the key,
    // and creating a child client with the same credentials — both should pass.
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/v1/epics"))
        .and(header("x-api-key", "sk_live_forwarded_key"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({ "data": [] })))
        .mount(&server)
        .await;

    // Parent client (orchestrator)
    let parent = ApiClient::new(&server.uri(), "sk_live_forwarded_key");
    parent.verify_auth().await.unwrap();

    // Child client (sprint-run) — constructed from parent's credentials
    let child = ApiClient::new(&server.uri(), parent.api_key());
    child.verify_auth().await.unwrap();
}

#[tokio::test]
async fn verify_auth_fails_with_empty_key() {
    let server = MockServer::start().await;
    // Server rejects empty key with 401
    Mock::given(method("GET"))
        .and(path("/v1/epics"))
        .respond_with(ResponseTemplate::new(401).set_body_string("missing api key"))
        .mount(&server)
        .await;

    let client = ApiClient::new(&server.uri(), "");
    let result = client.verify_auth().await;
    assert!(matches!(result, Err(ApiError::Auth(_))));
}

// ── PUT + header verification ────────────────────────

#[tokio::test]
async fn put_sends_api_key_header() {
    let server = MockServer::start().await;

    Mock::given(method("PUT"))
        .and(path("/v1/_meta/tables/test"))
        .and(header("x-api-key", "sk_test_abc123"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"ok": true})))
        .mount(&server)
        .await;

    let client = test_client(&server);
    let body = json!({"columns": []});
    let resp: serde_json::Value = client.put("/v1/_meta/tables/test", &body).await.unwrap();
    assert_eq!(resp["ok"], true);
}
