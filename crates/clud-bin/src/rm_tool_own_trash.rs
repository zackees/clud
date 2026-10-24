//! This session's own trash entries (#2096).
//!
//! A deletion an agent makes lands in `~/.clud/trash`, outside every root, so
//! nothing let it erase the entry afterwards and a "leave no files behind"
//! goal could never finish. An entry whose `safe-rm` manifest or `clud trash`
//! owner marker names the caller's session id may be purged with
//! `safe-rm --purge`; trashing it again is refused, because that would only
//! move it into a new entry.

use std::path::{Path, PathBuf};

/// Written by `clud trash` into its quarantine dir: the session that made it.
pub const OWNER_MARKER: &str = ".clud-trash-owner.json";

/// The audit reason for a path allowed by this rule.
pub const REASON: &str = "a trash entry this session created (#2096)";

/// Refusal for a non-purge call on one of the session's own entries.
pub const NEEDS_PURGE: &str =
    "is already in this session's clud trash; pass --purge to delete it for real";

/// Record `session` as the owner of the quarantine dir `dir`. Best effort.
pub fn write_owner(dir: &Path, session: &str) {
    let body = serde_json::json!({ "version": 1, "session_id": session });
    let _ = std::fs::write(dir.join(OWNER_MARKER), body.to_string());
}

/// The canonical entries directly under `trash_root` that `session` created.
pub fn load(trash_root: &Path, session: &str) -> Vec<PathBuf> {
    let Ok(entries) = std::fs::read_dir(trash_root) else {
        return Vec::new();
    };
    entries
        .filter_map(Result::ok)
        .filter(|e| e.file_type().is_ok_and(|t| t.is_dir()))
        .map(|e| e.path())
        .filter(|dir| owned_by(dir, session))
        .filter_map(|dir| crate::path_norm::canonicalize_plain(&dir).ok())
        .collect()
}

fn owned_by(dir: &Path, session: &str) -> bool {
    [super::TRASH_MANIFEST, OWNER_MARKER].iter().any(|name| {
        std::fs::read(dir.join(name))
            .ok()
            .and_then(|bytes| serde_json::from_slice::<serde_json::Value>(&bytes).ok())
            .is_some_and(|v| v.get("session_id").and_then(|s| s.as_str()) == Some(session))
    })
}

#[cfg(test)]
#[path = "rm_tool_own_trash_tests.rs"]
mod tests;
