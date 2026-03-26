# claude-history

> "人間は全部覚えてる、思い出せないだけ"

A CLI tool to search and browse Claude Code conversation logs.

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

### search — regex full-text search

```sh
claude-history search "pattern"
claude-history search -i "pattern"              # case-insensitive
claude-history search -l "pattern"              # show matched session paths only
claude-history search --verbose "pattern"       # project, branch, cwd, version
claude-history search --json "pattern"          # JSON output
claude-history search --project "my-repo" "pattern"
claude-history search --since 2025-01-01 --until 2025-03-31 "pattern"
claude-history search -n 10 "pattern"           # limit results
claude-history search -C 120 "pattern"          # context chars (default: 80)
```

### sessions — list sessions

```sh
claude-history sessions
claude-history sessions --project my-repo
claude-history sessions --exclude-project miu
claude-history sessions --since 2025-03-01
claude-history sessions --json
```

### show — display a session's conversation

```sh
claude-history show <session-id>
claude-history show <session-id> -n 20          # last 20 messages
claude-history show <session-id> --color=always # force color (for fzf preview)
```

### fzf integration

Browse sessions interactively with fzf:

```sh
claude-history sessions --json \
  | jq -r '.[] | [.cwd, .sessionId, .project, .lastActivity, .firstUserMessage] | @tsv' \
  | fzf \
      --with-nth=2.. \
      --preview 'claude-history show --color=always --max-messages=10 {2}' \
      --bind 'enter:become(echo -n " cd {1} && claude --resume {2}" | pbcopy)'
```

## How it works

Streams JSONL conversation logs under `~/.claude/projects/` via BufReader and matches with regex.

- Searches only `user` and `assistant` messages
- Handles both string and array formats of the `content` field
- Deduplicates assistant streaming chunks (same `requestId`) — only the final chunk (`stop_reason` non-null) is used
- Streaming design keeps memory usage low even for 100MB+ files
- Parallel processing with rayon for fast search across many files

## License

MIT
