use epic_runner::types::{SprintEvent, SprintEventType};
use uuid::Uuid;

#[test]
fn test_event_cost_usd_populated() {
    // NodeCompleted events should carry cost_usd from the executor result
    let event = SprintEvent {
        sprint_id: Uuid::new_v4(),
        event_type: SprintEventType::NodeCompleted,
        node_id: Some("builder".to_string()),
        node_label: Some("Builder".to_string()),
        summary: "Builder → Completed".to_string(),
        detail: Some(serde_json::json!({
            "node_key": "builder",
            "status": "Completed",
            "cost_usd": 0.42,
        })),
        cost_usd: Some(0.42),
        timestamp: chrono::Utc::now(),
    };

    assert!(
        event.cost_usd.is_some(),
        "cost_usd must be set on NodeCompleted events"
    );
    assert_eq!(event.cost_usd.unwrap(), 0.42);

    // Verify serialization includes cost_usd
    let json = serde_json::to_value(&event).unwrap();
    assert!(
        json["cost_usd"].is_number(),
        "cost_usd must serialize as a number"
    );
    assert!((json["cost_usd"].as_f64().unwrap() - 0.42).abs() < f64::EPSILON);
}

#[test]
fn test_event_cost_usd_none_for_non_cost_events() {
    // Started events should have cost_usd = None (serializes as null)
    let event = SprintEvent {
        sprint_id: Uuid::new_v4(),
        event_type: SprintEventType::Started,
        node_id: None,
        node_label: None,
        summary: "Sprint started".to_string(),
        detail: None,
        cost_usd: None,
        timestamp: chrono::Utc::now(),
    };

    assert!(event.cost_usd.is_none());

    let json = serde_json::to_value(&event).unwrap();
    assert!(
        json["cost_usd"].is_null(),
        "cost_usd should be null for non-cost events"
    );
}

#[test]
fn test_sprint_completed_event_carries_total_cost() {
    // The sprint Completed event should carry the total cost
    let total_cost = 1.23;
    let event = SprintEvent {
        sprint_id: Uuid::new_v4(),
        event_type: SprintEventType::Completed,
        node_id: None,
        node_label: None,
        summary: "Sprint 1 completed: mission satisfied".to_string(),
        detail: Some(serde_json::json!({
            "intent_satisfied": true,
            "impediment": false,
            "total_cost_usd": total_cost,
        })),
        cost_usd: Some(total_cost),
        timestamp: chrono::Utc::now(),
    };

    assert_eq!(event.cost_usd, Some(1.23));

    let json = serde_json::to_value(&event).unwrap();
    assert!((json["cost_usd"].as_f64().unwrap() - 1.23).abs() < f64::EPSILON);
}

#[test]
fn test_event_cost_usd_round_trips_through_serde() {
    let event = SprintEvent {
        sprint_id: Uuid::new_v4(),
        event_type: SprintEventType::NodeCompleted,
        node_id: Some("judge".to_string()),
        node_label: Some("Judge".to_string()),
        summary: "Judge → Completed".to_string(),
        detail: None,
        cost_usd: Some(0.0567),
        timestamp: chrono::Utc::now(),
    };

    let json_str = serde_json::to_string(&event).unwrap();
    let deserialized: SprintEvent = serde_json::from_str(&json_str).unwrap();
    assert_eq!(deserialized.cost_usd, Some(0.0567));
}
