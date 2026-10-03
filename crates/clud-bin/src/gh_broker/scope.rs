//! Targeted write invalidation (#1743, phase 2).
//!
//! Phase 1 marked every cached read stale after any in-session `gh` call
//! that may write. That also thawed listings that can no longer change
//! (the jobs of a finished run), so phase 2 tags both sides:
//!
//! - [`key_tags`]: each cached read carries tags derived from its path.
//!   `{repo}#num:{n}` for an issue or PR and everything under it,
//!   `run:{id}` for a run and its jobs, `{repo}#runs` for the run list and
//!   single jobs, `{repo}#checks` for check runs. Each repo tag also comes as
//!   `*#...`, which a write whose repo is unknown sends. Every other read
//!   carries [`OTHER`].
//! - [`write_tags`]: a write the parser recognizes names the tags it may
//!   change, plus [`OTHER`]. Anything else returns `None`: the shim then
//!   asks for the phase-1 global invalidation.
//!
//! So a recognized write still refreshes every untagged read, exactly as in
//! phase 1, but leaves alone the tagged reads of other issues, PRs and runs.

use std::ffi::OsString;

use super::collection::is_number;

/// The tag of every read that names no issue, PR, run or check.
pub const OTHER: &str = "other";
/// Most tags one invalidation may carry.
pub const MAX_TAGS: usize = 16;

/// Whether a tag from the shim is well formed.
pub fn valid_tag(tag: &str) -> bool {
    !tag.is_empty()
        && tag.len() <= 200
        && tag.chars().all(|c| {
            c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_' | '/' | ':' | '#' | '*')
        })
}

fn push_repo(tags: &mut Vec<String>, repo: &str, tag: &str) {
    tags.push(format!("{repo}#{tag}"));
    if repo != "*" {
        tags.push(format!("*#{tag}"));
    }
}

/// The tags of a cached read of `endpoint`.
pub fn key_tags(endpoint: &str) -> Vec<String> {
    let path = endpoint
        .trim_start_matches('/')
        .split(['?', '#'])
        .next()
        .unwrap_or("");
    let segments: Vec<&str> = path.split('/').collect();
    let mut tags = Vec::new();
    if let ["repos", owner, repo, rest @ ..] = segments.as_slice() {
        let repo = format!("{owner}/{repo}").to_ascii_lowercase();
        match rest {
            ["issues" | "pulls", n, ..] if is_number(n) => {
                push_repo(&mut tags, &repo, &format!("num:{n}"));
            }
            ["actions", "runs"] | ["actions", "jobs", ..] => push_repo(&mut tags, &repo, "runs"),
            ["actions", "runs", id, ..] if is_number(id) => tags.push(format!("run:{id}")),
            ["commits", _, "check-runs" | "check-suites" | "status" | "statuses"]
            | ["check-runs" | "check-suites", ..] => push_repo(&mut tags, &repo, "checks"),
            _ => {}
        }
    }
    if tags.is_empty() {
        tags.push(OTHER.to_string());
    }
    tags
}

/// `o/r`, `host/o/r` or a `-R` value -> lowercase `o/r`.
fn repo_slug(value: &str) -> Option<String> {
    let parts: Vec<&str> = value.split('/').filter(|p| !p.is_empty()).collect();
    if parts.len() < 2 || parts.len() > 3 {
        return None;
    }
    let (owner, repo) = (parts[parts.len() - 2], parts[parts.len() - 1]);
    let ok = |s: &str| {
        s.chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_'))
    };
    (ok(owner) && ok(repo)).then(|| format!("{owner}/{repo}").to_ascii_lowercase())
}

/// A PR or issue selector: `5`, `#5`, or a `https://host/o/r/pull/5` URL
/// (which also names the repo).
fn number_selector(word: &str) -> Option<(String, Option<String>)> {
    let bare = word.strip_prefix('#').unwrap_or(word);
    if is_number(bare) {
        return Some((bare.to_string(), None));
    }
    let rest = word
        .strip_prefix("https://")
        .or_else(|| word.strip_prefix("http://"))?;
    let parts: Vec<&str> = rest.split(['/', '?', '#']).collect();
    match parts.as_slice() {
        [_, owner, repo, "pull" | "pulls" | "issues", n, ..] if is_number(n) => {
            Some((n.to_string(), Some(repo_slug(&format!("{owner}/{repo}"))?)))
        }
        _ => None,
    }
}

