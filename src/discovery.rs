use crate::model::{Session, Source, State};
use crate::note;
use crate::summarize::Summarizer;
use crate::tint;
use crate::transcript;
use anyhow::Result;
use serde::Deserialize;
use std::collections::HashSet;
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
/// Name of the linked git worktree the cwd lives in, or None for the main checkout or a
/// non-repo. Derived straight from git, so it's correct no matter where the worktree
/// sits on disk — including layouts where worktrees are separated from the main checkout
/// (e.g. `repo/.worktrees/<name>` alongside `repo/main`) — and from any subdirectory.
fn worktree_name(cwd: &str) -> Option<String> {
    let out = std::process::Command::new("git")
        .arg("-C")
        .arg(cwd)
        .args(["rev-parse", "--absolute-git-dir"])
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    worktree_from_git_dir(String::from_utf8_lossy(&out.stdout).trim())
}

/// A linked worktree's git dir is `<main>/.git/worktrees/<name>`; the main checkout's is
/// just `<main>/.git`. Pull `<name>` out, or None when the path isn't a worktree's.
fn worktree_from_git_dir(git_dir: &str) -> Option<String> {
    let name = git_dir.split("/worktrees/").nth(1)?.split('/').next()?;
    (!name.is_empty()).then(|| name.to_string())
}

/// Resolve the worktree a session belongs to, returning its (name, path).
///
/// Two ways a session relates to a worktree:
///   1. Its cwd *is* inside a linked worktree — detected straight from the cwd.
///   2. It runs from the main checkout but works on a `branch` that's checked out in a
///      sibling worktree. This is common when Claude is always launched from the source
///      repo; the cwd stays at the main checkout, but the branch identifies the worktree.
///
/// Case 2 is resolved by matching the branch against `git worktree list`, which is
/// authoritative regardless of where the worktree sits on disk.
fn worktree_for(cwd: &str, branch: Option<&str>) -> Option<(String, String)> {
    if let Some(name) = worktree_name(cwd) {
        return Some((name, cwd.to_string()));
    }
    let path = worktree_path_for_branch(cwd, branch?)?;
    // Reuse the cwd-based check on the matched path: it yields the name for a linked
    // worktree and None for the main checkout, so a `main`-branch session stays unmarked.
    let name = worktree_name(&path)?;
    Some((name, path))
}

