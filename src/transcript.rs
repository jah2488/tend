use serde_json::Value;
use std::path::Path;

/// Everything we deterministically extract from a session transcript.
#[derive(Default, Debug, Clone)]
pub struct Analysis {
    pub total_tokens: u64,
    pub context_tokens: u64,
    /// Total tool_use calls across the transcript.
    pub tool_calls: u64,
    /// WebFetch + WebSearch calls.
    pub web_requests: u64,
    pub integrations: Vec<String>,
    pub errored: bool,
    /// First real user message — used by the stub summarizer until AI is wired in.
    pub first_user_text: Option<String>,
    /// Most recent assistant text — a better "current state" hint for the stub.
    pub last_assistant_text: Option<String>,
    /// Most recent user prompt — the best "what is it doing now" signal.
    pub last_prompt: Option<String>,
    /// Model the session is running, e.g. "claude-opus-4-7".
    pub model: Option<String>,
    /// Git branch the session is working on (skips detached "HEAD").
    pub git_branch: Option<String>,
    /// Latest working directory recorded in the transcript. Tracks `cd`s the session
    /// makes (e.g. into a worktree), unlike the launch-time cwd in the session file.
    pub cwd: Option<String>,
    /// Most recent PR opened during the session, if any.
    pub pr_number: Option<u64>,
    pub pr_url: Option<String>,
    /// Wall-clock span from first to last transcript activity.
    pub active_span_ms: Option<i64>,
}

fn sum_usage(usage: &Value) -> u64 {
    let g = |k: &str| usage.get(k).and_then(Value::as_u64).unwrap_or(0);
    g("input_tokens") + g("output_tokens") + g("cache_creation_input_tokens") + g("cache_read_input_tokens")
}

fn context_size(usage: &Value) -> u64 {
    let g = |k: &str| usage.get(k).and_then(Value::as_u64).unwrap_or(0);
    g("input_tokens") + g("cache_read_input_tokens") + g("cache_creation_input_tokens")
}

/// Pull "Notion" out of "mcp__claude_ai_Notion__notion-fetch" (or the server name for local MCP).
fn integration_from_tool(name: &str) -> Option<String> {
    let rest = name.strip_prefix("mcp__")?;
    let server = rest.split("__").next().unwrap_or(rest);
    let server = server.strip_prefix("claude_ai_").unwrap_or(server);
    if server.is_empty() {
        return None;
    }
    Some(server.replace('_', " "))
}

/// Extract readable text from a message `content` field (string or block array).
fn text_of_content(content: &Value) -> Option<String> {
    if let Some(s) = content.as_str() {
        let s = s.trim();
        return (!s.is_empty()).then(|| s.to_string());
    }
    if let Some(arr) = content.as_array() {
        for block in arr {
            if block.get("type").and_then(Value::as_str) == Some("text") {
                if let Some(t) = block.get("text").and_then(Value::as_str) {
                    let t = t.trim();
                    if !t.is_empty() {
                        return Some(t.to_string());
                    }
                }
            }
        }
    }
    None
}

/// Harness-injected wrappers that show up as "user" messages but aren't real prompts —
/// task notifications, system reminders, slash-command scaffolding, hook output, etc.
const SYNTHETIC_TAGS: &[&str] = &[
    "<task-notification",
    "<system-reminder",
    "<command-name",
    "<command-message",
    "<command-args",
    "<local-command-stdout",
    "<local-command-stderr",
    "<bash-input",
    "<bash-stdout",
    "<bash-stderr",
    "<user-prompt-submit-hook",
    "<tool-use-id",
    "<task-id",
];

/// Strip harness markup from a user message. Returns None if the message is purely
/// synthetic (e.g. a task notification); otherwise the human text before any such tag.
fn clean_prompt(text: &str) -> Option<String> {
    let t = text.trim();
    if SYNTHETIC_TAGS.iter().any(|tag| t.starts_with(tag)) {
        return None;
    }
    // A real prompt can have a reminder/notification block appended — cut it off.
    let end = SYNTHETIC_TAGS
        .iter()
        .filter_map(|tag| t.find(tag))
        .min()
        .unwrap_or(t.len());
    let cleaned = t[..end].trim();
    (!cleaned.is_empty()).then(|| cleaned.to_string())
}

