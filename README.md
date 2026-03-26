# claude-history

> "人間は全部覚えてる、思い出せないだけ"

A CLI tool to search Claude Code conversation logs.

Existing memory tools summarize on write, irreversibly discarding information. claude-history searches the raw JSONL data directly — recalling past conversations as they actually happened.

## Install

```sh
cargo install --git https://github.com/m5d215/claude-history
```

Or build locally:

```sh
git clone https://github.com/m5d215/claude-history
cd claude-history
cargo install --path .
```

## Usage

```sh
# Basic search (regex)
claude-history search "pattern"

# Case-insensitive
claude-history search -i "pattern"

# Show matched session paths only
claude-history search -l "pattern"

# Verbose output (project, branch, cwd, version)
claude-history search --verbose "pattern"

# JSON output
claude-history search --json "pattern"

# Filter by project
claude-history search --project "my-repo" "pattern"

# Filter by date range
claude-history search --since 2025-01-01 --until 2025-03-31 "pattern"

# Limit results
claude-history search -n 10 "pattern"

# Context characters around match (default: 80)
claude-history search -C 120 "pattern"
```

## How it works

Streams JSONL conversation logs under `~/.claude/projects/` via BufReader and matches with regex.

- Searches only `user` and `assistant` messages
- Handles both string and array formats of the `content` field
- Deduplicates assistant streaming chunks (same `requestId`) — only the final chunk (`stop_reason` non-null) is used
- Streaming design keeps memory usage low even for 100MB+ files

## License

MIT
