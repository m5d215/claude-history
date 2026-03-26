use std::collections::HashMap;
use std::fs::File;
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};

use anyhow::Result;
use chrono::{DateTime, Utc};
use rayon::prelude::*;
use serde_json::Value;

use crate::jsonl::extract_text_into;
use crate::output::extract_project_name;
use crate::search::{parse_timestamp, BUF_SIZE};

pub struct SessionInfo {
    pub session_id: String,
    pub file_path: PathBuf,
    pub project: String,
    pub started_at: String,
    pub last_activity: String,
    pub first_user_message: String,
}

struct SessionInfoBuilder {
    session_id: String,
    file_path: PathBuf,
    project: String,
    earliest_timestamp: String,
    latest_timestamp: String,
    first_user_message: Option<String>,
}

impl SessionInfoBuilder {
    fn new(session_id: String, file_path: PathBuf, project: String, timestamp: String) -> Self {
        Self {
            session_id,
            file_path,
            project,
            earliest_timestamp: timestamp.clone(),
            latest_timestamp: timestamp,
            first_user_message: None,
        }
    }

    fn update_timestamp(&mut self, timestamp: &str) {
        if timestamp < self.earliest_timestamp.as_str() {
            self.earliest_timestamp = timestamp.to_string();
        }
        if timestamp > self.latest_timestamp.as_str() {
            self.latest_timestamp = timestamp.to_string();
        }
    }

    fn into_session_info(self) -> SessionInfo {
        SessionInfo {
            session_id: self.session_id,
            file_path: self.file_path,
            project: self.project,
            started_at: self.earliest_timestamp,
            last_activity: self.latest_timestamp,
            first_user_message: self.first_user_message.unwrap_or_default(),
        }
    }
}

pub fn collect_sessions_parallel(
    files: &[PathBuf],
    since: Option<DateTime<Utc>>,
    until: Option<DateTime<Utc>>,
) -> Vec<SessionInfo> {
    let file_results: Vec<Vec<SessionInfo>> = files
        .par_iter()
        .map(|file_path| extract_sessions_from_file(file_path, since, until).unwrap_or_default())
        .collect();

    // Merge sessions with the same session_id across files
    let mut merged: HashMap<String, SessionInfo> = HashMap::new();
    for session in file_results.into_iter().flatten() {
        merged
            .entry(session.session_id.clone())
            .and_modify(|existing| {
                if session.started_at < existing.started_at {
                    existing.started_at = session.started_at.clone();
                    existing.file_path = session.file_path.clone();
                }
                if session.last_activity > existing.last_activity {
                    existing.last_activity = session.last_activity.clone();
                }
                if existing.first_user_message.is_empty() && !session.first_user_message.is_empty()
                {
                    existing.first_user_message = session.first_user_message.clone();
                }
            })
            .or_insert(session);
    }

    let mut all_sessions: Vec<SessionInfo> = merged.into_values().collect();

    // Sort by last_activity descending (most recent first)
    // RFC3339 timestamps sort correctly as strings
    all_sessions.sort_by(|a, b| b.last_activity.cmp(&a.last_activity));
    all_sessions
}

