use regex::Regex;
use serde_json::Value;

/// Extract searchable text from a JSONL record, writing into the provided buffer.
/// Returns the slice of the buffer that was written.
pub fn extract_text_into<'a>(record: &'a Value, buf: &'a mut String) -> &'a str {
    buf.clear();

    let message = match record.get("message") {
        Some(m) => m,
        None => return "",
    };

    let content = match message.get("content") {
        Some(c) => c,
        None => return "",
    };

    match content {
        Value::String(s) => return s.as_str(),
        Value::Array(arr) => {
            for item in arr {
                if let Some(text) = item.get("text").and_then(|v| v.as_str()) {
                    if !buf.is_empty() {
                        buf.push('\n');
                    }
                    buf.push_str(text);
                }
                if let Some(thinking) = item.get("thinking").and_then(|v| v.as_str()) {
                    if !buf.is_empty() {
                        buf.push('\n');
                    }
                    buf.push_str(thinking);
                }
                if let Some(content_val) = item.get("content") {
                    match content_val {
                        Value::Array(inner) => {
                            for inner_item in inner {
                                if let Some(text) =
                                    inner_item.get("text").and_then(|v| v.as_str())
                                {
                                    if !buf.is_empty() {
                                        buf.push('\n');
                                    }
                                    buf.push_str(text);
                                }
                            }
                        }
                        Value::String(s) => {
                            if !buf.is_empty() {
                                buf.push('\n');
                            }
                            buf.push_str(s);
                        }
                        _ => {}
                    }
                }
                if let Some(Value::Object(map)) = item.get("input") {
                    for v in map.values() {
                        if let Value::String(s) = v {
                            if !buf.is_empty() {
                                buf.push('\n');
                            }
                            buf.push_str(s);
                        }
                    }
                }
            }
        }
        _ => return "",
    }

    buf.as_str()
}

/// Extract only text blocks from a JSONL record (excludes thinking, tool_use input).
/// Used by `show` command where thinking/tool details are not useful.
pub fn extract_text_only<'a>(record: &'a Value, buf: &'a mut String) -> &'a str {
    buf.clear();

    let message = match record.get("message") {
        Some(m) => m,
        None => return "",
    };

    let content = match message.get("content") {
        Some(c) => c,
        None => return "",
    };

    match content {
        Value::String(s) => return s.as_str(),
        Value::Array(arr) => {
            for item in arr {
                if let Some(text) = item.get("text").and_then(|v| v.as_str()) {
                    if !buf.is_empty() {
                        buf.push('\n');
                    }
                    buf.push_str(text);
                }
                // Skip thinking, tool_use input — only extract text blocks
                if let Some(content_val) = item.get("content") {
                    match content_val {
                        Value::Array(inner) => {
                            for inner_item in inner {
                                if let Some(text) =
                                    inner_item.get("text").and_then(|v| v.as_str())
                                {
                                    if !buf.is_empty() {
                                        buf.push('\n');
                                    }
                                    buf.push_str(text);
                                }
                            }
                        }
                        Value::String(s) => {
                            if !buf.is_empty() {
                                buf.push('\n');
                            }
                            buf.push_str(s);
                        }
                        _ => {}
                    }
                }
            }
        }
        _ => return "",
    }

    buf.as_str()
}

/// Extract tool_use names from an assistant message's content array.
pub fn extract_tool_names(record: &Value) -> Vec<String> {
    let Some(content) = record
        .get("message")
        .and_then(|m| m.get("content"))
        .and_then(|c| c.as_array())
    else {
        return Vec::new();
    };

    content
        .iter()
        .filter_map(|item| {
            let item_type = item.get("type").and_then(|v| v.as_str())?;
            if item_type == "tool_use" {
                item.get("name").and_then(|v| v.as_str()).map(String::from)
            } else {
                None
            }
        })
        .collect()
}

/// Quick check: does the raw line contain the pattern as a literal substring?
/// This avoids JSON parsing for lines that can't possibly match.
#[inline]
pub fn line_might_match(line: &str, re: &Regex, literal_prefix: Option<&str>) -> bool {
    if let Some(prefix) = literal_prefix {
        // Fast path: check literal substring with memchr-accelerated contains
        memchr::memmem::find(line.as_bytes(), prefix.as_bytes()).is_some()
    } else {
        // No literal prefix extractable, must check via regex on raw line
        re.is_match(line)
    }
}

