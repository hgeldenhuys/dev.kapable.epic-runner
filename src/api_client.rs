use std::time::Duration;

use reqwest::{Client, RequestBuilder, StatusCode};
use serde::{de::DeserializeOwned, Deserialize, Serialize};

#[derive(Deserialize)]
pub struct DataWrapper<T> {
    pub data: T,
}

#[derive(Clone)]
pub struct ApiClient {
    client: Client,
    pub base_url: String,
    api_key: String,
}

#[derive(Debug, thiserror::Error)]
pub enum ApiError {
    #[error("HTTP error: {0}")]
    Http(#[from] reqwest::Error),
    #[error("Not found: {0}")]
    NotFound(String),
    #[error("Validation error: {0}")]
    Validation(String),
    #[error("Auth error: {0}")]
    Auth(String),
    #[error("Server error: {0}")]
    Server(String),
}

impl ApiError {
    /// Returns true if the error is transient and worth retrying.
    fn is_retryable(&self) -> bool {
        match self {
            ApiError::Http(e) => e.is_timeout() || e.is_connect(),
            ApiError::Server(_) => true, // 5xx
            _ => false,
        }
    }
}

pub const DEFAULT_API_URL: &str = "https://api.kapable.dev";

/// Maximum number of send attempts (1 initial + 2 retries).
const MAX_ATTEMPTS: u32 = 3;
/// Base backoff delay in milliseconds; doubled on each retry.
const BACKOFF_BASE_MS: u64 = 1_000;

impl ApiClient {
    /// Create a client with explicit URL and key (no config cascade).
    pub fn new(base_url: &str, api_key: &str) -> Self {
        Self {
            client: Self::build_client(),
            base_url: base_url.to_string(),
            api_key: api_key.to_string(),
        }
    }

    /// Returns the API key (needed for forwarding to child processes).
    pub fn api_key(&self) -> &str {
        &self.api_key
    }

    pub fn from_env_with_overrides(
        url_override: Option<String>,
        key_override: Option<String>,
    ) -> Result<Self, Box<dyn std::error::Error>> {
        use crate::config::*;

        let project_config = find_project_config().and_then(|p| read_config(&p).ok());
        let home_config = home_config_path().and_then(|p| read_config(&p).ok());

        let base_url = url_override
            .or_else(|| std::env::var("KAPABLE_API_URL").ok())
            .or_else(|| {
                project_config
                    .as_ref()
                    .and_then(|c| c.base_url().map(String::from))
            })
            .or_else(|| {
                home_config
                    .as_ref()
                    .and_then(|c| c.base_url().map(String::from))
            })
            .unwrap_or_else(|| DEFAULT_API_URL.to_string());

        // Key cascade: CLI flag → env KAPABLE_DATA_KEY → config data_key → env KAPABLE_ADMIN_API_KEY → config api_key
        // Prefer project-scoped data keys (sk_live_) over admin keys (sk_admin_)
        let api_key = key_override
            .or_else(|| std::env::var("KAPABLE_DATA_KEY").ok())
            .or_else(|| {
                project_config
                    .as_ref()
                    .and_then(|c| c.data_key().map(String::from))
            })
            .or_else(|| {
                home_config
                    .as_ref()
                    .and_then(|c| c.data_key().map(String::from))
            })
            .or_else(|| std::env::var("KAPABLE_ADMIN_API_KEY").ok())
            .or_else(|| {
                project_config
                    .as_ref()
                    .and_then(|c| c.api_key().map(String::from))
            })
            .or_else(|| {
                home_config
                    .as_ref()
                    .and_then(|c| c.api_key().map(String::from))
            })
            .ok_or("No API key (--key, KAPABLE_DATA_KEY, or .epic-runner/config.toml)")?;

        Ok(Self {
            client: Self::build_client(),
            base_url,
            api_key,
        })
    }

    /// Build a reqwest Client with a 30-second request timeout.
    fn build_client() -> Client {
        Client::builder()
            .timeout(Duration::from_secs(30))
            .build()
            .expect("Failed to build HTTP client")
    }

    // ── Public HTTP methods ────────────────────────────────────────────