pub fn extract_sessions_from_file(
    file_path: &Path,
    since: Option<DateTime<Utc>>,
    until: Option<DateTime<Utc>>,
) -> Result<Vec<SessionInfo>> {
    let file = File::open(file_path)?;
    let reader = BufReader::with_capacity(BUF_SIZE, file);
    let project = extract_project_name(file_path);
    let mut builders: HashMap<String, SessionInfoBuilder> = HashMap::new();
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

        let session_id = match record.get("sessionId").and_then(|v| v.as_str()) {
            Some(id) => id.to_string(),
            None => continue,
        };

        let timestamp = match record.get("timestamp").and_then(|v| v.as_str()) {
            Some(ts) => ts.to_string(),
            None => continue,
        };

        let builder = builders
            .entry(session_id.clone())
            .or_insert_with(|| {
                SessionInfoBuilder::new(
                    session_id,
                    file_path.to_path_buf(),
                    project.clone(),
                    timestamp.clone(),
                )
            });

        builder.update_timestamp(&timestamp);

        // Capture first user message (skip system-injected content like XML tags)
        let record_type = record.get("type").and_then(|v| v.as_str()).unwrap_or("");
        if record_type == "user" && builder.first_user_message.is_none() {
            let text = extract_text_into(&record, &mut text_buf);
            if !text.is_empty() && !text.starts_with('<') {
                builder.first_user_message = Some(truncate_message(text, 80));
            }
        }
    }

    let sessions: Vec<SessionInfo> = builders
        .into_values()
        .filter(|b| {
            // Apply date filter on last_activity
            if (since.is_some() || until.is_some())
                && let Some(ts) = parse_timestamp(&b.latest_timestamp)
            {
                if since.as_ref().is_some_and(|s| ts < *s) {
                    return false;
                }
                if until.as_ref().is_some_and(|u| ts > *u) {
                    return false;
                }
            }
            true
        })
        .map(|b| b.into_session_info())
        .collect();

    Ok(sessions)
}

