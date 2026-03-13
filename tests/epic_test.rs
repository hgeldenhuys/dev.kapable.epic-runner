use epic_runner::api_client::ApiClient;
use epic_runner::commands::{epic, CliConfig};
use serde_json::json;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

/// Helper: create an ApiClient pointed at the mock server.
fn test_client(server: &MockServer) -> ApiClient {
    ApiClient::new(&server.uri(), "sk_test_abc123")
}

fn cli_config() -> CliConfig {
    CliConfig {
        json: false,
        verbose: false,
    }
}

// ── Epic creation: code uniqueness tests ─────────────────

/// AC: "Normal creation succeeds with monotonically increasing instance number"
/// Given zero AUTH epics exist globally, creating with domain AUTH produces AUTH-001.
#[tokio::test]
async fn epic_create_first_in_domain_gets_001() {
    let server = MockServer::start().await;
    let client = test_client(&server);
    let cli = cli_config();

    // Mock: product lookup (GET /v1/products?slug=test-product)
    Mock::given(method("GET"))
        .and(path("/v1/products"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "data": [{"id": "prod-aaa", "slug": "test-product"}]
        })))
        .mount(&server)
        .await;

    // Mock: global epics list — empty (no AUTH epics exist)
    Mock::given(method("GET"))
        .and(path("/v1/epics"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "data": []
        })))
        .mount(&server)
        .await;

    // Mock: POST creates the epic — expect exactly 1 call
    Mock::given(method("POST"))
        .and(path("/v1/epics"))
        .respond_with(ResponseTemplate::new(201).set_body_json(json!({
            "id": "epic-new-1",
            "code": "AUTH-001",
            "domain": "AUTH",
            "title": "First"
        })))
        .expect(1)
        .mount(&server)
        .await;

    let args = epic::EpicArgs {
        action: epic::EpicAction::Create {
            product: "test-product".into(),
            domain: "AUTH".into(),
            title: "First".into(),
            intent: "test".into(),
            criteria: None,
        },
    };

    let result = epic::run(args, &client, &cli).await;
    assert!(result.is_ok(), "Expected success but got: {:?}", result);
}

/// AC: "Generated code collision returns clear error"
/// Given AUTH-001 exists globally (but with a different domain in the DB, simulating
/// a race condition where domain_count misses it), the defense-in-depth code_exists
/// check detects the collision. No POST should be made.
#[tokio::test]
async fn epic_create_duplicate_code_returns_error_without_post() {
    let server = MockServer::start().await;
    let client = test_client(&server);
    let cli = cli_config();

    // Mock: product lookup
    Mock::given(method("GET"))
        .and(path("/v1/products"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "data": [{"id": "prod-aaa", "slug": "test-product"}]
        })))
        .mount(&server)
        .await;

    // Mock: global epics list — AUTH-001 exists with a DIFFERENT domain ("OTHER")
    // so domain_count for "AUTH" = 0, generating code AUTH-001 again.
    // The defense-in-depth code_exists check should catch this collision.
    Mock::given(method("GET"))
        .and(path("/v1/epics"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "data": [{
                "id": "epic-existing",
                "code": "AUTH-001",
                "domain": "OTHER",
                "product_id": "prod-aaa"
            }]
        })))
        .mount(&server)
        .await;

    // POST should NOT be called — expect 0 hits
    Mock::given(method("POST"))
        .and(path("/v1/epics"))
        .respond_with(ResponseTemplate::new(201).set_body_json(json!({})))
        .expect(0)
        .mount(&server)
        .await;

    let args = epic::EpicArgs {
        action: epic::EpicAction::Create {
            product: "test-product".into(),
            domain: "AUTH".into(),
            title: "Dup test".into(),
            intent: "test".into(),
            criteria: None,
        },
    };

    let result = epic::run(args, &client, &cli).await;
    assert!(result.is_err(), "Expected error for duplicate code");
    let err_msg = result.unwrap_err().to_string();
    assert!(
        err_msg.contains("already exists globally"),
        "Error should mention global existence, got: {err_msg}"
    );
    assert!(
        err_msg.contains("Epic codes must be unique across all products"),
        "Error should mention cross-product uniqueness, got: {err_msg}"
    );
}