    /// Pre-flight auth check: verify the API key is valid by hitting a lightweight
    /// endpoint. Returns Ok(()) on success, or an ApiError::Auth on 401/403.
    ///
    /// This MUST be called before spawning child processes or starting expensive
    /// ceremony work. Catches invalid/missing credential forwarding early instead
    /// of wasting sprints on 401 failures (see AUTH-002 / ER-024).
    pub async fn verify_auth(&self) -> Result<(), ApiError> {
        // Use GET /v1/epics?limit=1 as a cheap auth probe — it's a table every
        // project has, and the limit keeps the payload minimal. Any authenticated
        // endpoint would work; this one is guaranteed to exist.
        let url = format!("{}{}", self.base_url, "/v1/epics?limit=1");
        let key = self.api_key.clone();
        let resp = self
            .send_with_retry(|| self.client.get(&url).header("x-api-key", &key))
            .await?;
        let status = resp.status();
        if status.is_success() {
            Ok(())
        } else {
            let body = resp.text().await.unwrap_or_default();
            Err(self.status_to_error(status, body))
        }
    }

    pub async fn get<T: DeserializeOwned>(&self, path: &str) -> Result<T, ApiError> {
        let url = format!("{}{}", self.base_url, path);
        let key = self.api_key.clone();
        let resp = self
            .send_with_retry(|| self.client.get(&url).header("x-api-key", &key))
            .await?;
        self.handle_response(resp).await
    }

    /// GET with query string parameters for server-side filtering.
    /// Appends params as `?key1=value1&key2=value2` to the URL.
    /// The caller is responsible for client-side fallback if the server ignores the params.
    pub async fn get_with_params<T: DeserializeOwned>(
        &self,
        path: &str,
        params: &[(&str, &str)],
    ) -> Result<T, ApiError> {
        let url = if params.is_empty() {
            format!("{}{}", self.base_url, path)
        } else {
            let query: String = params
                .iter()
                .map(|(k, v)| format!("{}={}", urlencoding::encode(k), urlencoding::encode(v)))
                .collect::<Vec<_>>()
                .join("&");
            format!("{}{}?{}", self.base_url, path, query)
        };
        let key = self.api_key.clone();
        let resp = self
            .send_with_retry(|| self.client.get(&url).header("x-api-key", &key))
            .await?;
        self.handle_response(resp).await
    }

    pub async fn post<B: Serialize, T: DeserializeOwned>(
        &self,
        path: &str,
        body: &B,
    ) -> Result<T, ApiError> {
        let url = format!("{}{}", self.base_url, path);
        let key = self.api_key.clone();
        // Serialise once; clone bytes on each retry attempt.
        let body_bytes = serde_json::to_vec(body)
            .map_err(|e| ApiError::Server(format!("JSON serialization failed: {e}")))?;
        let resp = self
            .send_with_retry(|| {
                self.client
                    .post(&url)
                    .header("x-api-key", &key)
                    .header("content-type", "application/json")
                    .body(body_bytes.clone())
            })
            .await?;
        self.handle_response(resp).await
    }

    pub async fn patch<B: Serialize, T: DeserializeOwned>(
        &self,
        path: &str,
        body: &B,
    ) -> Result<T, ApiError> {
        let url = format!("{}{}", self.base_url, path);
        let key = self.api_key.clone();
        let body_bytes = serde_json::to_vec(body)
            .map_err(|e| ApiError::Server(format!("JSON serialization failed: {e}")))?;
        let resp = self
            .send_with_retry(|| {
                self.client
                    .patch(&url)
                    .header("x-api-key", &key)
                    .header("content-type", "application/json")
                    .body(body_bytes.clone())
            })
            .await?;
        self.handle_response(resp).await
    }

    pub async fn put<B: Serialize, T: DeserializeOwned>(
        &self,
        path: &str,
        body: &B,
    ) -> Result<T, ApiError> {
        let url = format!("{}{}", self.base_url, path);
        let key = self.api_key.clone();
        let body_bytes = serde_json::to_vec(body)
            .map_err(|e| ApiError::Server(format!("JSON serialization failed: {e}")))?;
        let resp = self
            .send_with_retry(|| {
                self.client
                    .put(&url)
                    .header("x-api-key", &key)
                    .header("content-type", "application/json")
                    .body(body_bytes.clone())
            })
            .await?;
        self.handle_response(resp).await
    }

