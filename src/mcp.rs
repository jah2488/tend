//! `tend mcp` — a stdio MCP server exposing the session model to MCP clients
//! (e.g. another Claude Code session) for both query and annotation.
//!
//! Speaks JSON-RPC 2.0 over stdio, one message per line: `initialize`,
//! `notifications/initialized`, `tools/list`, `tools/call`. No async runtime —
//! a tight read-line / dispatch / write-line loop, like the other non-TTY text
//! modes (`--list`, `--digest`). Reads the same `~/.claude/` data via
//! `discovery` and `transcript`; writes the same `tend-color` / `tend-note`
//! file conventions the tint/note modules own.

use crate::discovery;
use crate::model::{Session, State};
use crate::note;
use crate::summarize::StubSummarizer;
use crate::tint;
use crate::transcript::{self, Digest};
use anyhow::Result;
use serde_json::{json, Value};
use std::io::{BufRead, Write};

/// MCP protocol version this server speaks — the widely-deployed 2024-11-05.
const PROTOCOL_VERSION: &str = "2024-11-05";

/// Run the stdio JSON-RPC loop until stdin closes.
pub fn serve() -> Result<()> {
    let stdin = std::io::stdin();
    let stdout = std::io::stdout();
    let mut out = stdout.lock();

    for line in stdin.lock().lines() {
        let line = match line {
            Ok(l) => l,
            Err(_) => break,
        };
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        let msg: Value = match serde_json::from_str(trimmed) {
            Ok(v) => v,
            Err(_) => continue, // not a JSON-RPC message we understand; skip
        };
        // A message with no `id` is a notification: handled, never answered.
        let id = msg.get("id").cloned();
        let Some(method) = msg.get("method").and_then(Value::as_str) else {
            continue;
        };
        match handle(method, msg.get("params")) {
            Ok(Some(result)) => {
                if let Some(id) = id {
                    let _ = writeln!(out, "{}", json!({ "jsonrpc": "2.0", "id": id, "result": result }));
                    let _ = out.flush();
                }
            }
            Ok(None) => {} // a notification — nothing to write
            Err(err) => {
                if let Some(id) = id {
                    let _ = writeln!(out, "{}", json!({ "jsonrpc": "2.0", "id": id, "error": err }));
                    let _ = out.flush();
                }
            }
        }
    }
    Ok(())
}

/// Dispatch a method to a result (Some → respond, None → notification), or an
/// MCP error object on a protocol-level failure.
fn handle(method: &str, params: Option<&Value>) -> Result<Option<Value>, Value> {
    match method {
        "initialize" => Ok(Some(json!({
            "protocolVersion": PROTOCOL_VERSION,
            "capabilities": { "tools": {} },
            "serverInfo": { "name": "tend", "version": env!("CARGO_PKG_VERSION") },
        }))),
        "notifications/initialized" => Ok(None),
        "tools/list" => Ok(Some(json!({ "tools": tool_list() }))),
        "tools/call" => call_tool(params).map(Some),
        other => Err(json!({ "code": -32601, "message": format!("method not found: {other}") })),
    }
}

