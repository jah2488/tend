use crate::actions::Action;
use crate::model::{Session, Source, State};
use crate::transcript::{base_name, Digest, EventKind};
use ratatui::{
    layout::{Constraint, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Clear, List, ListItem, ListState, Padding, Paragraph},
    Frame,
};

/// Modal action-picker state: which applicable actions to offer for the selected
/// session, and the cursor within them. `items` holds indices into the actions slice.
pub struct Menu {
    pub items: Vec<usize>,
    pub selected: usize,
}

impl Menu {
    pub fn next(&mut self) {
        if !self.items.is_empty() {
            self.selected = (self.selected + 1) % self.items.len();
        }
    }

    pub fn prev(&mut self) {
        if !self.items.is_empty() {
            self.selected = (self.selected + self.items.len() - 1) % self.items.len();
        }
    }
}

/// The session-digest overlay: a frozen, scrollable read of one session's transcript.
/// Pre-rendered at open time (lines are owned) so it's immune to the list reordering
/// underneath it on the next 2s refresh, and cheap to redraw each frame.
pub struct Detail {
    pub title: String,
    pub lines: Vec<Line<'static>>,
    pub scroll: u16,
}

impl Detail {
    fn max_scroll(&self) -> u16 {
        self.lines.len().saturating_sub(1) as u16
    }
    pub fn scroll_down(&mut self) {
        self.scroll = (self.scroll + 1).min(self.max_scroll());
    }
    pub fn scroll_up(&mut self) {
        self.scroll = self.scroll.saturating_sub(1);
    }
}

const BAR_W: usize = 20;
const CONTEXT_LIMIT: f32 = 200_000.0;
const RULE: &str = "\u{258C}"; // ▌
const SPINNER: [&str; 8] = ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧"];
const DIM: Color = Color::Rgb(0x82, 0x88, 0x96); // secondary text
const PIP: &str = "\u{25CF}"; // ● — user-chosen tint, or two-space placeholder
/// Visual width of the pip column (`● ` or `  `). Reserved on every row so
/// names stay aligned whether or not a tint is set.
const PIP_W: usize = 2;

/// Render the leading pip column for a session: `● ` in the tint color when
/// set, or two blank spaces (preserving alignment) when not. The pip is a
/// user-chosen flag distinct from the state-driven left rule, so it gets its
/// own dedicated span.
fn pip_span(s: &Session) -> Span<'static> {
    match s.tint {
        Some(t) => Span::styled(format!("{PIP} "), Style::default().fg(t.color())),
        None => Span::raw("  "),
    }
}

/// Trim the "claude-" prefix so models read as "opus-4-7" / "sonnet-4-6".
fn short_model(m: &str) -> String {
    m.strip_prefix("claude-").unwrap_or(m).to_string()
}

// ── Per-column field strings for the compact rows (empty == column absent). ──
fn f_cwd(s: &Session) -> String {
    short_cwd(&s.cwd)
}
fn f_branch(s: &Session) -> String {
    s.git_branch.as_ref().map_or(String::new(), |b| format!("\u{2387} {}", b))
}
fn f_worktree(s: &Session) -> String {
    // ⑂ (a fork glyph, width-1) marks a linked worktree so sibling sessions in the same
    // repo stay distinct — same monochrome style as the ⎇ branch marker.
    s.worktree.as_ref().map_or(String::new(), |w| format!("\u{2442} {}", w))
}
fn f_model(s: &Session) -> String {
    s.model.as_deref().map(short_model).unwrap_or_default()
}
fn f_tokens(s: &Session) -> String {
    format!("{} tok", fmt_tokens(s.total_tokens))
}
fn f_age(s: &Session) -> String {
    fmt_age(s.age_ms)
}
fn f_cpu(s: &Session) -> String {
    match s.cpu_pct {
        Some(c) if c >= 0.1 => format!("{:.0}% cpu", c),
        _ => String::new(),
    }
}