/// Try to extract a literal prefix from a regex pattern for fast pre-filtering.
pub fn extract_literal_prefix(pattern: &str) -> Option<String> {
    // Simple heuristic: if the pattern starts with literal characters (no regex metacharacters),
    // use those as a pre-filter
    let mut prefix = String::new();
    let mut chars = pattern.chars().peekable();

    // Skip case-insensitive flag prefix
    if pattern.starts_with("(?i)") {
        return None; // Can't do case-insensitive literal matching easily
    }

    while let Some(&ch) = chars.peek() {
        match ch {
            '.' | '*' | '+' | '?' | '[' | ']' | '(' | ')' | '{' | '}' | '|' | '^' | '$' => {
                break
            }
            '\\' => {
                chars.next();
                if let Some(&escaped) = chars.peek() {
                    match escaped {
                        'd' | 'w' | 's' | 'D' | 'W' | 'S' | 'b' | 'B' => break,
                        _ => {
                            prefix.push(escaped);
                            chars.next();
                        }
                    }
                } else {
                    break;
                }
            }
            _ => {
                prefix.push(ch);
                chars.next();
            }
        }
    }

    if prefix.len() >= 3 {
        Some(prefix)
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::Value;

    // --- extract_text_into ---

    #[test]
    fn extract_text_string_content() {
        let record: Value = serde_json::json!({
            "type": "user",
            "message": { "content": "hello world" }
        });
        let mut buf = String::new();
        let text = extract_text_into(&record, &mut buf);
        assert_eq!(text, "hello world");
    }

    #[test]
    fn extract_text_array_with_text() {
        let record: Value = serde_json::json!({
            "type": "assistant",
            "message": {
                "content": [
                    { "type": "text", "text": "first part" },
                    { "type": "text", "text": "second part" }
                ]
            }
        });
        let mut buf = String::new();
        let text = extract_text_into(&record, &mut buf);
        assert_eq!(text, "first part\nsecond part");
    }

    #[test]
    fn extract_text_array_with_thinking() {
        let record: Value = serde_json::json!({
            "type": "assistant",
            "message": {
                "content": [
                    { "type": "thinking", "thinking": "let me think..." },
                    { "type": "text", "text": "here is my answer" }
                ]
            }
        });
        let mut buf = String::new();
        let text = extract_text_into(&record, &mut buf);
        assert!(text.contains("let me think..."));
        assert!(text.contains("here is my answer"));
    }

    #[test]
    fn extract_text_tool_result_string_content() {
        let record: Value = serde_json::json!({
            "type": "user",
            "message": {
                "content": [
                    {
                        "type": "tool_result",
                        "tool_use_id": "toolu_123",
                        "content": "tool output text"
                    }
                ]
            }
        });
        let mut buf = String::new();
        let text = extract_text_into(&record, &mut buf);
        assert_eq!(text, "tool output text");
    }

    #[test]
    fn extract_text_tool_result_array_content() {
        let record: Value = serde_json::json!({
            "type": "user",
            "message": {
                "content": [
                    {
                        "type": "tool_result",
                        "tool_use_id": "toolu_123",
                        "content": [
                            { "type": "text", "text": "line 1" },
                            { "type": "text", "text": "line 2" }
                        ]
                    }
                ]
            }
        });
        let mut buf = String::new();
        let text = extract_text_into(&record, &mut buf);
        assert!(text.contains("line 1"));
        assert!(text.contains("line 2"));
    }

    #[test]
    fn extract_text_tool_use_input() {
        let record: Value = serde_json::json!({
            "type": "assistant",
            "message": {
                "content": [
                    {
                        "type": "tool_use",
                        "name": "Bash",
                        "input": { "command": "ls -la", "description": "list files" }
                    }
                ]
            }
        });
        let mut buf = String::new();
        let text = extract_text_into(&record, &mut buf);
        assert!(text.contains("ls -la"));
        assert!(text.contains("list files"));
    }

    #[test]
    fn extract_text_no_message() {
        let record: Value = serde_json::json!({ "type": "system" });
        let mut buf = String::new();
        let text = extract_text_into(&record, &mut buf);
        assert_eq!(text, "");
    }

    #[test]
    fn extract_text_no_content() {
        let record: Value = serde_json::json!({
            "type": "user",
            "message": { "role": "user" }
        });
        let mut buf = String::new();
        let text = extract_text_into(&record, &mut buf);
        assert_eq!(text, "");
    }

    // --- extract_literal_prefix ---

    #[test]
    fn literal_prefix_simple() {
        assert_eq!(
            extract_literal_prefix("Terraform"),
            Some("Terraform".to_string())
        );
    }

    #[test]
    fn literal_prefix_with_regex_suffix() {
        assert_eq!(
            extract_literal_prefix("hello.*world"),
            Some("hello".to_string())
        );
    }

    #[test]
    fn literal_prefix_too_short() {
        assert_eq!(extract_literal_prefix("ab"), None);
    }

    #[test]
    fn literal_prefix_case_insensitive_returns_none() {
        assert_eq!(extract_literal_prefix("(?i)hello"), None);
    }

    #[test]
    fn literal_prefix_starts_with_metachar() {
        assert_eq!(extract_literal_prefix(".*hello"), None);
    }

    #[test]
    fn literal_prefix_escaped_chars() {
        assert_eq!(
            extract_literal_prefix("foo\\.bar"),
            Some("foo.bar".to_string())
        );
    }

    // --- line_might_match ---

    #[test]
    fn line_might_match_with_literal() {
        assert!(line_might_match(
            r#"{"type":"user","message":{"content":"hello Terraform world"}}"#,
            &Regex::new("Terraform").unwrap(),
            Some("Terraform"),
        ));
    }

    #[test]
    fn line_might_match_without_literal() {
        assert!(!line_might_match(
            r#"{"type":"user","message":{"content":"hello world"}}"#,
            &Regex::new("Terraform").unwrap(),
            Some("Terraform"),
        ));
    }

    #[test]
    fn line_might_match_regex_fallback() {
        assert!(line_might_match(
            r#"{"type":"user","message":{"content":"hello 123 world"}}"#,
            &Regex::new(r"\d+").unwrap(),
            None,
        ));
    }
}
