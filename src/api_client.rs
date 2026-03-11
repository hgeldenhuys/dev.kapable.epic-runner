use reqwest::{Client, StatusCode};
use serde::{de::DeserializeOwned, Deserialize, Serialize};

#[derive(Deserialize)]
pub struct DataWrapper<T> {
    pub data: T,
}

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

pub const DEFAULT_API_URL: &str = "https://api.kapable.dev";

impl ApiClient {
    /// Create a client with explicit URL and key (no config cascade).
    pub fn new(base_url: &str, api_key: &str) -> Self {
        Self {
            client: Client::new(),
            base_url: base_url.to_string(),
            api_key: api_key.to_string(),
        }
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
            client: Client::new(),
            base_url,
            api_key,
        })
    }

    pub async fn get<T: DeserializeOwned>(&self, path: &str) -> Result<T, ApiError> {
        let resp = self
            .client
            .get(format!("{}{}", self.base_url, path))
            .header("x-api-key", &self.api_key)
            .send()
            .await?;
        self.handle_response(resp).await
    }

    pub async fn post<B: Serialize, T: DeserializeOwned>(
        &self,
        path: &str,
        body: &B,
    ) -> Result<T, ApiError> {
        let resp = self
            .client
            .post(format!("{}{}", self.base_url, path))
            .header("x-api-key", &self.api_key)
            .json(body)
            .send()
            .await?;
        self.handle_response(resp).await
    }

    pub async fn patch<B: Serialize, T: DeserializeOwned>(
        &self,
        path: &str,
        body: &B,
    ) -> Result<T, ApiError> {
        let resp = self
            .client
            .patch(format!("{}{}", self.base_url, path))
            .header("x-api-key", &self.api_key)
            .json(body)
            .send()
            .await?;
        self.handle_response(resp).await
    }

    pub async fn put<B: Serialize, T: DeserializeOwned>(
        &self,
        path: &str,
        body: &B,
    ) -> Result<T, ApiError> {
        let resp = self
            .client
            .put(format!("{}{}", self.base_url, path))
            .header("x-api-key", &self.api_key)
            .json(body)
            .send()
            .await?;
        self.handle_response(resp).await
    }

    pub async fn delete(&self, path: &str) -> Result<(), ApiError> {
        let resp = self
            .client
            .delete(format!("{}{}", self.base_url, path))
            .header("x-api-key", &self.api_key)
            .send()
            .await?;
        if resp.status().is_success() {
            Ok(())
        } else {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            Err(self.status_to_error(status, body))
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

    /// Resolve a short ID prefix to a full UUID by scanning the table.
    /// If the input is already a full UUID (36 chars), returns it as-is.
    pub async fn resolve_id(&self, table: &str, prefix: &str) -> Result<String, ApiError> {
        // Full UUID — pass through
        if prefix.len() == 36 && prefix.contains('-') {
            return Ok(prefix.to_string());
        }

        let resp: DataWrapper<Vec<serde_json::Value>> =
            self.get(&format!("/v1/{table}")).await?;
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
