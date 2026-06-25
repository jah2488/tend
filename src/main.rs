mod actions;
mod discovery;
mod model;
mod mcp;
mod note;
mod store;
mod summarize;
mod tint;
mod transcript;
mod ui;

use anyhow::Result;
use model::{Session, State};
use ratatui::crossterm::event::{self, Event, KeyCode, KeyEventKind, KeyModifiers};
use std::io::IsTerminal;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};
use summarize::{StubSummarizer, Summarizer};
use ui::{Detail, Menu};

/// Frame interval while something is animating (Working spinner, NeedsYou pulse).
const FRAME_ACTIVE: Duration = Duration::from_millis(100); // ~10fps
/// Slower frame interval when nothing animates — still ticks the idle MM:SS clock,
/// but ~5x fewer idle wakeups for an always-on dashboard (battery).
const FRAME_IDLE: Duration = Duration::from_millis(500);
const REFRESH: Duration = Duration::from_secs(2); // re-scan sessions

/// One-screen usage, printed for `-h/--help` (stdout) or a usage error (stderr).
fn print_usage(out: impl std::io::Write) {
    let mut out = out;
    let _ = write!(
        out,
        "tend {} — keep your Claude Code sessions in view\n\n\
         USAGE:\n    tend [OPTIONS]\n\n\
         OPTIONS:\n\
         \x20   (none)              launch the live dashboard (requires a terminal)\n\
         \x20   --list              print parsed sessions as text and exit\n\
         \x20   --list-actions      print discovered tend-action-* extensions and exit\n\
         \x20   --digest [ID|NAME]  print one session's digest as text and exit\n\
         \x20   mcp                  run the MCP server over stdio (for Claude / MCP clients)\n\
         \x20   -h, --help          show this help and exit\n\
         \x20   -V, --version       show version and exit\n",
        env!("CARGO_PKG_VERSION"),
    );
}

struct App {
    sessions: Vec<Session>,
    selected: usize,
    tick: u64,
    summarizer: Box<dyn Summarizer>,
    last_refresh: Instant,
    /// Action extensions discovered on PATH at startup (not re-scanned per refresh —
    /// that would respawn describe processes every couple seconds).
    actions: Vec<actions::Action>,
    /// The action picker, when open.
    menu: Option<Menu>,
    /// The session-digest overlay, when open.
    detail: Option<Detail>,
    /// Transient footer message (e.g. the result of running an action).
    status_msg: Option<String>,
}

impl App {
    fn new() -> Self {
        let summarizer: Box<dyn Summarizer> = Box::new(StubSummarizer);
        let sessions = discovery::load_sessions(summarizer.as_ref()).unwrap_or_default();
        App {
            sessions,
            selected: 0,
            tick: 0,
            summarizer,
            last_refresh: Instant::now(),
            actions: actions::discover(),
            menu: None,
            detail: None,
            status_msg: None,
        }
    }

    /// Open the digest overlay for the selected session, reading its transcript fresh.
    /// The panel is frozen at open time, so it's unaffected by list refreshes underneath.
    fn open_detail(&mut self) {
        let Some(s) = self.sessions.get(self.selected) else {
            return;
        };
        match &s.transcript_path {
            Some(p) => {
                let digest = transcript::digest(p);
                self.status_msg = None;
                self.detail = Some(ui::build_detail(s, &digest));
            }
            None => self.status_msg = Some("no transcript for this session yet".into()),
        }
    }

    fn refresh(&mut self) {
        if let Ok(s) = discovery::load_sessions(self.summarizer.as_ref()) {
            self.sessions = s;
            if self.selected >= self.sessions.len() {
                self.selected = self.sessions.len().saturating_sub(1);
            }
        }
        self.last_refresh = Instant::now();
    }

    fn select_next(&mut self) {
        self.status_msg = None;
        if !self.sessions.is_empty() {
            self.selected = (self.selected + 1) % self.sessions.len();
        }
    }

    fn select_prev(&mut self) {
        self.status_msg = None;
        if !self.sessions.is_empty() {
            self.selected = (self.selected + self.sessions.len() - 1) % self.sessions.len();
        }
    }

