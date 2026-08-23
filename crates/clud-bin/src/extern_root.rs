//! Where a repo's foreign checkouts live (zackees/clud#986).
//!
//! ```text
//! ~/dev/myrepo            <- the repo
//! ~/dev/myrepo-extern/    <- its foreign checkouts
//!     running-process/
//!     soldr/
//! ```
//!
//! ## Why beside the repo rather than inside it
//!
//! Anything under the repo root has to be excluded by every tool pointed at
//! that root, and a wrong exclusion fails **silently**. `ci/banned_imports.py`
//! carried `extern-repos` where the directory is `.extern-repos` for its whole
//! life: the membership test never matched, so every lint run walked into
//! every cloned dependency, and nothing ever went red. That is the failure
//! mode this layout removes — not by configuring the tools, but by putting the
//! checkouts somewhere the tools pointed at the repo cannot reach.
//!
//! It also makes containment a disjoint question. While checkouts were nested
//! inside the parent, "is this path the parent's business?" needed
//! most-specific-first root matching and a rule stated as an exception
//! (DD-052). A sibling is outside the repo tree, so the two sets never
//! overlap.
//!
//! ## Worktrees share one sibling
//!
//! The location derives from the **main** repo root, so `~/dev/zccache` and a
//! worktree at `~/dev/zccache-wt-1360` both use `~/dev/zccache-extern` rather
//! than cloning the same dependency once per worktree. That is usually what
//! you want; when it is not, the checkouts are still ordinary directories the
//! user can move.
//!
//! ## Claiming
//!
//! `~/dev/myrepo-extern` might already be somebody's real repo. clud writes a
//! marker naming the owner on first use, and refuses to adopt a non-empty
//! directory that has no marker rather than scattering clones through it.

use std::path::{Path, PathBuf};

/// Suffix appended to the repo's own directory name.
pub const EXTERN_SUFFIX: &str = "-extern";

/// Marker claiming a sibling directory for a repo.
pub const CLAIM_FILE: &str = ".clud-extern";

/// The pre-#986 in-tree location, still read so existing checkouts keep
/// working while users move them.
pub const LEGACY_DIR_NAME: &str = ".extern-repos";

/// Where `repo_root`'s foreign checkouts belong.
///
/// `None` when the layout cannot be expressed: a repo at a filesystem root has
/// no parent to put a sibling in, and a directory name that is not valid UTF-8
/// cannot have a suffix appended predictably. Callers must decide what that
/// means for them rather than being handed a wrong path — see
/// [`allowed_clone_roots`], which keeps the old guard behavior in that case.
#[must_use]
pub fn sibling_for(repo_root: &Path) -> Option<PathBuf> {
    let parent = repo_root.parent()?;
    let name = repo_root.file_name()?.to_str()?;
    if name.is_empty() {
        return None;
    }
    Some(parent.join(format!("{name}{EXTERN_SUFFIX}")))
}

/// The legacy in-tree location for `repo_root`.
#[must_use]
pub fn legacy_for(repo_root: &Path) -> PathBuf {
    repo_root.join(LEGACY_DIR_NAME)
}

/// Every location a checkout may currently live in, most-preferred first.
///
/// Both are returned during the migration window: discovery has to keep
/// finding checkouts users already have, and the clone guard has to keep
/// allowing the destination the skill documented until that text catches up.
#[must_use]
pub fn known_roots(repo_root: &Path) -> Vec<PathBuf> {
    let mut roots = Vec::new();
    if let Some(sibling) = sibling_for(repo_root) {
        roots.push(sibling);
    }
    roots.push(legacy_for(repo_root));
    roots
}

/// Where a `git clone` may land.
///
/// Identical to [`known_roots`] today, named separately because the guard's
/// question ("may this clone go here?") and discovery's ("where might a
/// checkout already be?") diverge once the legacy location is retired: the
/// guard stops allowing it before discovery stops looking for it.
#[must_use]
pub fn allowed_clone_roots(repo_root: &Path) -> Vec<PathBuf> {
    known_roots(repo_root)
}

/// Whether `path` sits inside one of `roots`.
///
/// Compared on normalized keys, so Windows drive-letter and separator spelling
/// do not decide it — `Path::starts_with` is component-wise and would answer
/// `false` for `C:\repo` versus `c:\Repo`.
#[must_use]
pub fn is_within_any(path: &Path, roots: &[PathBuf]) -> bool {
    roots.iter().any(|root| is_within(path, root))
}

fn is_within(path: &Path, root: &Path) -> bool {
    let path_key = crate::path_norm::normalize_for_key(path);
    let root_key = crate::path_norm::normalize_for_key(root);
    if path_key == root_key {
        return true;
    }
    let prefix = if root_key.ends_with('/') {
        root_key
    } else {
        format!("{root_key}/")
    };
    path_key.starts_with(&prefix)
}

/// Whether clud may put checkouts in `sibling` on behalf of `repo_root`.
///
/// An absent or empty directory is free to claim. One clud already claimed for
/// this repo is fine. Anything else — a directory holding somebody's work, or
/// claimed by a different repo — is refused, because the name is a guess
/// derived from the repo's own name and guessing wrong must not scatter clones
/// through an unrelated project.
#[must_use]
pub fn claim_state(sibling: &Path, repo_root: &Path) -> ClaimState {
    let Ok(entries) = std::fs::read_dir(sibling) else {
        // Missing, or unreadable; creating it is the caller's business.
        return ClaimState::Available;
    };
    let mut any = false;
    for entry in entries.flatten() {
        if entry.file_name() == CLAIM_FILE {
            return match read_claim(sibling) {
                Some(owner) if same_repo(&owner, repo_root) => ClaimState::OursAlready,
                Some(owner) => ClaimState::ClaimedByOther { owner },
                None => ClaimState::OursAlready,
            };
        }
        any = true;
    }
    if any {
        ClaimState::OccupiedUnclaimed
    } else {
        ClaimState::Available
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ClaimState {
    /// Absent or empty; clud may use it.
    Available,
    /// Already claimed by this repo.
    OursAlready,
    /// Claimed by a different repo.
    ClaimedByOther { owner: String },
    /// Holds files but no claim — somebody else's directory.
    OccupiedUnclaimed,
}

impl ClaimState {
    #[must_use]
    pub fn usable(&self) -> bool {
        matches!(self, Self::Available | Self::OursAlready)
    }
}

fn read_claim(sibling: &Path) -> Option<String> {
    let text = std::fs::read_to_string(sibling.join(CLAIM_FILE)).ok()?;
    let value: serde_json::Value = serde_json::from_str(&text).ok()?;
    value
        .get("repo")
        .and_then(serde_json::Value::as_str)
        .map(ToOwned::to_owned)
}

fn same_repo(recorded: &str, repo_root: &Path) -> bool {
    crate::path_norm::normalize_for_key(Path::new(recorded))
        == crate::path_norm::normalize_for_key(repo_root)
}

/// Write the claim marker, creating the directory if needed.
pub fn claim(sibling: &Path, repo_root: &Path) -> std::io::Result<()> {
    std::fs::create_dir_all(sibling)?;
    let body = serde_json::json!({
        "repo": repo_root.to_string_lossy(),
        "note": "clud keeps this repo's foreign checkouts here (see clud#986). \
                 Safe to delete when nothing is using them.",
    });
    std::fs::write(sibling.join(CLAIM_FILE), format!("{body:#}\n"))
}

#[cfg(test)]
#[path = "extern_root_tests.rs"]
mod extern_root_tests;
