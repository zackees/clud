//! Issue #2103 item 1–2: find the repos the in-session `git`/`gh` shim saw.
//!
//! Only `clud` launches used to register a repo (`record_repo_visit`), so a
//! worktree made by any other route stayed invisible to `gc list`. The shim
//! already logs every call's `cwd` and `argv` (`shim_telemetry`); this module
//! tails that log from a persisted byte offset and resolves each `cwd`, and
//! each argv token that names an existing directory, to a repository's main
//! checkout. It parses no subcommands and spawns no process: resolution reads
//! `.git`, `gitdir` and `commondir` files only, so every linked worktree and
//! submodule path collapses onto the checkout that owns it.
//!
//! Runs on the repo-worktree probe thread, never on the registry worker
//! (#946); the worker only upserts the rows it is handed.

use std::collections::BTreeMap;
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};

/// Most bytes one pass reads. The log rotates at 8 MiB, so a backlog drains
/// over a few ticks instead of one long read.
const MAX_BYTES_PER_PASS: u64 = 4 * 1024 * 1024;

/// Where the hints come from and where the read offset persists.
pub(super) struct HintSource {
    pub(super) log: PathBuf,
    pub(super) cursor: PathBuf,
}

impl HintSource {
    /// `<state>/logs/shim/git-gh.jsonl`, with its offset in
    /// `<state>/gc/shim-hints.offset`.
    pub(super) fn under(state: &Path) -> Self {
        HintSource {
            log: crate::shim_telemetry::log_path(state),
            cursor: state.join("gc").join("shim-hints.offset"),
        }
    }
}

/// Read the log past the saved offset and return `(repo_root, cwd)` for each
/// repository it names, keyed by repo root. Only complete lines are
/// consumed; a partial last line is read again next pass. A log shorter than
/// the offset has rotated, so reading restarts at its beginning (the tail of
/// the rotated file is not revisited). Every error leaves the offset as it
/// was and yields what was read so far.
pub(super) fn ingest(source: &HintSource) -> BTreeMap<String, String> {
    let mut repos = BTreeMap::new();
    let saved = std::fs::read_to_string(&source.cursor)
        .ok()
        .and_then(|text| text.trim().parse::<u64>().ok())
        .unwrap_or(0);
    let Ok(mut file) = std::fs::File::open(&source.log) else {
        return repos;
    };
    let len = file.metadata().map(|meta| meta.len()).unwrap_or(0);
    let start = if len < saved { 0 } else { saved };
    if file.seek(SeekFrom::Start(start)).is_err() {
        return repos;
    }
    let mut buf = Vec::new();
    if file
        .take(MAX_BYTES_PER_PASS)
        .read_to_end(&mut buf)
        .is_err()
    {
        return repos;
    }
    let Some(last_newline) = buf.iter().rposition(|&b| b == b'\n') else {
        return repos;
    };
    for line in buf[..=last_newline].split(|&b| b == b'\n') {
        collect_line(line, &mut repos);
    }
    let next = start + last_newline as u64 + 1;
    if let Some(dir) = source.cursor.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    let _ = std::fs::write(&source.cursor, next.to_string());
    repos
}

fn collect_line(line: &[u8], repos: &mut BTreeMap<String, String>) {
    let Ok(record) = serde_json::from_slice::<serde_json::Value>(line) else {
        return;
    };
    let Some(cwd) = record.get("cwd").and_then(serde_json::Value::as_str) else {
        return;
    };
    let cwd_path = Path::new(cwd);
    let tokens = record
        .get("argv")
        .and_then(serde_json::Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(serde_json::Value::as_str)
        .map(|token| token.split_once('=').map_or(token, |(_, value)| value))
        .filter(|token| !token.is_empty() && !token.starts_with('-'));
    let candidates = std::iter::once(cwd_path.to_path_buf())
        .chain(tokens.map(|token| cwd_path.join(token)))
        .filter(|path| path.is_dir());
    for dir in candidates {
        if let Some(root) = repo_root_for(&dir) {
            repos
                .entry(root.to_string_lossy().into_owned())
                .or_insert_with(|| cwd.to_string());
        }
    }
}

/// The main checkout that owns `dir`, from filesystem reads alone. A linked
/// worktree's `.git` file points at `<common>/worktrees/<name>`, whose
/// `commondir` names the shared `.git`; its parent is the main checkout. A
/// submodule's `.git` file points at a git dir with no `commondir`, so the
/// submodule checkout is its own root. `None` outside any repository, for a
/// bare repository, and for a linked worktree of a submodule (its common
/// dir is not a `.git`, so its checkout cannot be named without git).
pub(super) fn repo_root_for(dir: &Path) -> Option<PathBuf> {
    let dir = std::fs::canonicalize(dir).ok()?;
    for ancestor in dir.ancestors() {
        let dot_git = ancestor.join(".git");
        let Ok(meta) = std::fs::metadata(&dot_git) else {
            continue;
        };
        if meta.is_dir() {
            return Some(ancestor.to_path_buf());
        }
        return checkout_from_gitfile(ancestor, &dot_git);
    }
    None
}

fn checkout_from_gitfile(checkout: &Path, gitfile: &Path) -> Option<PathBuf> {
    let text = std::fs::read_to_string(gitfile).ok()?;
    let gitdir = checkout.join(text.lines().next()?.strip_prefix("gitdir:")?.trim());
    let Ok(common) = std::fs::read_to_string(gitdir.join("commondir")) else {
        return Some(checkout.to_path_buf());
    };
    let common = std::fs::canonicalize(gitdir.join(common.trim())).ok()?;
    if common.file_name()? != ".git" {
        return None;
    }
    common.parent().map(Path::to_path_buf)
}

#[cfg(test)]
#[path = "shim_hints_tests.rs"]
mod tests;