/// Path of the worktree that has `branch` checked out, via `git -C <cwd> worktree list`.
/// Works from the main checkout — git reports every worktree and its branch.
fn worktree_path_for_branch(cwd: &str, branch: &str) -> Option<String> {
    let out = std::process::Command::new("git")
        .arg("-C")
        .arg(cwd)
        .args(["worktree", "list", "--porcelain"])
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    // Porcelain output is blank-line-separated blocks of `worktree <path>` / `branch <ref>`.
    let text = String::from_utf8_lossy(&out.stdout);
    let target = format!("refs/heads/{branch}");
    let mut path: Option<&str> = None;
    for line in text.lines() {
        if let Some(p) = line.strip_prefix("worktree ") {
            path = Some(p);
        } else if line.strip_prefix("branch ") == Some(target.as_str()) {
            return path.map(str::to_string);
        }
    }
    None
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

        // The session file records only the launch dir; the transcript tracks the
        // session's live cwd, which follows `cd`s into a worktree. Prefer the latter
        // when it still exists, else fall back to the launch dir (the transcript path
        // can be stale if a worktree was moved or removed).
        let mut cwd = match analysis.cwd.clone() {
            Some(c) if std::path::Path::new(&c).is_dir() => c,
            _ => f.cwd.clone(),
        };

        // Resolve the worktree from the cwd, or — for sessions launched from the main
        // checkout — from the branch via `git worktree list`. When found, surface the
        // worktree's own path so the displayed cwd matches where the work lives.
        let worktree = match worktree_for(&cwd, analysis.git_branch.as_deref()) {
            Some((name, path)) => {
                cwd = path;
                Some(name)
            }
            None => None,
        };

        let name = f.name.clone().unwrap_or_else(|| {
            cwd.rsplit('/')
                .next()
                .filter(|s| !s.is_empty())
                .unwrap_or("session")
                .to_string()
        });

        let tint = tint::read_for(&f.session_id);
        let note = note::read_for(&f.session_id);

        let mut session = Session {
            session_id: f.session_id.clone(),
            transcript_path: transcript.clone(),
            source,
            state,
            name,
            cwd,
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
            worktree,
            pr_number: analysis.pr_number,
            pr_url: analysis.pr_url.clone(),
            active_span_ms: analysis.active_span_ms,
            cpu_pct,
            tint,
            note,
        };

        session.summary = summarizer.summarize(&session, &analysis);
        sessions.push(session);
    }

    // Sweep stale tint files (session ID no longer present), guarded on a
    // non-empty enumeration so a transient FS error doesn't wipe colors. tend
    // is the natural place for GC because it already walks the session universe.
    if !sessions.is_empty() {
        let ids: HashSet<String> = sessions.iter().map(|s| s.session_id.clone()).collect();
        tint::gc(&ids);
        note::gc(&ids);
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
    use super::{worktree_for, worktree_from_git_dir, worktree_name};
    use std::path::Path;
    use std::process::Command;

    #[test]
    fn extracts_worktree_name_from_git_dir() {
        assert_eq!(
            worktree_from_git_dir("/Users/me/repo/.git/worktrees/feature-x"),
            Some("feature-x".to_string())
        );
    }

    #[test]
    fn main_checkout_and_submodules_are_not_worktrees() {
        assert_eq!(worktree_from_git_dir("/Users/me/repo/.git"), None); // main checkout
        assert_eq!(worktree_from_git_dir("/Users/me/repo/.git/modules/sub"), None); // submodule
    }

    /// Unique scratch dir for one test, removed on drop.
    struct TmpDir(std::path::PathBuf);
    impl TmpDir {
        fn new(tag: &str) -> Self {
            let p = std::env::temp_dir().join(format!("tend-test-{}-{}", tag, std::process::id()));
            let _ = std::fs::remove_dir_all(&p);
            std::fs::create_dir_all(&p).unwrap();
            TmpDir(p)
        }
    }
    impl Drop for TmpDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn git(dir: &Path, args: &[&str]) {
        let ok = Command::new("git")
            .arg("-C")
            .arg(dir)
            // self-contained identity so the test doesn't depend on global git config
            .args(["-c", "user.email=t@t", "-c", "user.name=t"])
            .args(args)
            .output()
            .expect("git runs")
            .status
            .success();
        assert!(ok, "git {args:?} failed");
    }

    // Reproduces Matt's layout: worktrees in `repo/.worktrees/<name>`, separated from the
    // main checkout `repo/claims`. Detection must work from the worktree and its subdirs.
    #[test]
    fn detects_separated_worktree_via_git() {
        let tmp = TmpDir::new("sepwt");
        let main = tmp.0.join("claims");
        let wt = tmp.0.join(".worktrees/CO-5100-canonical-lookup-tuples");
        std::fs::create_dir_all(&main).unwrap();
        git(&main, &["init", "-q"]);
        git(&main, &["commit", "-q", "--allow-empty", "-m", "init"]);
        git(&main, &["worktree", "add", "-q", wt.to_str().unwrap(), "-b", "CO-5100"]);

        let name = Some("CO-5100-canonical-lookup-tuples".to_string());
        assert_eq!(worktree_name(wt.to_str().unwrap()), name); // from worktree root
        let sub = wt.join("src/inner");
        std::fs::create_dir_all(&sub).unwrap();
        assert_eq!(worktree_name(sub.to_str().unwrap()), name); // from a nested subdir
        assert_eq!(worktree_name(main.to_str().unwrap()), None); // main checkout: no marker
    }

    // Matt's real case: Claude is launched from the main checkout (`claims`), so the cwd
    // never points at a worktree — but the session's branch is checked out in a sibling
    // worktree. Resolution must come from the branch, and surface the worktree's path.
    #[test]
    fn resolves_worktree_from_branch_in_main_checkout() {
        let tmp = TmpDir::new("branchwt");
        let main = tmp.0.join("claims");
        let wt = tmp.0.join(".worktrees/CO-5390-relocate-poc-email");
        std::fs::create_dir_all(&main).unwrap();
        git(&main, &["init", "-q"]);
        git(&main, &["commit", "-q", "--allow-empty", "-m", "init"]);
        git(&main, &["worktree", "add", "-q", wt.to_str().unwrap(), "-b", "CO-5390/relocate-poc-email"]);

        let main_s = main.to_str().unwrap();
        // `git worktree list` reports the canonical path (e.g. /private/tmp on macOS).
        let wt_real = std::fs::canonicalize(&wt).unwrap().to_str().unwrap().to_string();
        let name = "CO-5390-relocate-poc-email".to_string();
        // From the main checkout, the branch identifies the worktree (name + its path).
        assert_eq!(
            worktree_for(main_s, Some("CO-5390/relocate-poc-email")),
            Some((name.clone(), wt_real))
        );
        // A session on the main branch (or unknown branch) stays unmarked.
        assert_eq!(worktree_for(main_s, Some("main")), None);
        assert_eq!(worktree_for(main_s, None), None);
        // And if the cwd already is the worktree, that still resolves directly.
        assert_eq!(
            worktree_for(wt.to_str().unwrap(), None),
            Some((name, wt.to_str().unwrap().to_string()))
        );
    }
}
