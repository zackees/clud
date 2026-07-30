//! Pass 1 of the two-pass large-file scan (issue #556): read the git index
//! in-process and report tracked source files whose cached stat size already
//! crosses the threshold — no worktree walk, no ODB, no hashing, no subprocess.
//!
//! The git index stores each tracked file's last-known stat size, so on a git
//! repo this replaces the ~240–400 ms parallel walk with a ~1–5 ms mmap parse.
//! Untracked files and entries with an unusable cached size (racily-clean /
//! never-stat'd, recorded as size 0) are left to the pass-2 walker top-up.
//!
//! Private items of the parent module (`is_whitelisted_source`, `VENDOR_DIRS`,
//! `SIZE_THRESHOLD`, `LargeFile`) are visible here because a child module can
//! see its ancestors' private items.

use std::path::{Path, PathBuf};

use super::{is_whitelisted_source, LargeFile, SIZE_THRESHOLD, VENDOR_DIRS};

/// Outcome of the index pass, split so pass 2 knows exactly what to top up.
#[derive(Debug, Default, PartialEq, Eq)]
pub(super) struct IndexPassOutput {
    /// Tracked source files already over the threshold per the index size.
    pub(super) qualifying: Vec<LargeFile>,
    /// Tracked source paths whose cached size is unusable (0) and must be
    /// re-measured by the pass-2 walker rather than dropped or flagged.
    pub(super) needs_verification: Vec<PathBuf>,
}

/// Why the index pass could not produce a report; the caller falls back to the
/// walker (or, per #556, a killable `git ls-files --debug` subprocess).
#[derive(Debug, PartialEq, Eq)]
pub(super) enum IndexPassError {
    /// No resolvable git index (not a git repo, or `.git` indirection broke).
    NoIndex,
    /// The index existed but could not be parsed (corrupt / unsupported).
    Parse,
}

/// Resolve the git index path for `root`, including the linked-worktree case
/// where `<root>/.git` is a **file** containing `gitdir: <path>` (this is how
/// `.claude/worktrees/agent-*` worktrees are laid out — each has its own index).
pub(super) fn resolve_index_path(root: &Path) -> Option<PathBuf> {
    let dot_git = root.join(".git");
    let meta = std::fs::symlink_metadata(&dot_git).ok()?;
    if meta.is_dir() {
        return Some(dot_git.join("index"));
    }
    if meta.is_file() {
        let text = std::fs::read_to_string(&dot_git).ok()?;
        let gitdir = text
            .lines()
            .find_map(|line| line.trim().strip_prefix("gitdir:"))?
            .trim();
        if gitdir.is_empty() {
            return None;
        }
        let gitdir_path = Path::new(gitdir);
        let resolved = if gitdir_path.is_absolute() {
            gitdir_path.to_path_buf()
        } else {
            // gitdir is relative to the directory containing the `.git` file.
            root.join(gitdir_path)
        };
        return Some(resolved.join("index"));
    }
    None
}

/// Pass 1: parse the index and classify its tracked entries. `Err` on no/parse
/// failure so the caller can fall back.
pub(super) fn index_pass(root: &Path) -> Result<IndexPassOutput, IndexPassError> {
    let index_path = resolve_index_path(root).ok_or(IndexPassError::NoIndex)?;
    if !index_path.exists() {
        return Err(IndexPassError::NoIndex);
    }
    // `skip_hash: true` skips the trailing-checksum verification — this is a
    // read-only startup nudge on the latency-critical path, not a data-safety
    // operation, so we take the fast route git itself exposes for exactly this.
    let file = gix_index::File::at(
        &index_path,
        gix_index::hash::Kind::Sha1,
        true,
        gix_index::decode::Options::default(),
    )
    .map_err(|_| IndexPassError::Parse)?;

    let entries = file.entries().iter().filter_map(|entry| {
        // Regular files only: never follow/resolve symlinks (SYMLINK), and skip
        // submodule gitlinks (COMMIT) and sparse dir entries (DIR).
        if entry.mode != gix_index::entry::Mode::FILE
            && entry.mode != gix_index::entry::Mode::FILE_EXECUTABLE
        {
            return None;
        }
        // Index paths are `/`-separated bytes; keep non-UTF-8 paths out rather
        // than lossily guessing (the walker would still catch them in pass 2).
        let path = std::str::from_utf8(entry.path(&file)).ok()?;
        Some((PathBuf::from(path), entry.stat.size))
    });
    Ok(classify_entries(entries))
}

