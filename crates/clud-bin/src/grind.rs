//! `clud grind` — derive the current repo's issues page and inject a `/loop`
//! prompt into one ordinary interactive PTY session.
//!
//! `clud grind` with no argument resolves the git `origin` remote, maps it to
//! the forge's issues page (`<repo>/issues` for GitHub, `<repo>/-/issues` for
//! GitLab), prints a green notice, and inserts the resulting `/loop` prompt
//! into that session. An explicit URL argument is used verbatim.
//!
//! `grind` deliberately does **not** reuse `clud do <target>`'s `/goal` flow.
//! Its harness-owned `/loop` prompt works through the issues page inside the
//! interactive session. The intended contract is documented in
//! [`docs/architecture/grind.md`](../../../docs/architecture/grind.md): clud
//! must neither relaunch the harness nor use DONE/BLOCKED markers, a 200-turn
//! cap, or a headless prompt path. The current implementation still does those
//! things for some backends; that is a legacy runtime defect pending correction,
//! not this module's directive.

use std::path::Path;
use std::time::Duration;

use running_process::ReadStatus;

use crate::subprocess::ManagedSubprocess;
use crate::win_creation_flags::invisible_helper_creationflags;

/// Split a git remote URL into `(host, repo_path)`.
///
/// Accepts both the SSH scp-like form (`git@github.com:owner/repo.git`) and
/// scheme URLs (`https://github.com/owner/repo.git`, `ssh://git@host/owner/repo`).
/// The returned `host` has any `user@` and `:port` stripped; `repo_path` is the
/// remainder verbatim (trailing `.git` / `/` are trimmed by the caller).
fn split_remote(remote: &str) -> Option<(String, String)> {
    let remote = remote.trim();

    // scheme://[user@]host[:port]/path
    if let Some(idx) = remote.find("://") {
        let after = &remote[idx + 3..];
        // Drop optional `user@` userinfo.
        let after = after.rsplit('@').next().unwrap_or(after);
        let (authority, path) = after.split_once('/')?;
        let host = authority.split(':').next()?.trim();
        if host.is_empty() || path.is_empty() {
            return None;
        }
        return Some((host.to_string(), path.to_string()));
    }

    // scp-like: [user@]host:path (no scheme). Require the authority to be free
    // of `/` so a bare Windows path like `C:\repo` is not misread as a remote.
    if let Some((authority, path)) = remote.split_once(':') {
        if !authority.contains('/') && !path.is_empty() {
            let host = authority.rsplit('@').next().unwrap_or(authority).trim();
            if !host.is_empty() {
                return Some((host.to_string(), path.to_string()));
            }
        }
    }

    None
}

/// Map a git remote URL to its forge issues page.
///
/// GitHub issues live at `<repo>/issues`; GitLab nests them under
/// `<repo>/-/issues`. Enterprise hosts are matched loosely (`github` /
/// `gitlab` anywhere in the hostname). Any other host is rejected with a
/// user-facing message.
pub fn issues_url_from_remote(remote: &str) -> Result<String, String> {
    let (host, path) =
        split_remote(remote).ok_or_else(|| format!("could not parse git remote URL: {remote}"))?;
    let repo_path = path.trim_end_matches('/').trim_end_matches(".git");
    if repo_path.is_empty() {
        return Err(format!("git remote URL has no repository path: {remote}"));
    }
    let web = format!("https://{host}/{repo_path}");
    let host_lower = host.to_ascii_lowercase();
    if host_lower.contains("gitlab") {
        Ok(format!("{web}/-/issues"))
    } else if host_lower.contains("github") {
        Ok(format!("{web}/issues"))
    } else {
        Err(format!(
            "remote host `{host}` is not GitHub or GitLab; `clud grind` needs a \
             GitHub or GitLab remote, or pass an explicit issues URL"
        ))
    }
}

/// Read the `origin` remote URL from the git repo containing `cwd`.
/// Returns `None` when git fails, there is no `origin`, or the value is blank.
pub fn git_remote_url(cwd: &Path) -> Option<String> {
    let argv = vec![
        "git".to_string(),
        "-C".to_string(),
        cwd.to_string_lossy().to_string(),
        "config".to_string(),
        "--get".to_string(),
        "remote.origin.url".to_string(),
    ];
    let (code, out) = run_captured(argv).ok()?;
    if code != 0 {
        return None;
    }
    let url = out.trim();
    if url.is_empty() {
        None
    } else {
        Some(url.to_string())
    }
}

