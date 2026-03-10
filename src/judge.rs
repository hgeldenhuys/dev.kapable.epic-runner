use crate::types::JudgeVerdict;

/// Parse a JudgeVerdict from LLM output text.
pub fn parse_verdict(text: Option<&str>) -> Option<JudgeVerdict> {
    let text = text?;
    // Try direct parse
    if let Ok(v) = serde_json::from_str::<JudgeVerdict>(text) {
        return Some(v);
    }
    // Try stripping markdown fences
    let stripped = text
        .trim()
        .strip_prefix("```json")
        .or_else(|| text.trim().strip_prefix("```"))
        .and_then(|s| s.strip_suffix("```"))
        .unwrap_or(text);
    serde_json::from_str::<JudgeVerdict>(stripped.trim()).ok()
}

/// Evaluate intent satisfaction: requires both intent_satisfied AND confidence >= 0.7
pub fn evaluate_verdict(verdict: &Option<JudgeVerdict>) -> bool {
    match verdict {
        Some(v) => v.intent_satisfied && v.confidence >= 0.7,
        None => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_verdict_from_json() {
        let json = r#"{"intent_satisfied":true,"confidence":0.85,"criteria_results":[],"summary":"All good","evidence":["screenshot1"]}"#;
        let v = parse_verdict(Some(json)).unwrap();
        assert!(v.intent_satisfied);
        assert_eq!(v.confidence, 0.85);
    }

    #[test]
    fn parse_verdict_from_markdown_fenced() {
        let text = "```json\n{\"intent_satisfied\":false,\"confidence\":0.3,\"criteria_results\":[],\"summary\":\"Failed\",\"evidence\":[]}\n```";
        let v = parse_verdict(Some(text)).unwrap();
        assert!(!v.intent_satisfied);
    }

    #[test]
    fn parse_verdict_returns_none_for_garbage() {
        assert!(parse_verdict(Some("not json at all")).is_none());
    }

    #[test]
    fn evaluate_requires_both_satisfied_and_confidence() {
        let low_confidence = JudgeVerdict {
            intent_satisfied: true,
            confidence: 0.5,
            criteria_results: vec![],
            summary: "ok".to_string(),
            evidence: vec![],
        };
        assert!(!evaluate_verdict(&Some(low_confidence)));

        let not_satisfied = JudgeVerdict {
            intent_satisfied: false,
            confidence: 0.9,
            criteria_results: vec![],
            summary: "no".to_string(),
            evidence: vec![],
        };
        assert!(!evaluate_verdict(&Some(not_satisfied)));

        let good = JudgeVerdict {
            intent_satisfied: true,
            confidence: 0.8,
            criteria_results: vec![],
            summary: "yes".to_string(),
            evidence: vec![],
        };
        assert!(evaluate_verdict(&Some(good)));
    }

    #[test]
    fn evaluate_none_returns_false() {
        assert!(!evaluate_verdict(&None));
    }
}
