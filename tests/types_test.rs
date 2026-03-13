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
        epic_runner::types::SprintStatus::Cancelled,
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

#[test]
fn parked_status_serializes_as_parked() {
    let status = epic_runner::types::StoryStatus::Parked;
    let json = serde_json::to_string(&status).unwrap();
    assert_eq!(json, r#""parked""#);
}

#[test]
fn parked_status_deserializes_from_parked() {
    let status: epic_runner::types::StoryStatus = serde_json::from_str(r#""parked""#).unwrap();
    assert_eq!(status, epic_runner::types::StoryStatus::Parked);
}

#[test]
fn parked_status_display_is_parked() {
    let status = epic_runner::types::StoryStatus::Parked;
    assert_eq!(status.to_string(), "parked");
}

#[test]
fn parked_story_excluded_from_sprint_eligibility() {
    use epic_runner::types::StoryStatus;
    assert!(
        !StoryStatus::Parked.is_eligible_for_sprint(),
        "Parked stories must NOT be eligible for sprint selection"
    );
}

#[test]
fn parked_and_blocked_both_excluded_from_sprint() {
    use epic_runner::types::StoryStatus;
    // Parked = choosing not to yet; Blocked = want to but can't.
    // Neither should be auto-selected by the orchestrator.
    assert!(!StoryStatus::Parked.is_eligible_for_sprint());
    assert!(!StoryStatus::Blocked.is_eligible_for_sprint());
    assert!(!StoryStatus::Done.is_eligible_for_sprint());
    assert!(!StoryStatus::Deployed.is_eligible_for_sprint());
    assert!(!StoryStatus::InProgress.is_eligible_for_sprint());
}

#[test]
fn parked_eligible_statuses_are_only_ready_planned_draft() {
    use epic_runner::types::StoryStatus;
    // Whitelist: only these three are eligible
    assert!(StoryStatus::Ready.is_eligible_for_sprint());
    assert!(StoryStatus::Planned.is_eligible_for_sprint());
    assert!(StoryStatus::Draft.is_eligible_for_sprint());
}
