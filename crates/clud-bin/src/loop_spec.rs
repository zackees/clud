//! Task-spec resolution and done-signal plumbing for `clud loop`.
//!
//! Responsibilities:
//! - Classify the positional argument (GH URL, short-form `#42`, local file,
//!   literal prompt).
//! - Fetch a GH issue/PR body via `gh` (with `curl` fallback) and cache it
//!   under `<git-root>/.clud/loop/`.
//! - Locate the marker directory and detect DONE/BLOCKED terminal files.
//!
//! The actual iteration-control loop lives in `main.rs`; this module is
//! side-effectful only at loop-start (fetching/caching) and loop-iter-end
//! (marker polling).

use std::path::{Path, PathBuf};
use std::time::Duration;

use running_process_core::{
    CommandSpec, NativeProcess, ProcessConfig, ReadStatus, StderrMode, StdinMode,
};

pub const LOOP_DIR: &str = ".clud/loop";
pub const DONE_MARKER: &str = "DONE";
pub const BLOCKED_MARKER: &str = "BLOCKED";

/// How to interpret the positional argument passed to `clud loop`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TaskSpec {
    GhIssue {
        owner: String,
        repo: String,
        kind: GhKind,
        number: u32,
    },
    ShortForm(u32),
    File(PathBuf),
    Literal(String),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GhKind {
    Issue,
    Pr,
}

impl GhKind {
    fn as_gh_subcommand(self) -> &'static str {
        match self {
            GhKind::Issue => "issue",
            GhKind::Pr => "pr",
        }
    }

    fn label(self) -> &'static str {
        match self {
            GhKind::Issue => "issue",
            GhKind::Pr => "pull",
        }
    }
}

/// Classify a positional task argument. Input detection order:
///   1. GH issue/PR URL
///   2. Short-form `#42` or `42`
///   3. Local file path
///   4. Literal prompt
pub fn classify(input: &str) -> TaskSpec {
    if let Some(parsed) = parse_gh_url(input) {
        return parsed;
    }
    if let Some(n) = parse_short_form(input) {
        return TaskSpec::ShortForm(n);
    }
    let path = Path::new(input);
    if path.is_file() {
        return TaskSpec::File(path.to_path_buf());
    }
    TaskSpec::Literal(input.to_string())
}

fn parse_gh_url(input: &str) -> Option<TaskSpec> {
    // Keep dependency count minimal — manual parse instead of pulling regex.
    let s = input.strip_suffix('/').unwrap_or(input);
    let rest = s
        .strip_prefix("https://github.com/")
        .or_else(|| s.strip_prefix("http://github.com/"))?;
    let mut parts = rest.splitn(4, '/');
    let owner = parts.next()?.to_string();
    let repo = parts.next()?.to_string();
    let kind_seg = parts.next()?;
    let number_seg = parts.next()?;
    if owner.is_empty() || repo.is_empty() {
        return None;
    }
    let kind = match kind_seg {
        "issues" => GhKind::Issue,
        "pull" => GhKind::Pr,
        _ => return None,
    };
    let number: u32 = number_seg.parse().ok()?;
    Some(TaskSpec::GhIssue {
        owner,
        repo,
        kind,
        number,
    })
}

fn parse_short_form(input: &str) -> Option<u32> {
    let s = input.strip_prefix('#').unwrap_or(input);
    if s.is_empty() {
        return None;
    }
    s.parse().ok()
}

/// Find the git-root by walking upward from `start` looking for `.git`.
/// Falls back to `start` if no git root is found.
pub fn git_root_from(start: &Path) -> PathBuf {
    let mut cur = start.to_path_buf();
    loop {
        if cur.join(".git").exists() {
            return cur;
        }
        if !cur.pop() {
            return start.to_path_buf();
        }
    }
}

/// Resolve the `<git-root>/.clud/loop/` directory.
pub fn loop_dir(git_root: &Path) -> PathBuf {
    git_root.join(LOOP_DIR)
}

/// Path to the DONE marker.
pub fn done_path(git_root: &Path) -> PathBuf {
    loop_dir(git_root).join(DONE_MARKER)
}

/// Path to the BLOCKED marker.
pub fn blocked_path(git_root: &Path) -> PathBuf {
    loop_dir(git_root).join(BLOCKED_MARKER)
}

/// Marker state observed after an iteration.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MarkerState {
    Done(String),
    Blocked(String),
    None,
}

/// Read the marker state from `<git-root>/.clud/loop/`. DONE wins if both
/// exist (the agent's last word that the task resolved).
pub fn read_markers(git_root: &Path) -> MarkerState {
    let done = done_path(git_root);
    if done.is_file() {
        let body = std::fs::read_to_string(&done).unwrap_or_default();
        return MarkerState::Done(body.trim().to_string());
    }
    let blocked = blocked_path(git_root);
    if blocked.is_file() {
        let body = std::fs::read_to_string(&blocked).unwrap_or_default();
        return MarkerState::Blocked(body.trim().to_string());
    }
    MarkerState::None
}

