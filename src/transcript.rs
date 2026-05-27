use serde_json::Value;
use std::path::Path;

/// Everything we deterministically extract from a session transcript.
#[derive(Default, Debug, Clone)]
pub struct Analysis {
    pub total_tokens: u64,
    pub context_tokens: u64,
    pub integrations: Vec<String>,
    pub errored: bool,
    /// First real user message — used by the stub summarizer until AI is wired in.
    pub first_user_text: Option<String>,
    /// Most recent assistant text — a better "current state" hint for the stub.
    pub last_assistant_text: Option<String>,
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
    let mut a = Analysis::default();
    let Ok(text) = std::fs::read_to_string(path) else {
        return a;
    };

    let mut seen = std::collections::BTreeSet::new();
    let mut last_line_errored = false;

    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let Ok(v) = serde_json::from_str::<Value>(line) else { continue };
        let kind = v.get("type").and_then(Value::as_str).unwrap_or("");

        last_line_errored = v.get("isApiErrorMessage").and_then(Value::as_bool) == Some(true)
            || kind == "error";

        let msg = v.get("message").unwrap_or(&Value::Null);

        if kind == "assistant" {
            if let Some(usage) = msg.get("usage") {
                a.total_tokens += sum_usage(usage);
                a.context_tokens = context_size(usage); // last wins ≈ current context
            }
            if let Some(arr) = msg.get("content").and_then(Value::as_array) {
                for block in arr {
                    if block.get("type").and_then(Value::as_str) == Some("tool_use") {
                        if let Some(name) = block.get("name").and_then(Value::as_str) {
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
        } else if kind == "user" && a.first_user_text.is_none() && is_real_user_prompt(&v, msg) {
            if let Some(t) = text_of_content(msg.get("content").unwrap_or(&Value::Null)) {
                a.first_user_text = Some(t);
            }
        }
    }

    a.errored = last_line_errored;
    a.integrations = seen.into_iter().collect();
    a
}
