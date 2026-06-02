# tend

A small terminal UI that keeps your [Claude Code](https://claude.com/claude-code) sessions in view — which are **working**, which **need you**, and which are **done**.

If you run several Claude Code sessions at once (terminal, editor, SDK), it's easy to lose track of which one is blocked on a permission prompt and which is quietly churning. `tend` watches them all and shows a live, glanceable list.

## What it shows

Per session, at a glance:

- **State** — working · needs you · idle · done · stale · error (color-coded, animated when live)
- **What it's doing** — a one-line summary, current branch, and any PR opened during the session
- **Cost** — total tokens, a context-fullness bar, and live CPU%
- **Activity** — tool calls and web requests made, plus which integrations (Notion, Slack, …) it touched

Terminal sessions get a full card; editor/SDK sessions collapse into a compact one-liner so background work doesn't crowd out what you care about.

## Install

Requires a [Rust toolchain](https://rustup.rs).

```sh
git clone <this-repo> tend
cd tend
cargo install --path .
```

Then run it:

```sh
tend
```

Or just run it in place without installing:

```sh
cargo run --release
```

## How it works

`tend` reads the session metadata Claude Code already writes under `~/.claude/` — session files in `~/.claude/sessions/` and transcripts in `~/.claude/projects/`. It parses these read-only and never modifies them. It checks whether each session's process is still alive to tell live work from finished work.

No network calls, no config, no telemetry.

## Keys

| Key | Action |
| --- | --- |
| `↑` / `↓` or `j` / `k` | Move selection |
| `r` / `s` | Refresh now |
| `q` / `Esc` | Quit |

## License

MIT — see [LICENSE](LICENSE).
