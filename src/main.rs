mod actions;
mod discovery;
mod model;
mod summarize;
mod tint;
mod transcript;
mod ui;

use anyhow::Result;
use model::Session;
use ratatui::crossterm::event::{self, Event, KeyCode, KeyEventKind};
use std::time::{Duration, Instant};
use summarize::{StubSummarizer, Summarizer};
use ui::Menu;

const FRAME: Duration = Duration::from_millis(100); // ~10fps animation
const REFRESH: Duration = Duration::from_secs(2); // re-scan sessions

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
            status_msg: None,
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

    let mut terminal = ratatui::init();
    let mut app = App::new();

    let result = run(&mut terminal, &mut app);

    ratatui::restore();
    result
}

fn run(terminal: &mut ratatui::DefaultTerminal, app: &mut App) -> Result<()> {
    loop {
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
                app.status_msg.as_deref(),
            )
        })?;

        if event::poll(FRAME)? {
            if let Event::Key(key) = event::read()? {
                if key.kind == KeyEventKind::Press {
                    if app.menu.is_some() {
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
