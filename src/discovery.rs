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
    /// How the session was launched: "cli" (terminal) vs "sdk-ts"/"sdk-cli" (editor/SDK).
    entrypoint: Option<String>,
    /// Original process start time, e.g. "Fri May 29 15:34:43 2026" (UTC). Used to
    /// detect PID reuse — a recycled pid will have a very different start time.
    #[serde(rename = "procStart")]
    proc_start: Option<String>,
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

/// Absolute path of the executable backing a live pid, via macOS `proc_pidpath`.
fn process_path(pid: i32) -> Option<String> {
    if pid <= 0 {
        return None;
    }
    let mut buf = [0u8; 4096]; // PROC_PIDPATHINFO_MAXSIZE
    let n = unsafe {
        libc::proc_pidpath(pid, buf.as_mut_ptr() as *mut libc::c_void, buf.len() as u32)
    };
    if n <= 0 {
        return None;
    }
    Some(String::from_utf8_lossy(&buf[..n as usize]).into_owned())
}

/// What we learn about a live process from a single `ps` probe.
struct ProcInfo {
    cpu: f32,
    start_secs: Option<i64>,
}

/// Probe several pids at once: recent CPU% and process start time. One `ps` call.
fn probe_processes(pids: &[i32]) -> std::collections::HashMap<i32, ProcInfo> {
    let mut map = std::collections::HashMap::new();
    let live: Vec<String> = pids.iter().filter(|p| **p > 0).map(|p| p.to_string()).collect();
    if live.is_empty() {
        return map;
    }
    let Ok(out) = std::process::Command::new("ps")
        .args(["-o", "pid=,%cpu=,lstart=", "-p", &live.join(",")])
        .output()
    else {
        return map;
    };
    for line in String::from_utf8_lossy(&out.stdout).lines() {
        let mut it = line.split_whitespace();
        let Some(pid) = it.next().and_then(|s| s.parse::<i32>().ok()) else { continue };
        let cpu = it.next().and_then(|s| s.parse::<f32>().ok()).unwrap_or(0.0);
        // Remaining tokens are the lstart date: "Day Mon DD HH:MM:SS YYYY" (local time).
        let start_secs = parse_proc_date(&it.collect::<Vec<_>>().join(" "), false);
        map.insert(pid, ProcInfo { cpu, start_secs });
    }
    map
}

/// Parse a `ctime`-style date ("Fri May 29 15:34:43 2026"). `utc` controls whether
/// the naive time is read as UTC (the file's procStart) or local (ps lstart).
fn parse_proc_date(s: &str, utc: bool) -> Option<i64> {
    use chrono::TimeZone;
    let norm = s.split_whitespace().collect::<Vec<_>>().join(" ");
    let naive = chrono::NaiveDateTime::parse_from_str(&norm, "%a %b %e %H:%M:%S %Y").ok()?;
    if utc {
        Some(naive.and_utc().timestamp())
    } else {
        chrono::Local.from_local_datetime(&naive).single().map(|d| d.timestamp())
    }
}

/// Best-effort host name for an SDK session, from its executable path. Editors run
/// Claude out of their own data dir, e.g.
/// `~/Library/Application Support/Zed/.../claude` → "Zed".
fn origin_from_path(path: &str) -> Option<String> {
    let app = path.split("Application Support/").nth(1)?.split('/').next()?;
    match app {
        "" => None,
        "Code" => Some("VS Code".to_string()),
        other => Some(other.to_string()),
    }
}

/// Name of the linked git worktree the cwd lives in, or None for the main checkout.
///
/// A linked worktree's `.git` is a *file* like `gitdir: /repo/.git/worktrees/<name>`;
/// the main checkout's `.git` is a directory (so `read_to_string` fails → None).
fn worktree_name(cwd: &str) -> Option<String> {
    let content = std::fs::read_to_string(std::path::Path::new(cwd).join(".git")).ok()?;
    parse_worktree_gitdir(&content)
}

