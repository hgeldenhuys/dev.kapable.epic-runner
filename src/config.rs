use serde::Deserialize;

#[derive(Deserialize, Default, Debug, Clone)]
pub struct EpicRunnerConfig {
    pub api: Option<ApiSection>,
    pub project: Option<ProjectSection>,
    pub deploy: Option<DeploySection>,
}

#[derive(Deserialize, Debug, Clone)]
pub struct ApiSection {
    pub base_url: Option<String>,
    pub api_key: Option<String>,
}

#[derive(Deserialize, Debug, Clone)]
pub struct ProjectSection {
    pub project_id: Option<String>,
    pub product: Option<String>,
    /// Project-scoped data key (sk_live_*) for Data API + _meta operations
    pub data_key: Option<String>,
    /// Flow ID for ceremony sequence (optional — uses default if absent)
    pub ceremony_flow_id: Option<String>,
}

#[derive(Deserialize, Debug, Clone)]
pub struct DeploySection {
    /// Connect App Pipeline app ID (UUID)
    pub app_id: Option<String>,
    /// Admin API key for triggering deploys
    pub api_key: Option<String>,
    /// Platform API URL (default: https://api.kapable.dev)
    pub api_url: Option<String>,
    /// Deploy timeout in seconds (default: 300)
    pub timeout_secs: Option<u64>,
    /// Health check URL to verify after deploy
    pub health_url: Option<String>,
}

impl EpicRunnerConfig {
    pub fn api_key(&self) -> Option<&str> {
        self.api.as_ref()?.api_key.as_deref()
    }
    pub fn base_url(&self) -> Option<&str> {
        self.api.as_ref()?.base_url.as_deref()
    }
    pub fn project_id(&self) -> Option<&str> {
        self.project.as_ref()?.project_id.as_deref()
    }
    pub fn data_key(&self) -> Option<&str> {
        self.project.as_ref()?.data_key.as_deref()
    }
    pub fn ceremony_flow_id(&self) -> Option<&str> {
        self.project.as_ref()?.ceremony_flow_id.as_deref()
    }
    pub fn deploy_app_id(&self) -> Option<&str> {
        self.deploy.as_ref()?.app_id.as_deref()
    }
    pub fn deploy_api_key(&self) -> Option<&str> {
        self.deploy.as_ref()?.api_key.as_deref()
    }
    pub fn deploy_api_url(&self) -> Option<&str> {
        self.deploy.as_ref()?.api_url.as_deref()
    }
}

pub fn read_config(path: &str) -> Result<EpicRunnerConfig, Box<dyn std::error::Error>> {
    let content = std::fs::read_to_string(path)?;
    let config: EpicRunnerConfig = toml::from_str(&content)?;
    Ok(config)
}

/// Walk up from CWD looking for `.epic-runner/config.toml`, stopping at `.git` boundary.
pub fn find_project_config() -> Option<String> {
    let mut dir = std::env::current_dir().ok()?;
    loop {
        let config = dir.join(".epic-runner/config.toml");
        if config.exists() {
            return Some(config.to_string_lossy().to_string());
        }
        if dir.join(".git").exists() {
            return None;
        }
        if !dir.pop() {
            return None;
        }
    }
}

pub fn home_config_path() -> Option<String> {
    let home = std::env::var("HOME").ok()?;
    let path = format!("{home}/.epic-runner/config.toml");
    if std::path::Path::new(&path).exists() {
        Some(path)
    } else {
        None
    }
}

/// Resolve project_id from config cascade.
pub fn resolve_project_id() -> Result<String, Box<dyn std::error::Error>> {
    if let Some(path) = find_project_config() {
        if let Ok(config) = read_config(&path) {
            if let Some(pid) = config.project_id() {
                return Ok(pid.to_string());
            }
        }
    }
    if let Some(path) = home_config_path() {
        if let Ok(config) = read_config(&path) {
            if let Some(pid) = config.project_id() {
                return Ok(pid.to_string());
            }
        }
    }
    Err("No project_id configured. Run `epic-runner init` first.".into())
}
