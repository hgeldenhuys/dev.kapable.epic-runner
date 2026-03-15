//! Submit pipeline runs to the Kapable API and monitor progress.

use serde::{Deserialize, Serialize};
use uuid::Uuid;

use kapable_pipeline::types::PipelineDefinition;

use crate::api_client::ApiClient;

#[derive(Debug, Serialize)]
struct SubmitRequest {
    pipeline_definition: serde_json::Value,
    pipeline_name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    org_id: Option<Uuid>,
    repo_url: Option<String>,
    repo_ref: Option<String>,
    env: Option<serde_json::Value>,
    required_capabilities: Option<Vec<String>>,
    priority: Option<i32>,
    timeout_secs: Option<i32>,
}

#[derive(Debug, Deserialize)]
pub struct SubmitResponse {
    pub run_id: Uuid,
    pub job_id: Uuid,
}

#[derive(Debug, Deserialize)]
pub struct PipelineRunStatus {
    pub id: Uuid,
    pub status: String,
    pub error_message: Option<String>,
    pub completed_stages: Option<i32>,
    pub total_stages: Option<i32>,
}

/// Submit a pipeline definition for agent execution.
///
/// Authenticates via `x-api-key` (admin key) or `Authorization: Bearer` (service token).
/// The admin key is resolved from KAPABLE_ADMIN_API_KEY env var, config, or the
/// client's API key if it looks like an admin key (sk_admin_*).
pub async fn submit_pipeline(
    client: &ApiClient,
    definition: &PipelineDefinition,
    repo_url: Option<String>,
    repo_ref: Option<String>,
    env: Option<serde_json::Value>,
    capabilities: Option<Vec<String>>,
) -> Result<SubmitResponse, Box<dyn std::error::Error>> {
    let pipeline_json = serde_json::to_value(definition)?;

    // Resolve org_id from env or config for admin key auth
    let org_id = std::env::var("KAPABLE_ORG_ID")
        .ok()
        .and_then(|s| s.parse::<Uuid>().ok());

    let req = SubmitRequest {
        pipeline_definition: pipeline_json,
        pipeline_name: Some(definition.name.clone()),
        org_id,
        repo_url,
        repo_ref,
        env,
        required_capabilities: capabilities,
        priority: None,
        timeout_secs: definition.timeout_secs.map(|t| t as i32),
    };

    let api_url = &client.base_url;
    let token = client.api_key();

    // Resolve the correct auth header:
    // 1. KAPABLE_ADMIN_API_KEY env var (explicit admin key)
    // 2. Client key if it's an admin key (sk_admin_*)
    // 3. Client key if it's a service token (st_ci_*)
    // 4. Fall back to Bearer auth with whatever key we have
    let admin_key = std::env::var("KAPABLE_ADMIN_API_KEY").ok();
    let is_admin_key = token.starts_with("sk_admin_");
    let is_service_token = token.starts_with("st_ci_");

    let http = reqwest::Client::new();
    let mut request = http
        .post(format!("{}/v1/pipelines/run", api_url))
        .json(&req);

    if let Some(ref admin) = admin_key {
        request = request.header("x-api-key", admin.as_str());
    } else if is_admin_key {
        request = request.header("x-api-key", token);
    } else if is_service_token {
        request = request.header("Authorization", format!("Bearer {}", token));
    } else {
        // Data key — won't work, but try admin key from env as last resort
        request = request.header("x-api-key", token);
    }

    let resp = request.send().await?;

    if !resp.status().is_success() {
        let status = resp.status();
        let body = resp.text().await.unwrap_or_default();
        return Err(format!("Pipeline submission failed ({}): {}", status, body).into());
    }

    let result: SubmitResponse = resp.json().await?;
    Ok(result)
}

/// Fetch step logs for a pipeline run.
///
/// Returns raw log text, suitable for verdict extraction.
/// Optionally filter by stage_id and step_id.
pub async fn fetch_run_logs(
    client: &ApiClient,
    run_id: Uuid,
    stage_id: Option<&str>,
    step_id: Option<&str>,
) -> Result<String, Box<dyn std::error::Error>> {
    let api_url = &client.base_url;
    let admin_key = std::env::var("KAPABLE_ADMIN_API_KEY").ok();
    let http = reqwest::Client::new();

    let mut url = format!("{}/v1/pipeline-runs/{}/logs", api_url, run_id);
    let mut params = Vec::new();
    if let Some(s) = stage_id {
        params.push(format!("stage_id={}", s));
    }
    if let Some(s) = step_id {
        params.push(format!("step_id={}", s));
    }
    if !params.is_empty() {
        url = format!("{}?{}", url, params.join("&"));
    }

    let mut request = http.get(&url);
    if let Some(ref admin) = admin_key {
        request = request.header("x-api-key", admin.as_str());
    } else {
        request = request.header("x-api-key", client.api_key());
    }

    let resp = request.send().await?;
    if resp.status().is_success() {
        Ok(resp.text().await?)
    } else {
        Err(format!("Failed to fetch logs ({})", resp.status()).into())
    }
}

/// Poll for pipeline run completion.
///
/// Returns when the run reaches a terminal status (succeeded, failed, cancelled).
pub async fn wait_for_completion(
    client: &ApiClient,
    run_id: Uuid,
    poll_interval_secs: u64,
) -> Result<PipelineRunStatus, Box<dyn std::error::Error>> {
    let api_url = &client.base_url;
    let token = client.api_key();
    let admin_key = std::env::var("KAPABLE_ADMIN_API_KEY").ok();
    let http = reqwest::Client::new();

    loop {
        let mut request = http.get(format!("{}/v1/pipeline-runs/{}", api_url, run_id));
        // Pipeline run detail accepts admin key, session auth, or service token.
        // Use admin key if available, otherwise fall back to x-api-key.
        if let Some(ref admin) = admin_key {
            request = request.header("x-api-key", admin.as_str());
        } else {
            request = request.header("x-api-key", token);
        }
        let resp = request.send().await?;

        if resp.status().is_success() {
            let status: PipelineRunStatus = resp.json().await?;

            eprintln!(
                "[pipeline] Run {} -- status: {}, stages: {}/{}",
                run_id,
                status.status,
                status.completed_stages.unwrap_or(0),
                status.total_stages.unwrap_or(0),
            );

            match status.status.as_str() {
                "succeeded" | "failed" | "cancelled" => return Ok(status),
                _ => {}
            }
        } else {
            let code = resp.status();
            eprintln!("[pipeline] Status check failed ({}), retrying...", code);
        }

        tokio::time::sleep(std::time::Duration::from_secs(poll_interval_secs)).await;
    }
}
