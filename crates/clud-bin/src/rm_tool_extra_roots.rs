//! The human-set, reasoned root override for `safe-rm` (#1668, DD-135
//! "Override", DD-137).
//!
//! `safe_rm.extra_roots` in the user's `~/.clud/settings.json` names extra
//! directories, each with a required reason. Entries strictly under one are
//! deletable under the temp-root rules (DD-128): never the extra root itself,
//! the operand's parent canonicalized first so a symlink cannot escape, and
//! on Unix every existing entry from the extra root down owned by the
//! caller. At load an entry is dropped, with a visible message, when it has
//! no reason, does not resolve to an existing directory, is a filesystem
//! root, is HOME or an ancestor of it, holds `.git`, or (Unix) is not owned
//! by the caller. [`verdict`] decides one entry over injected
//! [`EntryFacts`]; [`probe`] gathers them. Every deletion allowed only by an
//! override carries its reason into both audit logs.

use std::path::{Path, PathBuf};

use crate::clud_settings::{SafeRmExtraRootEntry, SAFE_RM_EXTRA_ROOTS_KEY};

/// One honored extra root: canonical directory plus the human's reason.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExtraRoot {
    pub root: PathBuf,
    pub reason: String,
}

/// What [`verdict`] needs to know about one well-formed settings entry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EntryFacts {
    /// The path as written.
    pub raw: String,
    pub reason: String,
    /// Its canonical form, or why it could not be resolved.
    pub canonical: Result<PathBuf, String>,
    pub is_dir: bool,
    /// The canonical HOME, when known.
    pub home: Option<PathBuf>,
    /// The directory's owner uid (Unix), and the uid it must be.
    pub owner: Option<u32>,
    pub me: Option<u32>,
}

/// Whether the entry is honored, or the message saying why not.
pub fn verdict(facts: &EntryFacts) -> Result<ExtraRoot, String> {
    let refuse = |why: &str| {
        Err(format!(
            "{SAFE_RM_EXTRA_ROOTS_KEY} entry {} ignored: {why}",
            facts.raw
        ))
    };
    if facts.reason.trim().is_empty() {
        return refuse("missing a non-empty \"reason\"");
    }
    if !Path::new(&facts.raw).is_absolute() {
        return refuse("the path must be absolute");
    }
    let root = match &facts.canonical {
        Ok(root) => root.clone(),
        Err(error) => return refuse(&format!("cannot resolve it: {error}")),
    };
    if !facts.is_dir {
        return refuse("is not a directory");
    }
    if root.parent().is_none() {
        return refuse("is a filesystem root");
    }
    if facts
        .home
        .as_ref()
        .is_some_and(|home| home.starts_with(&root))
    {
        return refuse("is your home directory or one of its ancestors");
    }
    if root
        .components()
        .any(|c| c.as_os_str().to_string_lossy().eq_ignore_ascii_case(".git"))
    {
        return refuse("is git metadata");
    }
    if let (Some(owner), Some(me)) = (facts.owner, facts.me) {
        if owner != me {
            return refuse("is not owned by you");
        }
    }
    Ok(ExtraRoot {
        root,
        reason: facts.reason.trim().to_string(),
    })
}

/// Gather [`EntryFacts`] for `entry` from the filesystem.
pub fn probe(entry: &SafeRmExtraRootEntry, home: Option<&Path>, me: Option<u32>) -> EntryFacts {
    let canonical = crate::path_norm::canonicalize_plain(Path::new(&entry.path))
        .map_err(|error| error.to_string());
    let meta = canonical
        .as_ref()
        .ok()
        .and_then(|p| std::fs::metadata(p).ok());
    EntryFacts {
        raw: entry.path.clone(),
        reason: entry.reason.clone(),
        is_dir: meta.as_ref().is_some_and(std::fs::Metadata::is_dir),
        owner: meta.as_ref().and_then(owner_of),
        canonical,
        home: home.and_then(|h| crate::path_norm::canonicalize_plain(h).ok()),
        me,
    }
}

#[cfg(unix)]
fn owner_of(meta: &std::fs::Metadata) -> Option<u32> {
    use std::os::unix::fs::MetadataExt;
    Some(meta.uid())
}

#[cfg(not(unix))]
fn owner_of(_: &std::fs::Metadata) -> Option<u32> {
    None
}

/// Vet every well-formed entry; returns the honored roots and the messages
/// for the dropped ones (appended to `rejected`, the settings-shape ones).
pub fn load(
    entries: &[SafeRmExtraRootEntry],
    mut rejected: Vec<String>,
    home: Option<&Path>,
    me: Option<u32>,
) -> (Vec<ExtraRoot>, Vec<String>) {
    let mut roots: Vec<ExtraRoot> = Vec::new();
    for entry in entries {
        match verdict(&probe(entry, home, me)) {
            Ok(root) if roots.iter().any(|r| r.root == root.root) => {}
            Ok(root) => roots.push(root),
            Err(message) => rejected.push(message),
        }
    }
    (roots, rejected)
}

/// The text recorded as the reason for a deletion allowed by `root`.
pub fn grant_reason(root: &ExtraRoot) -> String {
    format!(
        "{SAFE_RM_EXTRA_ROOTS_KEY} override {}: {}",
        root.root.display(),
        root.reason
    )
}