    pub async fn delete(&self, path: &str) -> Result<(), ApiError> {
        let url = format!("{}{}", self.base_url, path);
        let key = self.api_key.clone();
        let resp = self
            .send_with_retry(|| self.client.delete(&url).header("x-api-key", &key))
            .await?;
        if resp.status().is_success() {
            Ok(())
        } else {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            Err(self.status_to_error(status, body))
        }
    }

    /// Resolve a short ID prefix to a full UUID by scanning the table.
    /// If the input is already a full UUID (36 chars), returns it as-is.
    pub async fn resolve_id(&self, table: &str, prefix: &str) -> Result<String, ApiError> {
        // Full UUID — pass through
        if prefix.len() == 36 && prefix.contains('-') {
            return Ok(prefix.to_string());
        }

        let resp: DataWrapper<Vec<serde_json::Value>> = self.get(&format!("/v1/{table}")).await?;
        let matches: Vec<&str> = resp
            .data
            .iter()
            .filter_map(|row| row["id"].as_str())
            .filter(|id| id.starts_with(prefix))
            .collect();

        match matches.len() {
            0 => Err(ApiError::NotFound(format!(
                "No {table} row matching prefix '{prefix}'"
            ))),
            1 => Ok(matches[0].to_string()),
            n => Err(ApiError::Validation(format!(
                "Ambiguous prefix '{prefix}' matches {n} rows in {table}. Use more characters."
            ))),
        }
    }

    // ── Internal helpers ───────────────────────────────────────────────

    /// Send a request built by `build_fn`, retrying on transient failures.
    ///
    /// `build_fn` is called once per attempt so that bodies can be re-used
    /// (reqwest `RequestBuilder` is consumed by `.send()`).
    async fn send_with_retry(
        &self,
        build_fn: impl Fn() -> RequestBuilder,
    ) -> Result<reqwest::Response, ApiError> {
        let mut attempt = 0u32;
        loop {
            attempt += 1;
            match build_fn().send().await {
                Ok(resp) => {
                    let status = resp.status();
                    // Retry on 429 (rate-limit) or 5xx transient errors.
                    if (status == StatusCode::TOO_MANY_REQUESTS || status.is_server_error())
                        && attempt < MAX_ATTEMPTS
                    {
                        let delay_ms = BACKOFF_BASE_MS * (1 << (attempt - 1));
                        tracing::warn!(
                            attempt,
                            status = status.as_u16(),
                            delay_ms,
                            "Retryable HTTP response — backing off"
                        );
                        tokio::time::sleep(Duration::from_millis(delay_ms)).await;
                        continue;
                    }
                    return Ok(resp);
                }
                Err(e) => {
                    let api_err = ApiError::Http(e);
                    if api_err.is_retryable() && attempt < MAX_ATTEMPTS {
                        let delay_ms = BACKOFF_BASE_MS * (1 << (attempt - 1));
                        tracing::warn!(
                            attempt,
                            delay_ms,
                            error = %api_err,
                            "Transient network error — backing off"
                        );
                        tokio::time::sleep(Duration::from_millis(delay_ms)).await;
                        continue;
                    }
                    tracing::error!(attempt, error = %api_err, "HTTP request failed");
                    return Err(api_err);
                }
            }
        }
    }

    async fn handle_response<T: DeserializeOwned>(
        &self,
        resp: reqwest::Response,
    ) -> Result<T, ApiError> {
        let status = resp.status();
        if status.is_success() {
            resp.json::<T>().await.map_err(ApiError::Http)
        } else {
            let body = resp.text().await.unwrap_or_default();
            Err(self.status_to_error(status, body))
        }
    }

    fn status_to_error(&self, status: StatusCode, body: String) -> ApiError {
        match status.as_u16() {
            401 => ApiError::Auth(format!(
                "Unauthorized (401): {body}\n  → Check your API key. Admin ops need sk_admin_*, data ops need sk_live_*"
            )),
            402 => ApiError::Auth(format!(
                "Payment required (402): {body}\n  → API quota exceeded. Check usage_metrics or bump the plan tier."
            )),
            403 => ApiError::Auth(format!(
                "Forbidden (403): {body}\n  → Key lacks permission. sk_live_ keys can't do DDL; sk_admin_ can't access project data."
            )),
            404 => ApiError::NotFound(format!(
                "Not found (404): {body}\n  → Table may not exist, or the route may be shadowed by a platform route."
            )),
            422 => ApiError::Validation(body),
            _ => ApiError::Server(format!("{status}: {body}")),
        }
    }
}
