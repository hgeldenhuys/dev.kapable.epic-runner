use crate::types::JudgeVerdict;

/// Result of evaluating a judge verdict — richer than a simple bool.
/// Allows the orchestrator to distinguish between "needs more implementation work"
/// and "code is done but deploy/browser verification is pending."
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VerdictResult {
    /// Epic intent is fully satisfied — close the epic.
    Satisfied,
    /// Code quality passes but browser/deploy ACs could not be verified.
    /// The orchestrator should NOT create a full implementation sprint.
    Provisional,
    /// More implementation work is needed — create next sprint.
    NotSatisfied,
}

/// Parse a JudgeVerdict from LLM output text.
///
/// Handles: bare JSON, markdown-fenced JSON, JSON with preamble/trailing text.
pub fn parse_verdict(text: Option<&str>) -> Option<JudgeVerdict> {
    let text = text?;
    let value = crate::json_extract::extract_json_object(text)?;
    serde_json::from_value::<JudgeVerdict>(value).ok()
}

/// Evaluate intent satisfaction from judge verdict.
/// Returns a three-state result: Satisfied, Provisional, or NotSatisfied.
///
/// Priority:
/// 1. intent_satisfied == true → Satisfied
/// 2. mission_progress >= 100 → Satisfied
/// 3. provisional == true → Provisional (code OK, deploy/browser pending)
/// 4. Otherwise → NotSatisfied
pub fn evaluate_verdict(verdict: &Option<JudgeVerdict>) -> VerdictResult {
    match verdict {
        Some(v) => {
            if v.intent_satisfied {
                return VerdictResult::Satisfied;
            }
            // Fallback: if mission_progress is 100%, treat as satisfied
            if let Some(progress) = v.mission_progress {
                if progress >= 100.0 {
                    return VerdictResult::Satisfied;
                }
            }
            // Provisional: code is fine but deploy/browser ACs can't be verified
            if v.provisional == Some(true) {
                return VerdictResult::Provisional;
            }
            VerdictResult::NotSatisfied
        }
        None => VerdictResult::NotSatisfied,
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
            provisional: None,
        };
        assert_eq!(
            evaluate_verdict(&Some(not_satisfied)),
            VerdictResult::NotSatisfied
        );

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
            provisional: None,
        };
        assert_eq!(evaluate_verdict(&Some(satisfied)), VerdictResult::Satisfied);
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
            provisional: None,
        };
        assert_eq!(
            evaluate_verdict(&Some(full_progress)),
            VerdictResult::Satisfied
        );
    }

    #[test]
    fn evaluate_none_returns_not_satisfied() {
        assert_eq!(evaluate_verdict(&None), VerdictResult::NotSatisfied);
    }

    #[test]
    fn evaluate_provisional_verdict() {
        // Code quality passes but deploy/browser ACs can't be verified
        let provisional = JudgeVerdict {
            intent_satisfied: false,
            confidence: 0.0,
            criteria_results: vec![],
            summary: "Code looks correct but cannot verify browser ACs — deploy was skipped"
                .to_string(),
            evidence: vec![],
            mission_progress: Some(90.0),
            deploy_ready: None,
            delta_stories: None,
            stories_completed: None,
            stories_to_regroom: None,
            action_items: None,
            changed_files: None,
            next_sprint_goal: None,
            story_updates: None,
            provisional: Some(true),
        };
        assert_eq!(
            evaluate_verdict(&Some(provisional)),
            VerdictResult::Provisional
        );
    }

    #[test]
    fn evaluate_provisional_false_is_not_satisfied() {
        // provisional: false should behave like not_satisfied
        let not_provisional = JudgeVerdict {
            intent_satisfied: false,
            confidence: 0.0,
            criteria_results: vec![],
            summary: "needs work".to_string(),
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
            provisional: Some(false),
        };
        assert_eq!(
            evaluate_verdict(&Some(not_provisional)),
            VerdictResult::NotSatisfied
        );
    }

    #[test]
    fn parse_verdict_with_provisional_field() {
        let json = r#"{"intent_satisfied":false,"confidence":0.0,"criteria_results":[],"summary":"Code OK, deploy pending","evidence":[],"provisional":true}"#;
        let v = parse_verdict(Some(json)).unwrap();
        assert!(!v.intent_satisfied);
        assert_eq!(v.provisional, Some(true));
    }
}
