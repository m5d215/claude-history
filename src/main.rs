mod config;
mod jsonl;
mod output;
mod path;
mod search;
mod sessions;
mod show;

use anyhow::{Context, Result};
use clap::{Parser, Subcommand};

use output::{
    print_default, print_files_only, print_json, print_sessions, print_sessions_json, print_verbose,
};
use path::{filter_by_profile, find_jsonl_files, resolve_projects_dirs};
use search::{
    parse_date_end, parse_date_start, search_files_parallel, search_parallel, SearchConfig,
};
use sessions::collect_sessions_parallel;
use show::{extract_messages_from_file, find_session_files, print_conversation};

#[derive(Parser)]
#[command(name = "claude-history", about = "Search Claude Code conversation logs")]
struct Cli {
    /// Override config directories (comma-separated). Takes precedence over config file and CLAUDE_CONFIG_DIR.
    #[arg(long, global = true)]
    config_dir: Option<String>,

    /// Restrict to a single profile by name (e.g. "personal", "default").
    #[arg(long, global = true)]
    profile: Option<String>,

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

        /// Exclude projects matching substring
        #[arg(long)]
        exclude_project: Vec<String>,

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

    let file_cfg = config::load().context("Failed to load config file")?;
    let config_file_dirs = file_cfg
        .as_ref()
        .and_then(|c| c.config_dirs.as_deref());
    let projects_dirs = resolve_projects_dirs(cli.config_dir.as_deref(), config_file_dirs)?;
    let projects_dirs = filter_by_profile(projects_dirs, cli.profile.as_deref())?;

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

            let jsonl_files = find_jsonl_files(&projects_dirs, project.as_deref())?;

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
                let matches = search_parallel(&jsonl_files, &projects_dirs, &config);
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
            exclude_project,
            since,
            until,
            json,
        } => {
            let since_dt = since.as_deref().map(parse_date_start).transpose()?;
            let until_dt = until.as_deref().map(parse_date_end).transpose()?;

            let jsonl_files = find_jsonl_files(&projects_dirs, project.as_deref())?;

            let mut sessions =
                collect_sessions_parallel(&jsonl_files, &projects_dirs, since_dt, until_dt);
            if !exclude_project.is_empty() {
                sessions.retain(|s| {
                    let project_path = s.project.replace('-', "/");
                    !exclude_project.iter().any(|ex| {
                        project_path.contains(ex.as_str()) || s.project.contains(ex.as_str())
                    })
                });
            }
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
            let files = find_session_files(&projects_dirs, &session_id)?;

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
