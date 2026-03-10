use epic_runner::stream::*;

#[test]
fn parses_system_event() {
    let line = r#"{"type":"system","subtype":"init","session_id":"abc-123"}"#;
    let event = parse_line(line).unwrap();
    assert!(matches!(event, StreamEvent::System { subtype, .. } if subtype == "init"));
}

#[test]
fn parses_result_event() {
    let line = r#"{"type":"result","result":"All done","session_id":"abc-123","cost_usd":0.05}"#;
    let event = parse_line(line).unwrap();
    assert_eq!(extract_result(&event), Some("All done"));
}

#[test]
fn parses_assistant_text_event() {
    let line = r#"{"type":"assistant","message":{"content":[{"type":"text","text":"Hello"}]}}"#;
    let event = parse_line(line).unwrap();
    assert!(matches!(event, StreamEvent::Assistant { .. }));
}

#[test]
fn parses_assistant_tool_use() {
    let line = r#"{"type":"assistant","message":{"content":[{"type":"tool_use","id":"t1","name":"Read","input":{}}]}}"#;
    let event = parse_line(line).unwrap();
    if let StreamEvent::Assistant { message } = event {
        assert!(
            matches!(&message.content[0], ContentBlock::ToolUse { name, .. } if name == "Read")
        );
    } else {
        panic!("Expected Assistant event");
    }
}

#[test]
fn empty_line_returns_none() {
    assert!(parse_line("").is_none());
    assert!(parse_line("   ").is_none());
}

#[test]
fn invalid_json_returns_none() {
    assert!(parse_line("not json").is_none());
}
