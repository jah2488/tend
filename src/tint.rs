//! User-chosen pip color for a session — distinct from the lifecycle `State`
//! color that drives the left-edge rule. Written by tend extensions (e.g.
//! `tend-color`); tend just reads.
//!
//! On-disk convention: `~/.claude/tend-color/<session-id>` is a plain text
//! file containing one lowercase color name (no extension, no JSON). Missing
//! file or unrecognized name → no pip.
//!
//! This module is the *consumer* side of the convention. The producer side
//! lives in the `tend-color` extension, but anything that writes the same
//! file works.

use ratatui::style::Color;
use std::collections::HashSet;
use std::path::PathBuf;

/// The seven named pip tints. `None` is represented by `Option::None` outside
/// this enum, matching the "missing file = no color" disk convention.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tint {
    Red,
    Orange,
    Yellow,
    Green,
    Blue,
    Purple,
    Gray,
}

impl Tint {
    /// Parse the on-disk lowercase name. Trims and matches case-insensitively
    /// so a hand-edited file with stray whitespace or different casing still
    /// works.
    pub fn parse(s: &str) -> Option<Self> {
        match s.trim().to_ascii_lowercase().as_str() {
            "red" => Some(Tint::Red),
            "orange" => Some(Tint::Orange),
            "yellow" => Some(Tint::Yellow),
            "green" => Some(Tint::Green),
            "blue" => Some(Tint::Blue),
            "purple" => Some(Tint::Purple),
            "gray" | "grey" => Some(Tint::Gray),
            _ => None,
        }
    }

    /// RGB used to draw the pip. Values harmonize with the existing
    /// `State::color()` palette so the pip doesn't clash with the chrome.
    pub fn color(self) -> Color {
        match self {
            Tint::Red => Color::Rgb(0xE0, 0x6C, 0x75),
            Tint::Orange => Color::Rgb(0xE5, 0x96, 0x4B),
            Tint::Yellow => Color::Rgb(0xE5, 0xC0, 0x7B),
            Tint::Green => Color::Rgb(0x98, 0xC3, 0x79),
            Tint::Blue => Color::Rgb(0x61, 0xAF, 0xEF),
            Tint::Purple => Color::Rgb(0xC6, 0x78, 0xDD),
            Tint::Gray => Color::Rgb(0x5C, 0x63, 0x70),
        }
    }

    /// Inverse of `parse`: the lowercase on-disk name. Used by writers.
    pub fn name(self) -> &'static str {
        match self {
            Tint::Red => "red",
            Tint::Orange => "orange",
            Tint::Yellow => "yellow",
            Tint::Green => "green",
            Tint::Blue => "blue",
            Tint::Purple => "purple",
            Tint::Gray => "gray",
        }
    }
}

/// Path to the tint directory: `~/.claude/tend-color/`. Not created here —
/// the consumer side never creates it, since the convention is "missing dir
/// = nobody's using tints, nothing to do."
pub fn dir() -> Option<PathBuf> {
    let home = std::env::var("HOME").ok()?;
    Some(PathBuf::from(home).join(".claude").join("tend-color"))
}

/// Read the tint for one session. Returns `None` when the directory doesn't
/// exist, the file doesn't exist, or the contents don't parse.
pub fn read_for(session_id: &str) -> Option<Tint> {
    let path = dir()?.join(session_id);
    let text = std::fs::read_to_string(&path).ok()?;
    Tint::parse(&text)
}

/// Delete any tint files whose names aren't in `current_ids`. Best-effort:
/// errors on individual files don't abort the sweep, and a missing tint dir
/// is fine (nothing to GC).
///
/// Also sweeps orphan `<id>.tmp` tempfiles left behind by a crashed writer
/// mid-rename, on the same liveness criterion.
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

/// Write the tint for a session, atomically (tmp + rename) so a crash mid-write
/// leaves no partial file (gc sweeps any `.tmp` orphan). Producer side, used by
/// `tend mcp`. `InvalidInput` for an unknown color or unsafe id; `NotFound` if
/// HOME is unset.
pub fn write_for(session_id: &str, color: &str) -> std::io::Result<()> {
    let t = Tint::parse(color)
        .ok_or_else(|| std::io::Error::new(std::io::ErrorKind::InvalidInput, "unknown color"))?;
    let dir = dir().ok_or_else(|| std::io::Error::new(std::io::ErrorKind::NotFound, "HOME unset"))?;
    crate::store::write_at(&dir, session_id, t.name())
}

/// Remove the tint for a session. A missing file is not an error.
pub fn clear_for(session_id: &str) -> std::io::Result<()> {
    match dir() {
        Some(dir) => crate::store::remove_at(&dir, session_id),
        None => Ok(()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    /// HOME is process-global, so any test that mutates it must serialize. Held
    /// across each affected test so parallel `cargo test` runs are safe.
    static HOME_LOCK: Mutex<()> = Mutex::new(());

    /// RAII guard: take the lock, swap HOME to `path`, restore on drop.
    struct ScopedHome {
        _guard: std::sync::MutexGuard<'static, ()>,
        prev: Option<std::ffi::OsString>,
    }
    impl ScopedHome {
        fn to(path: &std::path::Path) -> Self {
            let guard = HOME_LOCK.lock().unwrap_or_else(|e| e.into_inner());
            let prev = std::env::var_os("HOME");
            std::env::set_var("HOME", path);
            ScopedHome { _guard: guard, prev }
        }
        fn unset() -> Self {
            let guard = HOME_LOCK.lock().unwrap_or_else(|e| e.into_inner());
            let prev = std::env::var_os("HOME");
            std::env::remove_var("HOME");
            ScopedHome { _guard: guard, prev }
        }
    }
    impl Drop for ScopedHome {
        fn drop(&mut self) {
            match &self.prev {
                Some(v) => std::env::set_var("HOME", v),
                None => std::env::remove_var("HOME"),
            }
        }
    }

    #[test]
    fn parse_known_names() {
        assert_eq!(Tint::parse("red"), Some(Tint::Red));
        assert_eq!(Tint::parse("purple\n"), Some(Tint::Purple));
        assert_eq!(Tint::parse("  YELLOW  "), Some(Tint::Yellow));
        assert_eq!(Tint::parse("grey"), Some(Tint::Gray)); // British spelling
    }

    #[test]
    fn parse_unknown_yields_none() {
        assert_eq!(Tint::parse("magenta"), None);
        assert_eq!(Tint::parse(""), None);
        assert_eq!(Tint::parse("not a color"), None);
    }

    #[test]
    fn read_for_returns_none_when_dir_missing() {
        let tmp = std::env::temp_dir().join(format!("tend-tint-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(&tmp).unwrap();
        let _h = ScopedHome::to(&tmp);
        assert_eq!(read_for("any-id"), None);
        drop(_h);
        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn gc_with_home_unset_is_noop() {
        let _h = ScopedHome::unset();
        let empty: HashSet<String> = HashSet::new();
        gc(&empty); // must not panic; dir() returns None → early return
    }

    /// The genuine missing-dir branch: HOME is set but ~/.claude/tend-color doesn't
    /// exist, so read_dir errors and gc early-returns without panicking.
    #[test]
    fn gc_with_missing_tint_dir_is_noop() {
        let tmp = std::env::temp_dir().join(format!("tend-tint-nodir-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(&tmp).unwrap();
        let _h = ScopedHome::to(&tmp); // tmp exists, but tmp/.claude/tend-color does not
        let empty: HashSet<String> = HashSet::new();
        gc(&empty); // read_dir Err → early return, no panic
        drop(_h);
        let _ = std::fs::remove_dir_all(&tmp);
    }
}