    /// Open the action picker for the selected session, filtered to applicable actions.
    /// Surfaces a footer note instead when there's nothing to offer.
    fn open_menu(&mut self) {
        let Some(s) = self.sessions.get(self.selected) else {
            return;
        };
        let items: Vec<usize> = self
            .actions
            .iter()
            .enumerate()
            .filter(|(_, a)| a.applicable(s))
            .map(|(i, _)| i)
            .collect();
        if items.is_empty() {
            self.status_msg = Some(if self.actions.is_empty() {
                "no actions installed — see the Extensions section of the README".into()
            } else {
                "no actions apply to this session".into()
            });
            return;
        }
        self.status_msg = None;
        self.menu = Some(Menu { items, selected: 0 });
    }

    /// Run the action at `idx` against the selected session, then refresh (its work may
    /// have changed the session — e.g. a new commit).
    fn run_action(&mut self, idx: usize, terminal: &mut ratatui::DefaultTerminal) {
        let Some(session) = self.sessions.get(self.selected).cloned() else {
            return;
        };
        let result = self.actions[idx].run(&session, terminal);
        let label = self.actions[idx].label.clone();
        self.status_msg = Some(match result {
            Ok(s) if s.success() => format!("\u{2713} {label} done"),
            Ok(s) => format!(
                "\u{2715} {label} exited {}",
                s.code().map_or_else(|| "(signal)".to_string(), |c| c.to_string())
            ),
            Err(e) => format!("\u{2715} {label}: {e}"),
        });
        self.refresh();
    }
}

