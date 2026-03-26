use std::collections::HashSet;
use std::fs::File;
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::Arc;

use anyhow::{Context, Result};
use chrono::{DateTime, NaiveDate, Utc};
use rayon::prelude::*;
use regex::Regex;
use serde_json::Value;

use crate::jsonl::{extract_literal_prefix, extract_text_into, line_might_match};
use crate::output::{extract_project_name, extract_snippet};

pub const BUF_SIZE: usize = 64 * 1024; // 64 KB buffer

pub struct SearchMatch {
    pub session_id: String,
    pub file_path: PathBuf,
    pub timestamp: String,
    pub msg_type: String,
    pub matched_text: String,
    pub project: String,
    pub git_branch: String,
    pub cwd: String,
    pub version: String,
}

pub struct SearchConfig {
    pub re: Regex,
    pub since: Option<DateTime<Utc>>,
    pub until: Option<DateTime<Utc>>,
    pub context_chars: usize,
    pub max_results: usize,
}

pub fn parse_date_start(s: &str) -> Result<DateTime<Utc>> {
    let date = NaiveDate::parse_from_str(s, "%Y-%m-%d")
        .with_context(|| format!("Invalid date format: {s} (expected YYYY-MM-DD)"))?;
    Ok(date.and_hms_opt(0, 0, 0).unwrap().and_utc())
}

pub fn parse_date_end(s: &str) -> Result<DateTime<Utc>> {
    let date = NaiveDate::parse_from_str(s, "%Y-%m-%d")
        .with_context(|| format!("Invalid date format: {s} (expected YYYY-MM-DD)"))?;
    Ok(date.and_hms_opt(23, 59, 59).unwrap().and_utc())
}

pub fn parse_timestamp(ts: &str) -> Option<DateTime<Utc>> {
    DateTime::parse_from_rfc3339(ts).ok().map(|dt| dt.to_utc())
}

pub fn search_files_parallel(files: &[PathBuf], config: &SearchConfig) -> Vec<PathBuf> {
    let done = Arc::new(AtomicBool::new(false));
    let count = Arc::new(AtomicUsize::new(0));

    let results: Vec<Option<PathBuf>> = files
        .par_iter()
        .map(|file_path| {
            if done.load(Ordering::Relaxed) {
                return None;
            }
            let matched = search_file_exists(file_path, config).unwrap_or(false);
            if matched {
                let prev = count.fetch_add(1, Ordering::Relaxed);
                if config.max_results > 0 && prev + 1 >= config.max_results {
                    done.store(true, Ordering::Relaxed);
                }
                Some(file_path.clone())
            } else {
                None
            }
        })
        .collect();

    let mut matched: Vec<PathBuf> = results.into_iter().flatten().collect();
    if config.max_results > 0 {
        matched.truncate(config.max_results);
    }
    matched
}

pub fn search_parallel(files: &[PathBuf], config: &SearchConfig) -> Vec<SearchMatch> {
    let done = Arc::new(AtomicBool::new(false));
    let count = Arc::new(AtomicUsize::new(0));

    let file_results: Vec<Vec<SearchMatch>> = files
        .par_iter()
        .map(|file_path| {
            if done.load(Ordering::Relaxed) {
                return Vec::new();
            }
            let mut matches = Vec::new();
            let _ = search_file(file_path, config, &mut matches);
            if !matches.is_empty() && config.max_results > 0 {
                let prev = count.fetch_add(matches.len(), Ordering::Relaxed);
                if prev + matches.len() >= config.max_results {
                    done.store(true, Ordering::Relaxed);
                }
            }
            matches
        })
        .collect();

    let mut all_matches: Vec<SearchMatch> =
        file_results.into_iter().flat_map(|v| v.into_iter()).collect();
    if config.max_results > 0 {
        all_matches.truncate(config.max_results);
    }
    all_matches
}

