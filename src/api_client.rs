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

        let api_key = key_override
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
            .ok_or("No API key (--key, KAPABLE_ADMIN_API_KEY, or .epic-runner/config.toml)")?;

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

    fn status_to_error(&self, status: StatusCode, body: String) -> ApiError {
        match status.as_u16() {
            401 | 403 => ApiError::Auth(body),
            404 => ApiError::NotFound(body),
            422 => ApiError::Validation(body),
            _ => ApiError::Server(format!("{status}: {body}")),
        }
    }
}
