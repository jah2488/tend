//! Shared on-disk mechanics for the `tend-color` / `tend-note` file
//! conventions: an id-validating, atomic (tmp + rename) writer and remover.
//!
//! Both conventions key files by session id, so the safety check and the
//! crash-safe write live here once instead of in each module.

use std::path::Path;

/// Atomically write `body` to `<dir>/<session_id>` via a `.tmp` + rename, so a
/// crash mid-write leaves the previous contents (and `gc` sweeps the orphan
/// `.tmp`). Creates `dir` if needed. Rejects an unsafe id.
pub(crate) fn write_at(dir: &Path, session_id: &str, body: &str) -> std::io::Result<()> {
    check(session_id)?;
    std::fs::create_dir_all(dir)?;
    let tmp = dir.join(format!("{session_id}.tmp"));
    let final_path = dir.join(session_id);
    std::fs::write(&tmp, body)?;
    std::fs::rename(&tmp, &final_path)
}

/// Remove `<dir>/<session_id>`. A missing file is not an error. Rejects an
/// unsafe id.
pub(crate) fn remove_at(dir: &Path, session_id: &str) -> std::io::Result<()> {
    check(session_id)?;
    match std::fs::remove_file(dir.join(session_id)) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(e),
    }
}

/// A session id is used directly as a filename under `~/.claude/tend-*/`.
/// Reject anything that could escape that directory: empty, path separators,
/// NUL, the `.`/`..` traversal entries, or an unreasonable length. Producer-side
/// guard — readers never see attacker-controlled ids.
fn check(session_id: &str) -> std::io::Result<()> {
    if !id_is_safe(session_id) {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "invalid session id",
        ));
    }
    Ok(())
}

fn id_is_safe(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 256
        && !id.contains('/')
        && !id.contains('\\')
        && !id.contains('\0')
        && id != "."
        && id != ".."
}