/// Column widths for the compact mini-list, sized to the widest cell in each column
/// so every field lines up vertically across rows.
#[derive(Default)]
struct CompactCols {
    name: usize,
    badge: usize,
    cwd: usize,
    branch: usize,
    worktree: usize,
    model: usize,
    tokens: usize,
    age: usize,
    cpu: usize,
}

impl CompactCols {
    fn measure<'a>(rows: impl Iterator<Item = &'a Session> + Clone) -> Self {
        let w = |f: &dyn Fn(&Session) -> usize| rows.clone().map(f).max().unwrap_or(0);
        CompactCols {
            name: w(&|s| s.name.chars().count()),
            badge: w(&|s| badge_text(s).map_or(0, |b| b.chars().count() + 2)),
            cwd: w(&|s| f_cwd(s).chars().count()),
            branch: w(&|s| f_branch(s).chars().count()),
            worktree: w(&|s| f_worktree(s).chars().count()),
            model: w(&|s| f_model(s).chars().count()),
            tokens: w(&|s| f_tokens(s).chars().count()),
            age: w(&|s| f_age(s).chars().count()),
            cpu: w(&|s| f_cpu(s).chars().count()),
        }
    }
}

/// Append one aligned column to the meta string. Skips zero-width columns entirely;
/// blank cells keep their width (and a blank separator) so following columns stay aligned.
/// Numeric columns are right-aligned (left-padded); text columns are left-aligned.
fn push_col(meta: &mut String, field: &str, col: usize, right: bool) {
    if col == 0 {
        return;
    }
    meta.push_str(if field.is_empty() { "   " } else { " \u{00B7} " }); // " · " or blank
    if right {
        meta.push_str(&format!("{:>w$}", field, w = col));
    } else {
        meta.push_str(&format!("{:<w$}", field, w = col));
    }
}

/// Top-line stats for a full card, shown dim right after the title:
/// tokens · model · active · cpu (each included only when known).
fn card_stats(s: &Session) -> String {
    let mut parts = vec![f_tokens(s)];
    if let Some(m) = &s.model {
        parts.push(short_model(m));
    }
    if let Some(a) = s.active_span_ms {
        parts.push(format!("active {}", fmt_age(a)));
    }
    let cpu = f_cpu(s);
    if !cpu.is_empty() {
        parts.push(cpu);
    }
    parts.join(" \u{00B7} ")
}

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
        State::Idle => 0.65,
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

/// A live, ticking idle duration: `MM:SS`, `H:MM:SS`, or `Dd H:MM:SS`.
fn fmt_idle_clock(ms: i64) -> String {
    let total = (ms / 1000).max(0);
    let (s, m, h, d) = (total % 60, (total / 60) % 60, (total / 3600) % 24, total / 86_400);
    if d > 0 {
        format!("{}d {}:{:02}:{:02}", d, h, m, s)
    } else if h > 0 {
        format!("{}:{:02}:{:02}", h, m, s)
    } else {
        format!("{:02}:{:02}", m, s)
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
    // Truncate any line that overflowed `width`: the wrapped second line, or a single
    // long token that never triggered a break (a URL or path wrap2 leaves verbatim).
    for line in lines.iter_mut() {
        if line.chars().count() > width {
            let cut: String = line.chars().take(width.saturating_sub(1)).collect();
            *line = format!("{}\u{2026}", cut.trim_end());
        }
    }
    lines
}

/// The inner text of a session's badge, e.g. "sdk" or "Zed · sdk". None for plain terminals.
fn badge_text(s: &Session) -> Option<String> {
    let tag = s.source.badge()?;
    Some(match &s.origin {
        Some(o) => format!("{} · {}", o, tag),
        None => tag.to_string(),
    })
}

/// Status text shown on the right of each row. Idle sessions get a live ticking
/// clock so you can see how long they've been sitting there.
fn idle_status(s: &Session, now_offset_ms: i64) -> String {
    if s.state == State::Idle {
        format!(
            "{} {} {}",
            s.state.glyph(),
            s.state.label(),
            fmt_idle_clock(s.age_ms + now_offset_ms),
        )
    } else if s.state == State::NeedsYou {
        // Surface what it's blocked on, e.g. "◆ NEEDS YOU · permission prompt".
        match &s.waiting_for {
            Some(w) => format!("{} {} \u{00B7} {}", s.state.glyph(), s.state.label(), w),
            None => format!("{} {}", s.state.glyph(), s.state.label()),
        }
    } else {
        format!("{} {}", s.state.glyph(), s.state.label())
    }
}

fn rule_span(state: State, tick: u64) -> Span<'static> {
    let c = scaled(state.color(), rule_brightness(state, tick));
    Span::styled(format!("{RULE} "), Style::default().fg(c))
}