/// Remove stale DONE/BLOCKED markers so we start from a clean slate.
pub fn clear_markers(git_root: &Path) {
    let _ = std::fs::remove_file(done_path(git_root));
    let _ = std::fs::remove_file(blocked_path(git_root));
}

/// Ensure the `.clud/loop/` dir exists under the given git root.
pub fn ensure_loop_dir(git_root: &Path) -> std::io::Result<PathBuf> {
    let dir = loop_dir(git_root);
    std::fs::create_dir_all(&dir)?;
    Ok(dir)
}

/// Cache file path for a GH issue/PR.
pub fn cache_path(git_root: &Path, owner: &str, repo: &str, kind: GhKind, number: u32) -> PathBuf {
    loop_dir(git_root).join(format!(
        "{}__{}__{}-{}.md",
        sanitize(owner),
        sanitize(repo),
        kind.label(),
        number
    ))
}

fn sanitize(s: &str) -> String {
    s.chars()
        .map(|c| {
            if c.is_alphanumeric() || c == '-' {
                c
            } else {
                '_'
            }
        })
        .collect()
}

/// Resolve a short-form `#42` input by asking `gh` for the current repo.
/// Returns `(owner, repo)` or an error string suitable for the user.
pub fn resolve_current_repo() -> Result<(String, String), String> {
    let (exit_code, stdout) = run_gh_capture(&["repo", "view", "--json", "owner,name"])?;
    if exit_code != 0 {
        return Err(format!("`gh repo view` failed with exit {exit_code}"));
    }
    // Two-field JSON: minimal parse without pulling serde_json dep here.
    let v: serde_json::Value =
        serde_json::from_str(&stdout).map_err(|e| format!("gh JSON parse: {e}"))?;
    let owner = v
        .get("owner")
        .and_then(|o| o.get("login"))
        .and_then(|s| s.as_str())
        .ok_or_else(|| "gh response missing owner.login".to_string())?
        .to_string();
    let name = v
        .get("name")
        .and_then(|s| s.as_str())
        .ok_or_else(|| "gh response missing name".to_string())?
        .to_string();
    Ok((owner, name))
}

/// Fetched issue/PR data, minimal shape needed for prompt assembly.
#[derive(Debug, Clone)]
pub struct IssueDoc {
    pub url: String,
    pub title: String,
    pub body: String,
    pub state: String,
    pub author: String,
    pub labels: Vec<String>,
    pub comments: Vec<IssueComment>,
    pub updated_at: String,
}

#[derive(Debug, Clone)]
pub struct IssueComment {
    pub author: String,
    pub created_at: String,
    pub body: String,
}

/// Fetch a GH issue or PR via `gh` CLI. Callers should pass the same `kind`
/// that produced the input URL (issue vs pull).
pub fn fetch_via_gh(
    owner: &str,
    repo: &str,
    kind: GhKind,
    number: u32,
) -> Result<IssueDoc, String> {
    let slug = format!("{owner}/{repo}");
    let number_str = number.to_string();
    let args: &[&str] = &[
        kind.as_gh_subcommand(),
        "view",
        &number_str,
        "--repo",
        &slug,
        "--json",
        "number,title,body,state,author,labels,comments,updatedAt,url",
    ];
    let (exit_code, stdout) = run_gh_capture(args)?;
    if exit_code != 0 {
        return Err(format!(
            "`gh {} view` failed with exit {exit_code}",
            kind.as_gh_subcommand()
        ));
    }
    parse_gh_json(&stdout)
}

/// Run `gh` with `args`, capturing combined stdout/stderr. Returns
/// `(exit_code, captured_output)`. Uses `running-process-core` per the
/// repo's subprocess policy (see ci/check-banned-imports).
fn run_gh_capture(args: &[&str]) -> Result<(i32, String), String> {
    let mut argv = vec!["gh".to_string()];
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
        containment: None,
    };
    let process = NativeProcess::new(config);
    process
        .start()
        .map_err(|e| format!("failed to start `gh`: {e}"))?;

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