fn tool_list() -> Vec<Value> {
    vec![
        json!({
            "name": "list_sessions",
            "description": "List every Claude Code session tend sees (terminals, editors, SDKs), newest-first. Each entry: id, name, state, needs_you, source, branch, worktree, cwd, tokens, age_ms, summary, tint, note.",
            "inputSchema": { "type": "object", "properties": {} }
        }),
        json!({
            "name": "needs_attention",
            "description": "List only sessions that need the user now: state needs-you (blocked on a permission/input prompt) or error.",
            "inputSchema": { "type": "object", "properties": {} }
        }),
        json!({
            "name": "get_session",
            "description": "Full detail for one session by id (call list_sessions first to find the id).",
            "inputSchema": { "type": "object", "properties": { "id": { "type": "string" } }, "required": ["id"] }
        }),
        json!({
            "name": "get_digest",
            "description": "A deeper read of one session's transcript: tool histogram, integrations, files changed/read, PR, errors, and the full chronological timeline. By id.",
            "inputSchema": { "type": "object", "properties": { "id": { "type": "string" } }, "required": ["id"] }
        }),
        json!({
            "name": "set_tint",
            "description": "Tag a session with a colored pip in tend's dashboard. color: red, orange, yellow, green, blue, purple, or gray.",
            "inputSchema": { "type": "object",
                "properties": { "id": { "type": "string" }, "color": { "type": "string", "enum": ["red","orange","yellow","green","blue","purple","gray"] } },
                "required": ["id", "color"] }
        }),
        json!({
            "name": "set_note",
            "description": "Attach a short one-line note to a session, shown in tend's dashboard under the summary. Use for 'blocked on review', 'mine', 'do not merge'. An empty note clears it.",
            "inputSchema": { "type": "object", "properties": { "id": { "type": "string" }, "note": { "type": "string" } }, "required": ["id", "note"] }
        }),
        json!({
            "name": "clear_tint",
            "description": "Remove a session's tint pip.",
            "inputSchema": { "type": "object", "properties": { "id": { "type": "string" } }, "required": ["id"] }
        }),
        json!({
            "name": "clear_note",
            "description": "Remove a session's note.",
            "inputSchema": { "type": "object", "properties": { "id": { "type": "string" } }, "required": ["id"] }
        }),
    ]
}

/// Run a tool. A known tool always returns a tool-result (isError flags an
/// app-level failure); an unknown tool or missing params is a protocol error.
fn call_tool(params: Option<&Value>) -> Result<Value, Value> {
    let params = params.ok_or_else(|| rpc_err(-32602, "missing params"))?;
    let name = params
        .get("name")
        .and_then(Value::as_str)
        .ok_or_else(|| rpc_err(-32602, "missing tool name"))?;
    let empty = json!({});
    let args = params.get("arguments").unwrap_or(&empty);

    let outcome: std::result::Result<String, String> = match name {
        "list_sessions" => list_sessions(),
        "needs_attention" => needs_attention(),
        "get_session" => get_session(args),
        "get_digest" => get_digest(args),
        "set_tint" => set_tint(args),
        "set_note" => set_note(args),
        "clear_tint" => clear_tint(args),
        "clear_note" => clear_note(args),
        other => return Err(rpc_err(-32601, format!("unknown tool: {other}"))),
    };
    let (text, is_error) = match outcome {
        Ok(t) => (t, false),
        Err(t) => (t, true),
    };
    Ok(json!({ "content": [{ "type": "text", "text": text }], "isError": is_error }))
}

fn rpc_err<S: Into<String>>(code: i32, message: S) -> Value {
    json!({ "code": code, "message": message.into() })
}

// ── tools ──

fn load() -> Vec<Session> {
    let stub = StubSummarizer;
    discovery::load_sessions(&stub).unwrap_or_default()
}

fn find(id: &str) -> Option<Session> {
    load().into_iter().find(|s| s.session_id == id)
}

fn list_sessions() -> std::result::Result<String, String> {
    let rows: Vec<Value> = load().iter().map(session_brief).collect();
    Ok(pretty(json!({ "sessions": rows, "count": rows.len() })))
}

fn needs_attention() -> std::result::Result<String, String> {
    let rows: Vec<Value> = load()
        .iter()
        .filter(|s| matches!(s.state, State::NeedsYou | State::Error))
        .map(session_brief)
        .collect();
    Ok(pretty(json!({ "sessions": rows, "count": rows.len() })))
}

fn get_session(args: &Value) -> std::result::Result<String, String> {
    let id = arg_str(args, "id")?;
    let s = find(&id).ok_or_else(|| format!("no session with id `{id}`"))?;
    Ok(pretty(session_detail(&s)))
}

fn get_digest(args: &Value) -> std::result::Result<String, String> {
    let id = arg_str(args, "id")?;
    let s = find(&id).ok_or_else(|| format!("no session with id `{id}`"))?;
    let Some(path) = s.transcript_path.as_deref() else {
        return Err(format!("session `{id}` has no transcript on disk yet"));
    };
    Ok(pretty(digest_json(&transcript::digest(path))))
}

