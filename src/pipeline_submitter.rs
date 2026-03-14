//! Submit pipeline runs to the Kapable API and monitor progress.

use serde::{Deserialize, Serialize};
use uuid::Uuid;

use kapable_pipeline::types::PipelineDefinition;

use crate::api_client::ApiClient;

#[derive(Debug, Serialize)]
struct SubmitRequest {
    pipeline_definition: serde_json::Value,
    pipeline_name: Option<String>,
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
pub async fn submit_pipeline(
    client: &ApiClient,
    definition: &PipelineDefinition,
    repo_url: Option<String>,
    repo_ref: Option<String>,
    env: Option<serde_json::Value>,
    capabilities: Option<Vec<String>>,
) -> Result<SubmitResponse, Box<dyn std::error::Error>> {
    let pipeline_json = serde_json::to_value(definition)?;

    let req = SubmitRequest {
        pipeline_definition: pipeline_json,
        pipeline_name: Some(definition.name.clone()),
        repo_url,
        repo_ref,
        env,
        required_capabilities: capabilities,
        priority: None,
        timeout_secs: definition.timeout_secs.map(|t| t as i32),
    };

    // Use raw reqwest since ApiClient is geared toward Data API (x-api-key),
    // but the pipeline submission endpoint uses Bearer auth.
    let api_url = &client.base_url;
    let token = client.api_key();

    let http = reqwest::Client::new();
    let resp = http
        .post(format!("{}/v1/pipelines/run", api_url))
        .header("Authorization", format!("Bearer {}", token))
        .json(&req)
        .send()
        .await?;

    if !resp.status().is_success() {
        let status = resp.status();
        let body = resp.text().await.unwrap_or_default();
        return Err(format!("Pipeline submission failed ({}): {}", status, body).into());
    }

    let result: SubmitResponse = resp.json().await?;
    Ok(result)
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
    let http = reqwest::Client::new();

    loop {
        let resp = http
            .get(format!("{}/v1/pipeline-runs/{}", api_url, run_id))
            .header("Authorization", format!("Bearer {}", token))
            .send()
            .await?;

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
