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

**Worktrees:** if you run several sessions in the same repo across different [git worktrees](https://git-scm.com/docs/git-worktree), `tend` shows the worktree name (marked `⑂`) right next to the branch, so sibling sessions stay easy to tell apart.

## Install

The quickest way — downloads the latest release binary into `~/.local/bin`,
verifies its SHA-256 checksum, and handles `chmod`/Gatekeeper for you:

```sh
curl -fsSL https://raw.githubusercontent.com/jah2488/tend/main/install.sh | bash
```

Piping a script straight into a shell deserves a look first — read exactly what
you'd be running here:
[`install.sh`](https://github.com/jah2488/tend/blob/main/install.sh). It uses
strict mode, installs only to `~/.local/bin` (override with `TEND_INSTALL_DIR`),
needs no root, and runs nothing it downloads. If you'd rather not pipe, download
that file, read it, and run it locally.

Make sure `~/.local/bin` is on your `PATH`, then run `tend`. A pre-built binary
is published for macOS (Apple Silicon); other platforms build from source below.

### From source

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

## Update

Already have `tend` installed? Pull the latest and reinstall:

```sh
cd tend
git pull
cargo install --path . --force
```

`--force` is required — `cargo install` skips reinstalling unless you pass it. Then quit any running instance (`q`) and relaunch `tend`; a running process keeps the old binary until you restart it.

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