/// Pull "<name>" out of a worktree `.git` file body (`gitdir: …/worktrees/<name>`).
/// Returns None for submodules (`…/modules/…`) or anything without a worktree path.
fn parse_worktree_gitdir(content: &str) -> Option<String> {
    let gitdir = content.strip_prefix("gitdir:")?.trim();
    let name = gitdir.split("/worktrees/").nth(1)?.split('/').next()?;
    (!name.is_empty()).then(|| name.to_string())
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

/// List priority for a state (lower = higher up). Live, attention-needing sessions
/// rise; the resting states (Idle/Done/Stale) share the bottom tier and are then
/// ordered by age so the longest-inactive sinks to the very bottom.
fn state_rank(state: State) -> u8 {
    match state {
        State::NeedsYou => 0,
        State::Working => 1,
        State::Error => 2,
        State::Idle | State::Done | State::Stale => 3,
    }
}

/// Interactive terminal sessions sort above the non-interactive (SDK/editor) mini-list.
fn source_rank(source: Source) -> u8 {
    match source {
        Source::Terminal => 0,
        _ => 1,
    }
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
            // Live but idle, or no status at all (e.g. SDK sessions): not actively working.
            // Don't claim "Working" without evidence — that's what made stale/background
            // sessions look busy.
            _ => State::Idle,
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

    // Parse every session file first so we can probe all their pids in one `ps` call.
    let files: Vec<SessionFile> = entries
        .flatten()
        .filter(|e| e.path().extension().and_then(|s| s.to_str()) == Some("json"))
        .filter_map(|e| std::fs::read_to_string(e.path()).ok())
        .filter_map(|t| serde_json::from_str::<SessionFile>(&t).ok())
        .collect();
    let procs = probe_processes(&files.iter().map(|f| f.pid).collect::<Vec<_>>());

    for f in &files {
        let proc = procs.get(&f.pid);

        // A live pid alone isn't enough — pids get recycled. If the running process
        // started at a very different time than the file recorded, it's a different
        // process wearing the same pid, so the original session is gone.
        let same_process = match (
            f.proc_start.as_deref().and_then(|s| parse_proc_date(s, true)),
            proc.and_then(|p| p.start_secs),
        ) {
            (Some(want), Some(got)) => (want - got).abs() <= 120,
            _ => true, // can't verify either side — trust the liveness check
        };
        let alive = pid_alive(f.pid) && same_process;

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

        let state = resolve_state(f, alive, age_ms, analysis.errored);

        // CPU is only meaningful while the original process is actually running.
        let cpu_pct = alive.then(|| proc.map(|p| p.cpu)).flatten();

        // "cli" (or a legacy file with no entrypoint) is an interactive terminal session;
        // anything else (sdk-ts, sdk-cli, …) was launched by an editor/SDK.
        let source = match f.entrypoint.as_deref() {
            Some("cli") | None => Source::Terminal,
            Some(_) => Source::Sdk,
        };

        // For SDK sessions, try to name the host editor from the running process.
        let origin = (source == Source::Sdk)
            .then(|| process_path(f.pid).as_deref().and_then(origin_from_path))
            .flatten();

        let name = f.name.clone().unwrap_or_else(|| {
            f.cwd
                .rsplit('/')
                .next()
                .filter(|s| !s.is_empty())
                .unwrap_or("session")
                .to_string()
        });

        let mut session = Session {
            source,
            state,
            name,
            cwd: f.cwd.clone(),
            waiting_for: f.waiting_for.clone(),
            total_tokens: analysis.total_tokens,
            context_tokens: analysis.context_tokens,
            tool_calls: analysis.tool_calls,
            web_requests: analysis.web_requests,
            age_ms,
            summary: String::new(),
            integrations: analysis.integrations.clone(),
            origin,
            model: analysis.model.clone(),
            git_branch: analysis.git_branch.clone(),
            worktree: worktree_name(&f.cwd),
            pr_number: analysis.pr_number,
            pr_url: analysis.pr_url.clone(),
            active_span_ms: analysis.active_span_ms,
            cpu_pct,
        };

        session.summary = summarizer.summarize(&session, &analysis);
        sessions.push(session);
    }

    // Interactive sessions first, then the SDK mini-list. Within each group: by state
    // priority (needs-you → working → resting), then by age so the most recently active
    // is highest and the longest-idle falls to the very bottom of its group.
    sessions.sort_by(|a, b| {
        source_rank(a.source)
            .cmp(&source_rank(b.source))
            .then(state_rank(a.state).cmp(&state_rank(b.state)))
            .then(a.age_ms.cmp(&b.age_ms))
    });
    Ok(sessions)
}

#[cfg(test)]
mod tests {
    use super::parse_worktree_gitdir;

    #[test]
    fn extracts_worktree_name() {
        assert_eq!(
            parse_worktree_gitdir("gitdir: /Users/me/repo/.git/worktrees/feature-x\n"),
            Some("feature-x".to_string())
        );
    }

    #[test]
    fn ignores_submodules_and_garbage() {
        assert_eq!(parse_worktree_gitdir("gitdir: /Users/me/repo/.git/modules/sub\n"), None);
        assert_eq!(parse_worktree_gitdir("not a gitdir file"), None);
    }
}
