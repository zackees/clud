use std::path::Path;
use std::time::{Duration, SystemTime};

use running_process::{NativeProcess, ProcessConfig, ReadStatus, StderrMode, StdinMode};

use crate::gc::TrackedEntry;
use crate::subprocess;
use crate::win_creation_flags::invisible_helper_creationflags;
use crate::worktrees;

#[cfg(test)]
use super::ENV_TEST_GH_BIN;
use super::{DEFAULT_EXTERN_REPO_STALE_AFTER_SECS, ENV_GC_EXTERN_REPO_MAX_AGE_SECS};

pub(super) fn extern_repo_stale_after() -> Duration {
    let secs = std::env::var(ENV_GC_EXTERN_REPO_MAX_AGE_SECS)
        .ok()
        .and_then(|raw| raw.trim().parse::<u64>().ok())
        .unwrap_or(DEFAULT_EXTERN_REPO_STALE_AFTER_SECS);
    Duration::from_secs(secs)
}

pub(super) fn extern_repo_is_purgeable(entry: &TrackedEntry, stale_after: Duration) -> bool {
    let path = Path::new(&entry.path);
    if !path.is_dir() {
        return false;
    }
    let Some(mtime) = most_recent_mtime(path) else {
        return false;
    };
    let Ok(age) = SystemTime::now().duration_since(mtime) else {
        return false;
    };
    if age < stale_after {
        return false;
    }
    let Some(branch) = entry
        .branch
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .or_else(|| crate::gc::best_effort_branch(path))
    else {
        return false;
    };
    let Some(slug) = repo_slug_for_extern_repo(path) else {
        return false;
    };
    gh_pr_list_reports_merged(&branch, &slug)
}

fn most_recent_mtime(path: &Path) -> Option<SystemTime> {
    let metadata = std::fs::symlink_metadata(path).ok()?;
    let mut latest = metadata.modified().ok()?;
    if metadata.is_dir() {
        let entries = std::fs::read_dir(path).ok()?;
        for entry in entries.flatten() {
            if let Some(child_mtime) = most_recent_mtime(&entry.path()) {
                if child_mtime > latest {
                    latest = child_mtime;
                }
            }
        }
    }
    Some(latest)
}

fn repo_slug_for_extern_repo(path: &Path) -> Option<String> {
    let remote = worktrees::run_git(path, &["remote", "get-url", "origin"]).ok()?;
    parse_github_slug_from_remote_url(&remote)
}

pub(super) fn parse_github_slug_from_remote_url(remote: &str) -> Option<String> {
    let s = remote.trim().trim_end_matches('/');
    if let Some(rest) = s.strip_prefix("git@github.com:") {
        return slug_from_github_path(rest);
    }
    for prefix in [
        "https://github.com/",
        "http://github.com/",
        "ssh://git@github.com/",
        "git://github.com/",
    ] {
        if let Some(rest) = s.strip_prefix(prefix) {
            return slug_from_github_path(rest);
        }
    }
    None
}

fn slug_from_github_path(path: &str) -> Option<String> {
    let clean = path.trim().trim_matches('/').trim_end_matches(".git");
    let mut parts = clean.split('/');
    let owner = parts.next()?.trim();
    let repo = parts.next()?.trim().trim_end_matches(".git");
    if owner.is_empty() || repo.is_empty() {
        return None;
    }
    Some(format!("{owner}/{repo}"))
}

fn gh_pr_list_reports_merged(branch: &str, slug: &str) -> bool {
    let args = vec![
        "pr".to_string(),
        "list".to_string(),
        "--head".to_string(),
        branch.to_string(),
        "--state".to_string(),
        "all".to_string(),
        "--json".to_string(),
        "mergedAt,url".to_string(),
        "--repo".to_string(),
        slug.to_string(),
    ];
    let Ok((exit_code, stdout)) = run_gh_capture(&args) else {
        return false;
    };
    exit_code == 0 && gh_pr_list_json_has_merged(&stdout)
}

pub(super) fn gh_pr_list_json_has_merged(stdout: &str) -> bool {
    let Ok(value) = serde_json::from_str::<serde_json::Value>(stdout) else {
        return false;
    };
    value
        .as_array()
        .map(|items| {
            items.iter().any(|item| {
                item.get("mergedAt")
                    .and_then(|merged_at| merged_at.as_str())
                    .map(|s| !s.trim().is_empty())
                    .unwrap_or(false)
            })
        })
        .unwrap_or(false)
}

fn run_gh_capture(args: &[String]) -> Result<(i32, String), String> {
    let mut argv = vec![gh_program()];
    argv.extend(args.iter().cloned());
    let config = ProcessConfig {
        command: subprocess::command_spec_for_subprocess(argv),
        cwd: None,
        env: None,
        capture: true,
        stderr_mode: StderrMode::Stdout,
        creationflags: invisible_helper_creationflags(),
        create_process_group: false,
        stdin_mode: StdinMode::Null,
        nice: None,
    };
    let process = NativeProcess::new(config);
    process
        .start()
        .map_err(|e| format!("failed to start gh: {e}"))?;

    let mut buf = Vec::<u8>::new();
    loop {
        match process.read_combined(Some(Duration::from_millis(100))) {
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
    let exit_code = process
        .wait(Some(Duration::from_secs(30)))
        .map_err(|e| format!("waiting for gh: {e}"))?;
    Ok((exit_code, String::from_utf8_lossy(&buf).to_string()))
}

fn gh_program() -> String {
    #[cfg(test)]
    {
        if let Some(path) = std::env::var_os(ENV_TEST_GH_BIN) {
            if !path.is_empty() {
                return path.to_string_lossy().to_string();
            }
        }
    }
    "gh".to_string()
}
