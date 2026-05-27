use crate::model::{Session, Source, State};
use crate::summarize::Summarizer;
use crate::transcript;
use anyhow::Result;
use serde::Deserialize;
use std::path::PathBuf;

/// Finished-but-recent sessions stay "Done" for this long, then fade to "Stale".
const STALE_AFTER_MS: i64 = 60 * 60 * 1000;

#[derive(Deserialize)]
struct SessionFile {
    pid: i32,
    #[serde(rename = "sessionId")]
    session_id: String,
    cwd: String,
    status: Option<String>,
    #[serde(rename = "waitingFor")]
    waiting_for: Option<String>,
    name: Option<String>,
    #[serde(rename = "updatedAt")]
    updated_at: Option<i64>,
    #[serde(rename = "startedAt")]
    started_at: Option<i64>,
}

fn claude_dir() -> PathBuf {
    let home = std::env::var("HOME").unwrap_or_default();
    PathBuf::from(home).join(".claude")
}

/// True if a process with this pid is currently alive.
fn pid_alive(pid: i32) -> bool {
    if pid <= 0 {
        return false;
    }
    // kill(pid, 0): 0 => alive & ours; EPERM => alive but not ours; ESRCH => gone.
    let rc = unsafe { libc::kill(pid, 0) };
    if rc == 0 {
        return true;
    }
    std::io::Error::last_os_error().raw_os_error() == Some(libc::EPERM)
}

/// Claude Code encodes a cwd into a project dir name by replacing `/` and `.` with `-`.
fn encode_cwd(cwd: &str) -> String {
    cwd.chars()
        .map(|c| if c == '/' || c == '.' { '-' } else { c })
        .collect()
}

fn now_ms() -> i64 {
    chrono::Utc::now().timestamp_millis()
}

/// Resolve the lifecycle state from the live file + process liveness.
fn resolve_state(f: &SessionFile, alive: bool, age_ms: i64, errored: bool) -> State {
    if errored {
        return State::Error;
    }
    if alive {
        return match f.status.as_deref() {
            Some("busy") => State::Working,
            Some("waiting") => State::NeedsYou,
            // Live but unknown status: treat as working so it isn't mistaken for done.
            _ => State::Working,
        };
    }
    if age_ms <= STALE_AFTER_MS {
        State::Done
    } else {
        State::Stale
    }
}

/// Load and fully populate every known session, newest activity first.
pub fn load_sessions(summarizer: &dyn Summarizer) -> Result<Vec<Session>> {
    let dir = claude_dir().join("sessions");
    let mut sessions = Vec::new();

    let entries = match std::fs::read_dir(&dir) {
        Ok(e) => e,
        Err(_) => return Ok(sessions), // no sessions dir yet — empty dashboard
    };

    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().and_then(|s| s.to_str()) != Some("json") {
            continue;
        }
        let Ok(text) = std::fs::read_to_string(&path) else { continue };
        let Ok(f) = serde_json::from_str::<SessionFile>(&text) else { continue };

        let alive = pid_alive(f.pid);

        // Locate the transcript: ~/.claude/projects/<encoded-cwd>/<sessionId>.jsonl
        let transcript_path = claude_dir()
            .join("projects")
            .join(encode_cwd(&f.cwd))
            .join(format!("{}.jsonl", f.session_id));
        let transcript = transcript_path.exists().then_some(transcript_path);

        let analysis = transcript
            .as_ref()
            .map(|p| transcript::analyze(p))
            .unwrap_or_default();

        // Age: prefer the file's updatedAt, else its mtime, else now.
        let updated = f.updated_at.or(f.started_at).unwrap_or_else(now_ms);
        let age_ms = (now_ms() - updated).max(0);

        let state = resolve_state(&f, alive, age_ms, analysis.errored);

        let name = f.name.clone().unwrap_or_else(|| {
            f.cwd
                .rsplit('/')
                .next()
                .filter(|s| !s.is_empty())
                .unwrap_or("session")
                .to_string()
        });

        let mut session = Session {
            id: f.session_id.clone(),
            source: Source::Terminal,
            state,
            name,
            cwd: f.cwd.clone(),
            waiting_for: f.waiting_for.clone(),
            total_tokens: analysis.total_tokens,
            context_tokens: analysis.context_tokens,
            age_ms,
            summary: String::new(),
            integrations: analysis.integrations.clone(),
            transcript,
        };

        session.summary = summarizer.summarize(&session, &analysis);
        sessions.push(session);
    }

    // Most recently active first.
    sessions.sort_by_key(|s| s.age_ms);
    Ok(sessions)
}
