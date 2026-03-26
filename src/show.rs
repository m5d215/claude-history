use std::collections::HashSet;
use std::fs::File;
use std::io::{BufRead, BufReader, BufWriter, Write};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde_json::Value;

use crate::jsonl::{extract_text_only, extract_tool_names};
use crate::output::format_timestamp;
use crate::search::BUF_SIZE;

pub struct ConversationMessage {
    pub timestamp: String,
    pub role: String,
    pub content: String,
}

/// Find JSONL files that contain the given session_id.
/// Tries filename match first (UUID.jsonl), then falls back to scanning content.
pub fn find_session_files(base_dir: &Path, session_id: &str) -> Result<Vec<PathBuf>> {
    let mut matched = Vec::new();
    let mut candidates = Vec::new();

    for entry in walkdir::WalkDir::new(base_dir)
        .into_iter()
        .filter_map(|e| e.ok())
    {
        let path = entry.path();
        if path.extension().is_none_or(|ext| ext != "jsonl") {
            continue;
        }

        // Fast path: filename stem matches session_id
        if path.file_stem().is_some_and(|stem| stem.to_string_lossy() == session_id) {
            matched.push(path.to_path_buf());
        } else {
            candidates.push(path.to_path_buf());
        }
    }

    // If we found by filename, also check subagent files
    if !matched.is_empty() {
        // Check candidates that are in the same project directory
        for candidate in &candidates {
            if scan_file_for_session(candidate, session_id)? {
                matched.push(candidate.clone());
            }
        }
        return Ok(matched);
    }

    // Fallback: scan all files for the session_id
    for candidate in &candidates {
        if scan_file_for_session(candidate, session_id)? {
            matched.push(candidate.clone());
        }
    }

    Ok(matched)
}

/// Check if a file contains any record with the given session_id.
/// Uses a two-stage check: fast substring pre-filter, then JSON field verification.
fn scan_file_for_session(file_path: &Path, session_id: &str) -> Result<bool> {
    let file = File::open(file_path)?;
    let reader = BufReader::with_capacity(BUF_SIZE, file);

    for line in reader.lines() {
        let line = match line {
            Ok(l) => l,
            Err(_) => continue,
        };
        // Fast pre-filter: skip lines that can't contain the session_id
        if !line.contains(session_id) {
            continue;
        }
        // Verify via JSON field to avoid false positives
        if let Ok(record) = serde_json::from_str::<Value>(&line)
            && record.get("sessionId").and_then(|v| v.as_str()) == Some(session_id)
        {
            return Ok(true);
        }
    }

    Ok(false)
}

/// Extract conversation messages from a file for a given session_id.
pub fn extract_messages_from_file(
    file_path: &Path,
    session_id: &str,
) -> Result<Vec<ConversationMessage>> {
    let file = File::open(file_path).with_context(|| format!("Cannot open {}", file_path.display()))?;
    let reader = BufReader::with_capacity(BUF_SIZE, file);
    let mut messages = Vec::new();
    let mut seen_request_ids: HashSet<String> = HashSet::new();
    let mut text_buf = String::new();

    for line in reader.lines() {
        let line = match line {
            Ok(l) => l,
            Err(_) => continue,
        };

        if line.is_empty() {
            continue;
        }

        let record: Value = match serde_json::from_str(&line) {
            Ok(v) => v,
            Err(_) => continue,
        };

        // Filter by session_id
        let record_session = record.get("sessionId").and_then(|v| v.as_str()).unwrap_or("");
        if record_session != session_id {
            continue;
        }

        let record_type = record.get("type").and_then(|v| v.as_str()).unwrap_or("");
        let timestamp = record
            .get("timestamp")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();

        match record_type {
            "user" => {
                let text = extract_text_only(&record, &mut text_buf);
                if !text.is_empty() {
                    // Check if this is a tool_result (skip it)
                    let is_tool_result = record
                        .get("message")
                        .and_then(|m| m.get("content"))
                        .and_then(|c| c.as_array())
                        .is_some_and(|arr| {
                            arr.iter().any(|item| {
                                item.get("type").and_then(|v| v.as_str()) == Some("tool_result")
                            })
                        });
                    if !is_tool_result {
                        messages.push(ConversationMessage {
                            timestamp,
                            role: "user".to_string(),
                            content: text.to_string(),
                        });
                    }
                }
            }
            "assistant" => {
                // Deduplicate streaming chunks
                if let Some(request_id) = record
                    .get("message")
                    .and_then(|m| m.get("requestId"))
                    .and_then(|v| v.as_str())
                {
                    let stop_reason = record
                        .get("message")
                        .and_then(|m| m.get("stop_reason"));
                    let is_final = stop_reason.is_some_and(|v| !v.is_null());
                    if !is_final {
                        continue;
                    }
                    if !seen_request_ids.insert(request_id.to_string()) {
                        continue;
                    }
                }

                // Extract tool_use names for summary
                let tool_names = extract_tool_names(&record);
                if !tool_names.is_empty() {
                    for name in &tool_names {
                        messages.push(ConversationMessage {
                            timestamp: timestamp.clone(),
                            role: "tool".to_string(),
                            content: name.clone(),
                        });
                    }
                }

                // Extract text content
                let text = extract_text_only(&record, &mut text_buf);
                if !text.is_empty() {
                    messages.push(ConversationMessage {
                        timestamp: timestamp.clone(),
                        role: "assistant".to_string(),
                        content: text.to_string(),
                    });
                }
            }
            _ => {}
        }
    }

    Ok(messages)
}

