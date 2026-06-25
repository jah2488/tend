//! External action extensions, discovered git-style on `PATH`.
//!
//! tend never mutates anything itself — it *dispatches*. An extension is any executable
//! named `tend-action-<name>` on the user's `PATH`. tend learns how to present it by
//! running `tend-action-<name> --tend-describe` (the extension self-describes, so there's
//! no config file), and invokes it by handing over the terminal with the selected
//! session's *locators* in the environment. The extension reads session data fresh from
//! disk itself — tend passes ids and paths, never a serialized snapshot.
//!
//! This keeps tend's promises intact: tend-the-binary still makes no network calls and
//! holds no config. Extensions are the user's own opt-in and run with their privileges.

use crate::model::{Session, Source, State};
use serde::Deserialize;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

/// How long an extension's `--tend-describe` may run before tend gives up on it. Bounds
/// the worst case where a misbehaving extension would otherwise stall startup.
const DESCRIBE_TIMEOUT: Duration = Duration::from_millis(500);

/// A discovered action extension.
pub struct Action {
    /// The `<name>` suffix of `tend-action-<name>` — a stable id, also the menu fallback.
    pub name: String,
    /// Absolute path to the executable.
    pub exe: PathBuf,
    /// Display label (from `--tend-describe`, else the name).
    pub label: String,
    /// Suggested single-char binding (advisory; tend owns collision resolution).
    pub key: Option<char>,
    /// Optional applicability filter; when set, the action is hidden for sessions it
    /// doesn't match.
    pub when: Option<When>,
}

/// What an extension prints (one line of JSON) in response to `--tend-describe`.
#[derive(Deserialize, Default)]
struct Describe {
    name: Option<String>,
    key: Option<String>,
    when: Option<When>,
}

/// Applicability rules. Every field is optional; a present field must match.
#[derive(Deserialize, Clone, Default)]
pub struct When {
    /// "terminal" or "sdk".
    source: Option<String>,
    /// Whether the session must (true) or must not (false) have a git branch.
    has_branch: Option<bool>,
    /// Lifecycle state slug, e.g. "done", "needs-you", "idle".
    state: Option<String>,
}

/// Stable slug for a lifecycle state, used by `When.state` matching.
fn state_slug(state: State) -> &'static str {
    match state {
        State::Working => "working",
        State::NeedsYou => "needs-you",
        State::Idle => "idle",
        State::Done => "done",
        State::Stale => "stale",
        State::Error => "error",
    }
}

/// Slug for a session's source, used by `When.source` matching and the `TEND_SOURCE` env.
fn source_slug(source: Source) -> &'static str {
    match source {
        Source::Terminal => "terminal",
        _ => "sdk",
    }
}

impl Action {
    /// Whether this action should be offered for `s`.
    pub fn applicable(&self, s: &Session) -> bool {
        let Some(w) = &self.when else { return true };
        if let Some(src) = &w.source {
            if !src.eq_ignore_ascii_case(source_slug(s.source)) {
                return false;
            }
        }
        if let Some(want) = w.has_branch {
            if s.git_branch.is_some() != want {
                return false;
            }
        }
        if let Some(st) = &w.state {
            if !st.eq_ignore_ascii_case(state_slug(s.state)) {
                return false;
            }
        }
        true
    }

    /// Run the action against `s`: hand the terminal to the child with the session's
    /// locators in the environment, then re-enter the TUI when it exits.
    pub fn run(
        &self,
        s: &Session,
        terminal: &mut ratatui::DefaultTerminal,
    ) -> std::io::Result<std::process::ExitStatus> {
        // Give the terminal back to the child: leave the alternate screen / raw mode so
        // it can print, prompt, and confirm normally (git-style, like shelling to $EDITOR).
        ratatui::restore();

        let mut cmd = Command::new(&self.exe);
        cmd.env("TEND_VERSION", env!("CARGO_PKG_VERSION"))
            .env("TEND_ACTION", &self.name)
            .env("TEND_SESSION_ID", &s.session_id)
            .env("TEND_PROJECT_DIR", &s.cwd)
            .env("TEND_SESSION_NAME", &s.name)
            .env("TEND_SOURCE", source_slug(s.source));
        if let Some(p) = &s.transcript_path {
            cmd.env("TEND_TRANSCRIPT", p);
        }
        if let Some(b) = &s.git_branch {
            cmd.env("TEND_GIT_BRANCH", b);
        }
        if let Some(w) = &s.worktree {
            cmd.env("TEND_WORKTREE", w);
        }

        let status = cmd.status();

        // Re-enter the TUI no matter how the child fared.
        *terminal = ratatui::init();
        status
    }
}