/// The tags an in-session write may change, or `None` for "any key": the
/// phase-1 global invalidation. `gh_repo` is the caller's `GH_REPO`.
pub fn write_tags(args: &[OsString], gh_repo: Option<&str>) -> Option<Vec<String>> {
    let words: Vec<&str> = args.iter().map(|a| a.to_str()).collect::<Option<_>>()?;
    if words.first() == Some(&"api") {
        return api_write_tags(&words[1..]);
    }
    let mut repo = None;
    if let Some(value) = gh_repo.filter(|r| !r.is_empty()) {
        repo = Some(repo_slug(value)?);
    }
    let mut rest = Vec::new();
    let mut i = 0;
    while i < words.len() {
        let word = words[i];
        if word == "-R" || word == "--repo" {
            repo = Some(repo_slug(words.get(i + 1)?)?);
            i += 2;
            continue;
        }
        if let Some(value) = word
            .strip_prefix("--repo=")
            .or_else(|| word.strip_prefix("-R").filter(|v| !v.is_empty()))
        {
            repo = Some(repo_slug(value)?);
        } else {
            rest.push(word);
        }
        i += 1;
    }
    // The selector must be the word right after the subcommand: a flag
    // value elsewhere (`--body 7`) is never mistaken for it.
    let [group, sub, selector, ..] = rest.as_slice() else {
        return None;
    };
    let mut tags = Vec::new();
    match (*group, *sub) {
        ("pr", "merge")
        | (
            "pr",
            "comment" | "close" | "reopen" | "edit" | "ready" | "review" | "lock" | "unlock",
        )
        | (
            "issue",
            "comment" | "close" | "reopen" | "edit" | "lock" | "unlock" | "pin" | "unpin",
        ) => {
            let (n, url_repo) = number_selector(selector)?;
            let repo = url_repo.or(repo).unwrap_or_else(|| "*".to_string());
            push_repo(&mut tags, &repo, &format!("num:{n}"));
            if *sub == "merge" {
                push_repo(&mut tags, &repo, "runs");
            }
        }
        ("run", "rerun" | "cancel" | "delete") if is_number(selector) => {
            let repo = repo.unwrap_or_else(|| "*".to_string());
            run_write(&mut tags, &repo, selector);
        }
        _ => return None,
    }
    tags.push(OTHER.to_string());
    Some(tags)
}

/// A rerun, cancel or delete of run `id` changes the run, its jobs, the
/// run list and the commit's check runs.
fn run_write(tags: &mut Vec<String>, repo: &str, id: &str) {
    tags.push(format!("run:{id}"));
    push_repo(tags, repo, "runs");
    push_repo(tags, repo, "checks");
}

/// `gh api` flags that take the next word as their value.
const API_VALUE_FLAGS: &[&str] = &[
    "-X",
    "--method",
    "-f",
    "-F",
    "--field",
    "--raw-field",
    "-H",
    "--header",
    "--input",
    "-q",
    "--jq",
    "-t",
    "--template",
    "-p",
    "--preview",
    "--hostname",
    "--cache",
];

