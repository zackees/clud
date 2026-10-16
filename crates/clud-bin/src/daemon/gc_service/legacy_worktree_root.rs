//! Moves worktrees out of the old root, `~/.clud/tmp-wt`, into
//! `~/.clud/worktrees` (#1962).
//!
//! The root was renamed because `tmp-` invited treating unpushed work as
//! disposable. Each GC tick moves what it safely can:
//!
//! - a git worktree moves with `git worktree move`, so git's records follow;
//! - any other entry (an unused reservation, a stray file) is renamed;
//! - an entry a live process is working in is skipped, since moving it
//!   would break that session; a later tick moves it once it is idle;
//! - a name already taken in the new root gets `-2`, `-3`, ...
//!
//! The old root is removed once it is empty. Nothing is ever deleted.

use std::path::{Path, PathBuf};

/// The old root's directory name under `~/.clud`.
pub(super) const LEGACY_DIR_NAME: &str = "tmp-wt";

/// Move what can be moved from `<home>/.clud/tmp-wt` into the worktree
/// root. Returns one log line per entry handled.
pub(super) fn migrate(home: &Path, live_cwds: &[PathBuf]) -> Vec<String> {
    let legacy = home.join(".clud").join(LEGACY_DIR_NAME);
    let Ok(entries) = std::fs::read_dir(&legacy) else {
        return Vec::new();
    };
    let root = crate::gc::worktree_root::worktree_root_for(home);
    if let Err(error) = std::fs::create_dir_all(&root) {
        return vec![format!(
            "legacy_worktree_root: cannot create {}: {error}",
            root.display()
        )];
    }
    let mut log = Vec::new();
    for entry in entries.flatten() {
        let from = entry.path();
        let canonical = from.canonicalize().unwrap_or_else(|_| from.clone());
        if live_cwds
            .iter()
            .any(|cwd| cwd.starts_with(&from) || cwd.starts_with(&canonical))
        {
            log.push(format!(
                "legacy_worktree_root: skipped {} (in use)",
                from.display()
            ));
            continue;
        }
        let to = free_destination(&root, &entry.file_name().to_string_lossy());
        log.push(match move_entry(&from, &to) {
            Ok(()) => format!(
                "legacy_worktree_root: moved {} -> {}",
                from.display(),
                to.display()
            ),
            Err(error) => format!("legacy_worktree_root: left {}: {error}", from.display()),
        });
    }
    // Fails while anything is left, which is the point.
    if std::fs::remove_dir(&legacy).is_ok() {
        log.push(format!(
            "legacy_worktree_root: removed {}",
            legacy.display()
        ));
    }
    log
}

/// Where an old entry goes: an old `<repo>-wt-<name>` becomes `<repo>-wt-<name>`,
/// anything else keeps its name; the first free ordinal either way.
fn free_destination(root: &Path, name: &str) -> PathBuf {
    let (repo, rest) = name.split_once("-wt-").unwrap_or((name, ""));
    crate::gc::worktree_root::suggest_wt_path_in(root, repo, rest)
}

/// A linked git worktree moves with `git worktree move`, run from its main
/// repository so git updates both ends of the link; anything else is
/// renamed (the two roots are siblings under `~/.clud`, so one volume).
fn move_entry(from: &Path, to: &Path) -> Result<(), String> {
    if !from.join(".git").is_file() {
        return std::fs::rename(from, to).map_err(|error| error.to_string());
    }
    let main = main_repository(from)?;
    let (from_text, to_text) = (from.to_string_lossy(), to.to_string_lossy());
    match super::extern_repo::probe_cmd_streams(
        "git",
        &main,
        &["worktree", "move", &from_text, &to_text],
        GIT_MOVE_TIMEOUT,
    ) {
        Some((0, _, _)) => Ok(()),
        Some((code, _, err)) => Err(format!("git worktree move exited {code}: {}", err.trim())),
        None => Err("git worktree move timed out or failed to start".to_string()),
    }
}

const GIT_MOVE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(120);

/// The main working tree of the repository `worktree` belongs to.
fn main_repository(worktree: &Path) -> Result<PathBuf, String> {
    let out = super::extern_repo::probe_cmd(
        "git",
        worktree,
        &["rev-parse", "--path-format=absolute", "--git-common-dir"],
        GIT_MOVE_TIMEOUT,
    )
    .filter(|(code, _)| *code == 0)
    .ok_or("git rev-parse --git-common-dir failed")?;
    let common = PathBuf::from(out.1.trim());
    Ok(match common.file_name() {
        Some(name) if name == ".git" => common.parent().map_or(common.clone(), Path::to_path_buf),
        _ => common,
    })
}

#[cfg(test)]
#[path = "legacy_worktree_root_tests.rs"]
mod tests;