fn parse_gh_json(stdout: &str) -> Result<IssueDoc, String> {
    let v: serde_json::Value =
        serde_json::from_str(stdout).map_err(|e| format!("gh JSON parse: {e}"))?;
    let url = v
        .get("url")
        .and_then(|s| s.as_str())
        .unwrap_or("")
        .to_string();
    let title = v
        .get("title")
        .and_then(|s| s.as_str())
        .unwrap_or("")
        .to_string();
    let body = v
        .get("body")
        .and_then(|s| s.as_str())
        .unwrap_or("")
        .to_string();
    let state = v
        .get("state")
        .and_then(|s| s.as_str())
        .unwrap_or("")
        .to_string();
    let author = v
        .get("author")
        .and_then(|o| o.get("login"))
        .and_then(|s| s.as_str())
        .unwrap_or("")
        .to_string();
    let updated_at = v
        .get("updatedAt")
        .and_then(|s| s.as_str())
        .unwrap_or("")
        .to_string();
    let labels = v
        .get("labels")
        .and_then(|a| a.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|e| e.get("name").and_then(|s| s.as_str()))
                .map(String::from)
                .collect()
        })
        .unwrap_or_default();
    let comments = v
        .get("comments")
        .and_then(|a| a.as_array())
        .map(|arr| {
            arr.iter()
                .map(|c| IssueComment {
                    author: c
                        .get("author")
                        .and_then(|o| o.get("login"))
                        .and_then(|s| s.as_str())
                        .unwrap_or("")
                        .to_string(),
                    created_at: c
                        .get("createdAt")
                        .and_then(|s| s.as_str())
                        .unwrap_or("")
                        .to_string(),
                    body: c
                        .get("body")
                        .and_then(|s| s.as_str())
                        .unwrap_or("")
                        .to_string(),
                })
                .collect()
        })
        .unwrap_or_default();

    Ok(IssueDoc {
        url,
        title,
        body,
        state,
        author,
        labels,
        comments,
        updated_at,
    })
}

/// Render a fetched issue into the cache file body (with YAML frontmatter).
pub fn render_cache(doc: &IssueDoc, fetched_at: &str) -> String {
    let mut out = String::new();
    out.push_str("---\n");
    out.push_str(&format!("url: {}\n", doc.url));
    out.push_str(&format!("fetched_at: {}\n", fetched_at));
    out.push_str(&format!("updated_at: {}\n", doc.updated_at));
    out.push_str("---\n\n");
    out.push_str(&format!("# {}\n\n", doc.title));
    if !doc.state.is_empty() {
        out.push_str(&format!("State: {}\n", doc.state));
    }
    if !doc.author.is_empty() {
        out.push_str(&format!("Author: @{}\n", doc.author));
    }
    if !doc.labels.is_empty() {
        out.push_str(&format!("Labels: {}\n", doc.labels.join(", ")));
    }
    out.push_str("\n## Body\n\n");
    out.push_str(&doc.body);
    out.push('\n');
    if !doc.comments.is_empty() {
        out.push_str("\n## Comments\n");
        for c in &doc.comments {
            out.push_str(&format!("\n### @{} ({})\n\n", c.author, c.created_at));
            out.push_str(&c.body);
            out.push('\n');
        }
    }
    out
}

