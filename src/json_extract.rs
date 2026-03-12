//! Robust JSON extraction from LLM output.
//!
//! LLMs frequently wrap JSON in markdown fences, preamble text, or trailing commentary.
//! This module provides a single extraction function used by all parsers (judge, scrum master, etc.).

/// Extract the first valid JSON object from text that may contain markdown fences,
/// preamble, or trailing commentary.
///
/// Strategy (ordered by likelihood):
/// 1. Direct parse (already clean JSON)
/// 2. Strip markdown ```json ... ``` fences (most common LLM wrapper)
/// 3. Find first `{` and last `}`, attempt parse of that substring
/// 4. Progressively shrink from the end on parse failure (handles trailing text after `}`)
pub fn extract_json_object(text: &str) -> Option<serde_json::Value> {
    let trimmed = text.trim();

    // 1. Direct parse
    if let Ok(v) = serde_json::from_str::<serde_json::Value>(trimmed) {
        if v.is_object() {
            return Some(v);
        }
    }

    // 2. Strip markdown fences (handles ```json, ```JSON, plain ```)
    if let Some(inner) = strip_markdown_fences(trimmed) {
        if let Ok(v) = serde_json::from_str::<serde_json::Value>(inner.trim()) {
            if v.is_object() {
                return Some(v);
            }
        }
    }

    // 3. Find first `{` and last `}`, try to parse that span
    let first_brace = trimmed.find('{')?;
    let last_brace = trimmed.rfind('}')?;
    if first_brace >= last_brace {
        return None;
    }

    let candidate = &trimmed[first_brace..=last_brace];
    if let Ok(v) = serde_json::from_str::<serde_json::Value>(candidate) {
        if v.is_object() {
            return Some(v);
        }
    }

    // 4. The outer braces matched but there might be nested issues.
    // Try finding each `}` from the end backwards in case trailing text
    // contains a `}` that's not part of the JSON.
    let bytes = trimmed.as_bytes();
    let mut pos = last_brace;
    loop {
        let candidate = &trimmed[first_brace..=pos];
        if let Ok(v) = serde_json::from_str::<serde_json::Value>(candidate) {
            if v.is_object() {
                return Some(v);
            }
        }
        // Find previous `}`
        if pos == 0 || pos <= first_brace {
            break;
        }
        pos -= 1;
        while pos > first_brace && bytes[pos] != b'}' {
            pos -= 1;
        }
        if pos <= first_brace {
            break;
        }
    }

    None
}

/// Extract the first valid JSON array from text that may contain markdown fences,
/// preamble, or trailing commentary. Same strategy as extract_json_object but for arrays.
pub fn extract_json_array(text: &str) -> Option<Vec<serde_json::Value>> {
    let trimmed = text.trim();

    // 1. Direct parse
    if let Ok(v) = serde_json::from_str::<serde_json::Value>(trimmed) {
        if let Some(arr) = v.as_array() {
            return Some(arr.clone());
        }
    }

    // 2. Strip markdown fences
    if let Some(inner) = strip_markdown_fences(trimmed) {
        if let Ok(v) = serde_json::from_str::<serde_json::Value>(inner.trim()) {
            if let Some(arr) = v.as_array() {
                return Some(arr.clone());
            }
        }
    }

    // 3. Find first `[` and last `]`, try to parse that span
    let first_bracket = trimmed.find('[')?;
    let last_bracket = trimmed.rfind(']')?;
    if first_bracket >= last_bracket {
        return None;
    }

    let candidate = &trimmed[first_bracket..=last_bracket];
    if let Ok(v) = serde_json::from_str::<serde_json::Value>(candidate) {
        if let Some(arr) = v.as_array() {
            return Some(arr.clone());
        }
    }

    // 4. Progressive shrink from end
    let bytes = trimmed.as_bytes();
    let mut pos = last_bracket;
    loop {
        let candidate = &trimmed[first_bracket..=pos];
        if let Ok(v) = serde_json::from_str::<serde_json::Value>(candidate) {
            if let Some(arr) = v.as_array() {
                return Some(arr.clone());
            }
        }
        if pos == 0 || pos <= first_bracket {
            break;
        }
        pos -= 1;
        while pos > first_bracket && bytes[pos] != b']' {
            pos -= 1;
        }
        if pos <= first_bracket {
            break;
        }
    }

    None
}