fn api_write_tags(words: &[&str]) -> Option<Vec<String>> {
    let mut endpoint = None;
    let mut i = 0;
    while i < words.len() {
        let word = words[i];
        if API_VALUE_FLAGS.contains(&word) {
            i += 2;
            continue;
        }
        if !word.starts_with('-') {
            if endpoint.is_some() {
                return None;
            }
            endpoint = Some(word);
        }
        i += 1;
    }
    let endpoint = endpoint?;
    if !super::classify::brokerable_endpoint(endpoint) {
        return None;
    }
    let path = endpoint.trim_start_matches('/').split(['?', '#']).next()?;
    let segments: Vec<&str> = path.split('/').collect();
    let mut tags = Vec::new();
    match segments.as_slice() {
        ["repos", owner, repo, "actions", "runs", id, ..] if is_number(id) => {
            run_write(&mut tags, &repo_slug(&format!("{owner}/{repo}"))?, id);
        }
        // A job, check-suite or check-run rerun reopens a run the path does
        // not name.
        ["repos", _, _, "actions", "jobs", ..]
        | ["repos", _, _, "check-suites" | "check-runs", ..] => return None,
        ["repos", owner, repo, "pulls", n, "merge"] if is_number(n) => {
            let repo = repo_slug(&format!("{owner}/{repo}"))?;
            push_repo(&mut tags, &repo, &format!("num:{n}"));
            push_repo(&mut tags, &repo, "runs");
        }
        _ => {
            tags = key_tags(endpoint);
            if tags == [OTHER] {
                return None;
            }
        }
    }
    tags.push(OTHER.to_string());
    Some(tags)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(words: &[&str]) -> Vec<OsString> {
        words.iter().map(OsString::from).collect()
    }

    fn tags(words: &[&str]) -> Option<Vec<String>> {
        write_tags(&args(words), None)
    }

    #[test]
    fn reads_are_tagged_by_what_they_name() {
        assert_eq!(
            key_tags("/repos/O/R/issues/5/comments?per_page=5"),
            ["o/r#num:5", "*#num:5"]
        );
        assert_eq!(key_tags("repos/o/r/pulls/5"), ["o/r#num:5", "*#num:5"]);
        assert_eq!(key_tags("repos/o/r/actions/runs/7/jobs"), ["run:7"]);
        assert_eq!(
            key_tags("repos/o/r/actions/runs?branch=x"),
            ["o/r#runs", "*#runs"]
        );
        assert_eq!(key_tags("repos/o/r/actions/jobs/9"), ["o/r#runs", "*#runs"]);
        assert_eq!(
            key_tags("repos/o/r/commits/abc/check-runs"),
            ["o/r#checks", "*#checks"]
        );
        for untagged in [
            "repos/o/r",
            "repos/o/r/pulls",
            "repos/o/r/issues/comments/9",
            "user",
            "search/issues?q=x",
        ] {
            assert_eq!(key_tags(untagged), [OTHER], "{untagged}");
        }
    }

    #[test]
    fn recognized_writes_name_their_targets() {
        assert_eq!(
            tags(&["pr", "merge", "5", "-R", "O/R", "--squash"]).unwrap(),
            ["o/r#num:5", "*#num:5", "o/r#runs", "*#runs", OTHER]
        );
        assert_eq!(
            tags(&["pr", "comment", "#5", "--body", "7"]).unwrap(),
            ["*#num:5", OTHER]
        );
        assert_eq!(
            tags(&[
                "issue",
                "comment",
                "https://github.com/o/r/issues/12",
                "-b",
                "x"
            ])
            .unwrap(),
            ["o/r#num:12", "*#num:12", OTHER]
        );
        assert_eq!(
            tags(&[
                "--repo=ghe.example.com/o/r",
                "run",
                "rerun",
                "99",
                "--failed"
            ])
            .unwrap(),
            [
                "run:99",
                "o/r#runs",
                "*#runs",
                "o/r#checks",
                "*#checks",
                OTHER
            ]
        );
        assert_eq!(
            write_tags(&args(&["run", "cancel", "99"]), Some("o/r")).unwrap(),
            [
                "run:99",
                "o/r#runs",
                "*#runs",
                "o/r#checks",
                "*#checks",
                OTHER
            ]
        );
        assert_eq!(
            tags(&[
                "api",
                "-X",
                "POST",
                "repos/o/r/actions/runs/7/rerun-failed-jobs"
            ])
            .unwrap(),
            [
                "run:7",
                "o/r#runs",
                "*#runs",
                "o/r#checks",
                "*#checks",
                OTHER
            ]
        );
        assert_eq!(
            tags(&["api", "repos/o/r/issues/5/comments", "-f", "body=x"]).unwrap(),
            ["o/r#num:5", "*#num:5", OTHER]
        );
        assert_eq!(
            tags(&["api", "-X", "PUT", "repos/o/r/pulls/5/merge"]).unwrap(),
            ["o/r#num:5", "*#num:5", "o/r#runs", "*#runs", OTHER]
        );
    }

    #[test]
    fn anything_else_is_a_global_invalidation() {
        for words in [
            &["pr", "merge"][..],
            &["pr", "merge", "my-branch"],
            &["pr", "comment", "--body", "x", "5"],
            &["pr", "create", "--fill"],
            &["run", "rerun", "--job", "5"],
            &["run", "rerun", "abc"],
            &["workflow", "run", "ci.yml"],
            &["auth", "switch"],
            &["pr", "merge", "5", "-R", "not-a-repo"],
            &["api", "-X", "POST", "graphql", "-f", "query=x"],
            &[
                "api",
                "-X",
                "POST",
                "repos/{owner}/{repo}/issues/5/comments",
            ],
            &["api", "-X", "PATCH", "repos/o/r/issues/comments/9"],
            &["api", "-X", "POST", "repos/o/r/actions/jobs/9/rerun"],
            &["api", "-X", "POST", "repos/o/r/check-suites/9/rerequest"],
            &["api", "-X", "POST", "repos/o/r/check-runs/9/rerequest"],
            &["api", "-X", "POST", "repos/o/r/git/refs"],
        ] {
            assert_eq!(tags(words), None, "{words:?}");
        }
        assert_eq!(write_tags(&args(&["pr", "merge", "5"]), Some("bad")), None);
    }

    #[test]
    fn tags_are_validated() {
        assert!(valid_tag("o/r#num:5"));
        assert!(valid_tag("*#runs"));
        assert!(valid_tag(OTHER));
        assert!(!valid_tag(""));
        assert!(!valid_tag("a b"));
        assert!(!valid_tag(&"x".repeat(201)));
    }
}