/// Skip meta/command/tool-result user lines that aren't real prompts.
fn is_real_user_prompt(line: &Value, msg: &Value) -> bool {
    if line.get("isMeta").and_then(Value::as_bool) == Some(true) {
        return false;
    }
    if let Some(arr) = msg.get("content").and_then(Value::as_array) {
        // A user turn that is purely a tool_result is not a prompt.
        let only_tool_results = arr
            .iter()
            .all(|b| b.get("type").and_then(Value::as_str) == Some("tool_result"));
        if !arr.is_empty() && only_tool_results {
            return false;
        }
    }
    true
}

pub fn analyze(path: &Path) -> Analysis {
    match std::fs::read_to_string(path) {
        Ok(text) => analyze_str(&text),
        Err(_) => Analysis::default(),
    }
}

fn analyze_str(text: &str) -> Analysis {
    let mut a = Analysis::default();
    let mut seen = std::collections::BTreeSet::new();
    let mut last_line_errored = false;
    let mut first_ms: Option<i64> = None;
    let mut last_ms: Option<i64> = None;

    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let Ok(v) = serde_json::from_str::<Value>(line) else { continue };
        let kind = v.get("type").and_then(Value::as_str).unwrap_or("");

        last_line_errored = v.get("isApiErrorMessage").and_then(Value::as_bool) == Some(true)
            || kind == "error";

        // Activity span, from the first to the last timestamped record.
        if let Some(ms) = v
            .get("timestamp")
            .and_then(Value::as_str)
            .and_then(|t| chrono::DateTime::parse_from_rfc3339(t).ok())
            .map(|d| d.timestamp_millis())
        {
            first_ms.get_or_insert(ms);
            last_ms = Some(ms);
        }

        // Branch follows the session even across cwd changes; ignore detached HEAD.
        if let Some(b) = v.get("gitBranch").and_then(Value::as_str) {
            if !b.is_empty() && b != "HEAD" {
                a.git_branch = Some(b.to_string());
            }
        }

        // Track the live cwd: the session file only records the launch dir, but a
        // session can `cd` into a worktree, which we want to reflect (last wins).
        if let Some(c) = v.get("cwd").and_then(Value::as_str) {
            if !c.is_empty() {
                a.cwd = Some(c.to_string());
            }
        }

        // A PR opened during the session.
        if let Some(url) = v.get("prUrl").and_then(Value::as_str) {
            a.pr_url = Some(url.to_string());
            a.pr_number = v.get("prNumber").and_then(Value::as_u64);
        }

        let msg = v.get("message").unwrap_or(&Value::Null);

        if kind == "assistant" {
            if let Some(m) = msg.get("model").and_then(Value::as_str) {
                a.model = Some(m.to_string());
            }
            if let Some(usage) = msg.get("usage") {
                a.total_tokens += sum_usage(usage);
                a.context_tokens = context_size(usage); // last wins ≈ current context
            }
            if let Some(arr) = msg.get("content").and_then(Value::as_array) {
                for block in arr {
                    if block.get("type").and_then(Value::as_str) == Some("tool_use") {
                        a.tool_calls += 1;
                        if let Some(name) = block.get("name").and_then(Value::as_str) {
                            if name == "WebFetch" || name == "WebSearch" {
                                a.web_requests += 1;
                            }
                            if let Some(integ) = integration_from_tool(name) {
                                seen.insert(integ);
                            }
                        }
                    }
                }
            }
            if let Some(t) = text_of_content(msg.get("content").unwrap_or(&Value::Null)) {
                a.last_assistant_text = Some(t);
            }
        } else if kind == "user" && is_real_user_prompt(&v, msg) {
            if let Some(t) = text_of_content(msg.get("content").unwrap_or(&Value::Null))
                .as_deref()
                .and_then(clean_prompt)
            {
                a.first_user_text.get_or_insert_with(|| t.clone());
                a.last_prompt = Some(t); // most recent real prompt wins
            }
        }
    }

    a.errored = last_line_errored;
    a.integrations = seen.into_iter().collect();
    a.active_span_ms = match (first_ms, last_ms) {
        (Some(f), Some(l)) if l > f => Some(l - f),
        _ => None,
    };
    a
}