const MAX_LINES_PER_MESSAGE: usize = 20;

/// Print conversation messages to stdout.
pub fn print_conversation(messages: &[ConversationMessage], max_messages: usize, use_color: bool) {
    let stdout = std::io::stdout();
    let mut out = BufWriter::new(stdout.lock());

    let display = if max_messages > 0 && messages.len() > max_messages {
        &messages[messages.len() - max_messages..]
    } else {
        messages
    };

    for (i, msg) in display.iter().enumerate() {
        if i > 0 {
            let _ = writeln!(out);
        }

        let ts = format_timestamp(&msg.timestamp);

        match msg.role.as_str() {
            "tool" => {
                if use_color {
                    let _ = writeln!(out, "\x1b[33m[tool] {}\x1b[0m", msg.content);
                } else {
                    let _ = writeln!(out, "[tool] {}", msg.content);
                }
            }
            role => {
                let (open, close) = if use_color {
                    match role {
                        "user" => ("\x1b[32m", "\x1b[0m"),
                        "assistant" => ("\x1b[36m", "\x1b[0m"),
                        _ => ("", ""),
                    }
                } else {
                    ("", "")
                };
                let _ = writeln!(out, "{}[{}] {}{}", open, role, ts, close);
                let lines: Vec<&str> = msg.content.lines().collect();
                if lines.len() > MAX_LINES_PER_MESSAGE {
                    for line in &lines[..MAX_LINES_PER_MESSAGE] {
                        let _ = writeln!(out, "{}", line);
                    }
                    let _ = writeln!(out, "...");
                } else {
                    let _ = writeln!(out, "{}", msg.content);
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write as IoWrite;

    fn write_jsonl(dir: &Path, name: &str, lines: &[Value]) -> PathBuf {
        let path = dir.join(name);
        let mut f = File::create(&path).unwrap();
        for line in lines {
            writeln!(f, "{}", serde_json::to_string(line).unwrap()).unwrap();
        }
        path
    }

    // --- find_session_files ---

    #[test]
    fn find_by_filename() {
        let dir = tempfile::tempdir().unwrap();
        let _path = write_jsonl(
            dir.path(),
            "abc-123.jsonl",
            &[serde_json::json!({
                "type": "user",
                "sessionId": "abc-123",
                "timestamp": "2026-03-26T10:00:00Z",
                "message": { "content": "hello" },
            })],
        );
        let files = find_session_files(dir.path(), "abc-123").unwrap();
        assert_eq!(files.len(), 1);
    }

    #[test]
    fn find_by_content_scan() {
        let dir = tempfile::tempdir().unwrap();
        let _path = write_jsonl(
            dir.path(),
            "other-name.jsonl",
            &[serde_json::json!({
                "type": "user",
                "sessionId": "target-session",
                "timestamp": "2026-03-26T10:00:00Z",
                "message": { "content": "hello" },
            })],
        );
        let files = find_session_files(dir.path(), "target-session").unwrap();
        assert_eq!(files.len(), 1);
    }

    #[test]
    fn find_no_match() {
        let dir = tempfile::tempdir().unwrap();
        let _path = write_jsonl(
            dir.path(),
            "test.jsonl",
            &[serde_json::json!({
                "type": "user",
                "sessionId": "other-session",
                "timestamp": "2026-03-26T10:00:00Z",
                "message": { "content": "hello" },
            })],
        );
        let files = find_session_files(dir.path(), "nonexistent").unwrap();
        assert!(files.is_empty());
    }

    // --- extract_messages_from_file ---

    #[test]
    fn extract_user_and_assistant() {
        let dir = tempfile::tempdir().unwrap();
        let path = write_jsonl(
            dir.path(),
            "test.jsonl",
            &[
                serde_json::json!({
                    "type": "user",
                    "sessionId": "sess-1",
                    "timestamp": "2026-03-26T10:00:00Z",
                    "message": { "content": "deploy the app" },
                }),
                serde_json::json!({
                    "type": "assistant",
                    "sessionId": "sess-1",
                    "timestamp": "2026-03-26T10:01:00Z",
                    "message": {
                        "requestId": "req_001",
                        "stop_reason": "end_turn",
                        "content": [{ "type": "text", "text": "done deploying" }]
                    },
                }),
            ],
        );
        let msgs = extract_messages_from_file(&path, "sess-1").unwrap();
        assert_eq!(msgs.len(), 2);
        assert_eq!(msgs[0].role, "user");
        assert_eq!(msgs[0].content, "deploy the app");
        assert_eq!(msgs[1].role, "assistant");
        assert_eq!(msgs[1].content, "done deploying");
    }

    #[test]
    fn extract_skips_tool_result() {
        let dir = tempfile::tempdir().unwrap();
        let path = write_jsonl(
            dir.path(),
            "test.jsonl",
            &[serde_json::json!({
                "type": "user",
                "sessionId": "sess-1",
                "timestamp": "2026-03-26T10:00:00Z",
                "message": {
                    "content": [{
                        "type": "tool_result",
                        "tool_use_id": "toolu_123",
                        "content": "file contents here"
                    }]
                },
            })],
        );
        let msgs = extract_messages_from_file(&path, "sess-1").unwrap();
        assert!(msgs.is_empty());
    }

    #[test]
    fn extract_tool_use_summary() {
        let dir = tempfile::tempdir().unwrap();
        let path = write_jsonl(
            dir.path(),
            "test.jsonl",
            &[serde_json::json!({
                "type": "assistant",
                "sessionId": "sess-1",
                "timestamp": "2026-03-26T10:00:00Z",
                "message": {
                    "requestId": "req_001",
                    "stop_reason": "end_turn",
                    "content": [
                        { "type": "text", "text": "Let me check" },
                        { "type": "tool_use", "name": "Bash", "input": { "command": "ls" } }
                    ]
                },
            })],
        );
        let msgs = extract_messages_from_file(&path, "sess-1").unwrap();
        assert_eq!(msgs.len(), 2);
        assert_eq!(msgs[0].role, "tool");
        assert_eq!(msgs[0].content, "Bash");
        assert_eq!(msgs[1].role, "assistant");
        assert_eq!(msgs[1].content, "Let me check");
    }

    #[test]
    fn extract_skips_non_final_assistant() {
        let dir = tempfile::tempdir().unwrap();
        let path = write_jsonl(
            dir.path(),
            "test.jsonl",
            &[
                serde_json::json!({
                    "type": "assistant",
                    "sessionId": "sess-1",
                    "timestamp": "2026-03-26T10:00:00Z",
                    "message": {
                        "requestId": "req_001",
                        "stop_reason": null,
                        "content": [{ "type": "text", "text": "partial" }]
                    },
                }),
                serde_json::json!({
                    "type": "assistant",
                    "sessionId": "sess-1",
                    "timestamp": "2026-03-26T10:00:01Z",
                    "message": {
                        "requestId": "req_001",
                        "stop_reason": "end_turn",
                        "content": [{ "type": "text", "text": "final answer" }]
                    },
                }),
            ],
        );
        let msgs = extract_messages_from_file(&path, "sess-1").unwrap();
        assert_eq!(msgs.len(), 1);
        assert_eq!(msgs[0].content, "final answer");
    }

    #[test]
    fn extract_filters_by_session_id() {
        let dir = tempfile::tempdir().unwrap();
        let path = write_jsonl(
            dir.path(),
            "test.jsonl",
            &[
                serde_json::json!({
                    "type": "user",
                    "sessionId": "sess-1",
                    "timestamp": "2026-03-26T10:00:00Z",
                    "message": { "content": "for session 1" },
                }),
                serde_json::json!({
                    "type": "user",
                    "sessionId": "sess-2",
                    "timestamp": "2026-03-26T10:00:00Z",
                    "message": { "content": "for session 2" },
                }),
            ],
        );
        let msgs = extract_messages_from_file(&path, "sess-1").unwrap();
        assert_eq!(msgs.len(), 1);
        assert_eq!(msgs[0].content, "for session 1");
    }

    #[test]
    fn extract_skips_thinking() {
        let dir = tempfile::tempdir().unwrap();
        let path = write_jsonl(
            dir.path(),
            "test.jsonl",
            &[serde_json::json!({
                "type": "assistant",
                "sessionId": "sess-1",
                "timestamp": "2026-03-26T10:00:00Z",
                "message": {
                    "requestId": "req_001",
                    "stop_reason": "end_turn",
                    "content": [
                        { "type": "thinking", "thinking": "let me think about this..." },
                        { "type": "text", "text": "here is my answer" }
                    ]
                },
            })],
        );
        let msgs = extract_messages_from_file(&path, "sess-1").unwrap();
        assert_eq!(msgs.len(), 1);
        assert_eq!(msgs[0].content, "here is my answer");
    }
}