/// The fixed-width column meta for a compact row. Same width for every row given the
/// same `cols`, which is what makes the cwd/tokens/age/cpu columns line up vertically.
fn compact_meta(s: &Session, cols: &CompactCols) -> String {
    let mut meta = format!("  \u{00B7}  {:<w$}", f_cwd(s), w = cols.cwd);
    push_col(&mut meta, &f_branch(s), cols.branch, false);
    push_col(&mut meta, &f_worktree(s), cols.worktree, false);
    push_col(&mut meta, &f_model(s), cols.model, false);
    push_col(&mut meta, &f_tokens(s), cols.tokens, true);
    push_col(&mut meta, &f_age(s), cols.age, true);
    push_col(&mut meta, &f_cpu(s), cols.cpu, true);
    meta
}

/// One-line row for non-interactive sessions (SDK / editor-launched), to keep the
/// background processes from crowding out the terminal sessions you care about.
/// `cols.name`/`cols.badge` are the widest name/badge across all compact rows, so the
/// badge column and everything after it line up vertically. Every row is padded to
/// the exact same total width, with the status hard-right-aligned.
/// Layout:  ▌ ◇ name….. [Zed · sdk]  ·  ~/path · model · 12K tok · 2d      ○ IDLE …
fn compact_item(s: &Session, tick: u64, width: usize, now_offset_ms: i64, cols: &CompactCols) -> ListItem<'static> {
    let color = s.state.color();
    let inner = width.saturating_sub(2); // account for the rule + space
    let status = idle_status(s, now_offset_ms);

    let meta = compact_meta(s, cols);

    let glyph = s.source.glyph();
    let name_field = format!("{:<w$}", s.name, w = cols.name);
    let badge_disp = badge_text(s).map_or(String::new(), |b| format!("[{}]", b));

    let mut spans = vec![
        rule_span(s.state, tick),
        Span::styled(glyph.to_string(), Style::default().fg(color)),
        Span::raw("  "),
        pip_span(s),
        Span::raw(name_field.clone()),
    ];
    let mut w = glyph.chars().count() + 2 + PIP_W + name_field.chars().count();
    if cols.badge > 0 {
        let badge_field = format!(" {:<w$}", badge_disp, w = cols.badge);
        w += badge_field.chars().count();
        spans.push(Span::styled(badge_field, Style::default().fg(DIM)));
    }
    w += meta.chars().count();
    spans.push(Span::styled(meta, Style::default().fg(DIM)));

    let pad = inner.saturating_sub(w).saturating_sub(status.chars().count());
    spans.push(Span::raw(" ".repeat(pad.max(1))));
    spans.push(Span::styled(
        status,
        Style::default().fg(color).add_modifier(Modifier::BOLD),
    ));

    ListItem::new(vec![Line::from(spans)])
}

