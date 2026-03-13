/// Integration tests for ER-025 (AUTH-002): Verify that the orchestrator correctly
/// forwards API credentials to the sprint-run child process via --url / --key flags.
///
/// These tests spawn the actual `epic-runner` binary as a subprocess (mirroring how
/// `orchestrate.rs` spawns `sprint-run`) and verify:
///   1. Valid credentials are forwarded and the child sends the correct x-api-key header
///   2. Invalid credentials trigger fast failure (not silent 401s that waste sprints)
///   3. Missing credentials are caught before any work starts
///   4. The exact key value survives the parent→child CLI arg handoff
///
/// These would have caught the 401 failure that burned 5 sprint records on VALIDATE-001.
use serde_json::json;
use wiremock::matchers::{header, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

/// Path to the compiled epic-runner binary (automatically set by `cargo test`).
fn epic_runner_bin() -> &'static str {
    env!("CARGO_BIN_EXE_epic-runner")
}

/// Spawn epic-runner as a subprocess with explicit --url and optional --key.
///
/// Uses `env_clear()` to prevent the host's KAPABLE_DATA_KEY, KAPABLE_ADMIN_API_KEY,
/// or config files from polluting the test through the credential cascade.
fn spawn_runner(url: &str, key: Option<&str>, extra_args: &[&str]) -> std::process::Command {
    let mut cmd = std::process::Command::new(epic_runner_bin());

    // Clear env to isolate from host credential cascade
    cmd.env_clear();

    // Re-add minimal env needed for the binary to run (TLS, DNS, etc.)
    if let Ok(p) = std::env::var("PATH") {
        cmd.env("PATH", p);
    }
    if let Ok(h) = std::env::var("HOME") {
        cmd.env("HOME", h);
    }
    // Suppress tracing output noise in tests
    cmd.env("RUST_LOG", "error");

    cmd.arg("--url").arg(url);
    if let Some(k) = key {
        cmd.arg("--key").arg(k);
    }
    for arg in extra_args {
        cmd.arg(arg);
    }
    cmd
}

// ── Test 1: Valid key is forwarded and received by the API ─────────────

#[tokio::test]
async fn subprocess_sprint_run_receives_forwarded_credentials() {
    let server = MockServer::start().await;
    let test_key = "sk_live_integration_test_key_42";

    // Mock the pre-flight auth check endpoint — expects the exact key
    Mock::given(method("GET"))
        .and(path("/v1/epics"))
        .and(header("x-api-key", test_key))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({ "data": [] })))
        .expect(1..) // MUST be called at least once with the correct key
        .named("pre-flight auth check")
        .mount(&server)
        .await;

    let output = spawn_runner(
        &server.uri(),
        Some(test_key),
        &["sprint-run", "00000000-0000-0000-0000-000000000001"],
    )
    .output()
    .expect("Failed to spawn epic-runner subprocess");

    let stderr = String::from_utf8_lossy(&output.stderr);

    // The process will fail after auth (sprint doesn't exist in wiremock),
    // but the pre-flight auth check MUST have passed — no auth failure message.
    assert!(
        !stderr.contains("pre-flight auth failed"),
        "Auth should pass with valid key. stderr:\n{stderr}"
    );
    assert!(
        !stderr.contains("credentials were not forwarded"),
        "Should not complain about credential forwarding. stderr:\n{stderr}"
    );

    // wiremock's .expect(1..) assertion will panic on drop if the mock was never
    // called with the correct x-api-key header — proving credential forwarding works.
}

// ── Test 2: Invalid key causes fast failure with clear error ───────────

#[tokio::test]
async fn subprocess_fails_fast_on_401_invalid_key() {
    let server = MockServer::start().await;

    // Return 401 for any auth check — simulates expired/wrong key
    Mock::given(method("GET"))
        .and(path("/v1/epics"))
        .respond_with(ResponseTemplate::new(401).set_body_string("invalid api key"))
        .named("401 rejection")
        .mount(&server)
        .await;

    let output = spawn_runner(
        &server.uri(),
        Some("sk_bad_expired_key"),
        &["sprint-run", "00000000-0000-0000-0000-000000000001"],
    )
    .output()
    .expect("Failed to spawn epic-runner subprocess");

    // Must exit non-zero — no silent failures
    assert!(
        !output.status.success(),
        "Process must exit with error on 401"
    );

    let stderr = String::from_utf8_lossy(&output.stderr);

    // Error message must mention auth failure so operators can diagnose
    assert!(
        stderr.contains("pre-flight auth failed")
            || stderr.contains("Auth error")
            || stderr.contains("Unauthorized"),
        "Should mention auth failure in stderr. Got:\n{stderr}"
    );
}

// ── Test 3: Forbidden (403) also triggers fast failure ─────────────────

