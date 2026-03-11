use super::definition::CeremonyFlow;

/// Load ceremony flow from: CLI override → config flow_id → embedded default.
pub async fn load_flow(
    flow_id_override: Option<&str>,
    config_flow_id: Option<&str>,
) -> Result<CeremonyFlow, Box<dyn std::error::Error>> {
    // 1. CLI override: load from file path (YAML)
    if let Some(path) = flow_id_override {
        if std::path::Path::new(path).exists() {
            let yaml = std::fs::read_to_string(path)?;
            return Ok(CeremonyFlow::from_yaml(&yaml)?);
        }
        // If not a file path, treat as flow ID (future: fetch from API)
        tracing::warn!(
            flow_id = path,
            "Flow ID fetch from API not yet implemented — using embedded default"
        );
    }

    // 2. Config flow_id: future — fetch from Kapable Flow API
    if let Some(flow_id) = config_flow_id {
        tracing::warn!(
            flow_id,
            "Flow API fetch not yet implemented — using embedded default"
        );
    }

    // 3. Embedded default
    Ok(CeremonyFlow::default_flow())
}
