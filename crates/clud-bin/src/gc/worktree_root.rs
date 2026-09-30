//! Issue #1485: the clud-owned root for agent worktrees, `~/.clud/tmp-wt/`.
//!
//! A **sibling** of `~/.clud/tmp` (the `session_tmp` root), never a child, so
//! the 72 h mtime sweep cannot see a worktree by construction: an old mtime is
//! not evidence that a checkout's work landed. Worktrees here are named
//! `<repo>-wt-<suffix>` (so `reconcile::has_nonempty_suffix` keeps matching)
//! and are reclaimed only by the repo-worktree verdict (DD-122/DD-123/DD-124).
//! The root itself is never a removal target: nothing in clud deletes it, and
//! `git worktree remove` only removes the child it is given.
//!
//! [`alloc_wt_path`] is the one allocator for entries (#1486): it reserves
//! the directory atomically, so a path it returns exists before any caller
//! prints it. An unused reservation stays an empty, non-git directory that
//! the daemon reclaims as `reserved-unused` after the same 24 h grace.
//!
//! Also owns the warn-only size check behind `worktrees.warn_bytes`: size
//! never deletes pinned work, it only produces a warning in `clud gc list`.

use std::fs;
use std::path::{Path, PathBuf};

/// Directory name under the clud home (`~/.clud`).
pub const WORKTREE_ROOT_DIR_NAME: &str = "tmp-wt";

/// Grace before a clean, commit-free worktree under the root counts as
/// abandoned (#1485 rule "abandoned-empty").
pub const ABANDONED_EMPTY_GRACE_SECS: u64 = 24 * 60 * 60;

/// Default for `worktrees.warn_bytes`: 50 GiB. `0` disables the warning.
pub const DEFAULT_WARN_BYTES: u64 = 50 * 1024 * 1024 * 1024;

/// Entry budget for one size check, so `clud gc list` stays bounded on a
/// root full of build trees. Exhausting it reports `Unknown`, never `Under`.
pub const SIZE_SCAN_ENTRY_BUDGET: usize = 500_000;

/// `<home>/.clud/tmp-wt`. Pure, for tests and for callers holding a home.
pub fn worktree_root_for(home: &Path) -> PathBuf {
    home.join(".clud").join(WORKTREE_ROOT_DIR_NAME)
}

/// `~/.clud/tmp-wt` without creating it; `None` with no home dir. Resolves
/// home exactly like [`super::session_tmp::session_tmp_dir`], so the two
/// roots are always siblings.
pub fn worktree_root() -> Option<PathBuf> {
    Some(worktree_root_for(&super::session_tmp::home_dir()?))
}

/// Create `<home>/.clud/tmp-wt` idempotently.
pub fn ensure_worktree_root_at(home: &Path) -> std::io::Result<PathBuf> {
    let root = worktree_root_for(home);
    fs::create_dir_all(&root)?;
    Ok(root)
}

/// Create `~/.clud/tmp-wt` idempotently. Called at session launch and at
/// daemon GC-worker start; failure is non-fatal (agents fall back to the
/// sibling layout they used before). A no-op under `cfg(test)` so unit
/// tests never create the real `~/.clud/tmp-wt`; they use
/// [`ensure_worktree_root_at`] with a tempdir.
pub fn ensure_worktree_root() -> Option<PathBuf> {
    if cfg!(test) {
        return None;
    }
    ensure_worktree_root_at(&super::session_tmp::home_dir()?).ok()
}

/// The infix between the repo slug and the suffix, shared with
/// `gc/reconcile.rs`'s `{repo_name}-wt-` matcher.
pub const WT_INFIX: &str = "-wt-";

/// Upper bound on collision ordinals (`-2`, `-3`, ...). Reaching it means
/// something is creating names in a loop, not a legitimate collision.
const MAX_ORDINAL: u32 = 10_000;

/// Issue #1486: reserve a fresh `~/.clud/tmp-wt/<slug>-wt-<suffix>` and
/// return it. See [`alloc_wt_path_in`].
pub fn alloc_wt_path(slug: &str, suffix: &str) -> std::io::Result<PathBuf> {
    let root = worktree_root()
        .ok_or_else(|| std::io::Error::other("no home directory; cannot resolve ~/.clud/tmp-wt"))?;
    alloc_wt_path_in(&root, slug, suffix)
}