/// AC: "Second epic in same domain gets incremented code"
/// Given AUTH-001 already exists, creating another AUTH epic produces AUTH-002.
#[tokio::test]
async fn epic_create_second_in_domain_gets_002() {
    let server = MockServer::start().await;
    let client = test_client(&server);
    let cli = cli_config();

    // Mock: product lookup
    Mock::given(method("GET"))
        .and(path("/v1/products"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "data": [{"id": "prod-aaa", "slug": "test-product"}]
        })))
        .mount(&server)
        .await;

    // Mock: global epics list — AUTH-001 already exists
    Mock::given(method("GET"))
        .and(path("/v1/epics"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "data": [{
                "id": "epic-existing",
                "code": "AUTH-001",
                "domain": "AUTH",
                "product_id": "prod-aaa"
            }]
        })))
        .mount(&server)
        .await;

    // Mock: POST creates AUTH-002 — expect exactly 1 call
    Mock::given(method("POST"))
        .and(path("/v1/epics"))
        .respond_with(ResponseTemplate::new(201).set_body_json(json!({
            "id": "epic-new-2",
            "code": "AUTH-002",
            "domain": "AUTH",
            "title": "Second"
        })))
        .expect(1)
        .mount(&server)
        .await;

    let args = epic::EpicArgs {
        action: epic::EpicAction::Create {
            product: "test-product".into(),
            domain: "AUTH".into(),
            title: "Second".into(),
            intent: "test".into(),
            criteria: None,
        },
    };

    let result = epic::run(args, &client, &cli).await;
    assert!(result.is_ok(), "Expected success but got: {:?}", result);
}

/// AC: "Global scan is performed even across products"
/// Given AUTH-001 exists in product-A, creating AUTH in product-B produces AUTH-002
/// (not AUTH-001), confirming global uniqueness.
#[tokio::test]
async fn epic_create_cross_product_gets_incremented_code() {
    let server = MockServer::start().await;
    let client = test_client(&server);
    let cli = cli_config();

    // Mock: product lookup for product-B
    Mock::given(method("GET"))
        .and(path("/v1/products"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "data": [{"id": "prod-bbb", "slug": "product-B"}]
        })))
        .mount(&server)
        .await;

    // Mock: global epics list — AUTH-001 exists in product-A (different product)
    Mock::given(method("GET"))
        .and(path("/v1/epics"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "data": [{
                "id": "epic-aaa",
                "code": "AUTH-001",
                "domain": "AUTH",
                "product_id": "prod-aaa"
            }]
        })))
        .mount(&server)
        .await;

    // Mock: POST creates AUTH-002 — expect exactly 1 call
    Mock::given(method("POST"))
        .and(path("/v1/epics"))
        .respond_with(ResponseTemplate::new(201).set_body_json(json!({
            "id": "epic-bbb",
            "code": "AUTH-002",
            "domain": "AUTH",
            "title": "Cross"
        })))
        .expect(1)
        .mount(&server)
        .await;

    let args = epic::EpicArgs {
        action: epic::EpicAction::Create {
            product: "product-B".into(),
            domain: "AUTH".into(),
            title: "Cross".into(),
            intent: "test".into(),
            criteria: None,
        },
    };

    let result = epic::run(args, &client, &cli).await;
    assert!(result.is_ok(), "Expected success but got: {:?}", result);
}

/// AC: "API mock test: duplicate-code scenario returns error without POST"
/// Variation: AUTH-001 exists in a DIFFERENT product — the global check still fires.
#[tokio::test]
async fn epic_create_duplicate_code_across_products_returns_error() {
    let server = MockServer::start().await;
    let client = test_client(&server);
    let cli = cli_config();

    // Mock: product lookup for product-B
    Mock::given(method("GET"))
        .and(path("/v1/products"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "data": [{"id": "prod-bbb", "slug": "product-B"}]
        })))
        .mount(&server)
        .await;

    // Mock: global epics list — AUTH-001 exists in product-A with domain "OTHER"
    // (race condition: domain_count for "AUTH" = 0, code = AUTH-001, but AUTH-001 exists)
    Mock::given(method("GET"))
        .and(path("/v1/epics"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "data": [{
                "id": "epic-aaa",
                "code": "AUTH-001",
                "domain": "OTHER",
                "product_id": "prod-aaa"
            }]
        })))
        .mount(&server)
        .await;

    // POST should NOT be called
    Mock::given(method("POST"))
        .and(path("/v1/epics"))
        .respond_with(ResponseTemplate::new(201).set_body_json(json!({})))
        .expect(0)
        .mount(&server)
        .await;

    let args = epic::EpicArgs {
        action: epic::EpicAction::Create {
            product: "product-B".into(),
            domain: "AUTH".into(),
            title: "Cross dup".into(),
            intent: "test".into(),
            criteria: None,
        },
    };

    let result = epic::run(args, &client, &cli).await;
    assert!(
        result.is_err(),
        "Expected error for cross-product duplicate"
    );
    let err_msg = result.unwrap_err().to_string();
    assert!(
        err_msg.contains("already exists globally"),
        "Error should mention global existence, got: {err_msg}"
    );
}
