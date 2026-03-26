use std::collections::HashSet;
use std::fs::File;
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::Arc;

use anyhow::{Context, Result};
use chrono::{DateTime, NaiveDate, Utc};
use clap::{Parser, Subcommand};
use rayon::prelude::*;
use regex::Regex;
use serde_json::Value;
use walkdir::WalkDir;

const BUF_SIZE: usize = 64 * 1024; // 64 KB buffer

#[derive(Parser)]
#[command(name = "claude-history", about = "Search Claude Code conversation logs")]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// Search JSONL conversation logs with regex
    Search {
        /// Regex pattern to search for
        pattern: String,

        /// Show only matching session file paths
        #[arg(short = 'l')]
        files_only: bool,

        /// Show verbose metadata (project, branch, model)
        #[arg(long)]
        verbose: bool,

        /// Output as JSON
        #[arg(long)]
        json: bool,

        /// Filter by project path (substring match)
        #[arg(long)]
        project: Option<String>,

        /// Filter: start date (YYYY-MM-DD)
        #[arg(long)]
        since: Option<String>,

        /// Filter: end date (YYYY-MM-DD)
        #[arg(long)]
        until: Option<String>,

        /// Case-insensitive search
        #[arg(short = 'i', long)]
        ignore_case: bool,

        /// Max results (0 = unlimited)
        #[arg(short = 'n', long, default_value_t = 0)]
        max_results: usize,

        /// Characters of context around match
        #[arg(short = 'C', long, default_value_t = 80)]
        context_chars: usize,
    },
}

struct SearchMatch {
    session_id: String,
    file_path: PathBuf,
    timestamp: String,
    msg_type: String,
    matched_text: String,
    project: String,
    git_branch: String,
    cwd: String,
    version: String,
}

struct SearchConfig {
    re: Regex,
    since: Option<DateTime<Utc>>,
    until: Option<DateTime<Utc>>,
    context_chars: usize,
    max_results: usize,
}

fn main() -> Result<()> {
    let cli = Cli::parse();

    match cli.command {
        Commands::Search {
            pattern,
            files_only,
            verbose,
            json,
            project,
            since,
            until,
            ignore_case,
            max_results,
            context_chars,
        } => {
            let regex_pattern = if ignore_case {
                format!("(?i){}", pattern)
            } else {
                pattern
            };
            let re = Regex::new(&regex_pattern).context("Invalid regex pattern")?;

            let since_dt = since.as_deref().map(parse_date_start).transpose()?;
            let until_dt = until.as_deref().map(parse_date_end).transpose()?;

            let base_dir = get_projects_dir()?;
            let jsonl_files = find_jsonl_files(&base_dir, project.as_deref())?;

            let config = SearchConfig {
                re,
                since: since_dt,
                until: until_dt,
                context_chars,
                max_results,
            };

            if files_only {
                let matched_files = search_files_parallel(&jsonl_files, &config);
                print_files_only(&matched_files);
            } else {
                let matches = search_parallel(&jsonl_files, &config);
                if json {
                    print_json(&matches);
                } else if verbose {
                    print_verbose(&matches);
                } else {
                    print_default(&matches);
                }
            }
        }
    }

    Ok(())
}