/// Discover every `tend-action-*` executable on `PATH`, newest-describe-wins, sorted by
/// label for a stable menu. The first match for a given `<name>` along `PATH` wins, just
/// like ordinary command resolution.
pub fn discover() -> Vec<Action> {
    let Some(path) = std::env::var_os("PATH") else {
        return Vec::new();
    };
    let mut seen = std::collections::HashSet::new();
    let mut actions = Vec::new();
    for dir in std::env::split_paths(&path) {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let fname = entry.file_name();
            let Some(fname) = fname.to_str() else { continue };
            let Some(name) = fname.strip_prefix("tend-action-") else {
                continue;
            };
            // Check executability before reserving the name, so a non-executable entry
            // (a backup file, a directory) can't shadow a runnable tend-action-<name>
            // later on PATH — matches how a shell resolves commands.
            if name.is_empty() || !is_executable(&entry.path()) {
                continue;
            }
            if !seen.insert(name.to_string()) {
                continue;
            }
            actions.push(build_action(name.to_string(), entry.path()));
        }
    }
    actions.sort_by_key(|a| a.label.to_lowercase());
    actions
}

#[cfg(unix)]
fn is_executable(path: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    std::fs::metadata(path)
        .map(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
        .unwrap_or(false)
}

#[cfg(not(unix))]
fn is_executable(path: &Path) -> bool {
    path.is_file()
}

fn build_action(name: String, exe: PathBuf) -> Action {
    let d = describe(&exe);
    let label = d.name.filter(|s| !s.is_empty()).unwrap_or_else(|| name.clone());
    let key = d.key.and_then(|k| k.chars().next());
    Action {
        name,
        exe,
        label,
        key,
        when: d.when,
    }
}

/// Run `<exe> --tend-describe` and parse its metadata, bounded by `DESCRIBE_TIMEOUT`.
/// Any failure (spawn error, timeout, garbled output) yields defaults — a bad extension
/// is still listed under its `<name>`, never crashes or hangs tend.
fn describe(exe: &Path) -> Describe {
    let mut child = match Command::new(exe)
        .arg("--tend-describe")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
    {
        Ok(c) => c,
        Err(_) => return Describe::default(),
    };

    let deadline = Instant::now() + DESCRIBE_TIMEOUT;
    loop {
        match child.try_wait() {
            Ok(Some(_)) => break,
            Ok(None) if Instant::now() >= deadline => {
                let _ = child.kill();
                let _ = child.wait();
                return Describe::default();
            }
            Ok(None) => std::thread::sleep(Duration::from_millis(10)),
            Err(_) => return Describe::default(),
        }
    }

    match child.wait_with_output() {
        Ok(out) => parse_describe(&String::from_utf8_lossy(&out.stdout)),
        Err(_) => Describe::default(),
    }
}

/// Parse the first non-empty line of describe output as JSON; defaults on any failure.
fn parse_describe(stdout: &str) -> Describe {
    stdout
        .lines()
        .map(str::trim)
        .find(|l| !l.is_empty())
        .and_then(|l| serde_json::from_str(l).ok())
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{Session, Source, State};
    use std::path::PathBuf;

    fn session(source: Source, state: State, branch: Option<&str>) -> Session {
        Session {
            session_id: "sid".into(),
            transcript_path: None,
            source,
            state,
            name: "demo".into(),
            cwd: "/tmp/demo".into(),
            waiting_for: None,
            total_tokens: 0,
            context_tokens: 0,
            tool_calls: 0,
            web_requests: 0,
            age_ms: 0,
            summary: String::new(),
            integrations: vec![],
            origin: None,
            model: None,
            git_branch: branch.map(str::to_string),
            worktree: None,
            pr_number: None,
            pr_url: None,
            active_span_ms: None,
            cpu_pct: None,
            tint: None,
            note: None,
        }
    }

    fn action(when: Option<When>) -> Action {
        Action {
            name: "ship".into(),
            exe: PathBuf::from("/bin/true"),
            label: "Ship".into(),
            key: Some('S'),
            when,
        }
    }

    #[test]
    fn no_when_means_always_applicable() {
        let a = action(None);
        assert!(a.applicable(&session(Source::Terminal, State::Done, Some("feat"))));
        assert!(a.applicable(&session(Source::Sdk, State::Idle, None)));
    }

    #[test]
    fn when_filters_by_source_branch_and_state() {
        let a = action(Some(When {
            source: Some("terminal".into()),
            has_branch: Some(true),
            state: Some("done".into()),
        }));
        // All three match.
        assert!(a.applicable(&session(Source::Terminal, State::Done, Some("feat"))));
        // Wrong source.
        assert!(!a.applicable(&session(Source::Sdk, State::Done, Some("feat"))));
        // Missing branch.
        assert!(!a.applicable(&session(Source::Terminal, State::Done, None)));
        // Wrong state.
        assert!(!a.applicable(&session(Source::Terminal, State::Idle, Some("feat"))));
    }

    #[test]
    fn has_branch_false_requires_no_branch() {
        let a = action(Some(When {
            source: None,
            has_branch: Some(false),
            state: None,
        }));
        assert!(a.applicable(&session(Source::Terminal, State::Idle, None)));
        assert!(!a.applicable(&session(Source::Terminal, State::Idle, Some("feat"))));
    }

    #[test]
    fn parse_describe_reads_first_json_line() {
        let d = parse_describe("{\"name\":\"Ship\",\"key\":\"S\",\"when\":{\"has_branch\":true}}\n");
        assert_eq!(d.name.as_deref(), Some("Ship"));
        assert_eq!(d.key.as_deref(), Some("S"));
        assert_eq!(d.when.and_then(|w| w.has_branch), Some(true));
    }

    #[test]
    fn parse_describe_tolerates_garbage_and_blank_lines() {
        assert!(parse_describe("").name.is_none());
        assert!(parse_describe("not json at all").name.is_none());
        // Leading blank lines are skipped; first real line is parsed.
        assert_eq!(parse_describe("\n\n{\"name\":\"X\"}").name.as_deref(), Some("X"));
    }

    // Exercises the real spawn + --tend-describe handshake against an on-disk stub,
    // without touching the global PATH (so it can't race other tests).
    #[cfg(unix)]
    #[test]
    fn describe_spawns_executable_and_reads_metadata() {
        use std::io::Write;
        use std::os::unix::fs::PermissionsExt;

        let dir = std::env::temp_dir().join(format!("tend-actions-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let exe = dir.join("tend-action-demo");
        let mut f = std::fs::File::create(&exe).unwrap();
        writeln!(
            f,
            "#!/bin/sh\necho '{{\"name\":\"Demo\",\"key\":\"D\",\"when\":{{\"has_branch\":true}}}}'"
        )
        .unwrap();
        std::fs::set_permissions(&exe, std::fs::Permissions::from_mode(0o755)).unwrap();

        assert!(is_executable(&exe));
        let a = build_action("demo".into(), exe);
        assert_eq!(a.label, "Demo");
        assert_eq!(a.key, Some('D'));
        // The `when` filter from describe is honored.
        assert!(a.applicable(&session(Source::Terminal, State::Done, Some("feat"))));
        assert!(!a.applicable(&session(Source::Terminal, State::Done, None)));

        let _ = std::fs::remove_dir_all(&dir);
    }
}
