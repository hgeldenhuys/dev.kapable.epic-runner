#[test]
fn story_status_serializes_snake_case() {
    let status = epic_runner::types::StoryStatus::InProgress;
    let json = serde_json::to_string(&status).unwrap();
    assert_eq!(json, r#""in_progress""#);
    let back: epic_runner::types::StoryStatus = serde_json::from_str(&json).unwrap();
    assert_eq!(back, epic_runner::types::StoryStatus::InProgress);
}

#[test]
fn supervisor_action_round_trips() {
    let action = epic_runner::types::SupervisorAction::ResumeWithRubberDuck;
    let json = serde_json::to_string(&action).unwrap();
    let back: epic_runner::types::SupervisorAction = serde_json::from_str(&json).unwrap();
    assert_eq!(
        back,
        epic_runner::types::SupervisorAction::ResumeWithRubberDuck
    );
}

#[test]
fn epic_status_round_trips() {
    let status = epic_runner::types::EpicStatus::Active;
    let json = serde_json::to_string(&status).unwrap();
    assert_eq!(json, r#""active""#);
    let back: epic_runner::types::EpicStatus = serde_json::from_str(&json).unwrap();
    assert_eq!(back, epic_runner::types::EpicStatus::Active);
}

#[test]
fn sprint_status_all_variants() {
    let statuses = vec![
        epic_runner::types::SprintStatus::Planning,
        epic_runner::types::SprintStatus::Executing,
        epic_runner::types::SprintStatus::Completed,
        epic_runner::types::SprintStatus::Failed,
    ];
    for status in statuses {
        let json = serde_json::to_string(&status).unwrap();
        let back: epic_runner::types::SprintStatus = serde_json::from_str(&json).unwrap();
        assert_eq!(back, status);
    }
}

#[test]
fn ceremony_status_round_trips() {
    let status = epic_runner::types::CeremonyStatus::Skipped;
    let json = serde_json::to_string(&status).unwrap();
    assert_eq!(json, r#""skipped""#);
}

#[test]
fn impediment_status_round_trips() {
    let status = epic_runner::types::ImpedimentStatus::Acknowledged;
    let json = serde_json::to_string(&status).unwrap();
    assert_eq!(json, r#""acknowledged""#);
}