/// Kind of a moment on the session timeline — drives the UI's glyph and color.
/// Prompt/Pr/Error are milestones; the rest are individual tool calls, bucketed
/// so reads, edits, shell, and web activity each read distinctly.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EventKind {
    Prompt,
    Pr,
    Error,
    Read,
    Edit,
    Bash,
    Web,
    /// Any other tool (Grep, Glob, Task, TodoWrite, MCP, …).
    Tool,
}

/// Bucket a tool name into the timeline category that drives its glyph/color.
fn tool_kind(name: &str) -> EventKind {
    match name {
        "Read" => EventKind::Read,
        "Edit" | "MultiEdit" | "Write" | "NotebookEdit" => EventKind::Edit,
        "Bash" | "BashOutput" | "KillShell" => EventKind::Bash,
        "WebSearch" | "WebFetch" => EventKind::Web,
        _ => EventKind::Tool,
    }
}

/// Last path segment of a file path, for compact timeline display.
fn base_name(path: &str) -> &str {
    path.rsplit('/').next().filter(|s| !s.is_empty()).unwrap_or(path)
}

/// `ToolName  <most useful argument>` — the file, command, query, url, or pattern,
/// so the timeline shows what each call actually touched, not just the tool name.
fn tool_summary(name: &str, input: &Value) -> String {
    let disp = display_tool_name(name);
    let g = |k: &str| input.get(k).and_then(Value::as_str);
    let detail = match name {
        "Read" | "Edit" | "MultiEdit" | "Write" | "NotebookEdit" => g("file_path").map(base_name),
        "Bash" => g("description").or_else(|| g("command")),
        "WebSearch" => g("query"),
        "WebFetch" => g("url"),
        "Grep" | "Glob" => g("pattern"),
        "Task" => g("description"),
        _ => None,
    };
    match detail {
        Some(d) if !d.is_empty() => format!("{disp}  {d}"),
        _ => disp,
    }
}

/// Cap on timeline events kept for very long sessions, newest retained.
const TIMELINE_CAP: usize = 1500;

/// One notable moment, timed relative to the session's first record.
#[derive(Debug, Clone, PartialEq)]
pub struct Event {
    /// ms since the session's first timestamped record.
    pub at_ms: i64,
    pub kind: EventKind,
    /// Short human text; the UI truncates to width.
    pub text: String,
}

/// A deeper, on-demand read of a transcript for the detail view. Unlike `Analysis`
/// (recomputed every 2s refresh for every session), this keeps the per-event detail
/// `analyze` discards, and is built only when the user opens the digest for one session.
#[derive(Default, Debug, Clone)]
pub struct Digest {
    pub total_tokens: u64,
    pub context_tokens: u64,
    pub active_span_ms: Option<i64>,
    /// Tool-use counts by display name, highest first.
    pub tool_counts: Vec<(String, u64)>,
    /// MCP integration counts by server name, highest first.
    pub integration_counts: Vec<(String, u64)>,
    pub web_requests: u64,
    /// Files passed to a writing tool (Edit/MultiEdit/Write/NotebookEdit), first-seen order.
    pub files_edited: Vec<String>,
    /// Distinct files passed to Read, first-seen order.
    pub files_read: Vec<String>,
    pub pr_number: Option<u64>,
    pub pr_url: Option<String>,
    pub error_count: u64,
    pub first_prompt: Option<String>,
    pub last_prompt: Option<String>,
    /// Moments in chronological order (prompts, tool calls, PRs, errors).
    pub timeline: Vec<Event>,
    /// Count of earliest events dropped when `timeline` hit its cap (0 if none).
    pub timeline_dropped: usize,
}