/// Whether any path component is a conventional vendor/deps directory — the
/// same names the walker prunes, applied here so a committed `vendor/` tree
/// does not dominate the tracked report.
fn is_in_vendor_dir(path: &Path) -> bool {
    path.components().any(|component| {
        component
            .as_os_str()
            .to_str()
            .is_some_and(|name| VENDOR_DIRS.contains(&name))
    })
}

/// Pure classification of `(path, cached_index_size)` entries into report
/// buckets. Separated from all IO so it is exhaustively unit-testable.
pub(super) fn classify_entries(entries: impl Iterator<Item = (PathBuf, u32)>) -> IndexPassOutput {
    let mut out = IndexPassOutput::default();
    for (path, size) in entries {
        if !is_whitelisted_source(&path) || is_in_vendor_dir(&path) {
            continue;
        }
        if size == 0 {
            // A cached size of 0 is the racily-clean / never-stat'd marker: the
            // index value is untrustworthy, so defer to the pass-2 walker.
            out.needs_verification.push(path);
            continue;
        }
        if u64::from(size) >= SIZE_THRESHOLD {
            out.qualifying.push(LargeFile {
                rel_path: path,
                size: u64::from(size),
            });
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn p(s: &str) -> PathBuf {
        PathBuf::from(s)
    }

    #[test]
    fn classify_splits_qualifying_verification_and_ignored() {
        let big = SIZE_THRESHOLD as u32 + 1;
        let entries = vec![
            (p("src/big.rs"), big),          // qualifies
            (p("src/small.rs"), 100),        // under threshold → ignored
            (p("src/racy.rs"), 0),           // size 0 → verification list
            (p("README.md"), big),           // not a source ext → ignored
            (p("app.min.js"), big),          // *.min.* → ignored
            (p("vendor/huge.rs"), big),      // vendor dir → ignored
            (p("node_modules/x/y.js"), big), // vendor dir → ignored
        ];
        let out = classify_entries(entries.into_iter());
        let qual: Vec<_> = out.qualifying.iter().map(|f| f.rel_path.clone()).collect();
        assert_eq!(qual, vec![p("src/big.rs")]);
        assert_eq!(out.qualifying[0].size, u64::from(big));
        assert_eq!(out.needs_verification, vec![p("src/racy.rs")]);
    }

    #[test]
    fn classify_threshold_is_inclusive() {
        let at = SIZE_THRESHOLD as u32;
        let out = classify_entries(std::iter::once((p("a.rs"), at)));
        assert_eq!(out.qualifying.len(), 1, "exactly threshold qualifies");
    }

    #[test]
    fn resolve_index_path_for_a_git_directory() {
        let tmp = TempDir::new().unwrap();
        std::fs::create_dir_all(tmp.path().join(".git")).unwrap();
        assert_eq!(
            resolve_index_path(tmp.path()),
            Some(tmp.path().join(".git").join("index"))
        );
    }

    #[test]
    fn resolve_index_path_follows_a_worktree_git_file() {
        // A linked worktree's `.git` is a file pointing at the real gitdir.
        let tmp = TempDir::new().unwrap();
        let real_gitdir = tmp.path().join("realrepo/.git/worktrees/wt");
        std::fs::create_dir_all(&real_gitdir).unwrap();
        let wt = tmp.path().join("wt");
        std::fs::create_dir_all(&wt).unwrap();
        std::fs::write(
            wt.join(".git"),
            format!("gitdir: {}\n", real_gitdir.display()),
        )
        .unwrap();
        assert_eq!(
            resolve_index_path(&wt),
            Some(real_gitdir.join("index")),
            "worktree must resolve to its own index"
        );
    }

    #[test]
    fn resolve_index_path_none_without_git() {
        let tmp = TempDir::new().unwrap();
        assert_eq!(resolve_index_path(tmp.path()), None);
    }

    #[test]
    fn index_pass_errors_without_an_index() {
        let tmp = TempDir::new().unwrap();
        assert_eq!(index_pass(tmp.path()), Err(IndexPassError::NoIndex));
    }
}
