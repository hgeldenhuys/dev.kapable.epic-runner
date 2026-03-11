use super::definition::CeremonyFlow;

/// Path where per-epic patched flows are persisted between sprints.
/// Format: `.epic-runner/flows/{epic_code}.yaml`
pub fn epic_flow_path(epic_code: &str) -> std::path::PathBuf {
    std::path::PathBuf::from(format!(".epic-runner/flows/{}.yaml", epic_code))
}

/// Load ceremony flow with cascade:
/// 1. CLI override (file path)
/// 2. Per-epic patched flow (`.epic-runner/flows/{epic_code}.yaml`)
/// 3. Config flow_id (future: API fetch)
/// 4. Embedded default
pub async fn load_flow(
    flow_id_override: Option<&str>,
    config_flow_id: Option<&str>,
    epic_code: Option<&str>,
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

    // 2. Per-epic patched flow (saved by SM between sprints)
    if let Some(code) = epic_code {
        let path = epic_flow_path(code);
        if path.exists() {
            let yaml = std::fs::read_to_string(&path)?;
            let flow = CeremonyFlow::from_yaml(&yaml)?;
            tracing::info!(
                epic_code = code,
                path = %path.display(),
                "Loaded per-epic patched flow"
            );
            return Ok(flow);
        }
    }

    // 3. Config flow_id: future — fetch from Kapable Flow API
    if let Some(flow_id) = config_flow_id {
        tracing::warn!(
            flow_id,
            "Flow API fetch not yet implemented — using embedded default"
        );
    }

    // 4. Embedded default
    Ok(CeremonyFlow::default_flow())
}

/// Save a patched flow for a specific epic.
/// Called by the orchestrator between sprints when SM recommends patches.
pub fn save_epic_flow(
    epic_code: &str,
    flow: &CeremonyFlow,
) -> Result<(), Box<dyn std::error::Error>> {
    let path = epic_flow_path(epic_code);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let yaml = serde_yaml::to_string(flow)?;
    std::fs::write(&path, &yaml)?;
    tracing::info!(
        epic_code,
        path = %path.display(),
        "Saved patched ceremony flow for next sprint"
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn epic_flow_path_format() {
        let path = epic_flow_path("AUTH-001");
        assert_eq!(path.to_string_lossy(), ".epic-runner/flows/AUTH-001.yaml");
    }

    /// Combined test for save/load/cascade — uses a single set_current_dir to avoid
    /// race conditions with parallel test execution.
    #[tokio::test]
    async fn save_load_and_cascade() {
        let dir = tempfile::tempdir().unwrap();
        let original_dir = std::env::current_dir().unwrap();
        std::env::set_current_dir(dir.path()).unwrap();

        // Part 1: save_epic_flow + read back
        let flow = CeremonyFlow::default_flow();
        save_epic_flow("TEST-001", &flow).unwrap();

        let path = epic_flow_path("TEST-001");
        assert!(path.exists());

        let loaded_yaml = std::fs::read_to_string(&path).unwrap();
        let loaded = CeremonyFlow::from_yaml(&loaded_yaml).unwrap();
        assert_eq!(loaded.nodes.len(), flow.nodes.len());
        assert_eq!(loaded.edges.len(), flow.edges.len());

        // Part 2: load_flow cascade picks up epic-specific flow
        let mut patched = CeremonyFlow::default_flow();
        patched.name = "Patched Flow".to_string();
        save_epic_flow("PATCH-001", &patched).unwrap();

        let loaded = load_flow(None, None, Some("PATCH-001")).await.unwrap();
        assert_eq!(loaded.name, "Patched Flow");

        // Part 3: load_flow without epic_code → embedded default
        let default = load_flow(None, None, None).await.unwrap();
        assert_eq!(default.name, "Default Sprint Ceremony");

        std::env::set_current_dir(original_dir).unwrap();
    }
}