fn set_tint(args: &Value) -> std::result::Result<String, String> {
    let id = arg_str(args, "id")?;
    let color = arg_str(args, "color")?;
    tint::write_for(&id, &color).map_err(|e| e.to_string())?;
    Ok(format!("tint for {id} set to {color}"))
}

fn set_note(args: &Value) -> std::result::Result<String, String> {
    let id = arg_str(args, "id")?;
    let note = arg_str(args, "note")?;
    note::write_for(&id, &note).map_err(|e| e.to_string())?;
    Ok(format!("note set for {id}"))
}

fn clear_tint(args: &Value) -> std::result::Result<String, String> {
    let id = arg_str(args, "id")?;
    tint::clear_for(&id).map_err(|e| e.to_string())?;
    Ok(format!("tint cleared for {id}"))
}

fn clear_note(args: &Value) -> std::result::Result<String, String> {
    let id = arg_str(args, "id")?;
    note::clear_for(&id).map_err(|e| e.to_string())?;
    Ok(format!("note cleared for {id}"))
}

fn arg_str(args: &Value, key: &str) -> std::result::Result<String, String> {
    args.get(key)
        .and_then(Value::as_str)
        .map(str::to_string)
        .ok_or_else(|| format!("missing required argument `{key}`"))
}

// ── JSON shaping ──

fn session_brief(s: &Session) -> Value {
    json!({
        "id": s.session_id,
        "name": s.name,
        "state": s.state.slug(),
        "needs_you": s.state == State::NeedsYou,
        "source": s.source.slug(),
        "branch": s.git_branch,
        "worktree": s.worktree,
        "cwd": s.cwd,
        "tokens": s.total_tokens,
        "age_ms": s.age_ms,
        "summary": s.summary,
        "tint": s.tint.map(|t| t.name()),
        "note": s.note,
    })
}

fn session_detail(s: &Session) -> Value {
    let mut v = session_brief(s);
    if let Some(obj) = v.as_object_mut() {
        obj.insert("context_tokens".into(), json!(s.context_tokens));
        obj.insert("tool_calls".into(), json!(s.tool_calls));
        obj.insert("web_requests".into(), json!(s.web_requests));
        obj.insert("integrations".into(), json!(s.integrations));
        obj.insert("waiting_for".into(), json!(s.waiting_for));
        obj.insert("origin".into(), json!(s.origin));
        obj.insert("model".into(), json!(s.model));
        obj.insert("pr_number".into(), json!(s.pr_number));
        obj.insert("pr_url".into(), json!(s.pr_url));
        obj.insert("active_span_ms".into(), json!(s.active_span_ms));
        obj.insert("cpu_pct".into(), json!(s.cpu_pct));
        obj.insert("transcript".into(), json!(s.transcript_path));
    }
    v
}

fn digest_json(d: &Digest) -> Value {
    json!({
        "total_tokens": d.total_tokens,
        "context_tokens": d.context_tokens,
        "active_span_ms": d.active_span_ms,
        "web_requests": d.web_requests,
        "error_count": d.error_count,
        "pr_number": d.pr_number,
        "pr_url": d.pr_url,
        "first_prompt": d.first_prompt,
        "last_prompt": d.last_prompt,
        "tools": d.tool_counts.iter().map(|(n, c)| json!({ "name": n, "count": c })).collect::<Vec<_>>(),
        "integrations": d.integration_counts.iter().map(|(n, c)| json!({ "name": n, "count": c })).collect::<Vec<_>>(),
        "files_changed": d.files_edited,
        "files_read_count": d.files_read.len(),
        "timeline": d.timeline.iter().map(|e| json!({
            "at_ms": e.at_ms,
            "kind": kind_slug(e.kind),
            "text": e.text,
        })).collect::<Vec<_>>(),
        "timeline_dropped": d.timeline_dropped,
    })
}

fn kind_slug(k: transcript::EventKind) -> &'static str {
    use transcript::EventKind::*;
    match k {
        Prompt => "prompt",
        Pr => "pr",
        Error => "error",
        Read => "read",
        Edit => "edit",
        Bash => "bash",
        Web => "web",
        Tool => "tool",
    }
}

fn pretty(v: Value) -> String {
    serde_json::to_string_pretty(&v).unwrap_or_default()
}