fn main() -> Result<()> {
    // Restore default SIGPIPE so the text outputs behave like a normal filter: a closed
    // downstream (`tend --digest big | head`) ends the process quietly instead of making
    // Rust's `println!` panic with "Broken pipe". Rust sets SIGPIPE to SIG_IGN at startup.
    unsafe {
        libc::signal(libc::SIGPIPE, libc::SIG_DFL);
    }

    // `-h/--help` and `-V/--version` come first — a utility answers these, never launches
    // the UI for them.
    if std::env::args().any(|a| a == "-h" || a == "--help") {
        print_usage(std::io::stdout());
        return Ok(());
    }
    if std::env::args().any(|a| a == "-V" || a == "--version") {
        println!("tend {}", env!("CARGO_PKG_VERSION"));
        return Ok(());
    }

    // `tend --list` prints the parsed sessions as plain text and exits — handy for
    // debugging the data pipeline without a TTY.
    if std::env::args().any(|a| a == "--list") {
        let summarizer = StubSummarizer;
        for s in discovery::load_sessions(&summarizer)? {
            println!(
                "[{:>9}] {:<32} {:>7} tok  ctx {:<7}  {}  uses: {}",
                s.state.label(),
                s.name,
                s.total_tokens,
                s.context_tokens,
                s.summary,
                if s.integrations.is_empty() {
                    "—".into()
                } else {
                    s.integrations.join(", ")
                },
            );
        }
        return Ok(());
    }

    // `tend mcp` speaks the Model Context Protocol over stdio, so another client
    // (e.g. a Claude Code session via the tend skill) can query sessions and
    // annotate them with tints/notes. A non-interactive text mode, like --list.
    if std::env::args().any(|a| a == "mcp") {
        return mcp::serve();
    }

    // `tend --list-actions` prints the discovered extensions and exits — lets extension
    // authors confirm their `tend-action-*` is found and its `--tend-describe` parsed,
    // without a TTY.
    if std::env::args().any(|a| a == "--list-actions") {
        let actions = actions::discover();
        if actions.is_empty() {
            println!("no actions found on PATH (looked for tend-action-*)");
        }
        for a in &actions {
            println!(
                "{:<16} {:<24} key {}  {}",
                a.name,
                a.label,
                a.key.map_or_else(|| "—".to_string(), |c| c.to_string()),
                a.exe.display(),
            );
        }
        return Ok(());
    }

    // `tend --digest [id|name]` prints one session's digest as plain text and exits —
    // verify the timeline/extraction without a TTY. Defaults to the first session.
    if let Some(pos) = std::env::args().position(|a| a == "--digest") {
        let needle = std::env::args().nth(pos + 1);
        let summarizer = StubSummarizer;
        let sessions = discovery::load_sessions(&summarizer)?;
        let sel = match &needle {
            Some(n) => sessions
                .iter()
                .find(|s| s.session_id.contains(n.as_str()) || s.name.contains(n.as_str())),
            None => sessions.first(),
        };
        let Some(s) = sel else {
            println!("no matching session");
            return Ok(());
        };
        let Some(tp) = &s.transcript_path else {
            println!("{} has no transcript on disk", s.name);
            return Ok(());
        };
        let d = transcript::digest(tp);
        let tool_total: u64 = d.tool_counts.iter().map(|(_, c)| c).sum();
        println!("digest · {}  [{}]", s.name, s.state.label());
        println!(
            "  {} tok · {} tool calls · {} integrations · {} files changed · {} timeline events (+{} elided)",
            d.total_tokens, tool_total, d.integration_counts.len(), d.files_edited.len(),
            d.timeline.len(), d.timeline_dropped,
        );
        println!(
            "  TOOLS: {}",
            d.tool_counts.iter().take(14).map(|(n, c)| format!("{n}×{c}")).collect::<Vec<_>>().join(", ")
        );
        println!("  TIMELINE:");
        for ev in &d.timeline {
            let t = ev.at_ms / 1000;
            // A 1-char family marker mirrors the TUI glyph; text already carries detail.
            let mark = match ev.kind {
                transcript::EventKind::Prompt => '>',
                transcript::EventKind::Pr => '@',
                transcript::EventKind::Error => 'x',
                transcript::EventKind::Read => 'r',
                transcript::EventKind::Edit => 'e',
                transcript::EventKind::Bash => '$',
                transcript::EventKind::Web => 'w',
                transcript::EventKind::Tool => '.',
            };
            println!("    {:>3}:{:02}  {}  {}", t / 60, t % 60, mark, ev.text);
        }
        return Ok(());
    }

    // Reaching here means no text-mode flag matched, so we're launching the TUI. Reject
    // any leftover arguments (unknown flags, typos like `--lst`) with a usage error and
    // exit 2, rather than silently ignoring them and starting the UI.
    if let Some(bad) = std::env::args().skip(1).find(|a| !a.is_empty()) {
        eprintln!("tend: unexpected argument '{bad}'\n");
        print_usage(std::io::stderr());
        std::process::exit(2);
    }

    // The dashboard needs a real terminal to draw into. Detect a non-tty stdout (piped or
    // redirected) or a terminal that can't render (TERM unset/dumb) and bail cleanly,
    // instead of panicking inside ratatui::init() or spewing escape sequences into a pipe.
    if !std::io::stdout().is_terminal() {
        eprintln!(
            "tend: not a terminal — the dashboard needs an interactive TTY.\n\
             For non-interactive use try: tend --list | --digest [ID] | --list-actions"
        );
        std::process::exit(1);
    }
    // stdout is a tty, but TERM is unset or 'dumb' — report the real condition instead
    // of the misleading "not a terminal" the combined guard used to print.
    let term_ok = std::env::var("TERM").map(|t| t != "dumb").unwrap_or(false);
    if !term_ok {
        eprintln!(
            "tend: TERM is unset or 'dumb' — the dashboard needs a capable terminal.\n\
             Try: TERM=xterm-256color tend"
        );
        std::process::exit(1);
    }

    // Restore the terminal on a fatal signal. ratatui's init() installs a panic hook, but
    // not a signal handler — without this, SIGTERM/SIGHUP/SIGINT would kill the process
    // mid-draw and leave the real terminal in raw mode + alternate screen ("bricked").
    // The handler just sets a flag; the loop notices it and exits through the normal
    // restore() path within one frame.
    let quit = Arc::new(AtomicBool::new(false));
    for sig in [
        signal_hook::consts::SIGINT,
        signal_hook::consts::SIGTERM,
        signal_hook::consts::SIGHUP,
    ] {
        signal_hook::flag::register(sig, Arc::clone(&quit))?;
    }

    let mut terminal = ratatui::init();
    let mut app = App::new();

    let result = run(&mut terminal, &mut app, &quit);

    ratatui::restore();
    result
}

