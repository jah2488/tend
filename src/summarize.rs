use crate::model::Session;
use crate::transcript::Analysis;

/// Produces the one-line "what is this session actually doing now" summary.
///
/// The whole point of v1 is that this is swappable: the stub below is pure local
/// heuristics, and an AI-backed implementation (Haiku via direct API or a Netlify
/// Function proxy) will drop in here without touching the rest of the app.
pub trait Summarizer {
    fn summarize(&self, session: &Session, analysis: &Analysis) -> String;
}

/// Condense arbitrary message text into a single tidy line.
fn one_line(s: &str, max: usize) -> String {
    let collapsed = s
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        // shed common leading markdown noise
        .trim_start_matches(['#', '-', '*', '>', ' '])
        .to_string();
    if collapsed.chars().count() <= max {
        collapsed
    } else {
        let cut: String = collapsed.chars().take(max.saturating_sub(1)).collect();
        format!("{}\u{2026}", cut.trim_end())
    }
}

/// Local, no-network placeholder: leans on the most recent assistant text as the
/// best available proxy for "current state", falling back to the opening ask.
pub struct StubSummarizer;

impl Summarizer for StubSummarizer {
    fn summarize(&self, session: &Session, analysis: &Analysis) -> String {
        if let Some(t) = &analysis.last_assistant_text {
            return one_line(t, 80);
        }
        if let Some(t) = &analysis.first_user_text {
            return one_line(t, 80);
        }
        format!("({})", session.name)
    }
}