pub fn truncate_message(text: &str, max_chars: usize) -> String {
    let normalized: String = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if normalized.chars().count() <= max_chars {
        return normalized;
    }
    // Truncate at char boundary
    let truncated: String = normalized.chars().take(max_chars).collect();
    match truncated.rfind(' ') {
        Some(pos) if pos > max_chars / 2 => format!("{}...", &truncated[..pos]),
        _ => format!("{}...", truncated),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write as IoWrite;

    fn write_jsonl(dir: &Path, lines: &[Value]) -> PathBuf {
        let path = dir.join("test-session.jsonl");
        let mut f = File::create(&path).unwrap();
        for line in lines {
            writeln!(f, "{}", serde_json::to_string(line).unwrap()).unwrap();
        }
        path
    }

    // --- truncate_message ---

    #[test]
    fn truncate_short_message() {
        assert_eq!(truncate_message("hello world", 80), "hello world");
    }

    #[test]
    fn truncate_long_message() {
        let msg = "a ".repeat(50); // 100 chars
        let result = truncate_message(&msg, 20);
        assert!(result.ends_with("..."));
        assert!(result.len() <= 24); // 20 + "..."
    }

    #[test]
    fn truncate_normalizes_whitespace() {
        assert_eq!(
            truncate_message("hello\n  world\ttab", 80),
            "hello world tab"
        );
    }

    #[test]
    fn truncate_multibyte() {
        let msg = "あいうえおかきくけこさしすせそ"; // 15 chars
        let result = truncate_message(msg, 10);
        assert!(result.ends_with("..."));
    }

    // --- extract_sessions_from_file ---

    #[test]
    fn single_session() {
        let dir = tempfile::tempdir().unwrap();
        let path = write_jsonl(
            dir.path(),
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
                    "timestamp": "2026-03-26T10:05:00Z",
                    "message": {
                        "requestId": "req_001",
                        "stop_reason": "end_turn",
                        "content": [{ "type": "text", "text": "done" }]
                    },
                }),
            ],
        );
        let sessions = extract_sessions_from_file(&path, None, None).unwrap();
        assert_eq!(sessions.len(), 1);
        assert_eq!(sessions[0].session_id, "sess-1");
        assert_eq!(sessions[0].started_at, "2026-03-26T10:00:00Z");
        assert_eq!(sessions[0].last_activity, "2026-03-26T10:05:00Z");
        assert_eq!(sessions[0].first_user_message, "deploy the app");
    }

    #[test]
    fn multiple_sessions_in_one_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = write_jsonl(
            dir.path(),
            &[
                serde_json::json!({
                    "type": "user",
                    "sessionId": "sess-1",
                    "timestamp": "2026-03-26T10:00:00Z",
                    "message": { "content": "first session" },
                }),
                serde_json::json!({
                    "type": "user",
                    "sessionId": "sess-2",
                    "timestamp": "2026-03-26T11:00:00Z",
                    "message": { "content": "second session" },
                }),
            ],
        );
        let sessions = extract_sessions_from_file(&path, None, None).unwrap();
        assert_eq!(sessions.len(), 2);
    }

    #[test]
    fn date_filter_excludes_old_sessions() {
        let dir = tempfile::tempdir().unwrap();
        let since = parse_timestamp("2026-03-01T00:00:00Z");
        let path = write_jsonl(
            dir.path(),
            &[
                serde_json::json!({
                    "type": "user",
                    "sessionId": "sess-old",
                    "timestamp": "2025-01-01T10:00:00Z",
                    "message": { "content": "old session" },
                }),
                serde_json::json!({
                    "type": "user",
                    "sessionId": "sess-new",
                    "timestamp": "2026-03-26T10:00:00Z",
                    "message": { "content": "new session" },
                }),
            ],
        );
        let sessions = extract_sessions_from_file(&path, since, None).unwrap();
        assert_eq!(sessions.len(), 1);
        assert_eq!(sessions[0].session_id, "sess-new");
    }

    #[test]
    fn session_without_user_message() {
        let dir = tempfile::tempdir().unwrap();
        let path = write_jsonl(
            dir.path(),
            &[serde_json::json!({
                "type": "system",
                "sessionId": "sess-1",
                "timestamp": "2026-03-26T10:00:00Z",
            })],
        );
        let sessions = extract_sessions_from_file(&path, None, None).unwrap();
        assert_eq!(sessions.len(), 1);
        assert_eq!(sessions[0].first_user_message, "");
    }

    #[test]
    fn first_user_message_is_first_not_last() {
        let dir = tempfile::tempdir().unwrap();
        let path = write_jsonl(
            dir.path(),
            &[
                serde_json::json!({
                    "type": "user",
                    "sessionId": "sess-1",
                    "timestamp": "2026-03-26T10:00:00Z",
                    "message": { "content": "first question" },
                }),
                serde_json::json!({
                    "type": "user",
                    "sessionId": "sess-1",
                    "timestamp": "2026-03-26T10:10:00Z",
                    "message": { "content": "second question" },
                }),
            ],
        );
        let sessions = extract_sessions_from_file(&path, None, None).unwrap();
        assert_eq!(sessions[0].first_user_message, "first question");
    }

    #[test]
    fn malformed_lines_skipped() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("test.jsonl");
        let mut f = File::create(&path).unwrap();
        writeln!(f, "not valid json").unwrap();
        writeln!(f, "").unwrap();
        writeln!(
            f,
            "{}",
            serde_json::to_string(&serde_json::json!({
                "type": "user",
                "sessionId": "sess-1",
                "timestamp": "2026-03-26T10:00:00Z",
                "message": { "content": "valid line" },
            }))
            .unwrap()
        )
        .unwrap();
        let sessions = extract_sessions_from_file(&path, None, None).unwrap();
        assert_eq!(sessions.len(), 1);
    }

    // --- collect_sessions_parallel ---

    #[test]
    fn parallel_sorts_by_last_activity_descending() {
        let dir = tempfile::tempdir().unwrap();

        let path1 = dir.path().join("old.jsonl");
        let mut f1 = File::create(&path1).unwrap();
        writeln!(
            f1,
            "{}",
            serde_json::to_string(&serde_json::json!({
                "type": "user",
                "sessionId": "sess-old",
                "timestamp": "2026-03-25T10:00:00Z",
                "message": { "content": "old" },
            }))
            .unwrap()
        )
        .unwrap();

        let path2 = dir.path().join("new.jsonl");
        let mut f2 = File::create(&path2).unwrap();
        writeln!(
            f2,
            "{}",
            serde_json::to_string(&serde_json::json!({
                "type": "user",
                "sessionId": "sess-new",
                "timestamp": "2026-03-26T10:00:00Z",
                "message": { "content": "new" },
            }))
            .unwrap()
        )
        .unwrap();

        let files = vec![path1, path2];
        let sessions = collect_sessions_parallel(&files, None, None);
        assert_eq!(sessions.len(), 2);
        assert_eq!(sessions[0].session_id, "sess-new");
        assert_eq!(sessions[1].session_id, "sess-old");
    }
}