/// Friendly tool label. Built-ins pass through; an MCP tool
/// `mcp__claude_ai_Notion__notion-fetch` collapses to `Notion·notion-fetch`.
fn display_tool_name(name: &str) -> String {
    match name.strip_prefix("mcp__") {
        Some(rest) => {
            let mut it = rest.splitn(2, "__");
            let server = it.next().unwrap_or("");
            let server = server.strip_prefix("claude_ai_").unwrap_or(server).replace('_', " ");
            match it.next() {
                Some(method) if !method.is_empty() => format!("{server}\u{00B7}{method}"),
                _ => server,
            }
        }
        None => name.to_string(),
    }
}

pub fn digest(path: &Path) -> Digest {
    match std::fs::read_to_string(path) {
        Ok(text) => digest_str(&text),
        Err(_) => Digest::default(),
    }
}

fn digest_str(text: &str) -> Digest {
    use std::collections::{HashMap, HashSet};
    let mut d = Digest::default();
    let mut tools: HashMap<String, u64> = HashMap::new();
    let mut integ: HashMap<String, u64> = HashMap::new();
    let mut read_seen: HashSet<String> = HashSet::new();
    let mut edit_seen: HashSet<String> = HashSet::new();
    let mut first_ms: Option<i64> = None;
    let mut last_ms: Option<i64> = None;
    let mut cur_ms: i64 = 0;
    let mut prev_errored = false;

    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let Ok(v) = serde_json::from_str::<Value>(line) else { continue };
        let kind = v.get("type").and_then(Value::as_str).unwrap_or("");

        if let Some(ms) = v
            .get("timestamp")
            .and_then(Value::as_str)
            .and_then(|t| chrono::DateTime::parse_from_rfc3339(t).ok())
            .map(|x| x.timestamp_millis())
        {
            first_ms.get_or_insert(ms);
            last_ms = Some(ms);
            cur_ms = ms;
        }
        let rel = first_ms.map_or(0, |f| (cur_ms - f).max(0));

        // Errors: count and timeline-mark only the rising edge, so a run of errored
        // lines is one event, not many.
        let errored = v.get("isApiErrorMessage").and_then(Value::as_bool) == Some(true)
            || kind == "error";
        if errored && !prev_errored {
            d.error_count += 1;
            d.timeline.push(Event { at_ms: rel, kind: EventKind::Error, text: "error".into() });
        }
        prev_errored = errored;

        // A PR opened during the session — mark only when the url changes.
        if let Some(url) = v.get("prUrl").and_then(Value::as_str) {
            if d.pr_url.as_deref() != Some(url) {
                let num = v.get("prNumber").and_then(Value::as_u64);
                let label = num.map_or_else(|| "opened PR".to_string(), |n| format!("opened PR #{n}"));
                d.timeline.push(Event { at_ms: rel, kind: EventKind::Pr, text: label });
                d.pr_url = Some(url.to_string());
                d.pr_number = num;
            }
        }

        let msg = v.get("message").unwrap_or(&Value::Null);

        if kind == "assistant" {
            if let Some(usage) = msg.get("usage") {
                d.total_tokens += sum_usage(usage);
                d.context_tokens = context_size(usage);
            }
            if let Some(arr) = msg.get("content").and_then(Value::as_array) {
                for block in arr {
                    if block.get("type").and_then(Value::as_str) != Some("tool_use") {
                        continue;
                    }
                    let Some(name) = block.get("name").and_then(Value::as_str) else { continue };
                    *tools.entry(display_tool_name(name)).or_default() += 1;
                    let input = block.get("input").unwrap_or(&Value::Null);
                    d.timeline.push(Event {
                        at_ms: rel,
                        kind: tool_kind(name),
                        text: tool_summary(name, input),
                    });
                    if name == "WebFetch" || name == "WebSearch" {
                        d.web_requests += 1;
                    }
                    if let Some(i) = integration_from_tool(name) {
                        *integ.entry(i).or_default() += 1;
                    }
                    // File touches, by op. Read is "looked at"; the writers are "changed".
                    let fp = input.get("file_path").and_then(Value::as_str);
                    if let Some(fp) = fp {
                        if name == "Read" {
                            if read_seen.insert(fp.to_string()) {
                                d.files_read.push(fp.to_string());
                            }
                        } else if matches!(name, "Edit" | "MultiEdit" | "Write" | "NotebookEdit")
                            && edit_seen.insert(fp.to_string())
                        {
                            d.files_edited.push(fp.to_string());
                        }
                    }
                }
            }
        } else if kind == "user" && is_real_user_prompt(&v, msg) {
            if let Some(t) = text_of_content(msg.get("content").unwrap_or(&Value::Null))
                .as_deref()
                .and_then(clean_prompt)
            {
                d.first_prompt.get_or_insert_with(|| t.clone());
                d.last_prompt = Some(t.clone());
                d.timeline.push(Event { at_ms: rel, kind: EventKind::Prompt, text: t });
            }
        }
    }

    d.active_span_ms = match (first_ms, last_ms) {
        (Some(f), Some(l)) if l > f => Some(l - f),
        _ => None,
    };
    // Highest count first; ties broken by name for stable, deterministic ordering.
    let by_count = |m: HashMap<String, u64>| -> Vec<(String, u64)> {
        let mut v: Vec<(String, u64)> = m.into_iter().collect();
        v.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
        v
    };
    d.tool_counts = by_count(tools);
    d.integration_counts = by_count(integ);
    // Keep the most recent events on a very long session; the UI notes the elision.
    if d.timeline.len() > TIMELINE_CAP {
        d.timeline_dropped = d.timeline.len() - TIMELINE_CAP;
        d.timeline.drain(0..d.timeline_dropped);
    }
    d
}

