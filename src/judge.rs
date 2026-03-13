use crate::types::JudgeVerdict;

/// Parse a JudgeVerdict from LLM output text.
///
/// Handles: bare JSON, markdown-fenced JSON, JSON with preamble/trailing text.
pub fn parse_verdict(text: Option<&str>) -> Option<JudgeVerdict> {
    let text = text?;
    let value = crate::json_extract::extract_json_object(text)?;
    serde_json::from_value::<JudgeVerdict>(value).ok()
}

/// Evaluate intent satisfaction from judge verdict.
/// The judge's `intent_satisfied` field is the primary signal.
/// Falls back to mission_progress >= 100 if intent_satisfied is false but progress is complete.
pub fn evaluate_verdict(verdict: &Option<JudgeVerdict>) -> bool {
    match verdict {
        Some(v) => {
            if v.intent_satisfied {
                return true;
            }
            // Fallback: if mission_progress is 100%, treat as satisfied
            if let Some(progress) = v.mission_progress {
                return progress >= 100.0;
            }
            false
        }
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
    fn parse_verdict_from_preamble_and_fenced() {
        let text = "Here's my verdict:\n\n```json\n{\"intent_satisfied\":true,\"confidence\":0.9,\"criteria_results\":[],\"summary\":\"Good\",\"evidence\":[]}\n```\n\nOverall great sprint.";
        let v = parse_verdict(Some(text)).unwrap();
        assert!(v.intent_satisfied);
        assert_eq!(v.confidence, 0.9);
    }

    #[test]
    fn parse_verdict_returns_none_for_garbage() {
        assert!(parse_verdict(Some("not json at all")).is_none());
    }

    #[test]
    fn evaluate_uses_intent_satisfied() {
        let not_satisfied = JudgeVerdict {
            intent_satisfied: false,
            confidence: 0.0,
            criteria_results: vec![],
            summary: "no".to_string(),
            evidence: vec![],
            mission_progress: Some(50.0),
            deploy_ready: None,
            delta_stories: None,
            stories_completed: None,
            stories_to_regroom: None,
            action_items: None,
            changed_files: None,
            next_sprint_goal: None,
            story_updates: None,
        };
        assert!(!evaluate_verdict(&Some(not_satisfied)));

        let satisfied = JudgeVerdict {
            intent_satisfied: true,
            confidence: 0.0,
            criteria_results: vec![],
            summary: "yes".to_string(),
            evidence: vec![],
            mission_progress: Some(100.0),
            deploy_ready: None,
            delta_stories: None,
            stories_completed: None,
            stories_to_regroom: None,
            action_items: None,
            changed_files: None,
            next_sprint_goal: None,
            story_updates: None,
        };
        assert!(evaluate_verdict(&Some(satisfied)));
    }

    #[test]
    fn evaluate_falls_back_to_mission_progress() {
        // intent_satisfied is false but mission_progress is 100 → satisfied
        let full_progress = JudgeVerdict {
            intent_satisfied: false,
            confidence: 0.0,
            criteria_results: vec![],
            summary: "done via progress".to_string(),
            evidence: vec![],
            mission_progress: Some(100.0),
            deploy_ready: None,
            delta_stories: None,
            stories_completed: None,
            stories_to_regroom: None,
            action_items: None,
            changed_files: None,
            next_sprint_goal: None,
            story_updates: None,
        };
        assert!(evaluate_verdict(&Some(full_progress)));
    }

    #[test]
    fn evaluate_none_returns_false() {
        assert!(!evaluate_verdict(&None));
    }
}
