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

/// The production ledger: the daemon's GC registry, asked over JSON-over-TCP
/// without spawning a daemon. Every failure reads as "unavailable".
#[derive(Debug)]
pub struct DaemonLedger {
    pub state_dir: Option<PathBuf>,
    pub session_id: Option<String>,
}

impl CreationLedger for DaemonLedger {
    fn lookup(&self, path: &Path) -> Result<Vec<CreatedEntry>, String> {
        let session = self.session_id.as_deref().ok_or(
            "no clud session id; launch through clud, or set CLUD_SESSION_ID to a unique value before starting this session",
        )?;
        let state = self.state_dir.as_deref().ok_or("no clud state directory")?;
        crate::daemon::gc_client_query_created(
            state,
            session,
            &path.to_string_lossy(),
            crate::daemon::LEDGER_QUERY_TIMEOUT,
        )
        .map_err(|error| format!("clud daemon not reachable: {error}"))
    }
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
    let rows = match &facts.rows {
        Ok(rows) => rows,
        Err(reason) => return Err(format!("creation ledger unavailable ({reason})")),
    };
    let mut candidates: Vec<&CreatedEntry> = rows
        .iter()
        .filter(|row| covers(row, &facts.target))
        .collect();
    if candidates.is_empty() {
        return Err("not created by this session (no creation ledger entry)".into());
    }
    // The deepest row first: its refusal is the most specific.
    candidates.sort_by_key(|row| std::cmp::Reverse(Path::new(&row.path).components().count()));
    let mut first_refusal = None;
    for row in candidates {
        match check_row(row, facts) {
            Ok(()) => {
                return Ok(format!(
                    "creation ledger: {} {} recorded for session {} by {}",
                    row.kind.as_str(),
                    row.path,
                    row.session_id,
                    row.role
                ));
            }
            Err(reason) => {
                first_refusal.get_or_insert(reason);
            }
        }
    }
    Err(first_refusal.unwrap_or_default())
}

fn check_row(row: &CreatedEntry, facts: &LedgerFacts) -> Result<(), String> {
    let recorded = Path::new(&row.path);
    let kind = row.kind.as_str();
    let Some(live) = facts.live.get(recorded) else {
        return Err(format!("recorded {kind} {} no longer exists", row.path));
    };
    if live.is_symlink {
        return Err(format!("recorded {kind} {} is now a symlink", row.path));
    }
    let live_kind = if live.is_dir { "dir" } else { "file" };
    if live_kind != kind {
        return Err(format!("recorded {kind} {} is now a {live_kind}", row.path));
    }
    let (Some(dev), Some(ino), Some((live_dev, live_ino)), Some(me)) =
        (row.dev, row.ino, live.id, facts.me)
    else {
        return Err(format!(
            "cannot verify the identity of recorded {kind} {} (the device/inode check is \
             Unix-only)",
            row.path
        ));
    };
    if (dev, ino) != (live_dev, live_ino) {
        return Err(format!(
            "recorded {kind} {} was replaced (device/inode differ)",
            row.path
        ));
    }
    if row.uid != Some(me) {
        return Err(format!("recorded {kind} {} is not owned by you", row.path));
    }
    for path in chain(row, &facts.target) {
        if let Some(entry) = facts.live.get(&path) {
            if entry.uid != Some(me) {
                return Err(format!("{} is not owned by you", path.display()));
            }
        }
    }
    Ok(())
}

#[cfg(test)]
#[path = "rm_tool_ledger_tests.rs"]
mod tests;