/// Check if a file contains any match (for -l mode).
pub fn search_file_exists(file_path: &Path, config: &SearchConfig) -> Result<bool> {
    let file = File::open(file_path)?;
    let reader = BufReader::with_capacity(BUF_SIZE, file);
    let mut seen_request_ids: HashSet<String> = HashSet::new();
    let literal_prefix = extract_literal_prefix(config.re.as_str());
    let mut text_buf = String::new();

    for line in reader.lines() {
        let line = match line {
            Ok(l) => l,
            Err(_) => continue,
        };

        if line.is_empty() {
            continue;
        }

        // Pre-filter: skip lines that can't match
        if !line_might_match(&line, &config.re, literal_prefix.as_deref()) {
            continue;
        }

        let record: Value = match serde_json::from_str(&line) {
            Ok(v) => v,
            Err(_) => continue,
        };

        if !should_process_record(&record, &mut seen_request_ids, config) {
            continue;
        }

        let text = extract_text_into(&record, &mut text_buf);
        if text.is_empty() {
            continue;
        }

        if config.re.is_match(text) {
            return Ok(true);
        }
    }

    Ok(false)
}

pub fn search_file(
    file_path: &Path,
    config: &SearchConfig,
    matches: &mut Vec<SearchMatch>,
) -> Result<()> {
    let file = File::open(file_path)?;
    let reader = BufReader::with_capacity(BUF_SIZE, file);
    let mut seen_request_ids: HashSet<String> = HashSet::new();
    let literal_prefix = extract_literal_prefix(config.re.as_str());
    let project = extract_project_name(file_path);
    let mut text_buf = String::new();

    for line in reader.lines() {
        let line = match line {
            Ok(l) => l,
            Err(_) => continue,
        };

        if line.is_empty() {
            continue;
        }

        // Pre-filter: skip lines that can't match
        if !line_might_match(&line, &config.re, literal_prefix.as_deref()) {
            continue;
        }

        let record: Value = match serde_json::from_str(&line) {
            Ok(v) => v,
            Err(_) => continue,
        };

        if !should_process_record(&record, &mut seen_request_ids, config) {
            continue;
        }

        let text = extract_text_into(&record, &mut text_buf);
        if text.is_empty() {
            continue;
        }

        if let Some(m) = config.re.find(text) {
            let snippet = extract_snippet(text, m.start(), m.end(), config.context_chars);

            let session_id = record
                .get("sessionId")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();

            let timestamp = record
                .get("timestamp")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();

            let record_type = record
                .get("type")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();

            let git_branch = record
                .get("gitBranch")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();

            let cwd = record
                .get("cwd")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();

            let version = record
                .get("version")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();

            matches.push(SearchMatch {
                session_id,
                file_path: file_path.to_path_buf(),
                timestamp,
                msg_type: record_type,
                matched_text: snippet,
                project: project.clone(),
                git_branch,
                cwd,
                version,
            });
        }
    }

    Ok(())
}

