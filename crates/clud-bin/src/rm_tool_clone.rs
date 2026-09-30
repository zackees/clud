//! A clone of an allowed repo outside the roots (#1573).
//!
//! `safe-rm` may remove a directory outside its roots when that directory is a
//! separate clone of an allowed repository **and** nothing in it would be
//! lost: its `origin` matches an allowed root's `origin`, it has no modified or
//! untracked files, no local branch holds commits missing from every remote
//! ref, and it has no stash. [`verdict`] decides that over [`CloneFacts`];
//! [`probe`] gathers the facts with git.

use std::path::Path;

/// What git says about a candidate clone.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct CloneFacts {
    /// `remote.origin.url` of the candidate, if set.
    pub origin: Option<String>,
    /// `remote.origin.url` of every allowed root that is a checkout.
    pub allowed_origins: Vec<String>,
    /// `git status --porcelain` (untracked included, ignored excluded) is non-empty.
    pub dirty: bool,
    /// Local branches holding commits no `refs/remotes/*` reaches.
    pub unpushed_branches: Vec<String>,
    /// Entries in `git stash list`.
    pub stashes: usize,
}

/// `Ok` when the clone may be removed, else the failing check.
pub(crate) fn verdict(facts: &CloneFacts) -> Result<(), String> {
    let Some(origin) = facts.origin.as_deref() else {
        return Err("origin mismatch (it has no origin)".into());
    };
    let origin = normalize_origin(origin);
    if !facts
        .allowed_origins
        .iter()
        .any(|allowed| normalize_origin(allowed) == origin)
    {
        return Err("origin mismatch".into());
    }
    if facts.dirty {
        return Err("dirty (modified or untracked files)".into());
    }
    if !facts.unpushed_branches.is_empty() {
        let branches = facts.unpushed_branches.join(", ");
        return Err(format!("unpushed commits on {branches}"));
    }
    if facts.stashes > 0 {
        return Err(format!("stash ({} entries)", facts.stashes));
    }
    Ok(())
}

/// A remote URL reduced to `host/owner/repo` (lowercase) so the scheme, a
/// user, a port, a trailing `.git` or `/`, and ssh-vs-https spellings all
/// compare equal. A local path keeps its case and only loses the suffixes.
pub(crate) fn normalize_origin(url: &str) -> String {
    let url = url.trim();
    let strip = |s: &str| -> String {
        let s = s.trim_end_matches('/');
        let s = s.strip_suffix(".git").unwrap_or(s);
        s.trim_end_matches('/').to_string()
    };
    let remote = |host: &str, path: &str| -> String {
        let host = host.rsplit('@').next().unwrap_or(host);
        let host = host.split(':').next().unwrap_or(host);
        let path = path.trim_start_matches('/').trim_start_matches('~');
        format!("{host}/{}", strip(path)).to_ascii_lowercase()
    };
    if let Some((scheme, rest)) = url.split_once("://") {
        if scheme.eq_ignore_ascii_case("file") {
            return strip(rest);
        }
        let (host, path) = rest.split_once('/').unwrap_or((rest, ""));
        return remote(host, path);
    }
    // scp-like `[user@]host:path`, but not a Windows drive (`C:\x`) and not a
    // local path whose first component holds a colon.
    if let Some((host, path)) = url.split_once(':') {
        let drive = host.len() == 1 && host.chars().all(|c| c.is_ascii_alphabetic());
        if !drive && !host.is_empty() && !host.contains(['/', '\\']) {
            return remote(host, path);
        }
    }
    strip(url)
}

