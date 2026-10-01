//! The creation ledger consult for a path outside every root (#1666, DD-135).
//!
//! A clud helper that created a path records it in the daemon's GC registry
//! (the `created` rows; the helper itself is #1667). `safe-rm` asks the daemon
//! only for a canonical path outside every root and every temp root, and
//! allows it when the path is a recorded file, or lies under (or is) a
//! recorded directory, the recorded entry's live device/inode still match,
//! and every existing entry from the recorded one down to the target is the
//! caller's (as for temp entries, DD-128). [`verdict`] decides that over
//! [`LedgerFacts`]; [`probe`] gathers them. Anything in doubt refuses: an
//! unreachable daemon, no session id, a missing row, a symlink or another
//! inode at the recorded path, or a platform without a file identity
//! (Windows, where the check is Unix-only).

use std::collections::BTreeMap;
use std::fmt::Debug;
use std::path::{Path, PathBuf};

use crate::gc::{CreatedEntry, CreatedKind};

/// Where `safe-rm` reads the ledger. The production source asks the daemon
/// over JSON-over-TCP; tests inject rows.
pub trait CreationLedger: Debug {
    /// The session's rows whose path equals `path` or, for a directory row,
    /// contains it. `Err` names why the ledger could not be read.
    fn lookup(&self, path: &Path) -> Result<Vec<CreatedEntry>, String>;
}

/// The live state of one path, from `symlink_metadata` (never followed).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LiveEntry {
    pub is_dir: bool,
    pub is_symlink: bool,
    /// `(dev, ino)` on Unix; `None` where no identity is available.
    pub id: Option<(u64, u64)>,
    /// Owner uid on Unix.
    pub uid: Option<u32>,
}

/// Everything [`verdict`] needs, gathered by [`probe`] or built by a test.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LedgerFacts {
    /// The canonical target.
    pub target: PathBuf,
    /// The ledger rows, or why the ledger was unavailable.
    pub rows: Result<Vec<CreatedEntry>, String>,
    /// Live state of every existing path from each row down to the target;
    /// a path absent here does not exist.
    pub live: BTreeMap<PathBuf, LiveEntry>,
    /// The uid every entry must belong to; `None` off Unix.
    pub me: Option<u32>,
}

/// Whether `row` names `target` (a file row) or contains it (a directory row).
fn covers(row: &CreatedEntry, target: &Path) -> bool {
    let recorded = Path::new(&row.path);
    match row.kind {
        CreatedKind::File => recorded == target,
        CreatedKind::Dir => target.starts_with(recorded),
    }
}

/// The paths from `row`'s recorded path down to `target`, both included.
fn chain(row: &CreatedEntry, target: &Path) -> Vec<PathBuf> {
    let recorded = PathBuf::from(&row.path);
    let mut out = vec![recorded.clone()];
    if let Ok(rest) = target.strip_prefix(&recorded) {
        let mut current = recorded;
        for part in rest.components() {
            current.push(part);
            out.push(current.clone());
        }
    }
    out
}

/// Gather [`LedgerFacts`] for `target` from `ledger` and the filesystem.
pub fn probe(target: &Path, ledger: &dyn CreationLedger, me: Option<u32>) -> LedgerFacts {
    let rows = ledger.lookup(target);
    let mut live = BTreeMap::new();
    if let Ok(rows) = &rows {
        for row in rows.iter().filter(|row| covers(row, target)) {
            for path in chain(row, target) {
                if live.contains_key(&path) {
                    continue;
                }
                if let Some(entry) = stat(&path) {
                    live.insert(path, entry);
                }
            }
        }
    }
    LedgerFacts {
        target: target.to_path_buf(),
        rows,
        live,
        me,
    }
}

fn stat(path: &Path) -> Option<LiveEntry> {
    let meta = std::fs::symlink_metadata(path).ok()?;
    let is_symlink = meta.file_type().is_symlink();
    #[cfg(unix)]
    let (id, uid) = {
        use std::os::unix::fs::MetadataExt;
        (Some((meta.dev(), meta.ino())), Some(meta.uid()))
    };
    #[cfg(not(unix))]
    let (id, uid) = (None, None);
    Some(LiveEntry {
        is_dir: !is_symlink && meta.is_dir(),
        is_symlink,
        id,
        uid,
    })
}

/// `Ok(reason)` naming the ledger row that allows deleting the target, or
/// `Err(clause)` for the refusal.
pub fn verdict(facts: &LedgerFacts) -> Result<String, String> {
    let _ = facts;
    Err("creation ledger not implemented".into())
}

#[cfg(test)]
#[path = "rm_tool_ledger_tests.rs"]
mod tests;
