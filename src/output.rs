use std::io::Write;
use std::path::PathBuf;

use chrono::DateTime;
use serde_json::Value;

use crate::search::SearchMatch;

pub fn print_default(matches: &[SearchMatch]) {
    let stdout = std::io::stdout();
    let mut out = std::io::BufWriter::new(stdout.lock());
    for m in matches {
        let ts = format_timestamp(&m.timestamp);
        let _ = writeln!(out, "{}\t{}\t[{}]\t{}", m.session_id, ts, m.msg_type, m.matched_text);
    }
    eprintln!("{} matches found", matches.len());
}

pub fn print_verbose(matches: &[SearchMatch]) {
    let stdout = std::io::stdout();
    let mut out = std::io::BufWriter::new(stdout.lock());
    for (i, m) in matches.iter().enumerate() {
        if i > 0 {
            let _ = writeln!(out, "---");
        }
        let ts = format_timestamp(&m.timestamp);
        let _ = writeln!(out, "Session:  {}", m.session_id);
        let _ = writeln!(out, "Time:     {}", ts);
        let _ = writeln!(out, "Type:     {}", m.msg_type);
        let _ = writeln!(out, "Project:  {}", m.project);
        let _ = writeln!(out, "Branch:   {}", m.git_branch);
        let _ = writeln!(out, "Cwd:      {}", m.cwd);
        let _ = writeln!(out, "Version:  {}", m.version);
        let _ = writeln!(out, "Match:    {}", m.matched_text);
    }
    eprintln!("{} matches found", matches.len());
}

pub fn print_json(matches: &[SearchMatch]) {
    let json_matches: Vec<Value> = matches
        .iter()
        .map(|m| {
            serde_json::json!({
                "sessionId": m.session_id,
                "filePath": m.file_path.to_string_lossy(),
                "timestamp": m.timestamp,
                "type": m.msg_type,
                "matchedText": m.matched_text,
                "project": m.project,
                "gitBranch": m.git_branch,
                "cwd": m.cwd,
                "version": m.version,
            })
        })
        .collect();

    println!("{}", serde_json::to_string(&json_matches).unwrap());
}

pub fn print_files_only(files: &[PathBuf]) {
    let stdout = std::io::stdout();
    let mut out = std::io::BufWriter::new(stdout.lock());
    for f in files {
        let _ = writeln!(out, "{}", f.display());
    }
    eprintln!("{} sessions matched", files.len());
}

pub fn format_timestamp(ts: &str) -> String {
    DateTime::parse_from_rfc3339(ts)
        .map(|dt| dt.format("%Y-%m-%d %H:%M").to_string())
        .unwrap_or_else(|_| ts.to_string())
}

pub fn extract_snippet(text: &str, start: usize, end: usize, context_chars: usize) -> String {
    let snippet_start = text
        .char_indices()
        .rev()
        .filter(|&(i, _)| i <= start)
        .nth(context_chars)
        .map(|(i, _)| i)
        .unwrap_or(0);

    let snippet_end = text
        .char_indices()
        .filter(|&(i, _)| i >= end)
        .nth(context_chars)
        .map(|(i, _)| i)
        .unwrap_or(text.len());

    let mut snippet = String::new();
    if snippet_start > 0 {
        snippet.push_str("...");
    }
    snippet.push_str(&text[snippet_start..snippet_end]);
    if snippet_end < text.len() {
        snippet.push_str("...");
    }

    snippet.split_whitespace().collect::<Vec<_>>().join(" ")
}

pub fn extract_project_name(file_path: &std::path::Path) -> String {
    let home = dirs::home_dir().unwrap_or_default();
    let projects_dir = home.join(".claude").join("projects");
    if let Ok(rel) = file_path.strip_prefix(&projects_dir) {
        rel.components()
            .next()
            .map(|c| c.as_os_str().to_string_lossy().to_string())
            .unwrap_or_default()
    } else {
        String::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // --- extract_snippet ---

    #[test]
    fn snippet_short_text() {
        let text = "hello world";
        let snippet = extract_snippet(text, 0, 5, 80);
        assert!(snippet.contains("hello"));
        assert!(snippet.contains("world"));
        assert!(!snippet.starts_with("..."));
    }

    #[test]
    fn snippet_with_ellipsis() {
        let text = "a".repeat(300);
        let snippet = extract_snippet(&text, 150, 155, 10);
        assert!(snippet.starts_with("..."));
        assert!(snippet.ends_with("..."));
    }

    #[test]
    fn snippet_normalizes_whitespace() {
        let text = "hello   \n  world   \t  foo";
        let snippet = extract_snippet(text, 0, 5, 80);
        assert_eq!(snippet, "hello world foo");
    }

    // --- format_timestamp ---

    #[test]
    fn format_valid_timestamp() {
        let result = format_timestamp("2026-03-26T06:00:00.000Z");
        assert_eq!(result, "2026-03-26 06:00");
    }

    #[test]
    fn format_invalid_timestamp_returns_original() {
        let result = format_timestamp("not-a-date");
        assert_eq!(result, "not-a-date");
    }
}
