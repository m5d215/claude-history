---
name: claude-history
description: Search past Claude Code sessions. TRIGGER when you need to search, grep, or read anything under ~/.claude/projects/.
---

# claude-history

Use the `claude-history` CLI instead of Grep/Read/Bash(grep) when searching session logs under `~/.claude/projects/`.

## Examples

```bash
# List matching sessions
claude-history search -i -l --verbose 'PATTERN'

# Show matches with context
claude-history search -i -C 200 'PATTERN'

# Filter by project
claude-history search -i -l --verbose 'PATTERN' --project 'claude-history'
```

Run `claude-history search -h` for advanced search options.