/// The facts for the clone at `dir`, reading origins lazily: the other checks
/// run only when the origin matches. A git failure is an error, never a pass.
pub(crate) fn probe(dir: &Path, allowed_origins: &[String]) -> Result<CloneFacts, String> {
    let git = |args: &[&str]| crate::worktrees::run_git(dir, args);
    let mut facts = CloneFacts {
        origin: git(&["config", "--get", "remote.origin.url"])
            .ok()
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty()),
        allowed_origins: allowed_origins.to_vec(),
        ..CloneFacts::default()
    };
    if verdict(&facts).is_err_and(|reason| reason.starts_with("origin mismatch")) {
        return Ok(facts);
    }
    let status = git(&[
        "status",
        "--porcelain",
        "--untracked-files=all",
        "--ignored=no",
    ])?;
    facts.dirty = !status.trim().is_empty();
    let branches = git(&["for-each-ref", "--format=%(refname)", "refs/heads"])?;
    for branch in branches.lines().map(str::trim).filter(|b| !b.is_empty()) {
        let count = git(&["rev-list", "--count", branch, "--not", "--remotes"])?;
        if count.trim() != "0" {
            let short = branch.strip_prefix("refs/heads/").unwrap_or(branch);
            facts.unpushed_branches.push(short.to_string());
        }
    }
    let stash = git(&["stash", "list"])?;
    facts.stashes = stash.lines().filter(|l| !l.trim().is_empty()).count();
    Ok(facts)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn clean() -> CloneFacts {
        CloneFacts {
            origin: Some("git@github.com:Zackees/Clud.git".into()),
            allowed_origins: vec!["https://github.com/zackees/clud".into()],
            ..CloneFacts::default()
        }
    }

    #[test]
    fn origins_normalize_across_spellings() {
        let want = "github.com/zackees/clud";
        for url in [
            "https://github.com/zackees/clud",
            "https://github.com/zackees/clud.git",
            "https://github.com/zackees/clud/",
            "https://user@GitHub.com/ZACKEES/clud.git",
            "git@github.com:zackees/clud.git",
            "ssh://git@github.com:22/zackees/clud.git",
            "ssh://git@github.com/zackees/clud",
            "git://github.com/zackees/clud.git",
        ] {
            assert_eq!(normalize_origin(url), want, "{url}");
        }
        assert_ne!(
            normalize_origin("git@github.com:zackees/clud-other.git"),
            want
        );
        assert_ne!(normalize_origin("git@gitlab.com:zackees/clud.git"), want);
        assert_eq!(normalize_origin("/srv/Repos/x.git/"), "/srv/Repos/x");
        assert_eq!(normalize_origin("file:///srv/Repos/x.git"), "/srv/Repos/x");
        assert_eq!(normalize_origin(r"C:\repos\x.git"), r"C:\repos\x");
    }

    #[test]
    fn a_clean_matching_clone_is_allowed() {
        assert_eq!(verdict(&clean()), Ok(()));
    }

    #[test]
    fn each_failing_check_is_named() {
        let cases: Vec<(CloneFacts, &str)> = vec![
            (
                CloneFacts {
                    origin: None,
                    ..clean()
                },
                "origin mismatch",
            ),
            (
                CloneFacts {
                    origin: Some("git@github.com:someone/else.git".into()),
                    ..clean()
                },
                "origin mismatch",
            ),
            (
                CloneFacts {
                    allowed_origins: vec![],
                    ..clean()
                },
                "origin mismatch",
            ),
            (
                CloneFacts {
                    dirty: true,
                    ..clean()
                },
                "dirty",
            ),
            (
                CloneFacts {
                    unpushed_branches: vec!["main".into(), "feat".into()],
                    ..clean()
                },
                "unpushed commits on main, feat",
            ),
            (
                CloneFacts {
                    stashes: 2,
                    ..clean()
                },
                "stash",
            ),
        ];
        for (facts, want) in cases {
            let reason = verdict(&facts).unwrap_err();
            assert!(reason.starts_with(want), "{facts:?}: {reason}");
        }
    }

    #[test]
    fn a_mismatched_origin_is_reported_before_local_state() {
        let facts = CloneFacts {
            origin: Some("https://example.com/other/repo".into()),
            dirty: true,
            stashes: 1,
            ..clean()
        };
        assert!(verdict(&facts).unwrap_err().starts_with("origin mismatch"));
    }
}
