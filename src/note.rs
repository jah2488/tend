//! User-chosen one-line note for a session — a free-form annotation distinct
//! from the auto-derived `summary`. Written by `tend mcp` (or any writer of
//! the `~/.claude/tend-note/<id>` file); the TUI just reads.
//!
//! On-disk convention: `~/.claude/tend-note/<session-id>` is a plain text
//! file containing a single short line. Missing file or blank contents → no
//! note. Mirrors the `tend-color` tint convention so the two stay symmetric.

use std::collections::HashSet;
use std::path::PathBuf;

/// Maximum note length tend will store or render; keeps the card to one line.
pub const MAX_LEN: usize = 120;

/// Path to the note directory: `~/.claude/tend-note/`. Not created by the
/// consumer side — "missing dir = nobody's using notes, nothing to do."
pub fn dir() -> Option<PathBuf> {
    let home = std::env::var("HOME").ok()?;
    Some(PathBuf::from(home).join(".claude").join("tend-note"))
}

/// Read the note for one session. Returns `None` when the directory doesn't
/// exist, the file doesn't exist, or the contents are blank.
pub fn read_for(session_id: &str) -> Option<String> {
    let path = dir()?.join(session_id);
    let text = std::fs::read_to_string(&path).ok()?;
    let text = text.trim();
    (!text.is_empty()).then(|| text.to_string())
}

/// Delete any note files whose names aren't in `current_ids`. Best-effort,
/// mirroring `tint::gc` — also sweeps orphan `<id>.tmp` tempfiles left by a
/// writer that crashed mid-rename, on the same liveness criterion.
pub fn gc(current_ids: &HashSet<String>) {
    let Some(d) = dir() else { return };
    let Ok(entries) = std::fs::read_dir(&d) else { return };
    for entry in entries.flatten() {
        let path = entry.path();
        let Some(name) = path.file_name().and_then(|n| n.to_str()) else { continue };

        if let Some(stem) = name.strip_suffix(".tmp") {
            if !current_ids.contains(stem) {
                let _ = std::fs::remove_file(&path);
            }
            continue;
        }

        if !current_ids.contains(name) {
            let _ = std::fs::remove_file(&path);
        }
    }
}

/// Write the note for a session, atomically. Newlines become spaces and the
/// text is trimmed and capped to `MAX_LEN` so the file is one short line; an
/// empty note clears it instead. Producer side, used by `tend mcp`.
pub fn write_for(session_id: &str, text: &str) -> std::io::Result<()> {
    let one_line = text.replace(['\n', '\r'], " ");
    let one_line = one_line.trim();
    if one_line.is_empty() {
        return clear_for(session_id);
    }
    let capped: String = one_line.chars().take(MAX_LEN).collect();
    let dir = dir().ok_or_else(|| std::io::Error::new(std::io::ErrorKind::NotFound, "HOME unset"))?;
    crate::store::write_at(&dir, session_id, &capped)
}

/// Remove the note for a session. A missing file is not an error.
pub fn clear_for(session_id: &str) -> std::io::Result<()> {
    match dir() {
        Some(dir) => crate::store::remove_at(&dir, session_id),
        None => Ok(()),
    }
}
