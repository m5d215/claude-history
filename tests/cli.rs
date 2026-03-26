use assert_cmd::Command;
use predicates::prelude::*;

fn cmd() -> Command {
    Command::cargo_bin("claude-history").unwrap()
}

// --- Subcommand routing ---

#[test]
fn no_args_shows_help() {
    cmd()
        .assert()
        .failure()
        .stderr(predicate::str::contains("Usage"));
}

#[test]
fn help_flag() {
    cmd()
        .arg("--help")
        .assert()
        .success()
        .stdout(predicate::str::contains("Search Claude Code conversation logs"));
}

#[test]
fn search_help() {
    cmd()
        .args(["search", "--help"])
        .assert()
        .success()
        .stdout(predicate::str::contains("Regex pattern to search for"));
}

#[test]
fn unknown_subcommand() {
    cmd()
        .arg("unknown")
        .assert()
        .failure()
        .stderr(predicate::str::contains("unrecognized subcommand"));
}

// --- Search: required args ---

#[test]
fn search_missing_pattern() {
    cmd()
        .arg("search")
        .assert()
        .failure()
        .stderr(predicate::str::contains("<PATTERN>").or(predicate::str::contains("required")));
}

// --- Search: flag combinations ---

#[test]
fn search_short_flags() {
    // -l, -i, -n, -C should all be accepted
    cmd()
        .args(["search", "-l", "-i", "-n", "5", "-C", "40", "test"])
        .assert()
        .success();
}

#[test]
fn search_long_flags() {
    cmd()
        .args([
            "search",
            "--verbose",
            "--ignore-case",
            "--max-results",
            "5",
            "--context-chars",
            "40",
            "test",
        ])
        .assert()
        .success();
}

#[test]
fn search_json_flag() {
    cmd()
        .args(["search", "--json", "xyznonexistent_cli_test"])
        .assert()
        .success()
        .stdout(predicate::str::starts_with("["));
}

#[test]
fn search_project_filter() {
    cmd()
        .args(["search", "--project", "some-project", "test"])
        .assert()
        .success();
}

#[test]
fn search_date_filters() {
    cmd()
        .args([
            "search",
            "--since",
            "2026-01-01",
            "--until",
            "2026-12-31",
            "xyznonexistent_cli_test",
        ])
        .assert()
        .success();
}

#[test]
fn search_invalid_date_format() {
    cmd()
        .args(["search", "--since", "not-a-date", "test"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("Invalid date format"));
}

#[test]
fn search_invalid_regex() {
    cmd()
        .args(["search", "[invalid"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("Invalid regex"));
}

#[test]
fn search_files_only_output() {
    // -l mode should output file paths, not match details
    cmd()
        .args(["search", "-l", "CLI_TEST_MARKER_12345"])
        .assert()
        .success();
}

#[test]
fn search_max_results_limits_output() {
    cmd()
        .args(["search", "-n", "1", "."])
        .assert()
        .success()
        .stderr(predicate::str::contains("1 matches found"));
}

// --- Sessions subcommand ---

#[test]
fn sessions_help() {
    cmd()
        .args(["sessions", "--help"])
        .assert()
        .success()
        .stdout(predicate::str::contains("List sessions with metadata"));
}

#[test]
fn sessions_runs_without_error() {
    cmd().arg("sessions").assert().success();
}

#[test]
fn sessions_project_filter() {
    cmd()
        .args(["sessions", "--project", "some-project"])
        .assert()
        .success();
}

#[test]
fn sessions_date_filters() {
    cmd()
        .args(["sessions", "--since", "2026-01-01", "--until", "2026-12-31"])
        .assert()
        .success();
}

#[test]
fn sessions_invalid_date() {
    cmd()
        .args(["sessions", "--since", "not-a-date"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("Invalid date format"));
}

#[test]
fn sessions_json_flag() {
    cmd()
        .args(["sessions", "--json"])
        .assert()
        .success()
        .stdout(predicate::str::starts_with("["));
}

// --- Show subcommand ---

#[test]
fn show_help() {
    cmd()
        .args(["show", "--help"])
        .assert()
        .success()
        .stdout(predicate::str::contains("Show conversation for a session"));
}

#[test]
fn show_missing_session_id() {
    cmd()
        .arg("show")
        .assert()
        .failure()
        .stderr(predicate::str::contains("<SESSION_ID>").or(predicate::str::contains("required")));
}

#[test]
fn show_nonexistent_session() {
    cmd()
        .args(["show", "nonexistent-session-id-12345"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("No session found"));
}

#[test]
fn show_max_messages_flag() {
    // -n flag should be accepted
    cmd()
        .args(["show", "-n", "5", "nonexistent-session-id-12345"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("No session found"));
}

// --- Flag conflicts / edge cases ---

#[test]
fn search_n_zero_means_unlimited() {
    // -n 0 should be accepted (means unlimited)
    cmd()
        .args(["search", "-n", "0", "xyznonexistent_cli_test"])
        .assert()
        .success();
}

#[test]
fn search_context_chars_zero() {
    cmd()
        .args(["search", "-C", "0", "xyznonexistent_cli_test"])
        .assert()
        .success();
}

#[test]
fn search_n_negative_rejected() {
    // clap should reject negative values for usize
    cmd()
        .args(["search", "-n", "-1", "test"])
        .assert()
        .failure();
}
