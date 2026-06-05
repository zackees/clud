//! Repo identity + scoping primitives (#267).
//!
//! `RepoScope` answers: which agent-memory bucket should this working tree
//! read and write? Primary key is the normalized `origin` URL; the
//! filesystem common-dir is the fallback for repos without a remote.
//! Branch is metadata, not a partition — cross-branch memory continuity
//! is the default (DD-014). Worktrees share scope via `git rev-parse
//! --git-common-dir`. Orphan branches share scope with main by default.
//!
//! The opt-out marker file lives at
//! `<common_dir>/.clud/memory-branch-isolate`. When present, the current
//! branch is treated as its own scope and `scope_key` gains a
//! `#branch=<name>` suffix.

use std::path::{Path, PathBuf};
use std::time::Duration;

use running_process::{
    CommandSpec, NativeProcess, ProcessConfig, ReadStatus, StderrMode, StdinMode,
};

use crate::memory::error::MemoryError;

/// Marker file (relative to `common_dir`) opting the current working tree
/// out of branch-as-metadata behavior. When present, the current branch
/// becomes its own scope partition.
pub const BRANCH_ISOLATE_MARKER: &str = ".clud/memory-branch-isolate";

/// Resolved scope for a working tree.
///
/// The `key` field is computed from `origin_url`/`common_dir` via
/// [`scope_key`]; expose both so callers can show the user *why* a given
/// key was selected (origin found vs. common-dir fallback) without
/// re-running git.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct RepoScope {
    pub key: String,
    pub origin_url: Option<String>,
    pub common_dir: PathBuf,
    pub branch: Option<String>,
    pub is_orphan: bool,
    pub is_worktree: bool,
    pub branch_isolated: bool,
}

/// Resolve the repo scope rooted at `cwd`.
///
/// Order:
/// 1. `git -C cwd rev-parse --git-common-dir` → canonical common dir
///    (handles worktrees: linked worktrees share their primary's common dir).
/// 2. `git -C cwd rev-parse --git-dir` → compared against the common dir to
///    detect a worktree.
/// 3. `git -C cwd remote get-url origin` → normalized via
///    [`normalize_origin_url`]; absent or empty → common-dir fallback.
/// 4. `git -C cwd symbolic-ref --short HEAD` → branch (None on detached HEAD).
/// 5. Orphan detection: `git -C cwd merge-base HEAD origin/HEAD` non-zero
///    AND `branch.is_some()` AND branch is not the resolved default branch.
/// 6. Branch-isolate marker at `<common_dir>/<BRANCH_ISOLATE_MARKER>`.
pub fn resolve_repo_scope(cwd: &Path) -> Result<RepoScope, MemoryError> {
    let common_dir_raw = run_git_capture(cwd, &["rev-parse", "--git-common-dir"])
        .ok_or_else(|| MemoryError::Migration(format!("not a git repo at {}", cwd.display())))?;
    let common_dir = canonicalize_relative_to(cwd, &common_dir_raw);

    let git_dir_raw = run_git_capture(cwd, &["rev-parse", "--git-dir"]).unwrap_or_default();
    let git_dir = canonicalize_relative_to(cwd, &git_dir_raw);
    let is_worktree = !git_dir_raw.is_empty() && git_dir != common_dir;

    let origin_url =
        run_git_capture(cwd, &["remote", "get-url", "origin"]).filter(|s| !s.is_empty());

    let branch =
        run_git_capture(cwd, &["symbolic-ref", "--short", "HEAD"]).filter(|s| !s.is_empty());

    let is_orphan = match branch.as_deref() {
        Some(b) => is_orphan_branch(cwd, b),
        None => false,
    };

    let branch_isolated = common_dir.join(BRANCH_ISOLATE_MARKER).exists();

    let scope = RepoScope {
        key: String::new(),
        origin_url,
        common_dir,
        branch,
        is_orphan,
        is_worktree,
        branch_isolated,
    };
    let key = scope_key(&scope);
    Ok(RepoScope { key, ..scope })
}

