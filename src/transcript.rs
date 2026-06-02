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

#[cfg(test)]
mod tests {
    use super::{analyze_str, clean_prompt};

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
}
