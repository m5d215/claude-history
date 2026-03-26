use std::fs::File;
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use chrono::{DateTime, NaiveDate, Utc};
use clap::{Parser, Subcommand};
use regex::Regex;
use serde_json::Value;
use walkdir::WalkDir;

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

            let mut matches: Vec<SearchMatch> = Vec::new();
            let mut matched_files: Vec<PathBuf> = Vec::new();

            for file_path in &jsonl_files {
                let file_matched = search_file(
                    file_path,
                    &re,
                    since_dt.as_ref(),
                    until_dt.as_ref(),
                    context_chars,
                    files_only,
                    &mut matches,
                )?;

                if files_only && file_matched {
                    matched_files.push(file_path.clone());
                }

                if max_results > 0 {
                    if files_only && matched_files.len() >= max_results {
                        break;
                    }
                    if !files_only && matches.len() >= max_results {
                        matches.truncate(max_results);
                        break;
                    }
                }
            }

            if files_only {
                print_files_only(&matched_files);
            } else if json {
                print_json(&matches);
            } else if verbose {
                print_verbose(&matches);
            } else {
                print_default(&matches);
            }
        }
    }

    Ok(())
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

fn extract_text(record: &Value) -> String {
    let message = match record.get("message") {
        Some(m) => m,
        None => return String::new(),
    };

    let content = match message.get("content") {
        Some(c) => c,
        None => return String::new(),
    };

    match content {
        Value::String(s) => s.clone(),
        Value::Array(arr) => {
            let mut parts = Vec::new();
            for item in arr {
                if let Some(text) = item.get("text").and_then(|v| v.as_str()) {
                    parts.push(text.to_string());
                }
                if let Some(thinking) = item.get("thinking").and_then(|v| v.as_str()) {
                    parts.push(thinking.to_string());
                }
                // tool_result content (can be string or array)
                if let Some(content_val) = item.get("content") {
                    if let Value::Array(inner) = content_val {
                        for inner_item in inner {
                            if let Some(text) = inner_item.get("text").and_then(|v| v.as_str()) {
                                parts.push(text.to_string());
                            }
                        }
                    } else if let Value::String(s) = content_val {
                        parts.push(s.clone());
                    }
                }
                // tool_use input values
                if let Some(Value::Object(map)) = item.get("input") {
                    for v in map.values() {
                        if let Value::String(s) = v {
                            parts.push(s.clone());
                        }
                    }
                }
            }
            parts.join("\n")
        }
        _ => String::new(),
    }
}

fn search_file(
    file_path: &Path,
    re: &Regex,
    since: Option<&DateTime<Utc>>,
    until: Option<&DateTime<Utc>>,
    context_chars: usize,
    files_only: bool,
    matches: &mut Vec<SearchMatch>,
) -> Result<bool> {
    let file = File::open(file_path)?;
    let reader = BufReader::new(file);
    let mut found = false;
    let mut seen_request_ids: std::collections::HashSet<String> =
        std::collections::HashSet::new();

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

        let record_type = record
            .get("type")
            .and_then(|v| v.as_str())
            .unwrap_or("");

        // Only search user and assistant messages
        if record_type != "user" && record_type != "assistant" {
            continue;
        }

        // Deduplicate assistant streaming chunks: only process final chunk
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
                    continue;
                }

                if !seen_request_ids.insert(request_id.to_string()) {
                    // Already processed this requestId
                    continue;
                }
            }
        }

        // Date filter
        if since.is_some() || until.is_some() {
            if let Some(ts_str) = record.get("timestamp").and_then(|v| v.as_str()) {
                if let Some(ts) = parse_timestamp(ts_str) {
                    if since.is_some_and(|s| ts < *s) {
                        continue;
                    }
                    if until.is_some_and(|u| ts > *u) {
                        continue;
                    }
                }
            }
        }

        let text = extract_text(&record);
        if text.is_empty() {
            continue;
        }

        if let Some(m) = re.find(&text) {
            found = true;

            if files_only {
                return Ok(true);
            }

            let snippet = extract_snippet(&text, m.start(), m.end(), context_chars);

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

            let project = extract_project_name(file_path);

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
                msg_type: record_type.to_string(),
                matched_text: snippet,
                project,
                git_branch,
                cwd,
                version,
            });
        }
    }

    Ok(found)
}

fn extract_project_name(file_path: &Path) -> String {
    // Walk up to find the project directory (parent of the JSONL or parent of subagents/)
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

    // Normalize whitespace for display
    snippet.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn print_default(matches: &[SearchMatch]) {
    for m in matches {
        let ts = format_timestamp(&m.timestamp);
        println!(
            "{}\t{}\t[{}]\t{}",
            m.session_id, ts, m.msg_type, m.matched_text
        );
    }
    eprintln!("{} matches found", matches.len());
}

fn print_verbose(matches: &[SearchMatch]) {
    for (i, m) in matches.iter().enumerate() {
        if i > 0 {
            println!("---");
        }
        let ts = format_timestamp(&m.timestamp);
        println!("Session:  {}", m.session_id);
        println!("Time:     {}", ts);
        println!("Type:     {}", m.msg_type);
        println!("Project:  {}", m.project);
        println!("Branch:   {}", m.git_branch);
        println!("Cwd:      {}", m.cwd);
        println!("Version:  {}", m.version);
        println!("Match:    {}", m.matched_text);
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
    for f in files {
        println!("{}", f.display());
    }
    eprintln!("{} sessions matched", files.len());
}

fn format_timestamp(ts: &str) -> String {
    DateTime::parse_from_rfc3339(ts)
        .map(|dt| dt.format("%Y-%m-%d %H:%M").to_string())
        .unwrap_or_else(|_| ts.to_string())
}
