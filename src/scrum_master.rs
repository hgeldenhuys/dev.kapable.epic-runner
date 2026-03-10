use serde::{Deserialize, Serialize};

/// SM observation recorded during/after a sprint.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SmObservation {
    pub category: ObservationCategory,
    pub description: String,
    pub severity: Severity,
    pub action_item: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum ObservationCategory {
    Process,
    Technical,
    Communication,
    Quality,
    Velocity,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum Severity {
    Low,
    Medium,
    High,
    Critical,
}

/// Sprint retrospective output.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RetroOutput {
    pub went_well: Vec<String>,
    pub friction_points: Vec<String>,
    pub action_items: Vec<String>,
    pub discovered_work: Vec<String>,
    pub observations: Vec<SmObservation>,
}

/// Parse SM retro output from LLM response.
pub fn parse_retro(text: Option<&str>) -> Option<RetroOutput> {
    let text = text?;
    // Try direct parse
    if let Ok(r) = serde_json::from_str::<RetroOutput>(text) {
        return Some(r);
    }
    // Try stripping markdown fences
    let stripped = text
        .trim()
        .strip_prefix("```json")
        .or_else(|| text.trim().strip_prefix("```"))
        .and_then(|s| s.strip_suffix("```"))
        .unwrap_or(text);
    serde_json::from_str::<RetroOutput>(stripped.trim()).ok()
}

/// Detect friction patterns across multiple retros.
pub fn detect_recurring_friction(retros: &[RetroOutput]) -> Vec<String> {
    use std::collections::HashMap;
    let mut friction_counts: HashMap<String, usize> = HashMap::new();
    for retro in retros {
        for friction in &retro.friction_points {
            let key = friction.to_lowercase();
            *friction_counts.entry(key).or_insert(0) += 1;
        }
    }
    let mut recurring: Vec<String> = friction_counts
        .into_iter()
        .filter(|(_, count)| *count >= 2)
        .map(|(key, count)| format!("{key} (x{count})"))
        .collect();
    recurring.sort();
    recurring
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_retro_from_json() {
        let json = r#"{"went_well":["fast"],"friction_points":["slow tests"],"action_items":["fix CI"],"discovered_work":["new bug"],"observations":[]}"#;
        let r = parse_retro(Some(json)).unwrap();
        assert_eq!(r.went_well, vec!["fast"]);
        assert_eq!(r.friction_points, vec!["slow tests"]);
    }

    #[test]
    fn detect_recurring_friction_works() {
        let retros = vec![
            RetroOutput {
                went_well: vec![],
                friction_points: vec!["slow tests".to_string(), "flaky CI".to_string()],
                action_items: vec![],
                discovered_work: vec![],
                observations: vec![],
            },
            RetroOutput {
                went_well: vec![],
                friction_points: vec!["slow tests".to_string()],
                action_items: vec![],
                discovered_work: vec![],
                observations: vec![],
            },
        ];
        let recurring = detect_recurring_friction(&retros);
        assert_eq!(recurring.len(), 1);
        assert!(recurring[0].contains("slow tests"));
    }
}
