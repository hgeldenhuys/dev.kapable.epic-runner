use serde::Deserialize;

#[derive(Debug, Deserialize)]
#[serde(tag = "type")]
pub enum StreamEvent {
    #[serde(rename = "system")]
    System {
        subtype: String,
        session_id: Option<String>,
    },
    #[serde(rename = "assistant")]
    Assistant { message: AssistantMessage },
    #[serde(rename = "result")]
    Result {
        result: String,
        session_id: String,
        /// Claude Code stream-json uses `total_cost_usd` since SDK v1.0.22
        #[serde(alias = "cost_usd")]
        total_cost_usd: Option<f64>,
    },
}

#[derive(Debug, Deserialize)]
pub struct AssistantMessage {
    pub content: Vec<ContentBlock>,
}

#[derive(Debug, Deserialize)]
#[serde(tag = "type")]
pub enum ContentBlock {
    #[serde(rename = "text")]
    Text { text: String },
    #[serde(rename = "tool_use")]
    ToolUse {
        id: String,
        name: String,
        input: serde_json::Value,
    },
    #[serde(rename = "tool_result")]
    ToolResult {
        tool_use_id: String,
        content: Option<String>,
    },
}

pub fn parse_line(line: &str) -> Option<StreamEvent> {
    let trimmed = line.trim();
    if trimmed.is_empty() {
        return None;
    }
    serde_json::from_str(trimmed).ok()
}

pub fn extract_result(event: &StreamEvent) -> Option<&str> {
    match event {
        StreamEvent::Result { result, .. } => Some(result.as_str()),
        _ => None,
    }
}

/// Extract message text from a SendUserMessage tool_use block.
/// Returns the message string if this content block is a SendUserMessage call.
pub fn extract_user_message(block: &ContentBlock) -> Option<String> {
    match block {
        ContentBlock::ToolUse { name, input, .. } if name == "SendUserMessage" => input
            .get("message")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string()),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn extract_user_message_from_send_user_message() {
        let block = ContentBlock::ToolUse {
            id: "toolu_123".into(),
            name: "SendUserMessage".into(),
            input: json!({"message": "Compiling — 3 of 5 stories done"}),
        };
        assert_eq!(
            extract_user_message(&block),
            Some("Compiling — 3 of 5 stories done".to_string())
        );
    }

    #[test]
    fn extract_user_message_ignores_other_tools() {
        let block = ContentBlock::ToolUse {
            id: "toolu_456".into(),
            name: "Edit".into(),
            input: json!({"file_path": "/tmp/test.rs", "old_string": "a", "new_string": "b"}),
        };
        assert_eq!(extract_user_message(&block), None);
    }

    #[test]
    fn extract_user_message_handles_missing_field() {
        let block = ContentBlock::ToolUse {
            id: "toolu_789".into(),
            name: "SendUserMessage".into(),
            input: json!({}),
        };
        assert_eq!(extract_user_message(&block), None);
    }

    #[test]
    fn extract_user_message_from_text_block() {
        let block = ContentBlock::Text {
            text: "Hello".into(),
        };
        assert_eq!(extract_user_message(&block), None);
    }

    #[test]
    fn parse_line_with_send_user_message() {
        let line = r#"{"type":"assistant","message":{"content":[{"type":"tool_use","id":"toolu_abc","name":"SendUserMessage","input":{"message":"Starting research phase"}}]}}"#;
        let event = parse_line(line).unwrap();
        match event {
            StreamEvent::Assistant { message } => {
                assert_eq!(message.content.len(), 1);
                let msg = extract_user_message(&message.content[0]);
                assert_eq!(msg, Some("Starting research phase".to_string()));
            }
            _ => panic!("Expected Assistant event"),
        }
    }
}
