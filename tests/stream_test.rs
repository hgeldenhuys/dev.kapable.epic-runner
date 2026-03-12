use epic_runner::stream::*;

#[test]
fn parses_system_event() {
    let line = r#"{"type":"system","subtype":"init","session_id":"abc-123"}"#;
    let event = parse_line(line).unwrap();
    assert!(matches!(event, StreamEvent::System { subtype, .. } if subtype == "init"));
}

#[test]
fn parses_result_event_with_total_cost_usd() {
    // Claude Code stream-json uses total_cost_usd since SDK v1.0.22
    let line =
        r#"{"type":"result","result":"All done","session_id":"abc-123","total_cost_usd":0.05}"#;
    let event = parse_line(line).unwrap();
    assert_eq!(extract_result(&event), Some("All done"));
    if let StreamEvent::Result { total_cost_usd, .. } = event {
        assert_eq!(total_cost_usd, Some(0.05));
    }
}

#[test]
fn parses_result_event_with_legacy_cost_usd() {
    // Backwards compat: cost_usd alias still works
    let line = r#"{"type":"result","result":"All done","session_id":"abc-123","cost_usd":0.05}"#;
    let event = parse_line(line).unwrap();
    assert_eq!(extract_result(&event), Some("All done"));
    if let StreamEvent::Result { total_cost_usd, .. } = event {
        assert_eq!(total_cost_usd, Some(0.05));
    }
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

// ── Fuzz / Edge Case Tests ─────────────────────────

#[test]
fn handles_unknown_type_gracefully() {
    // Claude might emit event types we don't recognize
    let line = r#"{"type":"unknown_future_event","data":"something"}"#;
    assert!(parse_line(line).is_none());
}

#[test]
fn handles_missing_required_fields() {
    // system without subtype
    let line = r#"{"type":"system"}"#;
    assert!(parse_line(line).is_none());

    // result without result field
    let line = r#"{"type":"result","session_id":"abc"}"#;
    assert!(parse_line(line).is_none());

    // assistant without message
    let line = r#"{"type":"assistant"}"#;
    assert!(parse_line(line).is_none());
}

#[test]
fn handles_null_cost_usd() {
    let line = r#"{"type":"result","result":"done","session_id":"abc","total_cost_usd":null}"#;
    let event = parse_line(line).unwrap();
    if let StreamEvent::Result { total_cost_usd, .. } = event {
        assert!(total_cost_usd.is_none());
    } else {
        panic!("Expected Result event");
    }
}

#[test]
fn handles_zero_cost() {
    let line = r#"{"type":"result","result":"done","session_id":"abc","total_cost_usd":0.0}"#;
    let event = parse_line(line).unwrap();
    if let StreamEvent::Result { total_cost_usd, .. } = event {
        assert_eq!(total_cost_usd, Some(0.0));
    } else {
        panic!("Expected Result event");
    }
}

#[test]
fn handles_large_cost() {
    let line = r#"{"type":"result","result":"done","session_id":"abc","total_cost_usd":999.99}"#;
    let event = parse_line(line).unwrap();
    if let StreamEvent::Result { total_cost_usd, .. } = event {
        assert_eq!(total_cost_usd, Some(999.99));
    } else {
        panic!("Expected Result event");
    }
}

#[test]
fn handles_empty_content_blocks() {
    let line = r#"{"type":"assistant","message":{"content":[]}}"#;
    let event = parse_line(line).unwrap();
    if let StreamEvent::Assistant { message } = event {
        assert!(message.content.is_empty());
    } else {
        panic!("Expected Assistant event");
    }
}

#[test]
fn handles_multiple_content_blocks() {
    let line = r#"{"type":"assistant","message":{"content":[{"type":"text","text":"thinking..."},{"type":"tool_use","id":"t1","name":"Bash","input":{"command":"ls"}}]}}"#;
    let event = parse_line(line).unwrap();
    if let StreamEvent::Assistant { message } = event {
        assert_eq!(message.content.len(), 2);
        assert!(
            matches!(&message.content[0], ContentBlock::Text { text } if text == "thinking...")
        );
        assert!(
            matches!(&message.content[1], ContentBlock::ToolUse { name, .. } if name == "Bash")
        );
    } else {
        panic!("Expected Assistant event");
    }
}

#[test]
fn handles_tool_result_block() {
    let line = r#"{"type":"assistant","message":{"content":[{"type":"tool_result","tool_use_id":"t1","content":"file contents here"}]}}"#;
    let event = parse_line(line).unwrap();
    if let StreamEvent::Assistant { message } = event {
        assert!(matches!(
            &message.content[0],
            ContentBlock::ToolResult {
                tool_use_id,
                content
            } if tool_use_id == "t1" && content.as_deref() == Some("file contents here")
        ));
    } else {
        panic!("Expected Assistant event");
    }
}

#[test]
fn handles_whitespace_variants() {
    // Leading/trailing whitespace
    let line = "  \t  {\"type\":\"system\",\"subtype\":\"init\"}  \n  ";
    let event = parse_line(line).unwrap();
    assert!(matches!(event, StreamEvent::System { .. }));
}

#[test]
fn handles_unicode_in_result() {
    let line = r#"{"type":"result","result":"Résultat: 成功 — ✓ done","session_id":"abc","total_cost_usd":0.01}"#;
    let event = parse_line(line).unwrap();
    assert_eq!(extract_result(&event), Some("Résultat: 成功 — ✓ done"));
}

#[test]
fn handles_very_long_result_text() {
    let long_text = "x".repeat(100_000);
    let line = format!(
        r#"{{"type":"result","result":"{}","session_id":"abc","total_cost_usd":0.5}}"#,
        long_text
    );
    let event = parse_line(&line).unwrap();
    if let StreamEvent::Result { result, .. } = event {
        assert_eq!(result.len(), 100_000);
    } else {
        panic!("Expected Result event");
    }
}

#[test]
fn handles_nested_json_in_tool_input() {
    let line = r#"{"type":"assistant","message":{"content":[{"type":"tool_use","id":"t1","name":"Write","input":{"file_path":"/tmp/test.json","content":"{\"nested\":{\"deep\":true}}"}}]}}"#;
    let event = parse_line(line).unwrap();
    if let StreamEvent::Assistant { message } = event {
        if let ContentBlock::ToolUse { input, .. } = &message.content[0] {
            assert!(input.get("file_path").is_some());
            assert!(input.get("content").is_some());
        } else {
            panic!("Expected ToolUse block");
        }
    } else {
        panic!("Expected Assistant event");
    }
}

#[test]
fn extract_result_returns_none_for_non_result() {
    let line = r#"{"type":"system","subtype":"init"}"#;
    let event = parse_line(line).unwrap();
    assert!(extract_result(&event).is_none());

    let line = r#"{"type":"assistant","message":{"content":[]}}"#;
    let event = parse_line(line).unwrap();
    assert!(extract_result(&event).is_none());
}

#[test]
fn handles_extra_fields_gracefully() {
    // Claude might add new fields in future versions
    let line = r#"{"type":"system","subtype":"init","session_id":"abc","new_field":"ignored","another":42}"#;
    let event = parse_line(line).unwrap();
    assert!(matches!(event, StreamEvent::System { subtype, .. } if subtype == "init"));
}

#[test]
fn rejects_truncated_json() {
    assert!(parse_line(r#"{"type":"system","subtype"#).is_none());
    assert!(parse_line(r#"{"type":"result","result":"trun"#).is_none());
}

#[test]
fn handles_newlines_in_result_text() {
    let line = r#"{"type":"result","result":"line1\nline2\nline3","session_id":"abc","total_cost_usd":0.01}"#;
    let event = parse_line(line).unwrap();
    if let StreamEvent::Result { result, .. } = event {
        assert!(result.contains("\\n") || result.contains('\n'));
    } else {
        panic!("Expected Result event");
    }
}
