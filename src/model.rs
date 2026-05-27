use ratatui::style::Color;

/// The lifecycle state of a session. Drives color, glyph, and animation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum State {
    Working,  // live process, actively churning
    NeedsYou, // live process, waiting on you (permission / input)
    Done,     // finished cleanly, recently
    Stale,    // finished a while ago, safe to ignore
    Error,    // last turn ended in an error
}

impl State {
    /// Base One Dark-ish color for the state's rule + glyph.
    pub fn color(self) -> Color {
        match self {
            State::Working => Color::Rgb(0x56, 0xB6, 0xC2),  // calm cyan-blue
            State::NeedsYou => Color::Rgb(0xE5, 0xC0, 0x7B), // warm amber
            State::Done => Color::Rgb(0x98, 0xC3, 0x79),     // soft green
            State::Stale => Color::Rgb(0x5C, 0x63, 0x70),    // dim grey
            State::Error => Color::Rgb(0xE0, 0x6C, 0x75),    // soft red
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            State::Working => "WORKING",
            State::NeedsYou => "NEEDS YOU",
            State::Done => "DONE",
            State::Stale => "STALE",
            State::Error => "ERROR",
        }
    }

    /// Trailing status glyph (the animated spinner is handled separately for Working).
    pub fn glyph(self) -> &'static str {
        match self {
            State::Working => "\u{25D0}", // ◐ (placeholder; spinner used in the rail)
            State::NeedsYou => "\u{25C6}", // ◆
            State::Done => "\u{2713}",    // ✓
            State::Stale => "\u{25CC}",   // ◌
            State::Error => "\u{2715}",   // ✕
        }
    }

    /// Whether this state animates (only the two that need your attention move).
    pub fn animates(self) -> bool {
        matches!(self, State::Working | State::NeedsYou)
    }
}

/// Where the session lives. Desktop is reserved for a future adapter.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Source {
    Terminal,
    #[allow(dead_code)]
    Desktop,
}

impl Source {
    pub fn glyph(self) -> &'static str {
        match self {
            Source::Terminal => "\u{276F}", // ❯
            Source::Desktop => "\u{2726}",  // ✦
        }
    }
}

#[derive(Debug, Clone)]
pub struct Session {
    pub id: String,
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
    /// ms since the session was last updated.
    pub age_ms: i64,
    /// One-line summary of what it's actually doing (stub until AI is wired).
    pub summary: String,
    /// Distinct integrations touched, e.g. ["Notion", "Slack"].
    pub integrations: Vec<String>,
    /// Path to the transcript JSONL, if found.
    pub transcript: Option<std::path::PathBuf>,
}