/// Resolve the URL `clud grind` inserts into its interactive `/loop` prompt.
///
/// An explicit `url` is returned verbatim (parity with `clud do`). Otherwise
/// the `origin` remote in `cwd` is mapped to its issues page. `Err` carries a
/// user-facing message when no forge remote can be resolved.
pub fn resolve_grind_target(cwd: &Path, url: Option<&str>) -> Result<String, String> {
    if let Some(url) = url {
        return Ok(url.to_string());
    }
    let remote = git_remote_url(cwd).ok_or_else(|| {
        "no git `origin` remote found; run `clud grind` inside a GitHub or GitLab \
         repository, or pass an explicit issues URL"
            .to_string()
    })?;
    issues_url_from_remote(&remote)
}

/// Green (when `color`), stderr-facing notice announcing the page about to be
/// ground. Mirrors the other `[clud]` launch notices.
pub fn grind_notice(url: &str, color: bool) -> String {
    if color {
        format!("\x1b[32m[clud] accessing {url} and grinding\x1b[0m")
    } else {
        format!("[clud] accessing {url} and grinding")
    }
}

fn run_captured(argv: Vec<String>) -> Result<(i32, String), String> {
    let process = ManagedSubprocess::start_inheriting_env(
        argv,
        None,
        true,
        invisible_helper_creationflags(),
    )?;
    let mut buf = Vec::<u8>::new();
    loop {
        match process.read_stdout(Some(Duration::from_millis(100))) {
            ReadStatus::Line(line) => buf.extend_from_slice(&line),
            ReadStatus::Timeout => {
                let _ = process.poll();
            }
            ReadStatus::Eof => break,
        }
    }
    let code = process.wait(Some(Duration::from_secs(10)))?;
    Ok((code, String::from_utf8_lossy(&buf).into_owned()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn github_https_maps_to_issues() {
        assert_eq!(
            issues_url_from_remote("https://github.com/zackees/clud.git").unwrap(),
            "https://github.com/zackees/clud/issues"
        );
    }

    #[test]
    fn github_https_without_git_suffix() {
        assert_eq!(
            issues_url_from_remote("https://github.com/zackees/clud").unwrap(),
            "https://github.com/zackees/clud/issues"
        );
    }

    #[test]
    fn github_ssh_scp_form() {
        assert_eq!(
            issues_url_from_remote("git@github.com:zackees/clud.git").unwrap(),
            "https://github.com/zackees/clud/issues"
        );
    }

    #[test]
    fn gitlab_https_nests_under_dash_issues() {
        assert_eq!(
            issues_url_from_remote("https://gitlab.com/group/proj.git").unwrap(),
            "https://gitlab.com/group/proj/-/issues"
        );
    }

    #[test]
    fn gitlab_ssh_scp_form() {
        assert_eq!(
            issues_url_from_remote("git@gitlab.com:group/proj.git").unwrap(),
            "https://gitlab.com/group/proj/-/issues"
        );
    }

    #[test]
    fn gitlab_subgroups_are_preserved() {
        assert_eq!(
            issues_url_from_remote("https://gitlab.com/group/sub/proj.git").unwrap(),
            "https://gitlab.com/group/sub/proj/-/issues"
        );
    }

    #[test]
    fn ssh_scheme_url_is_supported() {
        assert_eq!(
            issues_url_from_remote("ssh://git@github.com/zackees/clud.git").unwrap(),
            "https://github.com/zackees/clud/issues"
        );
    }

    #[test]
    fn enterprise_github_host_matches() {
        assert_eq!(
            issues_url_from_remote("git@github.example.com:team/repo.git").unwrap(),
            "https://github.example.com/team/repo/issues"
        );
    }

    #[test]
    fn self_hosted_gitlab_host_matches() {
        assert_eq!(
            issues_url_from_remote("https://gitlab.example.com/team/repo.git").unwrap(),
            "https://gitlab.example.com/team/repo/-/issues"
        );
    }

    #[test]
    fn unknown_host_is_rejected() {
        let err = issues_url_from_remote("git@bitbucket.org:team/repo.git").unwrap_err();
        assert!(err.contains("not GitHub or GitLab"), "{err}");
    }

    #[test]
    fn resolve_with_explicit_url_is_verbatim() {
        let out = resolve_grind_target(
            Path::new("."),
            Some("https://github.com/zackees/clud/issues/1"),
        )
        .unwrap();
        assert_eq!(out, "https://github.com/zackees/clud/issues/1");
    }

    #[test]
    fn notice_is_green_only_when_colored() {
        let colored = grind_notice("https://x/issues", true);
        assert!(colored.starts_with("\x1b[32m") && colored.ends_with("\x1b[0m"));
        assert_eq!(
            grind_notice("https://x/issues", false),
            "[clud] accessing https://x/issues and grinding"
        );
    }
}
