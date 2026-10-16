//! Claude Code's worktrees, built under clud's worktree root (#1962, #1971).
//!
//! Claude Code puts the worktrees for `--worktree`, `EnterWorktree` and
//! `isolation: "worktree"` subagents in `<repo>/.claude/worktrees/<name>`,
//! unless a `WorktreeCreate` hook (Claude Code 2.1.50+) creates them instead.
//! clud registers this handler (see `clud_hooks_compile::worktree_hooks_fragment`),
//! so they land at `~/.clud/worktrees/<repo>-wt-<name>`, where the daemon
//! reclaims them once their work lands, and `.claude/worktrees/` is never
//! created. Behavior otherwise mirrors Claude Code's own: a new branch
//! `worktree-<name>` from `origin/<default>`, or from `HEAD` when the
//! `worktree.baseRef` setting is `"head"`.
//!
//! Contract (Claude Code hooks docs): stdin is JSON with `cwd` and `name`;
//! stdout's last non-empty line is the worktree's absolute path, so nothing
//! else may be printed there; a non-zero exit fails the creation.
//! `WorktreeRemove` gets `worktree_path` and must remove it; this handler
//! removes only worktrees under clud's root, and never with `--force`, so a
//! worktree holding uncommitted changes is kept.

use std::path::{Path, PathBuf};

use serde_json::Value;

pub const WORKTREE_CREATE_EVENT: &str = "WorktreeCreate";
pub const WORKTREE_REMOVE_EVENT: &str = "WorktreeRemove";

/// Where a new worktree branches from, per Claude Code's `worktree.baseRef`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum BaseRef {
    /// `origin/<default>` (Claude Code's default).
    Fresh,
    /// The current checkout's `HEAD`.
    Head,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct CreateRequest {
    pub(crate) cwd: PathBuf,
    pub(crate) name: String,
}

pub(crate) fn parse_create(stdin: &str) -> Result<CreateRequest, String> {
    let payload: Value =
        serde_json::from_str(stdin).map_err(|error| format!("unreadable payload: {error}"))?;
    let cwd = payload
        .get("cwd")
        .and_then(Value::as_str)
        .ok_or("the payload has no cwd")?;
    let name = payload
        .get("name")
        .and_then(Value::as_str)
        .unwrap_or("worktree");
    Ok(CreateRequest {
        cwd: PathBuf::from(cwd),
        name: name.to_string(),
    })
}

pub(crate) fn parse_remove(stdin: &str) -> Result<PathBuf, String> {
    let payload: Value =
        serde_json::from_str(stdin).map_err(|error| format!("unreadable payload: {error}"))?;
    payload
        .get("worktree_path")
        .and_then(Value::as_str)
        .map(PathBuf::from)
        .ok_or_else(|| "the payload has no worktree_path".to_string())
}

/// One path component safe on every platform: letters, digits, `.`, `_`
/// and `-`; anything else becomes `-`. Never empty, never a dot name.
pub(crate) fn sanitize_component(raw: &str) -> String {
    let cleaned: String = raw
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() || matches!(ch, '.' | '_' | '-') {
                ch
            } else {
                '-'
            }
        })
        .take(64)
        .collect();
    let trimmed = cleaned.trim_matches(['-', '.']);
    if trimmed.is_empty() {
        "worktree".to_string()
    } else {
        trimmed.to_string()
    }
}

/// `worktree.baseRef` from the first settings document that sets it, in
/// precedence order (local, project, user).
pub(crate) fn base_ref_from(settings: &[Value]) -> BaseRef {
    let chosen = settings.iter().find_map(|document| {
        document
            .get("worktree")
            .and_then(|worktree| worktree.get("baseRef"))
            .and_then(Value::as_str)
    });
    match chosen {
        Some("head") => BaseRef::Head,
        _ => BaseRef::Fresh,
    }
}

/// `worktree-<name>`, then `worktree-<name>-2`, ... for an existing branch.
pub(crate) fn branch_candidates(name: &str) -> impl Iterator<Item = String> + '_ {
    std::iter::once(format!("worktree-{name}"))
        .chain((2..=100).map(move |ordinal| format!("worktree-{name}-{ordinal}")))
}

/// The settings documents that may set `worktree.baseRef`, highest
/// precedence first. Unreadable or missing files are skipped.
fn settings_documents(repo: &Path) -> Vec<Value> {
    let mut paths = vec![
        repo.join(".claude").join("settings.local.json"),
        repo.join(".claude").join("settings.json"),
    ];
    if let Some(home) = crate::home::user_home() {
        paths.push(home.join(".claude").join("settings.json"));
    }
    paths
        .iter()
        .filter_map(|path| std::fs::read_to_string(path).ok())
        .filter_map(|text| serde_json::from_str(&text).ok())
        .collect()
}