/// Normalize a `git remote get-url origin` value so equivalent SSH/HTTPS
/// pairs collapse to a single key.
///
/// Rules:
/// - Strip a trailing `.git` suffix.
/// - Trim trailing slashes.
/// - Lowercase the scheme and host **only** (the repo path stays
///   case-sensitive: GitHub treats `Foo/Bar` and `foo/bar` as equivalent
///   but other forges do not, and the path is the discriminating factor).
/// - Drop the default port for the detected scheme (`:22` for ssh,
///   `:443` for https, `:80` for http).
/// - Pass non-URL-shaped inputs through unchanged after the `.git`/slash
///   trim, so weird custom remotes don't get mangled.
pub fn normalize_origin_url(url: &str) -> String {
    let trimmed = url.trim();
    let no_git = trimmed.strip_suffix(".git").unwrap_or(trimmed);
    let no_slash = no_git.trim_end_matches('/');

    if let Some((scheme, rest)) = split_scheme(no_slash) {
        let scheme_lc = scheme.to_ascii_lowercase();
        let (host_port, path) = split_host_path(rest);
        let (host, port) = split_host_port(host_port);
        let host_lc = host.to_ascii_lowercase();
        let default_port = default_port_for(&scheme_lc);
        let host_port_norm = match port {
            Some(p) if Some(p) == default_port => host_lc,
            Some(p) => format!("{host_lc}:{p}"),
            None => host_lc,
        };
        return format!("{scheme_lc}://{host_port_norm}{path}");
    }

    if let Some((user_host, path)) = split_scp_like(no_slash) {
        let (user, host) = split_user_host(user_host);
        let host_lc = host.to_ascii_lowercase();
        let user_prefix = user.map(|u| format!("{u}@")).unwrap_or_default();
        return format!("ssh://{user_prefix}{host_lc}/{path}");
    }

    no_slash.to_string()
}

/// Compose the scope key used as the primary partition for memories.
///
/// - `repo://<normalized-origin>` when origin is present.
/// - `dir://<canonical-common-dir>` fallback.
/// - With `#branch=<name>` suffix when `branch_isolated == true` and a
///   branch is known.
pub fn scope_key(scope: &RepoScope) -> String {
    let base = match &scope.origin_url {
        Some(url) => format!("repo://{}", normalize_origin_url(url)),
        None => format!("dir://{}", scope.common_dir.display()),
    };
    if scope.branch_isolated {
        if let Some(b) = &scope.branch {
            return format!("{base}#branch={b}");
        }
    }
    base
}

/// Touch the branch-isolate marker file under `<common_dir>/.clud/`.
///
/// TODO(#262): the CLI verb `clud memory branch-isolate` belongs to the
/// CLI surface sub-issue and wires argv → this function.
pub fn branch_isolate(common_dir: &Path) -> Result<(), MemoryError> {
    let marker = common_dir.join(BRANCH_ISOLATE_MARKER);
    if let Some(parent) = marker.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(&marker, b"")?;
    Ok(())
}

/// Remove the branch-isolate marker file. No-op if absent.
///
/// TODO(#262): the CLI verb `clud memory branch-isolate --remove` belongs
/// to the CLI surface sub-issue and wires argv → this function.
pub fn branch_unisolate(common_dir: &Path) -> Result<(), MemoryError> {
    let marker = common_dir.join(BRANCH_ISOLATE_MARKER);
    match std::fs::remove_file(&marker) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(MemoryError::Io(e)),
    }
}

/// Build a predicate matching scope keys against a set of shell-style globs.
///
/// Empty `globs` matches nothing. Glob syntax: `*` matches any character
/// run (no path semantics — scope keys are not file paths), `?` matches a
/// single character, everything else is literal.
pub fn cross_repo_glob_filter(globs: &[String]) -> impl Fn(&str) -> bool + use<> {
    let patterns: Vec<String> = globs.to_vec();
    move |scope_key: &str| patterns.iter().any(|g| glob_matches(g, scope_key))
}

/// Run `git -C <cwd> <args...>` capturing stdout; return trimmed stdout or
/// `None` when git is missing or exits non-zero.
///
/// Uses `running_process::NativeProcess` per the workspace's banned-imports
/// rule. `gh_capture` in `loop_spec.rs` follows the same pattern.
fn run_git_capture(cwd: &Path, args: &[&str]) -> Option<String> {
    let (exit, out) = run_git(cwd, args)?;
    if exit != 0 {
        return None;
    }
    Some(out.trim().to_string())
}

fn is_orphan_branch(cwd: &Path, branch: &str) -> bool {
    let default = run_git_capture(
        cwd,
        &["symbolic-ref", "--short", "refs/remotes/origin/HEAD"],
    )
    .and_then(|s| s.strip_prefix("origin/").map(str::to_string));
    if default.as_deref() == Some(branch) {
        return false;
    }
    let head_against = match &default {
        Some(d) => format!("refs/remotes/origin/{d}"),
        // No `origin/HEAD` to compare against — fall back to "does HEAD
        // have any parent?" via `merge-base HEAD HEAD~1`. An orphan
        // branch's first commit has no parent so this fails.
        None => {
            return run_git(cwd, &["merge-base", "HEAD", "HEAD~1"])
                .map(|(exit, _)| exit != 0)
                .unwrap_or(false)
        }
    };
    run_git(cwd, &["merge-base", "HEAD", &head_against])
        .map(|(exit, _)| exit != 0)
        .unwrap_or(false)
}

