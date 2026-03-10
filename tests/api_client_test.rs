use std::io::Write;

#[test]
fn read_config_parses_full_toml() {
    let mut tmp = tempfile::NamedTempFile::new().unwrap();
    writeln!(
        tmp,
        r#"
[api]
base_url = "https://example.com"
api_key = "sk_test_123"

[project]
project_id = "abc-123"
product = "clowder"
ceremony_flow_id = "flow-uuid-here"
"#
    )
    .unwrap();
    let content = std::fs::read_to_string(tmp.path()).unwrap();
    let config: epic_runner::config::EpicRunnerConfig = toml::from_str(&content).unwrap();
    assert_eq!(config.base_url(), Some("https://example.com"));
    assert_eq!(config.api_key(), Some("sk_test_123"));
    assert_eq!(config.project_id(), Some("abc-123"));
    assert_eq!(config.ceremony_flow_id(), Some("flow-uuid-here"));
}

#[test]
fn read_config_handles_minimal_toml() {
    let mut tmp = tempfile::NamedTempFile::new().unwrap();
    writeln!(
        tmp,
        r#"
[api]
api_key = "sk_min"
"#
    )
    .unwrap();
    let content = std::fs::read_to_string(tmp.path()).unwrap();
    let config: epic_runner::config::EpicRunnerConfig = toml::from_str(&content).unwrap();
    assert_eq!(config.api_key(), Some("sk_min"));
    assert_eq!(config.base_url(), None);
    assert_eq!(config.project_id(), None);
}

#[test]
fn empty_config_has_none_values() {
    let config = epic_runner::config::EpicRunnerConfig::default();
    assert_eq!(config.api_key(), None);
    assert_eq!(config.base_url(), None);
    assert_eq!(config.project_id(), None);
    assert_eq!(config.ceremony_flow_id(), None);
}