#[cfg(test)]
mod tests {
    use super::{analyze_str, clean_prompt, digest_str, EventKind};

    #[test]
    fn counts_tool_calls_and_web_requests() {
        let jsonl = [
            r#"{"type":"assistant","message":{"content":[{"type":"tool_use","name":"Bash"},{"type":"tool_use","name":"WebFetch"}]}}"#,
            r#"{"type":"assistant","message":{"content":[{"type":"tool_use","name":"WebSearch"},{"type":"tool_use","name":"mcp__claude_ai_Notion__notion-fetch"}]}}"#,
        ]
        .join("\n");
        let a = analyze_str(&jsonl);
        assert_eq!(a.tool_calls, 4);
        assert_eq!(a.web_requests, 2);
        assert_eq!(a.integrations, vec!["Notion".to_string()]);
    }

    #[test]
    fn tracks_latest_cwd_across_lines() {
        let jsonl = [
            r#"{"type":"user","cwd":"/repo/claims","gitBranch":"main","message":{"content":"hi"}}"#,
            r#"{"type":"user","cwd":"/repo/.worktrees/CO-5100","gitBranch":"CO-5100","message":{"content":"go"}}"#,
        ]
        .join("\n");
        let a = analyze_str(&jsonl);
        assert_eq!(a.cwd.as_deref(), Some("/repo/.worktrees/CO-5100"));
        assert_eq!(a.git_branch.as_deref(), Some("CO-5100"));
    }

    #[test]
    fn drops_pure_synthetic_messages() {
        assert_eq!(clean_prompt("<task-notification> <task-id>abc</task-id> done"), None);
        assert_eq!(clean_prompt("  <system-reminder>be nice</system-reminder>"), None);
    }