/// Determine whether a JSONL record should be processed for search.
pub fn should_process_record(
    record: &Value,
    seen_request_ids: &mut HashSet<String>,
    config: &SearchConfig,
) -> bool {
    let record_type = record
        .get("type")
        .and_then(|v| v.as_str())
        .unwrap_or("");

    if record_type != "user" && record_type != "assistant" {
        return false;
    }

    // Deduplicate assistant streaming chunks
    if record_type == "assistant" {
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
                return false;
            }

            if !seen_request_ids.insert(request_id.to_string()) {
                return false;
            }
        }
    }

    // Date filter
    if config.since.is_some() || config.until.is_some() {
        if let Some(ts_str) = record.get("timestamp").and_then(|v| v.as_str()) {
            if let Some(ts) = parse_timestamp(ts_str) {
                if config.since.as_ref().is_some_and(|s| ts < *s) {
                    return false;
                }
                if config.until.as_ref().is_some_and(|u| ts > *u) {
                    return false;
                }
            }
        }
    }

    true
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;
    use std::fs::File;
    use std::io::Write as IoWrite;
    use std::path::Path;

    fn make_config(pattern: &str) -> SearchConfig {
        SearchConfig {
            re: Regex::new(pattern).unwrap(),
            since: None,
            until: None,
            context_chars: 80,
            max_results: 0,
        }
    }

    fn make_config_with_dates(
        pattern: &str,
        since: Option<&str>,
        until: Option<&str>,
    ) -> SearchConfig {
        SearchConfig {
            re: Regex::new(pattern).unwrap(),
            since: since.map(|s| parse_date_start(s).unwrap()),
            until: until.map(|s| parse_date_end(s).unwrap()),
            context_chars: 80,
            max_results: 0,
        }
    }

    fn write_jsonl(dir: &Path, lines: &[Value]) -> PathBuf {
        let path = dir.join("test-session.jsonl");
        let mut f = File::create(&path).unwrap();
        for line in lines {
            writeln!(f, "{}", serde_json::to_string(line).unwrap()).unwrap();
        }
        path
    }

    // --- should_process_record ---

    #[test]
    fn process_user_message() {
        let record: Value = serde_json::json!({
            "type": "user",
            "timestamp": "2026-01-15T10:00:00Z"
        });
        let config = make_config(".");
        let mut seen = HashSet::new();
        assert!(should_process_record(&record, &mut seen, &config));
    }

    #[test]
    fn skip_system_message() {
        let record: Value = serde_json::json!({ "type": "system" });
        let config = make_config(".");
        let mut seen = HashSet::new();
        assert!(!should_process_record(&record, &mut seen, &config));
    }

    #[test]
    fn skip_progress_message() {
        let record: Value = serde_json::json!({ "type": "progress" });
        let config = make_config(".");
        let mut seen = HashSet::new();
        assert!(!should_process_record(&record, &mut seen, &config));
    }

    #[test]
    fn assistant_non_final_chunk_skipped() {
        let record: Value = serde_json::json!({
            "type": "assistant",
            "message": {
                "requestId": "req_001",
                "stop_reason": null,
                "content": [{ "type": "text", "text": "partial" }]
            }
        });
        let config = make_config(".");
        let mut seen = HashSet::new();
        assert!(!should_process_record(&record, &mut seen, &config));
    }

    #[test]
    fn assistant_final_chunk_processed() {
        let record: Value = serde_json::json!({
            "type": "assistant",
            "message": {
                "requestId": "req_001",
                "stop_reason": "end_turn",
                "content": [{ "type": "text", "text": "final" }]
            }
        });
        let config = make_config(".");
        let mut seen = HashSet::new();
        assert!(should_process_record(&record, &mut seen, &config));
    }

    #[test]
    fn assistant_duplicate_request_id_skipped() {
        let record: Value = serde_json::json!({
            "type": "assistant",
            "message": {
                "requestId": "req_001",
                "stop_reason": "end_turn",
                "content": [{ "type": "text", "text": "final" }]
            }
        });
        let config = make_config(".");
        let mut seen = HashSet::new();
        assert!(should_process_record(&record, &mut seen, &config));
        // Second time with same requestId should be skipped
        assert!(!should_process_record(&record, &mut seen, &config));
    }

    #[test]
    fn date_filter_since() {
        let record: Value = serde_json::json!({
            "type": "user",
            "timestamp": "2026-01-01T10:00:00Z"
        });
        let config = make_config_with_dates(".", Some("2026-02-01"), None);
        let mut seen = HashSet::new();
        assert!(!should_process_record(&record, &mut seen, &config));
    }

    #[test]
    fn date_filter_until() {
        let record: Value = serde_json::json!({
            "type": "user",
            "timestamp": "2026-03-15T10:00:00Z"
        });
        let config = make_config_with_dates(".", None, Some("2026-02-28"));
        let mut seen = HashSet::new();
        assert!(!should_process_record(&record, &mut seen, &config));
    }

    #[test]
    fn date_filter_in_range() {
        let record: Value = serde_json::json!({
            "type": "user",
            "timestamp": "2026-02-15T10:00:00Z"
        });
        let config = make_config_with_dates(".", Some("2026-02-01"), Some("2026-02-28"));
        let mut seen = HashSet::new();
        assert!(should_process_record(&record, &mut seen, &config));
    }

    // --- parse_date ---

    #[test]
    fn parse_date_start_valid() {
        let dt = parse_date_start("2026-03-01").unwrap();
        assert_eq!(dt.format("%Y-%m-%d %H:%M:%S").to_string(), "2026-03-01 00:00:00");
    }

    #[test]
    fn parse_date_end_valid() {
        let dt = parse_date_end("2026-03-01").unwrap();
        assert_eq!(dt.format("%Y-%m-%d %H:%M:%S").to_string(), "2026-03-01 23:59:59");
    }

    #[test]
    fn parse_date_invalid() {
        assert!(parse_date_start("2026-13-01").is_err());
        assert!(parse_date_start("not-a-date").is_err());
    }

    // --- search_file integration ---

    #[test]
    fn search_file_finds_user_message() {
        let dir = tempfile::tempdir().unwrap();
        let path = write_jsonl(
            dir.path(),
            &[serde_json::json!({
                "type": "user",
                "sessionId": "sess-1",
                "timestamp": "2026-03-26T10:00:00Z",
                "message": { "content": "deploy the Terraform module" },
                "cwd": "/project",
                "gitBranch": "main",
                "version": "2.0.0"
            })],
        );
        let config = make_config("Terraform");
        let mut matches = Vec::new();
        search_file(&path, &config, &mut matches).unwrap();
        assert_eq!(matches.len(), 1);
        assert_eq!(matches[0].session_id, "sess-1");
        assert_eq!(matches[0].msg_type, "user");
        assert!(matches[0].matched_text.contains("Terraform"));
    }

    #[test]
    fn search_file_skips_non_final_assistant() {
        let dir = tempfile::tempdir().unwrap();
        let path = write_jsonl(
            dir.path(),
            &[
                serde_json::json!({
                    "type": "assistant",
                    "sessionId": "sess-1",
                    "timestamp": "2026-03-26T10:00:00Z",
                    "message": {
                        "requestId": "req_001",
                        "stop_reason": null,
                        "content": [{ "type": "text", "text": "partial Terraform" }]
                    },
                    "cwd": "/project", "gitBranch": "main", "version": "2.0.0"
                }),
                serde_json::json!({
                    "type": "assistant",
                    "sessionId": "sess-1",
                    "timestamp": "2026-03-26T10:00:01Z",
                    "message": {
                        "requestId": "req_001",
                        "stop_reason": "end_turn",
                        "content": [{ "type": "text", "text": "final Terraform answer" }]
                    },
                    "cwd": "/project", "gitBranch": "main", "version": "2.0.0"
                }),
            ],
        );
        let config = make_config("Terraform");
        let mut matches = Vec::new();
        search_file(&path, &config, &mut matches).unwrap();
        assert_eq!(matches.len(), 1);
        assert!(matches[0].matched_text.contains("final"));
    }

    #[test]
    fn search_file_date_filter() {
        let dir = tempfile::tempdir().unwrap();
        let path = write_jsonl(
            dir.path(),
            &[
                serde_json::json!({
                    "type": "user",
                    "sessionId": "sess-old",
                    "timestamp": "2025-01-01T10:00:00Z",
                    "message": { "content": "old Terraform" },
                    "cwd": "/project", "gitBranch": "main", "version": "1.0.0"
                }),
                serde_json::json!({
                    "type": "user",
                    "sessionId": "sess-new",
                    "timestamp": "2026-03-15T10:00:00Z",
                    "message": { "content": "new Terraform" },
                    "cwd": "/project", "gitBranch": "main", "version": "2.0.0"
                }),
            ],
        );
        let config = make_config_with_dates("Terraform", Some("2026-01-01"), None);
        let mut matches = Vec::new();
        search_file(&path, &config, &mut matches).unwrap();
        assert_eq!(matches.len(), 1);
        assert_eq!(matches[0].session_id, "sess-new");
    }

    #[test]
    fn search_file_no_match() {
        let dir = tempfile::tempdir().unwrap();
        let path = write_jsonl(
            dir.path(),
            &[serde_json::json!({
                "type": "user",
                "sessionId": "sess-1",
                "timestamp": "2026-03-26T10:00:00Z",
                "message": { "content": "hello world" },
                "cwd": "/project", "gitBranch": "main", "version": "2.0.0"
            })],
        );
        let config = make_config("xyznonexistent");
        let mut matches = Vec::new();
        search_file(&path, &config, &mut matches).unwrap();
        assert_eq!(matches.len(), 0);
    }

    #[test]
    fn search_file_exists_returns_true() {
        let dir = tempfile::tempdir().unwrap();
        let path = write_jsonl(
            dir.path(),
            &[serde_json::json!({
                "type": "user",
                "sessionId": "sess-1",
                "timestamp": "2026-03-26T10:00:00Z",
                "message": { "content": "Terraform module" },
                "cwd": "/project", "gitBranch": "main", "version": "2.0.0"
            })],
        );
        let config = make_config("Terraform");
        assert!(search_file_exists(&path, &config).unwrap());
    }

    #[test]
    fn search_file_exists_returns_false() {
        let dir = tempfile::tempdir().unwrap();
        let path = write_jsonl(
            dir.path(),
            &[serde_json::json!({
                "type": "user",
                "sessionId": "sess-1",
                "timestamp": "2026-03-26T10:00:00Z",
                "message": { "content": "hello world" },
                "cwd": "/project", "gitBranch": "main", "version": "2.0.0"
            })],
        );
        let config = make_config("xyznonexistent");
        assert!(!search_file_exists(&path, &config).unwrap());
    }

    #[test]
    fn search_file_skips_malformed_lines() {
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
                "message": { "content": "valid Terraform line" },
                "cwd": "/project", "gitBranch": "main", "version": "2.0.0"
            }))
            .unwrap()
        )
        .unwrap();
        let config = make_config("Terraform");
        let mut matches = Vec::new();
        search_file(&path, &config, &mut matches).unwrap();
        assert_eq!(matches.len(), 1);
    }

    #[test]
    fn search_file_mixed_content_types() {
        let dir = tempfile::tempdir().unwrap();
        let path = write_jsonl(
            dir.path(),
            &[
                // String content
                serde_json::json!({
                    "type": "user",
                    "sessionId": "sess-1",
                    "timestamp": "2026-03-26T10:00:00Z",
                    "message": { "content": "search for TARGET_WORD" },
                    "cwd": "/project", "gitBranch": "main", "version": "2.0.0"
                }),
                // Array content with text
                serde_json::json!({
                    "type": "assistant",
                    "sessionId": "sess-1",
                    "timestamp": "2026-03-26T10:00:01Z",
                    "message": {
                        "requestId": "req_002",
                        "stop_reason": "end_turn",
                        "content": [
                            { "type": "thinking", "thinking": "thinking about TARGET_WORD" },
                            { "type": "text", "text": "response text" }
                        ]
                    },
                    "cwd": "/project", "gitBranch": "main", "version": "2.0.0"
                }),
            ],
        );
        let config = make_config("TARGET_WORD");
        let mut matches = Vec::new();
        search_file(&path, &config, &mut matches).unwrap();
        assert_eq!(matches.len(), 2);
    }
}
