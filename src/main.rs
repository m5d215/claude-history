mod jsonl;
mod output;
mod search;
mod sessions;
mod show;

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use walkdir::WalkDir;

use output::{
    print_default, print_files_only, print_json, print_sessions, print_sessions_json, print_verbose,
};
use search::{
    parse_date_end, parse_date_start, search_files_parallel, search_parallel, SearchConfig,
};
use sessions::collect_sessions_parallel;
use show::{extract_messages_from_file, find_session_files, print_conversation};

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

    /// List sessions with metadata
    Sessions {
        /// Filter by project path (substring match)
        #[arg(long)]
        project: Option<String>,

        /// Filter: start date (YYYY-MM-DD)
        #[arg(long)]
        since: Option<String>,

        /// Filter: end date (YYYY-MM-DD)
        #[arg(long)]
        until: Option<String>,

        /// Output as JSON
        #[arg(long)]
        json: bool,
    },

    /// Show conversation for a session
    Show {
        /// Session ID to display
        session_id: String,

        /// Max messages to show (0 = unlimited)
        #[arg(short = 'n', long, default_value_t = 0)]
        max_messages: usize,

        /// Color output: always, never, auto (default: auto)
        #[arg(long, default_value = "auto")]
        color: String,
    },
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
            let re = regex::Regex::new(&regex_pattern).context("Invalid regex pattern")?;

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
        Commands::Sessions {
            project,
            since,
            until,
            json,
        } => {
            let since_dt = since.as_deref().map(parse_date_start).transpose()?;
            let until_dt = until.as_deref().map(parse_date_end).transpose()?;

            let base_dir = get_projects_dir()?;
            let jsonl_files = find_jsonl_files(&base_dir, project.as_deref())?;

            let sessions = collect_sessions_parallel(&jsonl_files, since_dt, until_dt);
            if json {
                print_sessions_json(&sessions);
            } else {
                print_sessions(&sessions);
            }
        }
        Commands::Show {
            session_id,
            max_messages,
            color,
        } => {
            let base_dir = get_projects_dir()?;
            let files = find_session_files(&base_dir, &session_id)?;

            if files.is_empty() {
                anyhow::bail!("No session found with ID: {}", session_id);
            }

            let mut all_messages = Vec::new();
            for file in &files {
                let mut msgs = extract_messages_from_file(file, &session_id)?;
                all_messages.append(&mut msgs);
            }

            // Sort by timestamp
            all_messages.sort_by(|a, b| a.timestamp.cmp(&b.timestamp));

            let use_color = match color.as_str() {
                "always" => true,
                "never" => false,
                _ => std::io::IsTerminal::is_terminal(&std::io::stdout()),
            };
            print_conversation(&all_messages, max_messages, use_color);
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