    #[test]
    fn keeps_real_prompt_and_trims_appended_markup() {
        assert_eq!(clean_prompt("Fix the bug"), Some("Fix the bug".into()));
        assert_eq!(
            clean_prompt("Fix the bug\n<system-reminder>context</system-reminder>"),
            Some("Fix the bug".into())
        );
    }

    #[test]
    fn digest_collects_tools_files_prs_and_timeline() {
        let jsonl = [
            r#"{"type":"user","timestamp":"2026-06-03T10:00:00Z","message":{"content":"Build the thing"}}"#,
            r#"{"type":"assistant","timestamp":"2026-06-03T10:00:05Z","message":{"usage":{"input_tokens":10,"output_tokens":5},"content":[{"type":"tool_use","name":"Read","input":{"file_path":"a.rs"}},{"type":"tool_use","name":"Edit","input":{"file_path":"a.rs"}}]}}"#,
            r#"{"type":"assistant","timestamp":"2026-06-03T10:00:10Z","prUrl":"http://x/pull/1","prNumber":1,"message":{"content":[{"type":"tool_use","name":"Read","input":{"file_path":"b.rs"}},{"type":"tool_use","name":"mcp__claude_ai_Notion__notion-fetch"}]}}"#,
        ]
        .join("\n");
        let d = digest_str(&jsonl);

        // Tool histogram: Read counted twice, sorted highest-first.
        assert_eq!(d.tool_counts[0], ("Read".to_string(), 2));
        assert!(d.tool_counts.contains(&("Edit".to_string(), 1)));
        assert!(d.tool_counts.contains(&("Notion\u{00B7}notion-fetch".to_string(), 1)));

        // Files split by op; reads deduped in first-seen order.
        assert_eq!(d.files_edited, vec!["a.rs".to_string()]);
        assert_eq!(d.files_read, vec!["a.rs".to_string(), "b.rs".to_string()]);

        // Integration count from the MCP tool.
        assert_eq!(d.integration_counts, vec![("Notion".to_string(), 1)]);

        assert_eq!(d.pr_number, Some(1));
        assert_eq!(d.first_prompt.as_deref(), Some("Build the thing"));

        // Timeline interleaves the prompt, each tool call, and the PR — in order.
        let kinds: Vec<EventKind> = d.timeline.iter().map(|e| e.kind).collect();
        assert_eq!(
            kinds,
            vec![
                EventKind::Prompt, // t0
                EventKind::Read,   // Read a.rs   @5s
                EventKind::Edit,   // Edit a.rs   @5s
                EventKind::Pr,     // opened PR   @10s
                EventKind::Read,   // Read b.rs   @10s
                EventKind::Tool,   // Notion fetch @10s
            ]
        );
        assert_eq!(d.timeline[0].at_ms, 0);
        assert_eq!(d.timeline[3].at_ms, 10_000);
        // Tool events carry their target, not just the tool name.
        assert_eq!(d.timeline[1].text, "Read  a.rs");
        assert!(d.timeline[5].text.starts_with("Notion\u{00B7}notion-fetch"));
    }

    #[test]
    fn digest_counts_errors_on_rising_edge_only() {
        let jsonl = [
            r#"{"type":"assistant","timestamp":"2026-06-03T10:00:00Z","message":{"content":[]}}"#,
            r#"{"type":"error","timestamp":"2026-06-03T10:00:01Z","isApiErrorMessage":true}"#,
            r#"{"type":"error","timestamp":"2026-06-03T10:00:02Z","isApiErrorMessage":true}"#,
            r#"{"type":"assistant","timestamp":"2026-06-03T10:00:03Z","message":{"content":[]}}"#,
            r#"{"type":"error","timestamp":"2026-06-03T10:00:04Z","isApiErrorMessage":true}"#,
        ]
        .join("\n");
        let d = digest_str(&jsonl);
        // Two error *runs*, not three error lines.
        assert_eq!(d.error_count, 2);
        assert_eq!(d.timeline.iter().filter(|e| e.kind == EventKind::Error).count(), 2);
    }
}
