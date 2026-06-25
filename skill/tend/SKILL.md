---
name: tend
description: Query and annotate your other Claude Code sessions through tend. Use when the user asks to check on background or parallel Claude Code sessions (which need attention, what a session is doing, its cost or timeline), or to tag a session with a color pip or a short note so it stands out in tend's dashboard. Requires the tend MCP server (registered from `tend mcp`).
---

# tend

tend is a read-only terminal dashboard for Claude Code sessions. This skill
drives it from inside a Claude Code session through the tend MCP server, so you
can check on sibling sessions and annotate them without leaving the
conversation.

## Setup (once)

1. Install a build of tend that ships the `mcp` subcommand.
2. Register the server with Claude Code:
   ```
   claude mcp add tend -- tend mcp
   ```
   or add to a project `.mcp.json`:
   ```json
   { "mcpServers": { "tend": { "command": "tend", "args": ["mcp"] } } }
   ```

## When to reach for it

- "Which of my sessions need me right now?" -> `needs_attention`
- "What is the <name> session actually doing?" -> `list_sessions` (find the id), then `get_digest`
- "How full is that session's context / how many tokens has it used?" -> `get_session`
- "Mark this one as blocked-on-review / mine / do-not-merge" -> `set_note` (and optionally `set_tint`)

## Tools

Read (no side effects):
- `list_sessions` — every session tend sees, newest-first.
- `needs_attention` — sessions in the `needs-you` or `error` state.
- `get_session` (id) — full detail for one session.
- `get_digest` (id) — a transcript deep-read: tool histogram, integrations, files changed/read, PR, errors, and the full chronological timeline.

Write (annotate; shown in tend's dashboard):
- `set_tint` (id, color) — a colored pip. color is one of red, orange, yellow, green, blue, purple, gray.
- `set_note` (id, note) — one short line under the summary. An empty note clears it.
- `clear_tint` (id) / `clear_note` (id).

## Annotation guidance

Use **tints** for a category (mine, priority, blocked) and **notes** for short,
actionable context ("blocked on review", "do not merge, waiting on Katie").
Keep notes to one line. Don't annotate reflexively; reserve it for things the
human needs to act on or tell apart at a glance. Anything you write is visible
to anyone running tend on the same machine.

## Notes

- tend reads `~/.claude/` read-only. These tools only add small annotation files
  under `~/.claude/tend-color/` and `~/.claude/tend-note/`; they never touch
  session data or transcripts.
- Find the exact session id with `list_sessions`, then pass it to the other
  tools.
