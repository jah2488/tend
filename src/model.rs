use crate::tint::Tint;
use ratatui::style::Color;
use std::path::PathBuf;

/// The lifecycle state of a session. Drives color, glyph, and animation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum State {
    Working,
    NeedsYou,
    Idle, // live process, but not actively working (status "idle" or unknown)
    Done,  // finished cleanly, recently
    Stale, // finished a while ago, safe to ignore
    Error, // last turn ended in an error
}

impl State {
    /// Base One Dark-ish color for the state's rule + glyph.
    pub fn color(self) -> Color {
        match self {
            State::Working => Color::Rgb(0x56, 0xB6, 0xC2),  // calm cyan-blue
            State::NeedsYou => Color::Rgb(0xE5, 0xC0, 0x7B), // warm amber
            State::Idle => Color::Rgb(0x6B, 0x73, 0x89),     // muted slate
            State::Done => Color::Rgb(0x98, 0xC3, 0x79),     // soft green
            State::Stale => Color::Rgb(0x5C, 0x63, 0x70),    // dim grey
            State::Error => Color::Rgb(0xE0, 0x6C, 0x75),    // soft red
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            State::Working => "WORKING",
            State::NeedsYou => "NEEDS YOU",
            State::Idle => "IDLE",
            State::Done => "DONE",
            State::Stale => "STALE",
            State::Error => "ERROR",
        }
    }

    /// Stable lowercase slug for JSON outputs (`tend mcp`) and the extension
    /// `when.state` filter. The single source of truth for the spelling.
    pub fn slug(self) -> &'static str {
        match self {
            State::Working => "working",
            State::NeedsYou => "needs-you",
            State::Idle => "idle",
            State::Done => "done",
            State::Stale => "stale",
            State::Error => "error",
        }
    }

    /// Trailing status glyph (the animated spinner is handled separately for Working).
    pub fn glyph(self) -> &'static str {
        match self {
            State::Working => "\u{25D0}", // ◐ (placeholder; spinner used in the rail)
            State::NeedsYou => "\u{25C6}", // ◆
            State::Idle => "\u{25CB}",    // ○
            State::Done => "\u{2713}",    // ✓
            State::Stale => "\u{25CC}",   // ◌
            State::Error => "\u{2715}",   // ✕
        }
    }
}

/// Where the session lives. Desktop is reserved for a future adapter.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Source {
    Terminal,
    Sdk, // launched programmatically by an editor/SDK (entrypoint sdk-ts, sdk-cli, …)
    #[allow(dead_code)]
    Desktop,
}

impl Source {
    pub fn glyph(self) -> &'static str {
        match self {
            Source::Terminal => "\u{276F}", // ❯
            Source::Sdk => "\u{25C7}",      // ◇
            Source::Desktop => "\u{2726}",  // ✦
        }
    }

    /// Short tag shown next to the session name, for sources that aren't a plain terminal.
    pub fn badge(self) -> Option<&'static str> {
        match self {
            Source::Sdk => Some("sdk"),
            _ => None,
        }
    }

    /// Stable lowercase slug for JSON outputs and the `TEND_SOURCE` env var.
    pub fn slug(self) -> &'static str {
        match self {
            Source::Terminal => "terminal",
            Source::Sdk => "sdk",
            Source::Desktop => "desktop",
        }
    }
}

#[derive(Debug, Clone)]
pub struct Session {
    /// Claude Code's stable session id — the durable handle an extension acts on
    /// (the UI itself works on list index, which reorders between refreshes).
    pub session_id: String,
    /// Absolute path to the session's transcript `.jsonl`, when it exists on disk.
    /// Passed to extensions as a locator so they read session data fresh themselves.
    pub transcript_path: Option<PathBuf>,
    pub source: Source,
    pub state: State,
    /// Human name: the session's own `name`, else the project dir name.
    pub name: String,
    pub cwd: String,
    /// What it's waiting on, when NeedsYou (e.g. "permission prompt").
    pub waiting_for: Option<String>,
    /// Cumulative tokens processed across the transcript.
    pub total_tokens: u64,
    /// Approx current context size (last turn), for the fullness bar.
    pub context_tokens: u64,
    /// Total tool_use calls across the transcript.
    pub tool_calls: u64,
    /// WebFetch + WebSearch calls.
    pub web_requests: u64,
    /// ms since the session was last updated.
    pub age_ms: i64,
    /// One-line summary of what it's actually doing (stub until AI is wired).
    pub summary: String,
    /// Distinct integrations touched, e.g. ["Notion", "Slack"].
    pub integrations: Vec<String>,
    /// For non-terminal sources, the host that launched it (e.g. "Zed"), if known.
    pub origin: Option<String>,
    /// Model the session runs, e.g. "claude-opus-4-7".
    pub model: Option<String>,
    /// Git branch the session is working on.
    pub git_branch: Option<String>,
    /// Linked git worktree name, when the cwd is a worktree (not the main checkout).
    pub worktree: Option<String>,
    /// Most recent PR opened during the session.
    pub pr_number: Option<u64>,
    pub pr_url: Option<String>,
    /// Wall-clock span of transcript activity (first→last).
    pub active_span_ms: Option<i64>,
    /// Live process CPU%, when the session is running.
    pub cpu_pct: Option<f32>,
    /// User-chosen pip color, distinct from the lifecycle State color. Set by
    /// `tend-color` (or any writer of the `~/.claude/tend-color/<id>` file).
    /// `None` = no pip; column space is still reserved for layout.
    pub tint: Option<Tint>,
    /// User-chosen one-line note, distinct from the auto-derived `summary`.
    /// Set by `tend mcp` (or any writer of the `~/.claude/tend-note/<id>`
    /// file). `None` = no note line drawn.
    pub note: Option<String>,
}