/// Reserve a fresh `<root>/<slug>-wt-<suffix>` directory and return its path,
/// which **exists on return**. On a collision it tries
/// `<slug>-wt-<suffix>-2`, `-3`, ... in order. Each attempt is one
/// `create_dir`, which fails if the name exists, so two concurrent callers
/// never receive the same path. This is the one allocator behind #1486's
/// refusal messages and `safe-gh-*` helpers: a printed path is always real.
///
/// `slug` and `suffix` must each be one non-empty path component with no
/// separator and no character Windows rejects; otherwise `InvalidInput`, and
/// nothing is reserved.
pub fn alloc_wt_path_in(root: &Path, slug: &str, suffix: &str) -> std::io::Result<PathBuf> {
    validate_component("slug", slug)?;
    validate_component("suffix", suffix)?;
    fs::create_dir_all(root)?;
    let base = format!("{slug}{WT_INFIX}{suffix}");
    for ordinal in 1..=MAX_ORDINAL {
        let name = if ordinal == 1 {
            base.clone()
        } else {
            format!("{base}-{ordinal}")
        };
        let candidate = root.join(name);
        match fs::create_dir(&candidate) {
            Ok(()) => return Ok(candidate),
            Err(err) if err.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(err) => return Err(err),
        }
    }
    Err(std::io::Error::other(format!(
        "no free ordinal for {base} under {} after {MAX_ORDINAL} attempts",
        root.display()
    )))
}

/// Separators, and the characters Windows rejects in a file name.
fn is_forbidden_char(c: char) -> bool {
    c.is_control() || matches!(c, '/' | '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|')
}

fn validate_component(what: &str, value: &str) -> std::io::Result<()> {
    let bad =
        value.is_empty() || value == "." || value == ".." || value.chars().any(is_forbidden_char);
    if bad {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            format!("invalid worktree {what} {value:?}: must be one plain path component"),
        ));
    }
    Ok(())
}

/// Result of a bounded size check.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SizeCheck {
    /// Walked the whole tree; total bytes.
    Under(u64),
    /// Stopped early once the running total passed the limit.
    Over(u64),
    /// The entry budget ran out before either answer was proven.
    Unknown,
}

/// Sum file sizes under `root`, stopping as soon as the total exceeds
/// `limit` (so an over-limit root costs no more than reaching the limit).
/// Symlinks are not followed. A missing root is `Under(0)`.
pub fn check_tree_size(root: &Path, limit: u64, entry_budget: usize) -> SizeCheck {
    let mut total: u64 = 0;
    let mut budget = entry_budget;
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let Ok(entries) = fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            if budget == 0 {
                return SizeCheck::Unknown;
            }
            budget -= 1;
            let Ok(meta) = entry.path().symlink_metadata() else {
                continue;
            };
            if meta.is_dir() {
                stack.push(entry.path());
            } else if meta.is_file() {
                total = total.saturating_add(meta.len());
                if total > limit {
                    return SizeCheck::Over(total);
                }
            }
        }
    }
    SizeCheck::Under(total)
}

/// The `clud gc list` warning line for a check, or `None` when there is
/// nothing to say. `warn_bytes == 0` disables it.
pub fn size_warning(root: &Path, warn_bytes: u64, check: SizeCheck) -> Option<String> {
    if warn_bytes == 0 {
        return None;
    }
    match check {
        SizeCheck::Under(_) => None,
        SizeCheck::Over(bytes) => Some(format!(
            "warning: {} holds at least {} bytes, over worktrees.warn_bytes ({warn_bytes}); \
             pinned worktrees are never deleted for size — review `clud gc list`",
            root.display(),
            bytes
        )),
        SizeCheck::Unknown => Some(format!(
            "warning: {} is too large to size within budget; it may exceed \
             worktrees.warn_bytes ({warn_bytes})",
            root.display()
        )),
    }
}

/// Whether `path` sits under `root` (canonicalized when possible, so a
/// symlinked home still matches).
pub fn is_under_worktree_root(path: &Path, root: &Path) -> bool {
    let canon = |p: &Path| fs::canonicalize(p).unwrap_or_else(|_| p.to_path_buf());
    let (path, root) = (canon(path), canon(root));
    path != root && path.starts_with(&root)
}

#[cfg(test)]
#[path = "worktree_root_tests.rs"]
mod tests;
