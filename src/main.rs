mod discovery;
mod model;
mod summarize;
mod transcript;
mod ui;

use anyhow::Result;
use model::Session;
use ratatui::crossterm::event::{self, Event, KeyCode, KeyEventKind};
use std::time::{Duration, Instant};
use summarize::{StubSummarizer, Summarizer};

const FRAME: Duration = Duration::from_millis(100); // ~10fps animation
const REFRESH: Duration = Duration::from_secs(2); // re-scan sessions

struct App {
    sessions: Vec<Session>,
    selected: usize,
    tick: u64,
    summarizer: Box<dyn Summarizer>,
    last_refresh: Instant,
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
        if !self.sessions.is_empty() {
            self.selected = (self.selected + 1) % self.sessions.len();
        }
    }

    fn select_prev(&mut self) {
        if !self.sessions.is_empty() {
            self.selected = (self.selected + self.sessions.len() - 1) % self.sessions.len();
        }
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
        terminal.draw(|f| ui::render(f, &app.sessions, app.selected, app.tick, offset_ms))?;

        if event::poll(FRAME)? {
            if let Event::Key(key) = event::read()? {
                if key.kind == KeyEventKind::Press {
                    match key.code {
                        KeyCode::Char('q') | KeyCode::Esc => break,
                        KeyCode::Down | KeyCode::Char('j') => app.select_next(),
                        KeyCode::Up | KeyCode::Char('k') => app.select_prev(),
                        KeyCode::Char('r') | KeyCode::Char('s') => app.refresh(),
                        _ => {}
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