/// Default prompt instructions appended to every loop-driven task when the
/// DONE/BLOCKED marker contract is active.
pub const DONE_MARKER_CONTRACT: &str = "\n\n---\n\
You are running in a ralph loop. The loop will re-invoke you up to N times \
with the same task until you complete it.\n\
\n\
When the task is fully resolved and verified (tests pass, lint clean where \
applicable), write `.clud/loop/DONE` with a one-line summary of what you \
did. Do not write DONE prematurely — only after you are confident the work \
is complete.\n\
\n\
If you cannot make progress (missing info, external dependency, needs \
human input), write `.clud/loop/BLOCKED` with a one-line reason and stop.\n\
\n\
Otherwise, continue working — you will be re-invoked.\n";

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classify_issue_url() {
        match classify("https://github.com/acme/widgets/issues/42") {
            TaskSpec::GhIssue {
                owner,
                repo,
                kind,
                number,
            } => {
                assert_eq!(owner, "acme");
                assert_eq!(repo, "widgets");
                assert_eq!(kind, GhKind::Issue);
                assert_eq!(number, 42);
            }
            other => panic!("expected GhIssue, got {other:?}"),
        }
    }

    #[test]
    fn classify_pr_url_trailing_slash() {
        match classify("https://github.com/acme/widgets/pull/7/") {
            TaskSpec::GhIssue { kind, number, .. } => {
                assert_eq!(kind, GhKind::Pr);
                assert_eq!(number, 7);
            }
            other => panic!("expected GhIssue(Pr), got {other:?}"),
        }
    }

    #[test]
    fn classify_short_form() {
        assert_eq!(classify("#42"), TaskSpec::ShortForm(42));
        assert_eq!(classify("1"), TaskSpec::ShortForm(1));
    }

    #[test]
    fn classify_literal() {
        match classify("do the task") {
            TaskSpec::Literal(s) => assert_eq!(s, "do the task"),
            other => panic!("expected Literal, got {other:?}"),
        }
    }

    #[test]
    fn classify_file() {
        let tmp = tempfile::NamedTempFile::new().expect("tmp");
        std::fs::write(tmp.path(), "task body").unwrap();
        let path_str = tmp.path().to_string_lossy().to_string();
        match classify(&path_str) {
            TaskSpec::File(p) => assert_eq!(p, tmp.path()),
            other => panic!("expected File, got {other:?}"),
        }
    }

    #[test]
    fn classify_rejects_wrong_host() {
        match classify("https://gitlab.com/acme/widgets/issues/1") {
            TaskSpec::Literal(_) => {}
            other => panic!("expected Literal, got {other:?}"),
        }
    }

    #[test]
    fn cache_path_sanitizes_segments() {
        let p = cache_path(
            Path::new("/tmp/repo"),
            "acme-co",
            "wid.gets",
            GhKind::Issue,
            42,
        );
        assert!(p
            .to_string_lossy()
            .ends_with("acme-co__wid_gets__issue-42.md"));
    }

    #[test]
    fn read_markers_none_when_missing() {
        let tmp = tempfile::tempdir().unwrap();
        assert_eq!(read_markers(tmp.path()), MarkerState::None);
    }

    #[test]
    fn read_markers_done() {
        let tmp = tempfile::tempdir().unwrap();
        ensure_loop_dir(tmp.path()).unwrap();
        std::fs::write(done_path(tmp.path()), "all good\n").unwrap();
        assert_eq!(
            read_markers(tmp.path()),
            MarkerState::Done("all good".to_string())
        );
    }

    #[test]
    fn read_markers_blocked() {
        let tmp = tempfile::tempdir().unwrap();
        ensure_loop_dir(tmp.path()).unwrap();
        std::fs::write(blocked_path(tmp.path()), "need secret\n").unwrap();
        assert_eq!(
            read_markers(tmp.path()),
            MarkerState::Blocked("need secret".to_string())
        );
    }

    #[test]
    fn read_markers_done_wins_over_blocked() {
        let tmp = tempfile::tempdir().unwrap();
        ensure_loop_dir(tmp.path()).unwrap();
        std::fs::write(done_path(tmp.path()), "done").unwrap();
        std::fs::write(blocked_path(tmp.path()), "ignored").unwrap();
        assert_eq!(
            read_markers(tmp.path()),
            MarkerState::Done("done".to_string())
        );
    }

    #[test]
    fn clear_markers_removes_both() {
        let tmp = tempfile::tempdir().unwrap();
        ensure_loop_dir(tmp.path()).unwrap();
        std::fs::write(done_path(tmp.path()), "x").unwrap();
        std::fs::write(blocked_path(tmp.path()), "y").unwrap();
        clear_markers(tmp.path());
        assert_eq!(read_markers(tmp.path()), MarkerState::None);
    }

    #[test]
    fn parse_gh_json_happy_path() {
        let raw = r#"{
            "url": "https://github.com/acme/widgets/issues/42",
            "title": "Bug: things break",
            "body": "when I do X, Y happens",
            "state": "OPEN",
            "author": {"login": "alice"},
            "labels": [{"name": "bug"}, {"name": "prio-high"}],
            "comments": [
                {"author": {"login": "bob"}, "createdAt": "2026-04-10T00:00:00Z", "body": "repro?"}
            ],
            "updatedAt": "2026-04-11T00:00:00Z"
        }"#;
        let doc = parse_gh_json(raw).expect("parse");
        assert_eq!(doc.title, "Bug: things break");
        assert_eq!(doc.author, "alice");
        assert_eq!(doc.labels, vec!["bug", "prio-high"]);
        assert_eq!(doc.comments.len(), 1);
        assert_eq!(doc.comments[0].author, "bob");
    }

    #[test]
    fn render_cache_includes_frontmatter_and_sections() {
        let doc = IssueDoc {
            url: "https://github.com/a/b/issues/1".into(),
            title: "t".into(),
            body: "b".into(),
            state: "OPEN".into(),
            author: "al".into(),
            labels: vec!["bug".into()],
            comments: vec![IssueComment {
                author: "bo".into(),
                created_at: "2026-04-10T00:00:00Z".into(),
                body: "note".into(),
            }],
            updated_at: "2026-04-11T00:00:00Z".into(),
        };
        let rendered = render_cache(&doc, "2026-04-16T10:00:00Z");
        assert!(rendered.starts_with("---\n"));
        assert!(rendered.contains("url: https://github.com/a/b/issues/1"));
        assert!(rendered.contains("fetched_at: 2026-04-16T10:00:00Z"));
        assert!(rendered.contains("# t"));
        assert!(rendered.contains("## Body"));
        assert!(rendered.contains("## Comments"));
        assert!(rendered.contains("@bo"));
    }
}
