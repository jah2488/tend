use crate::model::{Session, State};
use ratatui::{
    layout::{Constraint, Layout},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, List, ListItem, ListState, Padding, Paragraph},
    Frame,
};

const BAR_W: usize = 20;
const CONTEXT_LIMIT: f32 = 200_000.0;
const RULE: &str = "\u{258C}"; // ▌
const SPINNER: [&str; 8] = ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧"];

/// Scale an RGB color's brightness by `f` (0.0–1.0). Non-RGB colors pass through.
fn scaled(c: Color, f: f32) -> Color {
    match c {
        Color::Rgb(r, g, b) => Color::Rgb(
            (r as f32 * f) as u8,
            (g as f32 * f) as u8,
            (b as f32 * f) as u8,
        ),
        other => other,
    }
}

/// Per-state brightness factor for the rule, animated by `tick`.
fn rule_brightness(state: State, tick: u64) -> f32 {
    let t = tick as f32;
    match state {
        State::Working => 0.60 + 0.40 * (0.5 + 0.5 * (t * 0.40).sin()),
        State::NeedsYou => 0.50 + 0.50 * (0.5 + 0.5 * (t * 0.16).sin()),
        State::Stale => 0.55,
        _ => 1.0,
    }
}

fn fmt_tokens(n: u64) -> String {
    if n >= 1_000_000 {
        format!("{:.1}M", n as f64 / 1_000_000.0)
    } else if n >= 1_000 {
        format!("{}K", n / 1_000)
    } else {
        n.to_string()
    }
}

fn fmt_age(ms: i64) -> String {
    let s = ms / 1000;
    if s < 60 {
        format!("{}s", s)
    } else if s < 3600 {
        format!("{}m", s / 60)
    } else if s < 86_400 {
        format!("{}h", s / 3600)
    } else {
        format!("{}d", s / 86_400)
    }
}

fn short_cwd(cwd: &str) -> String {
    match std::env::var("HOME") {
        Ok(home) if cwd.starts_with(&home) => cwd.replacen(&home, "~", 1),
        _ => cwd.to_string(),
    }
}

/// Word-wrap into at most two lines of `width`, ellipsizing any overflow.
fn wrap2(s: &str, width: usize) -> Vec<String> {
    let mut lines: Vec<String> = Vec::new();
    let mut cur = String::new();
    for word in s.split_whitespace() {
        if cur.is_empty() {
            cur = word.to_string();
        } else if cur.chars().count() + 1 + word.chars().count() <= width {
            cur.push(' ');
            cur.push_str(word);
        } else {
            lines.push(std::mem::take(&mut cur));
            cur = word.to_string();
            if lines.len() == 2 {
                break;
            }
        }
    }
    if lines.len() < 2 && !cur.is_empty() {
        lines.push(cur);
    }
    if lines.len() == 2 && lines[1].chars().count() > width {
        let cut: String = lines[1].chars().take(width.saturating_sub(1)).collect();
        lines[1] = format!("{}\u{2026}", cut.trim_end());
    }
    lines
}

fn rule_span(state: State, tick: u64) -> Span<'static> {
    let c = scaled(state.color(), rule_brightness(state, tick));
    Span::styled(format!("{RULE} "), Style::default().fg(c))
}