fn run_git(cwd: &Path, args: &[&str]) -> Option<(i32, String)> {
    let mut argv: Vec<String> = vec![
        "git".to_string(),
        "-C".to_string(),
        cwd.display().to_string(),
    ];
    argv.extend(args.iter().map(|s| s.to_string()));
    let config = ProcessConfig {
        command: CommandSpec::Argv(argv),
        cwd: None,
        env: None,
        capture: true,
        stderr_mode: StderrMode::Stdout,
        creationflags: None,
        create_process_group: false,
        stdin_mode: StdinMode::Null,
        nice: None,
    };
    let process = NativeProcess::new(config);
    process.start().ok()?;
    let mut buf = Vec::<u8>::new();
    loop {
        match process.read_combined(Some(Duration::from_millis(50))) {
            ReadStatus::Line(event) => {
                buf.extend_from_slice(&event.line);
                buf.push(b'\n');
            }
            ReadStatus::Timeout => {
                if process.returncode().is_some() {
                    break;
                }
            }
            ReadStatus::Eof => break,
        }
    }
    let exit = process.wait(Some(Duration::from_secs(30))).ok()?;
    Some((exit, String::from_utf8_lossy(&buf).to_string()))
}

fn canonicalize_relative_to(cwd: &Path, raw: &str) -> PathBuf {
    let p = PathBuf::from(raw);
    let absolute = if p.is_absolute() { p } else { cwd.join(&p) };
    absolute.canonicalize().unwrap_or(absolute)
}

fn split_scheme(s: &str) -> Option<(&str, &str)> {
    let idx = s.find("://")?;
    let scheme = &s[..idx];
    if scheme.is_empty()
        || !scheme
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '+' || c == '-' || c == '.')
    {
        return None;
    }
    Some((scheme, &s[idx + 3..]))
}

fn split_host_path(s: &str) -> (&str, &str) {
    match s.find('/') {
        Some(i) => (&s[..i], &s[i..]),
        None => (s, ""),
    }
}

fn split_host_port(s: &str) -> (&str, Option<u16>) {
    if let Some(i) = s.rfind(':') {
        if let Ok(p) = s[i + 1..].parse::<u16>() {
            return (&s[..i], Some(p));
        }
    }
    (s, None)
}

fn split_user_host(s: &str) -> (Option<&str>, &str) {
    match s.find('@') {
        Some(i) => (Some(&s[..i]), &s[i + 1..]),
        None => (None, s),
    }
}

fn split_scp_like(s: &str) -> Option<(&str, &str)> {
    // SCP-like SSH: `[user@]host:path` — colon present, but no `://`.
    // Skip Windows-style `C:\...` paths (single ASCII letter before colon).
    let colon = s.find(':')?;
    if s.contains("://") {
        return None;
    }
    let host = &s[..colon];
    let path = &s[colon + 1..];
    if host.is_empty() || path.is_empty() {
        return None;
    }
    if host.len() == 1 && host.chars().next().unwrap().is_ascii_alphabetic() {
        return None;
    }
    Some((host, path))
}

fn default_port_for(scheme: &str) -> Option<u16> {
    match scheme {
        "ssh" | "git+ssh" => Some(22),
        "https" => Some(443),
        "http" => Some(80),
        "git" => Some(9418),
        _ => None,
    }
}

fn glob_matches(pattern: &str, text: &str) -> bool {
    let pat_chars: Vec<char> = pattern.chars().collect();
    let txt_chars: Vec<char> = text.chars().collect();
    glob_match_rec(&pat_chars, 0, &txt_chars, 0)
}