#[tokio::test]
async fn subprocess_fails_fast_on_403_forbidden() {
    let server = MockServer::start().await;

    Mock::given(method("GET"))
        .and(path("/v1/epics"))
        .respond_with(ResponseTemplate::new(403).set_body_string("key lacks permission"))
        .named("403 rejection")
        .mount(&server)
        .await;

    let output = spawn_runner(
        &server.uri(),
        Some("sk_live_wrong_scope"),
        &["sprint-run", "00000000-0000-0000-0000-000000000001"],
    )
    .output()
    .expect("Failed to spawn epic-runner subprocess");

    assert!(
        !output.status.success(),
        "Process must exit with error on 403"
    );

    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("pre-flight auth failed")
            || stderr.contains("Auth error")
            || stderr.contains("Forbidden"),
        "Should mention auth/permission failure. Got:\n{stderr}"
    );
}

// ── Test 4: Missing key is caught before any HTTP call ─────────────────

#[tokio::test]
async fn subprocess_fails_without_key() {
    let server = MockServer::start().await;

    // No mock needed — the process should error before making any HTTP call
    // because from_env_with_overrides() won't find a key (env is cleared).

    let output = spawn_runner(
        &server.uri(),
        None, // No --key flag
        &["sprint-run", "00000000-0000-0000-0000-000000000001"],
    )
    .output()
    .expect("Failed to spawn epic-runner subprocess");

    assert!(
        !output.status.success(),
        "Process must exit with error when no key is provided"
    );

    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("No API key") || stderr.contains("api_key") || stderr.contains("--key"),
        "Should mention missing API key. Got:\n{stderr}"
    );
}

// ── Test 5: Exact key identity survives parent→child handoff ───────────

#[tokio::test]
async fn subprocess_forwards_exact_key_value() {
    let server = MockServer::start().await;

    // Use a distinctive key with special characters that might get mangled
    let exact_key = "sk_live_ABCdef123_special-chars.ok";

    // Wiremock will ONLY respond 200 if the exact key is in the header.
    // Any mutation (truncation, encoding, quoting) → wiremock returns 404 → auth fails.
    Mock::given(method("GET"))
        .and(path("/v1/epics"))
        .and(header("x-api-key", exact_key))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({ "data": [] })))
        .expect(1..)
        .named("exact key match")
        .mount(&server)
        .await;

    let output = spawn_runner(
        &server.uri(),
        Some(exact_key),
        &["sprint-run", "00000000-0000-0000-0000-000000000001"],
    )
    .output()
    .expect("Failed to spawn epic-runner subprocess");

    let stderr = String::from_utf8_lossy(&output.stderr);

    // If the key was mangled, wiremock returns 404 (no matching mock),
    // which causes verify_auth to fail → "pre-flight auth failed" in stderr.
    assert!(
        !stderr.contains("pre-flight auth failed"),
        "Key value was corrupted during parent→child handoff. stderr:\n{stderr}"
    );
}

// ── Test 6: Simulated orchestrate credential forwarding pattern ────────

#[tokio::test]
async fn orchestrate_credential_forwarding_pattern() {
    // This test mirrors the exact pattern from orchestrate.rs lines 345-351:
    //   cmd.arg("--url").arg(&client.base_url)
    //      .arg("--key").arg(client.api_key())
    //      .arg("sprint-run").arg(sprint_id);
    //
    // We simulate the parent reading credentials, then constructing the child
    // command with those same credentials — proving the forwarding chain works.

    let server = MockServer::start().await;
    let parent_key = "sk_live_parent_orchestrator_key";

    Mock::given(method("GET"))
        .and(path("/v1/epics"))
        .and(header("x-api-key", parent_key))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({ "data": [] })))
        .expect(1..)
        .named("child receives parent key")
        .mount(&server)
        .await;

    // Step 1: Parent creates its client (simulated — we just hold the key)
    let parent_url = server.uri();
    let parent_api_key = parent_key; // In real code: client.api_key()

    // Step 2: Parent spawns child with its own credentials
    //         (this is the exact pattern from orchestrate.rs)
    let output = spawn_runner(
        &parent_url,
        Some(parent_api_key),
        &["sprint-run", "00000000-0000-0000-0000-000000000002"],
    )
    .output()
    .expect("Failed to spawn child process");

    let stderr = String::from_utf8_lossy(&output.stderr);

    // Step 3: Verify child authenticated successfully with parent's key
    assert!(
        !stderr.contains("pre-flight auth failed"),
        "Child process should auth with parent's forwarded key. stderr:\n{stderr}"
    );
    assert!(
        !stderr.contains("credentials were not forwarded"),
        "Credential forwarding should succeed. stderr:\n{stderr}"
    );
}