/// Build the multi-line card for one session.
fn session_item(s: &Session, tick: u64, width: usize, now_offset_ms: i64) -> ListItem<'static> {
    let color = s.state.color();
    let inner = width.saturating_sub(2); // account for the rule + space
    let mut lines: Vec<Line> = Vec::new();

    // ── header: spinner/glyph + name + stats ........... STATUS
    let lead = if s.state == State::Working {
        SPINNER[(tick as usize) % SPINNER.len()]
    } else {
        s.state.glyph()
    };
    let status = idle_status(s, now_offset_ms);
    let stats = card_stats(s); // tokens · model · active · cpu
    let left_w = lead.chars().count()
        + 2
        + PIP_W
        + s.name.chars().count()
        + 3
        + stats.chars().count();
    let pad = inner
        .saturating_sub(left_w)
        .saturating_sub(status.chars().count());
    lines.push(Line::from(vec![
        rule_span(s.state, tick),
        Span::styled(lead.to_string(), Style::default().fg(color)),
        Span::raw("  "),
        pip_span(s),
        Span::raw(s.name.clone()),
        Span::styled(format!("   {}", stats), Style::default().fg(DIM)),
        Span::raw(" ".repeat(pad.max(1))),
        Span::styled(
            status,
            Style::default().fg(color).add_modifier(Modifier::BOLD),
        ),
    ]));

    // ── line 2: the path
    lines.push(Line::from(vec![
        rule_span(s.state, tick),
        Span::styled(
            format!("{} {}", s.source.glyph(), short_cwd(&s.cwd)),
            Style::default().fg(DIM),
        ),
    ]));

    // ── line 3: branch + worktree (each shown only when present), same line so a
    // worktree reads as prominently as the branch and sibling sessions stay distinct.
    let branch = f_branch(s);
    let worktree = f_worktree(s);
    if !branch.is_empty() || !worktree.is_empty() {
        let mut spans = vec![rule_span(s.state, tick)];
        if !branch.is_empty() {
            spans.push(Span::styled(branch, Style::default().fg(DIM)));
        }
        if !worktree.is_empty() {
            let sep = if spans.len() > 1 { "    " } else { "" };
            spans.push(Span::styled(
                format!("{}{}", sep, worktree),
                Style::default().fg(DIM),
            ));
        }
        lines.push(Line::from(spans));
    }

    // ── PR opened during the session
    if let Some(url) = &s.pr_url {
        let pr = match s.pr_number {
            Some(n) => format!("\u{21E1} PR #{} · {}", n, url),
            None => format!("\u{21E1} {}", url),
        };
        lines.push(Line::from(vec![
            rule_span(s.state, tick),
            Span::styled(pr, Style::default().fg(DIM)),
        ]));
    }

    // ── breathing room
    lines.push(Line::from(rule_span(s.state, tick)));

    // ── summary (≤ 2 lines)
    for sl in wrap2(&s.summary, inner) {
        lines.push(Line::from(vec![
            rule_span(s.state, tick),
            Span::raw(sl),
        ]));
    }

    // ── user note (set via `tend mcp` / skill), shown distinct from the auto summary
    if let Some(note) = &s.note {
        lines.push(Line::from(vec![
            rule_span(s.state, tick),
            Span::styled(
                format!("\u{2756} {}", trunc(note, inner)),
                Style::default().fg(Color::Rgb(0xE5, 0xC0, 0x7B)),
            ),
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
    let mut tail: Vec<String> = Vec::new();
    if s.tool_calls > 0 {
        let mut tally = format!("{} tools", s.tool_calls);
        if s.web_requests > 0 {
            tally.push_str(&format!(" \u{00B7} {} web", s.web_requests));
        }
        tail.push(tally);
    }
    if !s.integrations.is_empty() {
        tail.push(s.integrations.join(" \u{00B7} "));
    }
    if !tail.is_empty() {
        bar_spans.push(Span::styled(
            format!("     {}", tail.join("   \u{00B7}   ")),
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
        .filter(|s| matches!(s.state, State::Idle | State::Stale | State::Done))
        .count();

    Line::from(vec![
        Span::styled(
            "tend",
            Style::default()
                .fg(Color::Rgb(0xAB, 0xB2, 0xBF))
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled(
            format!(" v{}", env!("CARGO_PKG_VERSION")),
            Style::default().fg(Color::Rgb(0x5C, 0x63, 0x70)),
        ),
        Span::styled(
            format!("   {live} live · {need} need you · {idle} idle"),
            Style::default().fg(Color::Rgb(0x82, 0x88, 0x96)),
        ),
    ])
}

#[allow(clippy::too_many_arguments)]
pub fn render(
    frame: &mut Frame,
    sessions: &[Session],
    selected: usize,
    tick: u64,
    now_offset_ms: i64,
    actions: &[Action],
    menu: Option<&Menu>,
    detail: Option<&Detail>,
    status: Option<&str>,
) {
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
        // Size the compact (non-terminal) columns to their widest cells so every field —
        // name, badge, cwd, branch, model, tokens, age, cpu — lines up vertically.
        let cols = CompactCols::measure(sessions.iter().filter(|s| s.source != Source::Terminal));

        let items: Vec<ListItem> = sessions
            .iter()
            .map(|s| {
                if s.source != Source::Terminal {
                    compact_item(s, tick, width, now_offset_ms, &cols)
                } else {
                    session_item(s, tick, width, now_offset_ms)
                }
            })
            .collect();
        let list = List::new(items)
            .block(Block::default().padding(Padding::new(2, 0, 0, 0)))
            .highlight_style(Style::default().bg(Color::Rgb(0x2C, 0x31, 0x3C)));
        let mut state = ListState::default();
        state.select(Some(selected.min(sessions.len().saturating_sub(1))));
        frame.render_stateful_widget(list, chunks[1], &mut state);
    }

    frame.render_widget(footer(actions, menu.is_some(), detail.is_some(), status), chunks[2]);

    // The action picker floats over the list once opened.
    if let Some(menu) = menu {
        let title = sessions
            .get(selected)
            .map(|s| format!(" actions · {} ", s.name))
            .unwrap_or_else(|| " actions ".to_string());
        render_menu(frame, chunks[1], actions, menu, &title);
    }

    // The digest panel floats over everything when open.
    if let Some(detail) = detail {
        render_detail(frame, chunks[1], detail);
    }
}

/// The footer help/status line. A transient `status` (e.g. an action result) takes
/// precedence; otherwise the help text adapts to whether the menu is open.
fn footer(actions: &[Action], menu_open: bool, detail_open: bool, status: Option<&str>) -> Paragraph<'static> {
    let dim = Style::default().fg(Color::Rgb(0x5C, 0x63, 0x70));
    if let Some(msg) = status {
        return Paragraph::new(format!("  {msg}")).style(dim);
    }
    let text = if detail_open {
        "  [↑↓] scroll   [tab/esc] close".to_string()
    } else if menu_open {
        "  [↑↓] choose   [enter] run   [esc] cancel".to_string()
    } else if actions.is_empty() {
        "  [↑↓] navigate   [tab] details   [r] refresh   [q] quit".to_string()
    } else {
        "  [↑↓] navigate   [tab] details   [enter] actions   [r] refresh   [q] quit".to_string()
    };
    Paragraph::new(text).style(dim)
}

/// A centered rectangle of the given size, clamped to `area`.
fn centered_rect(w: u16, h: u16, area: Rect) -> Rect {
    let w = w.min(area.width);
    let h = h.min(area.height);
    Rect {
        x: area.x + (area.width.saturating_sub(w)) / 2,
        y: area.y + (area.height.saturating_sub(h)) / 2,
        width: w,
        height: h,
    }
}

/// Draw the action picker as a bordered popup over the list.
fn render_menu(frame: &mut Frame, area: Rect, actions: &[Action], menu: &Menu, title: &str) {
    let sel = Color::Rgb(0xE5, 0xC0, 0x7B); // amber, matches NeedsYou accent
    let rows: Vec<(String, bool)> = menu
        .items
        .iter()
        .enumerate()
        .map(|(i, &ai)| {
            let a = &actions[ai];
            let key = a.key.map_or_else(|| "    ".to_string(), |c| format!("[{c}] "));
            (format!(" {key}{} ", a.label), i == menu.selected)
        })
        .collect();

    let inner_w = rows.iter().map(|(t, _)| t.chars().count()).max().unwrap_or(0);
    let w = (inner_w as u16 + 2).max(title.chars().count() as u16 + 2);
    let h = rows.len() as u16 + 2; // borders
    let rect = centered_rect(w, h, area);

    let items: Vec<ListItem> = rows
        .into_iter()
        .map(|(text, selected)| {
            let style = if selected {
                Style::default().fg(sel).add_modifier(Modifier::BOLD)
            } else {
                Style::default().fg(Color::Rgb(0xAB, 0xB2, 0xBF))
            };
            ListItem::new(Line::from(Span::styled(text, style)))
        })
        .collect();

    frame.render_widget(Clear, rect);
    frame.render_widget(
        List::new(items).block(
            Block::bordered()
                .title(title.to_string())
                .border_style(Style::default().fg(DIM)),
        ),
        rect,
    );
}

// ── Session digest ──

const TEXT: Color = Color::Rgb(0xAB, 0xB2, 0xBF); // primary readable text
const LBL_W: usize = 10; // section-label column (widest label "RESOURCES" + a gap)
const HIST_W: usize = 22; // max tool-histogram bar width
const BAR: Color = Color::Rgb(0x56, 0xB6, 0xC2); // tool bars — calm cyan

fn trunc(s: &str, n: usize) -> String {
    let s = s.split_whitespace().collect::<Vec<_>>().join(" ");
    if s.chars().count() <= n {
        s
    } else {
        format!("{}\u{2026}", s.chars().take(n.saturating_sub(1)).collect::<String>())
    }
}

/// A `LABEL  body…` line: dim fixed-width label, then content spans.
fn kv(label: &str, body: Vec<Span<'static>>) -> Line<'static> {
    let mut spans = vec![
        Span::raw("  "),
        Span::styled(format!("{:<w$}", label, w = LBL_W), Style::default().fg(DIM)),
    ];
    spans.extend(body);
    Line::from(spans)
}

fn plain(label: &str, body: String) -> Line<'static> {
    kv(label, vec![Span::styled(body, Style::default().fg(TEXT))])
}

/// Build the pre-rendered digest overlay for one session.
pub fn build_detail(s: &Session, d: &Digest) -> Detail {
    let mut lines: Vec<Line<'static>> = Vec::new();

    // Header: state · model · branch · worktree · cwd.
    let mut head = vec![
        Span::raw("  "),
        Span::styled(
            format!("{} {}", s.state.glyph(), s.state.label()),
            Style::default().fg(s.state.color()).add_modifier(Modifier::BOLD),
        ),
    ];
    if let Some(m) = &s.model {
        head.push(Span::styled(format!("   {}", short_model(m)), Style::default().fg(TEXT)));
    }
    let bw = f_branch(s);
    if !bw.is_empty() {
        head.push(Span::styled(format!("   {}", bw), Style::default().fg(DIM)));
    }
    let wt = f_worktree(s);
    if !wt.is_empty() {
        head.push(Span::styled(format!("  {}", wt), Style::default().fg(DIM)));
    }
    head.push(Span::styled(format!("   {}", short_cwd(&s.cwd)), Style::default().fg(DIM)));
    lines.push(Line::from(head));
    lines.push(Line::from(""));

    // COST.
    let span = d.active_span_ms.map(fmt_idle_clock).unwrap_or_else(|| "—".into());
    let pct = (d.context_tokens as f32 / CONTEXT_LIMIT * 100.0).clamp(0.0, 100.0);
    lines.push(plain(
        "COST",
        format!(
            "{} tok   ctx {:.0}%   span {}",
            fmt_tokens(d.total_tokens),
            pct,
            span
        ),
    ));
    lines.push(Line::from(""));

    // ASKED / LATEST — framing prompts, kept near the top before the long feed.
    if let Some(p) = &d.first_prompt {
        lines.push(plain("ASKED", format!("\u{201C}{}\u{201D}", trunc(p, 72))));
    }
    if let Some(p) = &d.last_prompt {
        if Some(p) != d.first_prompt.as_ref() {
            lines.push(plain("LATEST", format!("\u{201C}{}\u{201D}", trunc(p, 72))));
        }
    }
    lines.push(Line::from(""));

    // TOOLS histogram, highest-first, top 14.
    if d.tool_counts.is_empty() {
        lines.push(plain("TOOLS", "—".into()));
    } else {
        let max = d.tool_counts.iter().map(|(_, c)| *c).max().unwrap_or(1).max(1);
        for (i, (name, count)) in d.tool_counts.iter().take(14).enumerate() {
            let filled = ((*count as usize * HIST_W) / max as usize).max(1);
            let bar = format!("{}{}", "\u{2588}".repeat(filled), " ".repeat(HIST_W - filled));
            lines.push(kv(
                if i == 0 { "TOOLS" } else { "" },
                vec![
                    Span::styled(format!("{:<14}", trunc(name, 14)), Style::default().fg(TEXT)),
                    Span::styled(bar, Style::default().fg(BAR)),
                    Span::styled(format!(" {}", count), Style::default().fg(DIM)),
                ],
            ));
        }
        if d.tool_counts.len() > 14 {
            lines.push(kv("", vec![Span::styled(format!("+{} more", d.tool_counts.len() - 14), Style::default().fg(DIM))]));
        }
    }
    lines.push(Line::from(""));

    // RESOURCES: MCP integrations with counts, plus web requests.
    let mut res: Vec<String> = d.integration_counts.iter().map(|(n, c)| format!("{n} \u{00D7}{c}")).collect();
    if d.web_requests > 0 {
        res.push(format!("web \u{00D7}{}", d.web_requests));
    }
    lines.push(plain("RESOURCES", if res.is_empty() { "—".into() } else { res.join("  \u{00B7}  ") }));
    lines.push(Line::from(""));

    // FILES: changed (listed) and read (count + sample).
    if d.files_edited.is_empty() {
        lines.push(plain("FILES", "no files changed".into()));
    } else {
        let shown = d.files_edited.iter().take(12).map(|f| base_name(f)).collect::<Vec<_>>().join(" \u{00B7} ");
        let extra = d.files_edited.len().saturating_sub(12);
        let body = if extra > 0 { format!("{shown}  +{extra}") } else { shown };
        lines.push(kv("FILES", vec![
            Span::styled("changed  ", Style::default().fg(DIM)),
            Span::styled(body, Style::default().fg(TEXT)),
        ]));
    }
    if !d.files_read.is_empty() {
        lines.push(kv("", vec![
            Span::styled("read     ", Style::default().fg(DIM)),
            Span::styled(format!("{} files", d.files_read.len()), Style::default().fg(TEXT)),
        ]));
    }
    lines.push(Line::from(""));

    // OUTCOMES: PR opened, error count.
    let pr = match (&d.pr_url, d.pr_number) {
        (Some(_), Some(n)) => format!("PR #{n} opened"),
        (Some(u), None) => format!("PR opened ({u})"),
        _ => "no PR opened".into(),
    };
    let err = if d.error_count == 0 { "errors 0".to_string() } else { format!("errors {}", d.error_count) };
    let err_color = if d.error_count == 0 { DIM } else { Color::Rgb(0xE0, 0x6C, 0x75) };
    lines.push(kv("OUTCOMES", vec![
        Span::styled(format!("{:<22}", pr), Style::default().fg(TEXT)),
        Span::styled(err, Style::default().fg(err_color)),
    ]));
    lines.push(Line::from(""));

    // TIMELINE: full chronological feed — prompts, every tool call, PRs, errors.
    if !d.timeline.is_empty() {
        let dropped = d.timeline_dropped;
        if dropped > 0 {
            lines.push(kv("TIMELINE", vec![Span::styled(format!("+{dropped} earlier events elided"), Style::default().fg(DIM))]));
        }
        for (i, ev) in d.timeline.iter().enumerate() {
            let label = if i == 0 && dropped == 0 { "TIMELINE" } else { "" };
            // Glyph + color per kind; prompts/PRs/errors stand out, tool calls read by family.
            let (mark, color) = match ev.kind {
                EventKind::Prompt => ("\u{276F}", Color::Rgb(0x61, 0xAF, 0xEF)), // ❯ blue
                EventKind::Pr => ("\u{2387}", Color::Rgb(0x98, 0xC3, 0x79)),     // ⎇ green
                EventKind::Error => ("\u{2715}", Color::Rgb(0xE0, 0x6C, 0x75)),  // ✕ red
                EventKind::Read => ("\u{25B8}", Color::Rgb(0x82, 0x88, 0x96)),   // ▸ grey
                EventKind::Edit => ("\u{270E}", Color::Rgb(0xE5, 0xC0, 0x7B)),   // ✎ amber
                EventKind::Bash => ("\u{0024}", Color::Rgb(0x98, 0xC3, 0x79)),   // $ green
                EventKind::Web => ("\u{2316}", Color::Rgb(0x56, 0xB6, 0xC2)),    // ⌖ cyan
                EventKind::Tool => ("\u{25E6}", Color::Rgb(0x82, 0x88, 0x96)),   // ◦ grey
            };
            // Milestones (prompt/PR/error) in their accent; tool lines in plain text so the
            // glyph carries the family and the feed stays calm.
            let text_color = matches!(ev.kind, EventKind::Prompt | EventKind::Pr | EventKind::Error)
                .then_some(color)
                .unwrap_or(TEXT);
            lines.push(kv(label, vec![
                Span::styled(format!("{:>8} ", fmt_idle_clock(ev.at_ms)), Style::default().fg(DIM)),
                Span::styled(format!("{mark} "), Style::default().fg(color)),
                Span::styled(trunc(&ev.text, 84), Style::default().fg(text_color)),
            ]));
        }
        lines.push(Line::from(""));
    }

    Detail { title: format!(" digest \u{00B7} {} ", s.name), lines, scroll: 0 }
}

/// Draw the digest as a near-fullscreen scrollable panel over the list. Only the
/// visible slice is cloned per frame, so a 1500-line timeline stays cheap to redraw.
fn render_detail(frame: &mut Frame, area: Rect, detail: &Detail) {
    let rect = centered_rect(area.width.saturating_sub(2), area.height, area);
    let inner_h = rect.height.saturating_sub(2) as usize; // borders top+bottom
    let start = (detail.scroll as usize).min(detail.lines.len().saturating_sub(1));
    let visible: Vec<Line> = detail.lines.iter().skip(start).take(inner_h).cloned().collect();
    frame.render_widget(Clear, rect);
    frame.render_widget(
        Paragraph::new(visible).block(
            Block::bordered()
                .title(detail.title.clone())
                .border_style(Style::default().fg(DIM))
                .padding(Padding::new(1, 1, 0, 0)),
        ),
        rect,
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{Source, State};

    fn sdk(name: &str, cwd: &str, tokens: u64, age_ms: i64, cpu: Option<f32>) -> Session {
        Session {
            session_id: "test".into(),
            transcript_path: None,
            source: Source::Sdk,
            state: State::Idle,
            name: name.into(),
            cwd: cwd.into(),
            waiting_for: None,
            total_tokens: tokens,
            context_tokens: 0,
            tool_calls: 0,
            web_requests: 0,
            age_ms,
            summary: String::new(),
            integrations: vec![],
            origin: Some("Zed".into()),
            model: None,
            git_branch: None,
            worktree: None,
            pr_number: None,
            pr_url: None,
            active_span_ms: None,
            cpu_pct: cpu,
            tint: None,
            note: None,
        }
    }

    // Every compact row's meta must be the exact same display width, so the
    // cwd/tokens/age/cpu columns line up vertically regardless of cell contents.
    #[test]
    fn compact_metas_are_equal_width() {
        let rows = [
            sdk("webapp", "/Users/me/Projects/webapp", 1_500_000, 2 * 86_400_000, Some(37.0)),
            sdk("Projects", "/Users/me/Projects", 0, 3 * 86_400_000, Some(0.0)),
            sdk("x", "/tmp", 42, 5_000, Some(100.0)),
        ];
        let cols = CompactCols::measure(rows.iter());
        let widths: Vec<usize> = rows.iter().map(|s| compact_meta(s, &cols).chars().count()).collect();
        assert!(
            widths.windows(2).all(|w| w[0] == w[1]),
            "compact meta widths must be uniform, got {widths:?}"
        );
    }
}