fn glob_match_rec(pat: &[char], pi: usize, txt: &[char], ti: usize) -> bool {
    if pi == pat.len() {
        return ti == txt.len();
    }
    match pat[pi] {
        '*' => {
            // Collapse runs of '*' for tail recursion-ish behavior.
            let mut np = pi;
            while np < pat.len() && pat[np] == '*' {
                np += 1;
            }
            if np == pat.len() {
                return true;
            }
            for next_ti in ti..=txt.len() {
                if glob_match_rec(pat, np, txt, next_ti) {
                    return true;
                }
            }
            false
        }
        '?' => {
            if ti == txt.len() {
                false
            } else {
                glob_match_rec(pat, pi + 1, txt, ti + 1)
            }
        }
        c => {
            if ti < txt.len() && txt[ti] == c {
                glob_match_rec(pat, pi + 1, txt, ti + 1)
            } else {
                false
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;
    use tempfile::TempDir;

    /// Test-only helper to run `git -C <cwd> <args>` and assert success.
    /// Routes through the same `NativeProcess` plumbing as production code.
    fn must_git(cwd: &Path, args: &[&str]) {
        let (exit, out) = run_git(cwd, args).expect("git binary missing");
        assert!(
            exit == 0,
            "git {args:?} in {} exited {exit}; stdout/stderr:\n{out}",
            cwd.display()
        );
    }

    fn git_init(dir: &Path) {
        must_git(dir, &["init", "-q", "-b", "main"]);
        must_git(dir, &["config", "user.email", "test@example.com"]);
        must_git(dir, &["config", "user.name", "Test"]);
        must_git(dir, &["commit", "-q", "--allow-empty", "-m", "init"]);
    }

    #[test]
    fn normalize_origin_url_strips_dot_git() {
        assert_eq!(
            normalize_origin_url("https://github.com/zackees/clud.git"),
            "https://github.com/zackees/clud"
        );
    }

    #[test]
    fn normalize_origin_url_lowercases_scheme_and_host_only() {
        // Host lowercased; path preserved as-is.
        assert_eq!(
            normalize_origin_url("HTTPS://GitHub.com/Zackees/Clud"),
            "https://github.com/Zackees/Clud"
        );
    }

    #[test]
    fn normalize_origin_url_drops_default_ports() {
        assert_eq!(
            normalize_origin_url("https://github.com:443/foo/bar"),
            "https://github.com/foo/bar"
        );
        assert_eq!(
            normalize_origin_url("http://example.com:80/foo/bar"),
            "http://example.com/foo/bar"
        );
        assert_eq!(
            normalize_origin_url("ssh://git@github.com:22/foo/bar.git"),
            "ssh://git@github.com/foo/bar"
        );
    }

    #[test]
    fn normalize_origin_url_preserves_non_default_ports() {
        assert_eq!(
            normalize_origin_url("https://gitlab.example.com:8443/foo/bar"),
            "https://gitlab.example.com:8443/foo/bar"
        );
    }

    #[test]
    fn normalize_origin_url_trims_trailing_slash() {
        assert_eq!(
            normalize_origin_url("https://github.com/zackees/clud/"),
            "https://github.com/zackees/clud"
        );
        assert_eq!(
            normalize_origin_url("https://github.com/zackees/clud///"),
            "https://github.com/zackees/clud"
        );
    }

    #[test]
    fn normalize_origin_url_handles_scp_like_ssh() {
        // SCP-style ssh remote: user@host:path with no scheme prefix.
        assert_eq!(
            normalize_origin_url("git@github.com:zackees/clud.git"),
            "ssh://git@github.com/zackees/clud"
        );
    }

    #[test]
    fn resolve_repo_scope_uses_origin_when_present() {
        let tmp = TempDir::new().unwrap();
        let repo = tmp.path();
        git_init(repo);
        must_git(
            repo,
            &["remote", "add", "origin", "git@github.com:foo/bar.git"],
        );
        let scope = resolve_repo_scope(repo).unwrap();
        assert_eq!(
            scope.origin_url.as_deref(),
            Some("git@github.com:foo/bar.git")
        );
        assert!(scope.key.starts_with("repo://"));
        assert!(
            scope.key.contains("github.com/foo/bar"),
            "got key {}",
            scope.key
        );
        assert!(!scope.is_worktree);
        assert!(!scope.branch_isolated);
    }

    #[test]
    fn resolve_repo_scope_falls_back_to_common_dir_when_no_origin() {
        let tmp = TempDir::new().unwrap();
        let repo = tmp.path();
        git_init(repo);
        let scope = resolve_repo_scope(repo).unwrap();
        assert!(scope.origin_url.is_none());
        assert!(scope.key.starts_with("dir://"), "got key {}", scope.key);
    }

    #[test]
    fn resolve_repo_scope_detects_worktree() {
        let tmp = TempDir::new().unwrap();
        let primary = tmp.path().join("primary");
        std::fs::create_dir(&primary).unwrap();
        git_init(&primary);
        // Create a branch to point the worktree at.
        must_git(&primary, &["branch", "wt-branch"]);
        let wt = tmp.path().join("wt");
        must_git(
            &primary,
            &["worktree", "add", wt.to_str().unwrap(), "wt-branch"],
        );
        let primary_scope = resolve_repo_scope(&primary).unwrap();
        let wt_scope = resolve_repo_scope(&wt).unwrap();
        assert!(
            !primary_scope.is_worktree,
            "primary should not be a worktree"
        );
        assert!(wt_scope.is_worktree, "linked checkout should be a worktree");
        assert_eq!(
            primary_scope.common_dir, wt_scope.common_dir,
            "worktree must share common_dir with primary"
        );
        assert_eq!(
            primary_scope.key, wt_scope.key,
            "worktree must share scope key with primary"
        );
    }

    #[test]
    fn resolve_repo_scope_detects_orphan_branch() {
        let tmp = TempDir::new().unwrap();
        let repo = tmp.path();
        git_init(repo);
        // Fake an `origin/HEAD` symbolic ref pointing at main so the
        // orphan check has a baseline; the fake remote ref is just a
        // pointer to the local main commit.
        must_git(repo, &["update-ref", "refs/remotes/origin/main", "HEAD"]);
        must_git(
            repo,
            &[
                "symbolic-ref",
                "refs/remotes/origin/HEAD",
                "refs/remotes/origin/main",
            ],
        );
        must_git(repo, &["checkout", "-q", "--orphan", "ob"]);
        must_git(repo, &["commit", "-q", "--allow-empty", "-m", "orphan"]);
        let scope = resolve_repo_scope(repo).unwrap();
        assert_eq!(scope.branch.as_deref(), Some("ob"));
        assert!(scope.is_orphan, "orphan branch must be detected as orphan");
    }

    #[test]
    fn resolve_repo_scope_honors_branch_isolate_marker() {
        let tmp = TempDir::new().unwrap();
        let repo = tmp.path();
        git_init(repo);
        must_git(
            repo,
            &["remote", "add", "origin", "https://example.com/x/y.git"],
        );
        let common_dir = repo.join(".git");
        branch_isolate(&common_dir).unwrap();
        let scope = resolve_repo_scope(repo).unwrap();
        assert!(scope.branch_isolated);
        assert!(
            scope.key.contains("#branch="),
            "isolated scope key must include branch suffix; got {}",
            scope.key
        );
    }

    #[test]
    fn scope_key_format_with_origin() {
        let scope = RepoScope {
            key: String::new(),
            origin_url: Some("git@github.com:foo/bar.git".to_string()),
            common_dir: PathBuf::from("/x/.git"),
            branch: Some("main".to_string()),
            is_orphan: false,
            is_worktree: false,
            branch_isolated: false,
        };
        assert_eq!(scope_key(&scope), "repo://ssh://git@github.com/foo/bar");
    }

    #[test]
    fn scope_key_format_with_common_dir_fallback() {
        let scope = RepoScope {
            key: String::new(),
            origin_url: None,
            common_dir: PathBuf::from("/tmp/repo/.git"),
            branch: Some("main".to_string()),
            is_orphan: false,
            is_worktree: false,
            branch_isolated: false,
        };
        let key = scope_key(&scope);
        assert!(key.starts_with("dir://"), "got {key}");
        assert!(key.contains("repo"), "got {key}");
    }

    #[test]
    fn scope_key_format_includes_branch_when_isolated() {
        let scope = RepoScope {
            key: String::new(),
            origin_url: Some("https://github.com/foo/bar.git".to_string()),
            common_dir: PathBuf::from("/x/.git"),
            branch: Some("feature/x".to_string()),
            is_orphan: false,
            is_worktree: false,
            branch_isolated: true,
        };
        let key = scope_key(&scope);
        assert!(key.ends_with("#branch=feature/x"), "got {key}");
        assert!(key.starts_with("repo://https://github.com/foo/bar"));
    }

    #[test]
    fn cross_repo_glob_filter_matches_glob() {
        let f = cross_repo_glob_filter(&["repo://*github.com/zackees/*".to_string()]);
        assert!(f("repo://https://github.com/zackees/clud"));
        assert!(f("repo://ssh://git@github.com/zackees/other"));
        assert!(!f("repo://https://gitlab.example.com/zackees/clud"));
        assert!(!f("dir:///tmp/repo"));
    }

    #[test]
    fn cross_repo_glob_filter_empty_matches_nothing() {
        let f = cross_repo_glob_filter(&[]);
        assert!(!f("repo://anything"));
    }

    #[test]
    fn branch_isolate_and_unisolate_roundtrip() {
        let tmp = TempDir::new().unwrap();
        let common_dir = tmp.path();
        let marker = common_dir.join(BRANCH_ISOLATE_MARKER);
        assert!(!marker.exists());
        branch_isolate(common_dir).unwrap();
        assert!(marker.exists());
        branch_unisolate(common_dir).unwrap();
        assert!(!marker.exists());
        // Idempotent removal.
        branch_unisolate(common_dir).unwrap();
    }
}
