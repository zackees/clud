//! Classify a `clud do` target so the launch seeds the right skill.
//!
//! - A single issue, a PR, or a free-form goal seeds `/goal /do <target>`.
//! - A GitHub issue with at least one open native sub-issue is a meta issue,
//!   and seeds `/goal /grind <url>`.
//!
//! Deciding needs a GitHub query. When clud cannot answer it (no `gh`, not
//! authenticated, offline, an issue URL on a forge clud cannot query), the
//! launch is refused rather than guessed: a meta issue run through `/do`
//! lands as one PR, and a single issue run through `/grind` is ceremony.

use serde_json::Value;

/// Test and dry-run override: `single` or `meta` skips the GitHub query.
pub const DO_KIND_ENV: &str = "CLUD_DO_KIND";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DoKind {
    Single,
    Meta,
}

/// `owner`, `repo` and issue number of a GitHub issue URL.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IssueRef {
    pub owner: String,
    pub repo: String,
    pub number: u64,
}

/// Parse `[https://]github.com/<owner>/<repo>/issues/<n>[...]`.
pub fn github_issue(target: &str) -> Option<IssueRef> {
    let rest = target.trim();
    let rest = rest
        .strip_prefix("https://")
        .or_else(|| rest.strip_prefix("http://"))
        .unwrap_or(rest);
    let rest = rest.strip_prefix("www.").unwrap_or(rest);
    let path = rest.strip_prefix("github.com/")?;
    let mut parts = path.split(['/', '?', '#']);
    let owner = parts.next().filter(|s| !s.is_empty())?;
    let repo = parts.next().filter(|s| !s.is_empty())?;
    if parts.next()? != "issues" {
        return None;
    }
    let number = parts.next()?.parse().ok()?;
    Some(IssueRef {
        owner: owner.to_string(),
        repo: repo.to_string(),
        number,
    })
}

/// A non-GitHub URL whose path names an issue: something clud cannot query.
fn other_forge_issue(target: &str) -> bool {
    let lower = target.trim().to_ascii_lowercase();
    (lower.starts_with("https://") || lower.starts_with("http://"))
        && !lower.contains("://github.com/")
        && !lower.contains("://www.github.com/")
        && (lower.contains("/issues/") || lower.contains("/-/issues/"))
}

/// Classify `target`. `query` returns the issue's native sub-issues as the
/// `gh api` JSON array; `override_kind` is [`DO_KIND_ENV`].
pub fn classify<Q>(target: &str, override_kind: Option<&str>, query: Q) -> Result<DoKind, String>
where
    Q: FnOnce(&IssueRef) -> Result<Value, String>,
{
    match override_kind.map(str::trim) {
        Some("single") => return Ok(DoKind::Single),
        Some("meta") => return Ok(DoKind::Meta),
        Some("") | None => {}
        Some(other) => return Err(format!("{DO_KIND_ENV}={other} is not `single` or `meta`")),
    }
    let Some(issue) = github_issue(target) else {
        if other_forge_issue(target) {
            return Err(format!(
                "`clud do` cannot tell whether {target} is a meta issue: only GitHub \
                 issues can be queried for sub-issues. Set {DO_KIND_ENV}=single or \
                 {DO_KIND_ENV}=meta to choose."
            ));
        }
        // A PR URL, another URL, or free-form text: one deliverable.
        return Ok(DoKind::Single);
    };
    let sub_issues = query(&issue).map_err(|error| {
        format!(
            "`clud do` could not check whether {target} is a meta issue ({error}). \
             Fix `gh` (install it, `gh auth login`, or check the URL), or set \
             {DO_KIND_ENV}=single or {DO_KIND_ENV}=meta to choose."
        )
    })?;
    let items = sub_issues
        .as_array()
        .ok_or_else(|| format!("unexpected sub-issues response for {target}"))?;
    let open = items
        .iter()
        .filter(|item| item.get("state").and_then(Value::as_str) == Some("open"))
        .count();
    Ok(if open > 0 {
        DoKind::Meta
    } else {
        DoKind::Single
    })
}

/// The production query: `gh api repos/<o>/<r>/issues/<n>/sub_issues`.
pub fn query_sub_issues(issue: &IssueRef) -> Result<Value, String> {
    let endpoint = format!(
        "repos/{}/{}/issues/{}/sub_issues?per_page=100",
        issue.owner, issue.repo, issue.number
    );
    let (code, output) = crate::loop_spec::run_gh_capture(&["api", &endpoint])?;
    if code != 0 {
        let detail = output.lines().last().unwrap_or("").trim().to_string();
        return Err(format!("`gh api {endpoint}` exited {code}: {detail}"));
    }
    serde_json::from_str(&output).map_err(|error| format!("unparseable gh output: {error}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn never(_: &IssueRef) -> Result<Value, String> {
        panic!("no GitHub query expected")
    }

    #[test]
    fn parses_github_issue_urls() {
        let expected = Some(IssueRef {
            owner: "zackees".into(),
            repo: "clud".into(),
            number: 866,
        });
        assert_eq!(
            github_issue("https://github.com/zackees/clud/issues/866"),
            expected
        );
        assert_eq!(github_issue("github.com/zackees/clud/issues/866"), expected);
        assert_eq!(
            github_issue("https://github.com/zackees/clud/issues/866#x"),
            expected
        );
        assert_eq!(
            github_issue("https://github.com/zackees/clud/pull/866"),
            None
        );
        assert_eq!(github_issue("fix the launcher"), None);
    }

    #[test]
    fn open_sub_issues_make_a_meta_issue() {
        let target = "https://github.com/o/r/issues/1";
        let meta = classify(target, None, |_| {
            Ok(json!([{"state": "open"}, {"state": "closed"}]))
        });
        assert_eq!(meta, Ok(DoKind::Meta));
        let done = classify(target, None, |_| Ok(json!([{"state": "closed"}])));
        assert_eq!(done, Ok(DoKind::Single));
        let none = classify(target, None, |_| Ok(json!([])));
        assert_eq!(none, Ok(DoKind::Single));
    }

    #[test]
    fn free_form_and_prs_need_no_query() {
        assert_eq!(
            classify("refactor the launcher", None, never),
            Ok(DoKind::Single)
        );
        assert_eq!(
            classify("https://github.com/o/r/pull/5", None, never),
            Ok(DoKind::Single)
        );
    }

    #[test]
    fn a_failed_query_refuses_the_launch() {
        let error = classify("https://github.com/o/r/issues/1", None, |_| {
            Err("gh: command not found".into())
        })
        .unwrap_err();
        assert!(error.contains("could not check") && error.contains(DO_KIND_ENV));
    }

    #[test]
    fn other_forge_issues_refuse_rather_than_guess() {
        let error = classify("https://gitlab.com/g/p/-/issues/3", None, never).unwrap_err();
        assert!(error.contains("only GitHub"));
    }

    #[test]
    fn override_skips_the_query() {
        let target = "https://github.com/o/r/issues/1";
        assert_eq!(classify(target, Some("meta"), never), Ok(DoKind::Meta));
        assert_eq!(classify(target, Some("single"), never), Ok(DoKind::Single));
        assert!(classify(target, Some("bogus"), never).is_err());
    }
}