/// Strip markdown code fences from text.
/// Handles: ```json\n...\n```, ```\n...\n```, ```JSON\n...\n```
fn strip_markdown_fences(text: &str) -> Option<&str> {
    let text = text.trim();
    if !text.starts_with("```") {
        // Fences might not be at the start — find them
        let fence_start = text.find("```")?;
        let after_fence = &text[fence_start + 3..];
        // Skip the language tag (e.g., "json") and newline
        let content_start = after_fence.find('\n')? + 1;
        let content = &after_fence[content_start..];
        // Find closing fence
        let fence_end = content.rfind("```")?;
        return Some(&content[..fence_end]);
    }

    // Fences at the start
    let after_open = &text[3..];
    // Skip language tag + newline
    let content_start = after_open.find('\n')? + 1;
    let content = &after_open[content_start..];
    // Find closing fence
    let fence_end = content.rfind("```")?;
    Some(&content[..fence_end])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn direct_json() {
        let input = r#"{"went_well":["fast"],"friction_points":[]}"#;
        let v = extract_json_object(input).unwrap();
        assert_eq!(v["went_well"][0], "fast");
    }

    #[test]
    fn json_with_whitespace() {
        let input = "  \n  {\"key\": \"value\"}  \n  ";
        let v = extract_json_object(input).unwrap();
        assert_eq!(v["key"], "value");
    }

    #[test]
    fn markdown_fenced_json() {
        let input = "```json\n{\"key\": \"value\"}\n```";
        let v = extract_json_object(input).unwrap();
        assert_eq!(v["key"], "value");
    }

    #[test]
    fn markdown_fenced_no_lang() {
        let input = "```\n{\"key\": \"value\"}\n```";
        let v = extract_json_object(input).unwrap();
        assert_eq!(v["key"], "value");
    }

    #[test]
    fn preamble_then_fenced_json() {
        let input = "Here's my analysis:\n\n```json\n{\"went_well\":[\"good\"],\"friction_points\":[\"slow\"]}\n```\n";
        let v = extract_json_object(input).unwrap();
        assert_eq!(v["went_well"][0], "good");
        assert_eq!(v["friction_points"][0], "slow");
    }

    #[test]
    fn preamble_then_bare_json() {
        let input = "Here is the retrospective output:\n\n{\"went_well\":[\"fast\"],\"friction_points\":[]}";
        let v = extract_json_object(input).unwrap();
        assert_eq!(v["went_well"][0], "fast");
    }

    #[test]
    fn json_with_trailing_text() {
        let input = "{\"key\": \"value\"}\n\nLet me know if you need anything else!";
        let v = extract_json_object(input).unwrap();
        assert_eq!(v["key"], "value");
    }

    #[test]
    fn preamble_and_trailing_text() {
        let input = "Sure! Here's the JSON:\n\n{\"action_items\":[\"fix CI\"]}\n\nI hope this helps with the next sprint.";
        let v = extract_json_object(input).unwrap();
        assert_eq!(v["action_items"][0], "fix CI");
    }

    #[test]
    fn fenced_with_preamble_and_trailing() {
        let input = "## Retrospective\n\nAfter analyzing the sprint:\n\n```json\n{\"went_well\":[\"shipped\"],\"friction_points\":[\"flaky tests\"],\"action_items\":[\"stabilize CI\"],\"discovered_work\":[],\"observations\":[]}\n```\n\nThese findings should improve the next sprint.";
        let v = extract_json_object(input).unwrap();
        assert_eq!(v["went_well"][0], "shipped");
        assert_eq!(v["action_items"][0], "stabilize CI");
    }

    #[test]
    fn nested_json_objects() {
        let input = r#"{"observations":[{"category":"process","description":"Good flow","severity":"low","action_item":null}]}"#;
        let v = extract_json_object(input).unwrap();
        assert_eq!(v["observations"][0]["category"], "process");
    }

    #[test]
    fn returns_none_for_garbage() {
        assert!(extract_json_object("not json at all").is_none());
    }

    #[test]
    fn returns_none_for_empty() {
        assert!(extract_json_object("").is_none());
    }

    #[test]
    fn returns_none_for_array() {
        // We only extract objects, not arrays
        assert!(extract_json_object("[1, 2, 3]").is_none());
    }

    #[test]
    fn real_world_retro_output() {
        let input = r#"Based on my analysis of the sprint ceremony results, here is the retrospective:

```json
{
  "went_well": [
    "Research phase completed efficiently with clear file path identification",
    "Builder executed all story implementations successfully"
  ],
  "friction_points": [
    "Judge node had to process large output from builder",
    "No previous learnings available for sprint context"
  ],
  "action_items": [
    "Add file path annotations to story acceptance criteria",
    "Establish baseline sprint velocity metrics"
  ],
  "discovered_work": [
    "Need integration tests for the new API endpoints"
  ],
  "observations": [
    {
      "category": "process",
      "description": "First sprint had no prior learnings to build on",
      "severity": "low",
      "action_item": "This is expected for sprint 1 — no action needed"
    }
  ],
  "patterns_to_codify": [
    "Always include file paths with line numbers in research output"
  ],
  "sprint_health": "healthy"
}
```

This was a productive first sprint with healthy execution patterns."#;
        let v = extract_json_object(input).unwrap();
        assert_eq!(v["went_well"].as_array().unwrap().len(), 2);
        assert_eq!(v["friction_points"].as_array().unwrap().len(), 2);
        assert_eq!(v["sprint_health"], "healthy");
        assert_eq!(v["patterns_to_codify"].as_array().unwrap().len(), 1);
    }

    #[test]
    fn json_with_curly_braces_in_strings() {
        let input = r#"{"action_items":["Fix the {config} loading issue"]}"#;
        let v = extract_json_object(input).unwrap();
        assert_eq!(v["action_items"][0], "Fix the {config} loading issue");
    }
}