fn search_files_parallel(files: &[PathBuf], config: &SearchConfig) -> Vec<PathBuf> {
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

fn search_parallel(files: &[PathBuf], config: &SearchConfig) -> Vec<SearchMatch> {
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

fn get_projects_dir() -> Result<PathBuf> {
    let home = dirs::home_dir().context("Could not determine home directory")?;
    let dir = home.join(".claude").join("projects");
    if !dir.exists() {
        anyhow::bail!("Projects directory not found: {}", dir.display());
    }
    Ok(dir)
}

fn find_jsonl_files(base_dir: &Path, project_filter: Option<&str>) -> Result<Vec<PathBuf>> {
    let mut files = Vec::new();

    for entry in WalkDir::new(base_dir)
        .into_iter()
        .filter_map(|e| e.ok())
    {
        let path = entry.path();
        if path.extension().is_some_and(|ext| ext == "jsonl") {
            if let Some(filter) = project_filter {
                if let Ok(rel) = path.strip_prefix(base_dir) {
                    let project_dir = rel
                        .components()
                        .next()
                        .map(|c| c.as_os_str().to_string_lossy().to_string())
                        .unwrap_or_default();
                    let project_path = project_dir.replace('-', "/");
                    if !project_path.contains(filter) && !project_dir.contains(filter) {
                        continue;
                    }
                }
            }
            files.push(path.to_path_buf());
        }
    }

    Ok(files)
}

fn parse_date_start(s: &str) -> Result<DateTime<Utc>> {
    let date = NaiveDate::parse_from_str(s, "%Y-%m-%d")
        .with_context(|| format!("Invalid date format: {s} (expected YYYY-MM-DD)"))?;
    Ok(date.and_hms_opt(0, 0, 0).unwrap().and_utc())
}

fn parse_date_end(s: &str) -> Result<DateTime<Utc>> {
    let date = NaiveDate::parse_from_str(s, "%Y-%m-%d")
        .with_context(|| format!("Invalid date format: {s} (expected YYYY-MM-DD)"))?;
    Ok(date.and_hms_opt(23, 59, 59).unwrap().and_utc())
}

fn parse_timestamp(ts: &str) -> Option<DateTime<Utc>> {
    DateTime::parse_from_rfc3339(ts).ok().map(|dt| dt.to_utc())
}

/// Extract searchable text from a JSONL record, writing into the provided buffer.
/// Returns the slice of the buffer that was written.
fn extract_text_into<'a>(record: &'a Value, buf: &'a mut String) -> &'a str {
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

/// Quick check: does the raw line contain the pattern as a literal substring?
/// This avoids JSON parsing for lines that can't possibly match.
#[inline]
fn line_might_match(line: &str, re: &Regex, literal_prefix: Option<&str>) -> bool {
    if let Some(prefix) = literal_prefix {
        // Fast path: check literal substring with memchr-accelerated contains
        memchr::memmem::find(line.as_bytes(), prefix.as_bytes()).is_some()
    } else {
        // No literal prefix extractable, must check via regex on raw line
        re.is_match(line)
    }
}

/// Try to extract a literal prefix from a regex pattern for fast pre-filtering.
fn extract_literal_prefix(pattern: &str) -> Option<String> {
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

/// Check if a file contains any match (for -l mode).
fn search_file_exists(file_path: &Path, config: &SearchConfig) -> Result<bool> {
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

fn search_file(
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
fn should_process_record(
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

fn extract_project_name(file_path: &Path) -> String {
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

fn extract_snippet(text: &str, start: usize, end: usize, context_chars: usize) -> String {
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

fn print_default(matches: &[SearchMatch]) {
    let stdout = std::io::stdout();
    let mut out = std::io::BufWriter::new(stdout.lock());
    for m in matches {
        let ts = format_timestamp(&m.timestamp);
        let _ = writeln!(out, "{}\t{}\t[{}]\t{}", m.session_id, ts, m.msg_type, m.matched_text);
    }
    eprintln!("{} matches found", matches.len());
}

fn print_verbose(matches: &[SearchMatch]) {
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

fn print_json(matches: &[SearchMatch]) {
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

fn print_files_only(files: &[PathBuf]) {
    let stdout = std::io::stdout();
    let mut out = std::io::BufWriter::new(stdout.lock());
    for f in files {
        let _ = writeln!(out, "{}", f.display());
    }
    eprintln!("{} sessions matched", files.len());
}

fn format_timestamp(ts: &str) -> String {
    DateTime::parse_from_rfc3339(ts)
        .map(|dt| dt.format("%Y-%m-%d %H:%M").to_string())
        .unwrap_or_else(|_| ts.to_string())
}