/// Build the multi-line card for one session.
fn session_item(s: &Session, tick: u64, width: usize) -> ListItem<'static> {
    let color = s.state.color();
    let inner = width.saturating_sub(2); // account for the rule + space
    let mut lines: Vec<Line> = Vec::new();

    // ── header: spinner/glyph + name ........... STATUS
    let lead = if s.state == State::Working {
        SPINNER[(tick as usize) % SPINNER.len()]
    } else {
        s.state.glyph()
    };
    let status = format!("{} {}", s.state.glyph(), s.state.label());
    let left = format!("{}  {}", lead, s.name);
    let pad = inner
        .saturating_sub(left.chars().count())
        .saturating_sub(status.chars().count());
    lines.push(Line::from(vec![
        rule_span(s.state, tick),
        Span::styled(lead.to_string(), Style::default().fg(color)),
        Span::raw(format!("  {}", s.name)),
        Span::raw(" ".repeat(pad.max(1))),
        Span::styled(status, Style::default().fg(color).add_modifier(Modifier::BOLD)),
    ]));

    // ── meta: ❯ cwd · tokens · age
    let meta = format!(
        "{} {} · {} tok · {}",
        s.source.glyph(),
        short_cwd(&s.cwd),
        fmt_tokens(s.total_tokens),
        fmt_age(s.age_ms),
    );
    lines.push(Line::from(vec![
        rule_span(s.state, tick),
        Span::styled(meta, Style::default().fg(Color::Rgb(0x82, 0x88, 0x96))),
    ]));

    // ── breathing room
    lines.push(Line::from(rule_span(s.state, tick)));

    // ── summary (≤ 2 lines)
    for sl in wrap2(&s.summary, inner) {
        lines.push(Line::from(vec![
            rule_span(s.state, tick),
            Span::raw(sl),
        ]));
    }

    // ── context bar + integrations
    let filled = ((s.context_tokens as f32 / CONTEXT_LIMIT) * BAR_W as f32)
        .round()
        .clamp(0.0, BAR_W as f32) as usize;
    let mut bar_spans = vec![
        rule_span(s.state, tick),
        Span::raw("   "),
        Span::styled("\u{2501}".repeat(filled), Style::default().fg(color)),
        Span::styled(
            "\u{2508}".repeat(BAR_W - filled),
            Style::default().fg(Color::Rgb(0x3a, 0x40, 0x4b)),
        ),
    ];
    if !s.integrations.is_empty() {
        bar_spans.push(Span::styled(
            format!("     {}", s.integrations.join(" · ")),
            Style::default().fg(Color::Rgb(0x82, 0x88, 0x96)),
        ));
    }
    lines.push(Line::from(bar_spans));

    // ── faint divider that closes the card
    lines.push(Line::from(vec![
        rule_span(s.state, tick),
        Span::styled(
            "\u{2504}".repeat(inner.min(50)),
            Style::default().fg(Color::Rgb(0x32, 0x37, 0x40)),
        ),
    ]));
    lines.push(Line::from("")); // gutter between cards

    ListItem::new(lines)
}

fn header_line(sessions: &[Session]) -> Line<'static> {
    let live = sessions
        .iter()
        .filter(|s| matches!(s.state, State::Working | State::NeedsYou))
        .count();
    let need = sessions.iter().filter(|s| s.state == State::NeedsYou).count();
    let idle = sessions
        .iter()
        .filter(|s| matches!(s.state, State::Stale | State::Done))
        .count();

    Line::from(vec![
        Span::styled(
            "tend",
            Style::default()
                .fg(Color::Rgb(0xAB, 0xB2, 0xBF))
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled(
            format!("   {live} live · {need} need you · {idle} idle"),
            Style::default().fg(Color::Rgb(0x82, 0x88, 0x96)),
        ),
    ])
}

pub fn render(frame: &mut Frame, sessions: &[Session], selected: usize, tick: u64) {
    let chunks = Layout::vertical([
        Constraint::Length(2), // header
        Constraint::Min(1),    // list
        Constraint::Length(1), // footer
    ])
    .split(frame.area());

    frame.render_widget(
        Paragraph::new(header_line(sessions)).block(Block::default().padding(Padding::new(2, 0, 1, 0))),
        chunks[0],
    );

    let width = chunks[1].width.saturating_sub(3) as usize;
    if sessions.is_empty() {
        frame.render_widget(
            Paragraph::new("  No Claude Code sessions found. Start one with `claude` in a terminal.")
                .style(Style::default().fg(Color::Rgb(0x5C, 0x63, 0x70))),
            chunks[1],
        );
    } else {
        let items: Vec<ListItem> = sessions
            .iter()
            .map(|s| session_item(s, tick, width))
            .collect();
        let list = List::new(items)
            .block(Block::default().padding(Padding::new(2, 0, 0, 0)))
            .highlight_style(Style::default().bg(Color::Rgb(0x2C, 0x31, 0x3C)));
        let mut state = ListState::default();
        state.select(Some(selected.min(sessions.len().saturating_sub(1))));
        frame.render_stateful_widget(list, chunks[1], &mut state);
    }

    frame.render_widget(
        Paragraph::new("  [↑↓] navigate   [s] re-summarize   [r] refresh   [q] quit")
            .style(Style::default().fg(Color::Rgb(0x5C, 0x63, 0x70))),
        chunks[2],
    );
}