fn run(terminal: &mut ratatui::DefaultTerminal, app: &mut App, quit: &Arc<AtomicBool>) -> Result<()> {
    loop {
        // A fatal signal (SIGTERM/SIGHUP/SIGINT) flips this; exit via the normal restore().
        if quit.load(Ordering::Relaxed) {
            break;
        }

        // ms since the last scan, so idle timers tick smoothly between 2s refreshes.
        let offset_ms = app.last_refresh.elapsed().as_millis() as i64;
        terminal.draw(|f| {
            ui::render(
                f,
                &app.sessions,
                app.selected,
                app.tick,
                offset_ms,
                &app.actions,
                app.menu.as_ref(),
                app.detail.as_ref(),
                app.status_msg.as_deref(),
            )
        })?;

        // Throttle the frame rate when nothing animates — far fewer idle wakeups for an
        // always-on dashboard. Key events still wake the poll immediately.
        let animating = app.menu.is_none()
            && app.detail.is_none()
            && app
                .sessions
                .iter()
                .any(|s| matches!(s.state, State::Working | State::NeedsYou));
        let frame = if animating { FRAME_ACTIVE } else { FRAME_IDLE };

        if event::poll(frame)? {
            if let Event::Key(key) = event::read()? {
                if key.kind == KeyEventKind::Press {
                    // Universal shortcuts, regardless of the focused view.
                    if key.modifiers.contains(KeyModifiers::CONTROL) {
                        match key.code {
                            // Ctrl-C / Ctrl-D quit. Raw mode delivers these as key events,
                            // not signals, so the loop has to handle them explicitly.
                            KeyCode::Char('c') | KeyCode::Char('d') => break,
                            // Ctrl-Z suspends to the shell; we re-enter on resume.
                            KeyCode::Char('z') => {
                                suspend(terminal);
                                continue;
                            }
                            // Ctrl-L forces a full redraw.
                            KeyCode::Char('l') => {
                                let _ = terminal.clear();
                                continue;
                            }
                            _ => {}
                        }
                    }
                    if app.detail.is_some() {
                        // ── modal: digest panel is open ──
                        match key.code {
                            KeyCode::Esc | KeyCode::Tab | KeyCode::Char('q') => app.detail = None,
                            KeyCode::Down | KeyCode::Char('j') => {
                                if let Some(d) = &mut app.detail {
                                    d.scroll_down();
                                }
                            }
                            KeyCode::Up | KeyCode::Char('k') => {
                                if let Some(d) = &mut app.detail {
                                    d.scroll_up();
                                }
                            }
                            _ => {}
                        }
                    } else if app.menu.is_some() {
                        // ── modal: action picker is open ──
                        match key.code {
                            KeyCode::Esc => app.menu = None,
                            KeyCode::Down | KeyCode::Char('j') => {
                                if let Some(m) = &mut app.menu {
                                    m.next();
                                }
                            }
                            KeyCode::Up | KeyCode::Char('k') => {
                                if let Some(m) = &mut app.menu {
                                    m.prev();
                                }
                            }
                            KeyCode::Enter => {
                                let idx = app.menu.as_ref().map(|m| m.items[m.selected]);
                                if let Some(idx) = idx {
                                    app.menu = None;
                                    app.run_action(idx, terminal);
                                }
                            }
                            KeyCode::Char(c) => {
                                // A direct key binding fires its action without arrowing.
                                let idx = app.menu.as_ref().and_then(|m| {
                                    m.items
                                        .iter()
                                        .copied()
                                        .find(|&i| app.actions[i].key == Some(c))
                                });
                                if let Some(idx) = idx {
                                    app.menu = None;
                                    app.run_action(idx, terminal);
                                }
                            }
                            _ => {}
                        }
                    } else {
                        // ── normal: session list ──
                        match key.code {
                            KeyCode::Char('q') | KeyCode::Esc => break,
                            KeyCode::Down | KeyCode::Char('j') => app.select_next(),
                            KeyCode::Up | KeyCode::Char('k') => app.select_prev(),
                            KeyCode::Char('r') | KeyCode::Char('s') => app.refresh(),
                            KeyCode::Enter | KeyCode::Char('a') => app.open_menu(),
                            KeyCode::Tab => app.open_detail(),
                            _ => {}
                        }
                    }
                }
            }
        }

        app.tick = app.tick.wrapping_add(1);
        if app.last_refresh.elapsed() >= REFRESH {
            app.refresh();
        }
    }
    Ok(())
}

/// Suspend tend on Ctrl-Z: hand the terminal back to the shell, stop via SIGTSTP, then
/// re-enter the TUI when the shell foregrounds us again (after SIGCONT). Without restoring
/// first, a suspended tend would leave the shell in raw mode + alternate screen.
fn suspend(terminal: &mut ratatui::DefaultTerminal) {
    ratatui::restore();
    // Stops here until `fg` (SIGCONT); execution resumes on the next line.
    unsafe {
        libc::raise(libc::SIGTSTP);
    }
    *terminal = ratatui::init();
}