fn git(cwd: &Path, args: &[&str]) -> Result<String, String> {
    crate::worktrees::run_git(cwd, args)
}

/// The commit a new worktree starts from.
fn base_commit(request: &CreateRequest, repo: &Path, base: BaseRef) -> String {
    let fresh = base == BaseRef::Fresh;
    let origin_default = fresh
        .then(|| {
            git(
                repo,
                &[
                    "symbolic-ref",
                    "--quiet",
                    "--short",
                    "refs/remotes/origin/HEAD",
                ],
            )
        })
        .and_then(Result::ok)
        .map(|name| name.trim().to_string())
        .filter(|name| !name.is_empty());
    origin_default.unwrap_or_else(|| {
        git(&request.cwd, &["rev-parse", "HEAD"])
            .map(|sha| sha.trim().to_string())
            .unwrap_or_else(|_| "HEAD".to_string())
    })
}

fn free_branch(repo: &Path, name: &str) -> Result<String, String> {
    branch_candidates(name)
        .find(|branch| {
            git(
                repo,
                &[
                    "rev-parse",
                    "--verify",
                    "--quiet",
                    &format!("refs/heads/{branch}"),
                ],
            )
            .is_err()
        })
        .ok_or_else(|| format!("every worktree-{name} branch name is taken"))
}

fn create(request: &CreateRequest) -> Result<PathBuf, String> {
    let root = crate::gc::worktree_root::worktree_root()
        .ok_or("no home directory, so no ~/.clud/worktrees")?;
    let repo = crate::worktrees::locate_main_repo_root_from(&request.cwd)?;
    create_in(request, &root, base_ref_from(&settings_documents(&repo)))
}

/// [`create`] under an explicit worktree `root` and base, for tests.
pub(crate) fn create_in(
    request: &CreateRequest,
    root: &Path,
    base: BaseRef,
) -> Result<PathBuf, String> {
    let repo = crate::worktrees::locate_main_repo_root_from(&request.cwd)?;
    let slug = sanitize_component(
        &repo
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_default(),
    );
    let name = sanitize_component(&request.name);
    let path = crate::gc::worktree_root::alloc_wt_path_in(root, &slug, &name)
        .map_err(|error| format!("cannot reserve a worktree under ~/.clud/worktrees: {error}"))?;
    let branch = free_branch(&repo, &name)?;
    let base = base_commit(request, &repo, base);
    let target = path.to_string_lossy().into_owned();
    git(&repo, &["worktree", "add", "-b", &branch, &target, &base])
        .map_err(|error| format!("git worktree add failed: {error}"))?;
    Ok(path)
}

/// Handle `WorktreeCreate`: print the new worktree's path, exit 0; or
/// explain on stderr and exit 1, which fails the creation.
pub fn run_create(stdin: &str) -> i32 {
    match parse_create(stdin).and_then(|request| create(&request)) {
        Ok(path) => {
            println!("{}", path.display());
            0
        }
        Err(error) => {
            eprintln!("[clud worktree] {error}");
            1
        }
    }
}

/// Handle `WorktreeRemove` for a worktree clud created.
pub fn run_remove(stdin: &str) -> i32 {
    let outcome = parse_remove(stdin).and_then(|path| {
        let home = crate::home::user_home().ok_or("no home directory, so no worktree root")?;
        if !removable(&path, &home) {
            return Err(format!(
                "{} is not under clud's worktree root; leaving it alone",
                path.display()
            ));
        }
        let repo = crate::worktrees::locate_main_repo_root_from(&path)?;
        git(&repo, &["worktree", "remove", &path.to_string_lossy()])
            .map(|_| ())
            .map_err(|error| format!("kept {}: {error}", path.display()))
    });
    match outcome {
        Ok(()) => 0,
        Err(error) => {
            eprintln!("[clud worktree] {error}");
            1
        }
    }
}

/// Whether `path` is one of clud's worktrees: under `~/.clud/worktrees`, or
/// under the old `~/.clud/tmp-wt` that the daemon is still moving entries
/// out of (#1962).
fn removable(path: &Path, home: &Path) -> bool {
    let roots = [
        crate::gc::worktree_root::worktree_root_for(home),
        home.join(".clud").join("tmp-wt"),
    ];
    crate::extern_root::is_within_any(path, &roots)
}

#[cfg(test)]
#[path = "claude_worktree_hook_tests.rs"]
mod tests;
