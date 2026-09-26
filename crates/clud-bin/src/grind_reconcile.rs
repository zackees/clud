//! `clud grind reconcile` — never lose an issue in feature-branch mode (#1393 §3).
//!
//! Issues a feature-branch grind run fixed carry the `grind:on-feature` label and
//! a `<!-- grind:v1 ... -->` marker comment naming the feature PR. GitHub only
//! auto-closes an issue when its fix lands on the default branch, so an issue
//! can be closed by the wrong thing (a PR merged into a non-default branch, a
//! commit that never reached the default branch, a person) or left dangling
//! (feature PR closed unmerged). `reconcile` walks every labelled issue and
//! repairs it, reopens a sub-issue of a labelled meta issue that was closed by
//! hand mid-run, closes the top meta issue of a meta of metas once every
//! sub-issue has a legitimate closer (the one close an agent may make, #1392
//! §5), and reports every open feature PR so a waiting one is never silent.
//!
//! It reads GitHub state only (labels, markers, `ClosedEvent.closer`, the
//! compare API), never `run.json` or any other local file, and it is
//! idempotent: its own comments carry a `<!-- grind:reconcile ... -->` tag so a
//! second pass posts nothing new, and the label is removed last, so a pass that
//! fails midway is retried in full by the next one.
//!
//! The module is split in two:
//! * a **pure decision layer** (`parse_marker`, `closer_from_graphql`,
//!   `feature_from_pr_json`, `decide`, `decide_unlabelled_child`,
//!   `top_meta_closable`, …) that is unit-tested against recorded JSON, and
//! * a **thin gh I/O layer** (`run`) that fetches facts and applies actions via
//!   `running_process` (never `std::process`).

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::path::Path;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use running_process::ReadStatus;
use serde_json::Value;

use crate::subprocess::ManagedSubprocess;
use crate::win_creation_flags::invisible_helper_creationflags;

/// The label a feature-branch run puts on every issue it fixed.
pub const ON_FEATURE_LABEL: &str = "grind:on-feature";

/// The label grind puts on the sub-meta issues it creates (#1392 §5).
pub const SUB_META_LABEL: &str = "grind:meta";

/// A feature PR untouched for this long is reported as stale, and the stale
/// comment on its meta issue is posted at most once per this window.
pub const STALE_AFTER_SECS: i64 = 14 * 86_400;

// ---------------------------------------------------------------------------
// Pure decision layer
// ---------------------------------------------------------------------------

/// Parsed `<!-- grind:v1 feature-pr=#812 branch=... goal-pr=#805 run=... -->`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Marker {
    pub feature_pr: u64,
    pub branch: String,
    pub goal_pr: Option<u64>,
    pub run: String,
}

fn parse_pr_ref(v: &str) -> Option<u64> {
    v.strip_prefix('#').unwrap_or(v).parse().ok()
}

/// Parse the **last** grind marker in `body` (a single comment, or several
/// comment bodies joined together). Returns `None` when no well-formed marker
/// is present; the bare `<!-- grind:v1 -->` sub-meta marker is not one.
pub fn parse_marker(body: &str) -> Option<Marker> {
    let mut found = None;
    let mut rest = body;
    while let Some(start) = rest.find("<!-- grind:v1") {
        let after = &rest[start + "<!-- grind:v1".len()..];
        let Some(end) = after.find("-->") else {
            break;
        };
        if let Some(m) = parse_marker_fields(&after[..end]) {
            found = Some(m);
        }
        rest = &after[end + 3..];
    }
    found
}

fn parse_marker_fields(inner: &str) -> Option<Marker> {
    let mut feature_pr = None;
    let mut branch = None;
    let mut goal_pr = None;
    let mut run = None;
    for tok in inner.split_whitespace() {
        let Some((k, v)) = tok.split_once('=') else {
            continue;
        };
        match k {
            "feature-pr" => feature_pr = parse_pr_ref(v),
            "branch" => branch = Some(v.to_string()),
            "goal-pr" => goal_pr = parse_pr_ref(v),
            "run" => run = Some(v.to_string()),
            _ => {}
        }
    }
    Some(Marker {
        feature_pr: feature_pr?,
        branch: branch?,
        goal_pr,
        run: run?,
    })
}

/// Take the last marker across a list of comment bodies (oldest first).
pub fn last_marker<'a>(comments: impl IntoIterator<Item = &'a str>) -> Option<Marker> {
    comments.into_iter().filter_map(parse_marker).last()
}

/// The hidden tag on each comment reconcile posts, so a later pass can tell
/// it has already said something (`kind` is `reopen`, `unmerged` or `stale`).
pub fn reconcile_tag(kind: &str, feature_pr: u64) -> String {
    format!("<!-- grind:reconcile {kind} feature-pr=#{feature_pr} -->")
}

/// One issue comment: its body and, when GitHub gave one, its creation time.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommentFacts {
    pub body: String,
    pub created_at: Option<i64>,
}

/// When reconcile last posted a `kind` comment about `feature_pr`, from the
/// comments list. A tagged comment without a timestamp counts as `now`.
pub fn last_tagged(
    comments: &[CommentFacts],
    kind: &str,
    feature_pr: u64,
    now: i64,
) -> Option<i64> {
    let tag = reconcile_tag(kind, feature_pr);
    comments
        .iter()
        .filter(|c| c.body.contains(&tag))
        .map(|c| c.created_at.unwrap_or(now))
        .max()
}

/// What closed the issue, from GraphQL `ClosedEvent.closer`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Closer {
    /// Closed by hand (or not closed at all).
    None,
    PullRequest {
        merged: bool,
        base_ref: String,
    },
    Commit {
        on_default: bool,
    },
}

/// State of the feature PR named in the marker.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FeaturePrState {
    Open { stale: bool },
    ClosedUnmerged,
    Merged { base_ref: String },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IssueFacts {
    pub number: u64,
    pub open: bool,
    pub closer: Closer,
    pub feature_pr: u64,
    pub feature: FeaturePrState,
    /// A meta issue (it has sub-issues, or a `meta` label): the one that gets
    /// the stale comment.
    pub is_meta: bool,
    /// Reconcile already posted the "closed unmerged" recovery comment.
    pub recovery_noted: bool,
    /// When reconcile last posted the stale comment on this issue.
    pub stale_noted_at: Option<i64>,
    pub now: i64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Action {
    Reopen { reason: String },
    RemoveLabel,
    Close { cite: String },
    Comment { body: String },
    ReportStale { why: String },
}

/// Decide the repair actions for one labelled issue. Pure and idempotent: an
/// open issue whose feature PR is open and healthy yields no actions, and so
/// does every issue once the actions for its state have been applied.
///
/// `RemoveLabel` is always the last action: the label is what makes the next
/// pass look at the issue again, so it goes only after everything else
/// succeeded.
pub fn decide(facts: &IssueFacts, default_branch: &str) -> Vec<Action> {
    let n = facts.feature_pr;
    match &facts.feature {
        FeaturePrState::Merged { base_ref } if base_ref == default_branch => {
            let mut out = Vec::new();
            if facts.open {
                out.push(Action::Close {
                    cite: format!("Fixed by feature PR #{n}, merged into `{default_branch}`."),
                });
            }
            out.push(Action::RemoveLabel);
            out
        }
        FeaturePrState::ClosedUnmerged => {
            let mut out = Vec::new();
            if !facts.open {
                out.push(Action::Reopen {
                    reason: format!("feature PR #{n} was closed without merging"),
                });
            }
            if !facts.recovery_noted {
                out.push(Action::Comment {
                    body: format!(
                        "Feature PR #{n} was closed without merging: the fix is not on \
                         `{default_branch}`. Its commits remain at `refs/pull/{n}/head`.\n\n{}",
                        reconcile_tag("unmerged", n)
                    ),
                });
            }
            out.push(Action::RemoveLabel);
            out
        }
        // Merged into a non-default branch, or still open: not landed.
        FeaturePrState::Merged { .. } | FeaturePrState::Open { .. } => {
            let mut out = Vec::new();
            let mut unlabel = false;
            if !facts.open {
                if closer_landed(&facts.closer, default_branch) {
                    // C1/C4: the fix reached the default branch some other
                    // way; nothing is pending on the feature branch any more.
                    unlabel = true;
                } else {
                    out.extend(reopen_actions(&facts.closer, n, default_branch));
                }
            }
            if let Some(why) = stale_reason(&facts.feature, default_branch) {
                let noted = facts
                    .stale_noted_at
                    .is_some_and(|t| facts.now - t < STALE_AFTER_SECS);
                if facts.is_meta && !noted {
                    out.push(Action::Comment {
                        body: format!(
                            "Feature PR #{n} needs attention: {why}. Its fixes have not reached \
                             `{default_branch}`.\n\n{}",
                            reconcile_tag("stale", n)
                        ),
                    });
                }
                out.push(Action::ReportStale { why });
            }
            if unlabel {
                out.push(Action::RemoveLabel);
            }
            out
        }
    }
}

fn reopen_actions(closer: &Closer, feature_pr: u64, default_branch: &str) -> Vec<Action> {
    let reason = closer_reason(closer, default_branch);
    vec![
        Action::Reopen {
            reason: reason.clone(),
        },
        Action::Comment {
            body: format!(
                "Reopened by `clud grind reconcile`: {reason}. The fix is tracked by feature \
                 PR #{feature_pr} and has not reached `{default_branch}`.\n\n{}",
                reconcile_tag("reopen", feature_pr)
            ),
        },
    ]
}

fn stale_reason(feature: &FeaturePrState, default_branch: &str) -> Option<String> {
    match feature {
        FeaturePrState::Open { stale: true } => {
            Some("it is stale (no activity in 14 days, conflicting, or its branch is gone)".into())
        }
        FeaturePrState::Merged { base_ref } if base_ref != default_branch => Some(format!(
            "it was merged into `{base_ref}`, not the default branch `{default_branch}`"
        )),
        _ => None,
    }
}

/// Whether `closer` put the fix on the **current** default branch.
pub fn closer_landed(closer: &Closer, default_branch: &str) -> bool {
    match closer {
        Closer::PullRequest { merged, base_ref } => *merged && base_ref == default_branch,
        Closer::Commit { on_default } => *on_default,
        Closer::None => false,
    }
}

fn closer_reason(closer: &Closer, default_branch: &str) -> String {
    match closer {
        Closer::PullRequest {
            merged: true,
            base_ref,
        } => {
            format!("closed by a PR merged into `{base_ref}`, not `{default_branch}`")
        }
        Closer::PullRequest { .. } => "closed by an unmerged PR".to_string(),
        Closer::Commit { .. } => format!("closed by a commit not on `{default_branch}`"),
        Closer::None => "closed by hand before the fix landed".to_string(),
    }
}

/// A sub-issue of a labelled meta issue, as the REST sub-issues list gives it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SubIssue {
    pub number: u64,
    pub open: bool,
    /// `completed`, `not_planned`, `duplicate`, `reopened`; `None` if absent.
    pub state_reason: Option<String>,
    pub closed_at: Option<i64>,
    pub labels: Vec<String>,
}

/// Parse `GET repos/{o}/{r}/issues/{n}/sub_issues`.
pub fn sub_issues_from_json(v: &Value) -> Vec<SubIssue> {
    v.as_array()
        .into_iter()
        .flatten()
        .filter_map(|s| {
            Some(SubIssue {
                number: s.get("number")?.as_u64()?,
                open: s
                    .get("state")
                    .and_then(Value::as_str)
                    .is_none_or(|st| st.eq_ignore_ascii_case("open")),
                state_reason: s
                    .get("state_reason")
                    .and_then(Value::as_str)
                    .map(str::to_ascii_lowercase),
                closed_at: s
                    .get("closed_at")
                    .and_then(Value::as_str)
                    .and_then(parse_iso8601_utc),
                labels: label_names(s),
            })
        })
        .collect()
}

/// A closed, unlabelled sub-issue of a meta issue whose feature PR is still
/// open (a child closed by hand mid-run, before its goal landed): reopen it
/// unless its closer landed the fix, it was closed as not planned or a
/// duplicate (a deliberate decision, not a false "done"), or it was already
/// closed before the feature PR was opened (not this run's business).
pub fn decide_unlabelled_child(
    child: &SubIssue,
    closer: &Closer,
    feature_pr: u64,
    feature_created: Option<i64>,
    default_branch: &str,
) -> Vec<Action> {
    let deliberate = matches!(
        child.state_reason.as_deref(),
        Some("not_planned" | "duplicate")
    );
    let before_run = matches!(
        (child.closed_at, feature_created),
        (Some(closed), Some(created)) if closed < created
    );
    if child.open || deliberate || before_run || closer_landed(closer, default_branch) {
        return Vec::new();
    }
    reopen_actions(closer, feature_pr, default_branch)
}

/// One sub-issue of a top meta issue, judged by its closer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChildClose {
    pub number: u64,
    pub open: bool,
    /// Closed by a PR merged into the current default branch, or by a commit
    /// on it.
    pub landed: bool,
}

/// The top meta issue of a meta of metas is closed by reconcile, and only
/// once every sub-issue (bug child or sub-meta issue) is closed by a
/// legitimate closer (#1392 §5, C10/C11). No sub-issues: nothing to judge.
pub fn top_meta_closable(children: &[ChildClose]) -> bool {
    !children.is_empty() && children.iter().all(|c| !c.open && c.landed)
}

/// Extract the closer from a GraphQL response of `closer_query`. Returns the
/// closer plus a commit oid when the closer is a commit (whose on-default
/// status the caller resolves; it is reported as `on_default: false` here).
pub fn closer_from_graphql(v: &Value) -> (Closer, Option<String>) {
    let closer = v
        .pointer("/data/repository/issue/timelineItems/nodes")
        .and_then(Value::as_array)
        .and_then(|a| a.last())
        .and_then(|n| n.get("closer"));
    let Some(c) = closer.filter(|c| !c.is_null()) else {
        return (Closer::None, None);
    };
    match c.get("__typename").and_then(Value::as_str) {
        Some("PullRequest") => (
            Closer::PullRequest {
                merged: c.get("merged").and_then(Value::as_bool).unwrap_or(false),
                base_ref: c
                    .get("baseRefName")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_string(),
            },
            None,
        ),
        Some("Commit") => (
            Closer::Commit { on_default: false },
            c.get("oid").and_then(Value::as_str).map(str::to_string),
        ),
        _ => (Closer::None, None),
    }
}

/// `compare/<default>...<oid>` status: `behind`/`identical` => oid is on default.
pub fn commit_on_default(compare: &Value) -> bool {
    matches!(
        compare.get("status").and_then(Value::as_str),
        Some("behind") | Some("identical")
    )
}

/// The `gh pr view --json` fields reconcile reads.
pub const PR_FIELDS: &str =
    "state,mergedAt,baseRefName,updatedAt,createdAt,mergeable,headRefName,isDraft,reviewDecision";

/// Map `gh pr view --json <PR_FIELDS>`. `branch_exists` and `now_secs` feed
/// the staleness rule.
pub fn feature_from_pr_json(v: &Value, now_secs: i64, branch_exists: bool) -> FeaturePrState {
    let state = v.get("state").and_then(Value::as_str).unwrap_or("");
    let base_ref = v
        .get("baseRefName")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();
    let merged = state == "MERGED"
        || v.get("mergedAt")
            .and_then(Value::as_str)
            .is_some_and(|s| !s.is_empty());
    if merged {
        return FeaturePrState::Merged { base_ref };
    }
    if state == "CLOSED" {
        return FeaturePrState::ClosedUnmerged;
    }
    let old = v
        .get("updatedAt")
        .and_then(Value::as_str)
        .and_then(parse_iso8601_utc)
        .is_some_and(|t| now_secs - t > STALE_AFTER_SECS);
    let conflicting = v.get("mergeable").and_then(Value::as_str) == Some("CONFLICTING");
    FeaturePrState::Open {
        stale: old || conflicting || !branch_exists,
    }
}

/// One line describing an open feature PR, for the report printed at every
/// grind start and cron tick ("a waiting feature PR is never silent").
pub fn describe_open_feature(v: &Value) -> String {
    let draft = v.get("isDraft").and_then(Value::as_bool).unwrap_or(false);
    let review = v.get("reviewDecision").and_then(Value::as_str).unwrap_or("");
    let conflicting = v.get("mergeable").and_then(Value::as_str) == Some("CONFLICTING");
    let mut parts = vec![if draft { "draft" } else { "ready" }];
    match review {
        "REVIEW_REQUIRED" => parts.push("waiting for review"),
        "CHANGES_REQUESTED" => parts.push("changes requested"),
        "APPROVED" => parts.push("approved"),
        _ => {}
    }
    if conflicting {
        parts.push("conflicting");
    }
    parts.join(", ")
}

/// Parse a batched parent query (`i<N>: issue(number:N){parent{number state}}`)
/// into `(child, parent, parent_open)` triples.
pub fn parents_from_graphql(v: &Value, children: &[u64]) -> Vec<(u64, u64, bool)> {
    children
        .iter()
        .filter_map(|&n| {
            let p = v.pointer(&format!("/data/repository/i{n}/parent"))?;
            let number = p.get("number")?.as_u64()?;
            let open = p.get("state").and_then(Value::as_str) == Some("OPEN");
            Some((n, number, open))
        })
        .collect()
}

fn label_names(v: &Value) -> Vec<String> {
    v.get("labels")
        .and_then(Value::as_array)
        .map(|ls| {
            ls.iter()
                .filter_map(|l| l.get("name").and_then(Value::as_str).map(str::to_string))
                .collect()
        })
        .unwrap_or_default()
}

/// Parse `YYYY-MM-DDTHH:MM:SSZ` (GitHub's timestamp form) to Unix seconds.
pub fn parse_iso8601_utc(s: &str) -> Option<i64> {
    let b = s.as_bytes();
    if b.len() < 19 {
        return None;
    }
    let num = |r: std::ops::Range<usize>| s.get(r)?.parse::<i64>().ok();
    let (y, m, d) = (num(0..4)?, num(5..7)?, num(8..10)?);
    let (hh, mm, ss) = (num(11..13)?, num(14..16)?, num(17..19)?);
    // days_from_civil, Howard Hinnant (public domain).
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let mp = (m + 9) % 12;
    let doy = (153 * mp + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    let days = era * 146_097 + doe - 719_468;
    Some(days * 86_400 + hh * 3_600 + mm * 60 + ss)
}

// ---------------------------------------------------------------------------
// gh I/O layer
// ---------------------------------------------------------------------------

fn run_captured(cwd: &Path, argv: Vec<String>) -> Result<(i32, String), String> {
    let process = ManagedSubprocess::start_inheriting_env(
        argv,
        Some(cwd.to_path_buf()),
        true,
        invisible_helper_creationflags(),
    )?;
    let mut buf = Vec::<u8>::new();
    loop {
        match process.read_stdout(Some(Duration::from_millis(100))) {
            ReadStatus::Line(line) => {
                buf.extend_from_slice(&line);
                buf.push(b'\n');
            }
            ReadStatus::Timeout => {
                let _ = process.poll();
            }
            ReadStatus::Eof => break,
        }
    }
    let code = process.wait(Some(Duration::from_secs(60)))?;
    Ok((code, String::from_utf8_lossy(&buf).into_owned()))
}

fn gh(cwd: &Path, args: &[&str]) -> Result<(i32, String), String> {
    let mut argv = vec!["gh".to_string()];
    argv.extend(args.iter().map(|s| s.to_string()));
    run_captured(cwd, argv)
}

fn gh_json(cwd: &Path, args: &[&str]) -> Result<Value, String> {
    let (code, out) = gh(cwd, args)?;
    if code != 0 {
        return Err(format!("`gh {}` exited {code}", args.join(" ")));
    }
    serde_json::from_str(&out).map_err(|e| format!("`gh {}`: bad JSON: {e}", args.join(" ")))
}

fn graphql(cwd: &Path, query: &str) -> Result<Value, String> {
    let q = format!("query={query}");
    gh_json(
        cwd,
        &[
            "api",
            "graphql",
            "-F",
            "owner={owner}",
            "-F",
            "repo={repo}",
            "-f",
            &q,
        ],
    )
}

fn closer_query(number: u64) -> String {
    format!(
        "query($owner:String!,$repo:String!){{repository(owner:$owner,name:$repo){{\
         issue(number:{number}){{timelineItems(itemTypes:CLOSED_EVENT,last:1){{nodes{{\
         ... on ClosedEvent{{closer{{__typename ... on PullRequest{{merged baseRefName}} \
         ... on Commit{{oid}}}}}}}}}}}}}}}}"
    )
}

fn parent_query(numbers: &[u64]) -> String {
    let fields: Vec<String> = numbers
        .iter()
        .map(|n| format!("i{n}:issue(number:{n}){{parent{{number state}}}}"))
        .collect();
    format!(
        "query($owner:String!,$repo:String!){{repository(owner:$owner,name:$repo){{{}}}}}",
        fields.join(" ")
    )
}

fn now_secs() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

/// The closer of a closed issue, judged against the current default branch.
fn fetch_closer(cwd: &Path, number: u64, default_branch: &str) -> Result<Closer, String> {
    match closer_from_graphql(&graphql(cwd, &closer_query(number))?) {
        (Closer::Commit { .. }, Some(oid)) => {
            let path = format!("repos/{{owner}}/{{repo}}/compare/{default_branch}...{oid}");
            let on_default = gh_json(cwd, &["api", &path])
                .map(|c| commit_on_default(&c))
                .unwrap_or(false);
            Ok(Closer::Commit { on_default })
        }
        (c, _) => Ok(c),
    }
}

fn fetch_comments(cwd: &Path, number: u64) -> Result<Vec<CommentFacts>, String> {
    let n = number.to_string();
    let v = gh_json(cwd, &["issue", "view", &n, "--json", "comments"])?;
    Ok(v.get("comments")
        .and_then(Value::as_array)
        .map(|a| {
            a.iter()
                .map(|c| CommentFacts {
                    body: c
                        .get("body")
                        .and_then(Value::as_str)
                        .unwrap_or_default()
                        .to_string(),
                    created_at: c
                        .get("createdAt")
                        .and_then(Value::as_str)
                        .and_then(parse_iso8601_utc),
                })
                .collect()
        })
        .unwrap_or_default())
}

fn fetch_sub_issues(cwd: &Path, number: u64) -> Result<Vec<SubIssue>, String> {
    let path = format!("repos/{{owner}}/{{repo}}/issues/{number}/sub_issues?per_page=100");
    Ok(sub_issues_from_json(&gh_json(cwd, &["api", &path])?))
}

/// A feature PR as reconcile sees it, fetched once per pass.
#[derive(Debug, Clone)]
struct FeaturePr {
    state: FeaturePrState,
    branch: String,
    created_at: Option<i64>,
    describe: String,
}

fn fetch_feature(cwd: &Path, pr: u64, marker_branch: &str, now: i64) -> Result<FeaturePr, String> {
    let n = pr.to_string();
    let v = gh_json(cwd, &["pr", "view", &n, "--json", PR_FIELDS])?;
    let branch = v
        .get("headRefName")
        .and_then(Value::as_str)
        .unwrap_or(marker_branch)
        .to_string();
    let open = v.get("state").and_then(Value::as_str) == Some("OPEN");
    let branch_exists = !open || {
        let path = format!("repos/{{owner}}/{{repo}}/branches/{branch}");
        matches!(gh(cwd, &["api", &path]), Ok((0, _)))
    };
    Ok(FeaturePr {
        state: feature_from_pr_json(&v, now, branch_exists),
        created_at: v
            .get("createdAt")
            .and_then(Value::as_str)
            .and_then(parse_iso8601_utc),
        describe: describe_open_feature(&v),
        branch,
    })
}

fn apply(cwd: &Path, number: u64, action: &Action) -> Result<(), String> {
    let owned = number.to_string();
    let n: &str = owned.as_str();
    let (args, line): (Vec<&str>, String) = match action {
        Action::Reopen { reason } => (
            vec!["issue", "reopen", n],
            format!("#{number}: reopen ({reason})"),
        ),
        Action::RemoveLabel => (
            vec!["issue", "edit", n, "--remove-label", ON_FEATURE_LABEL],
            format!("#{number}: remove label {ON_FEATURE_LABEL}"),
        ),
        Action::Close { cite } => (
            vec!["issue", "close", n, "--comment", cite.as_str()],
            format!("#{number}: close ({cite})"),
        ),
        Action::Comment { body } => (
            vec!["issue", "comment", n, "--body", body.as_str()],
            format!("#{number}: comment"),
        ),
        Action::ReportStale { .. } => return Ok(()),
    };
    println!("{line}");
    let (code, _) = gh(cwd, &args)?;
    if code != 0 {
        return Err(format!("#{number}: `gh {}` exited {code}", args.join(" ")));
    }
    Ok(())
}

/// Apply `actions` in order, stopping at the first failure so the label (always
/// last) survives for the next pass to retry. Returns whether all succeeded.
fn apply_all(cwd: &Path, number: u64, actions: &[Action]) -> bool {
    for action in actions {
        if let Err(e) = apply(cwd, number, action) {
            eprintln!("{e}");
            return false;
        }
    }
    true
}

/// One pass's accumulated state.
#[derive(Default)]
struct Pass {
    failed: bool,
    features: HashMap<u64, FeaturePr>,
    /// Open feature PR -> the issues that wait on it.
    open_features: BTreeMap<u64, BTreeSet<u64>>,
    stale: BTreeMap<u64, String>,
    /// Meta issues whose parent may be a top meta issue.
    metas: BTreeSet<u64>,
}

impl Pass {
    fn feature(&mut self, cwd: &Path, marker: &Marker, now: i64) -> Result<FeaturePr, String> {
        if let Some(f) = self.features.get(&marker.feature_pr) {
            return Ok(f.clone());
        }
        let f = fetch_feature(cwd, marker.feature_pr, &marker.branch, now)?;
        self.features.insert(marker.feature_pr, f.clone());
        Ok(f)
    }
}

fn reconcile_issue(
    cwd: &Path,
    pass: &mut Pass,
    issue: &Value,
    labelled: &BTreeSet<u64>,
    default_branch: &str,
    now: i64,
) -> Result<(), String> {
    let Some(number) = issue.get("number").and_then(Value::as_u64) else {
        return Ok(());
    };
    let open = issue.get("state").and_then(Value::as_str) == Some("OPEN");
    let labels = label_names(issue);
    let comments = fetch_comments(cwd, number)?;
    let Some(marker) = last_marker(comments.iter().map(|c| c.body.as_str())) else {
        eprintln!("#{number}: labelled {ON_FEATURE_LABEL} but has no grind marker; skipping");
        return Ok(());
    };
    let feature = pass.feature(cwd, &marker, now)?;
    let closer = if open {
        Closer::None
    } else {
        fetch_closer(cwd, number, default_branch)?
    };
    let pending = !matches!(
        &feature.state,
        FeaturePrState::ClosedUnmerged | FeaturePrState::Merged { .. }
    ) || matches!(&feature.state, FeaturePrState::Merged { base_ref } if base_ref != default_branch);
    let subs = if pending {
        fetch_sub_issues(cwd, number)?
    } else {
        Vec::new()
    };
    let is_meta = !subs.is_empty()
        || labels
            .iter()
            .any(|l| l.eq_ignore_ascii_case("meta") || l == SUB_META_LABEL);
    let facts = IssueFacts {
        number,
        open,
        closer,
        feature_pr: marker.feature_pr,
        feature: feature.state.clone(),
        is_meta,
        recovery_noted: last_tagged(&comments, "unmerged", marker.feature_pr, now).is_some(),
        stale_noted_at: last_tagged(&comments, "stale", marker.feature_pr, now),
        now,
    };
    let actions = decide(&facts, default_branch);
    for a in &actions {
        if let Action::ReportStale { why } = a {
            pass.stale.insert(marker.feature_pr, why.clone());
        }
    }
    if !apply_all(cwd, number, &actions) {
        pass.failed = true;
    }
    if let FeaturePrState::Open { .. } = feature.state {
        pass.open_features
            .entry(marker.feature_pr)
            .or_default()
            .insert(number);
        // A child closed by hand before its goal landed carries no label yet.
        for child in subs.iter().filter(|s| !s.open && !labelled.contains(&s.number)) {
            let closer = fetch_closer(cwd, child.number, default_branch)?;
            let acts = decide_unlabelled_child(
                child,
                &closer,
                marker.feature_pr,
                feature.created_at,
                default_branch,
            );
            if !apply_all(cwd, child.number, &acts) {
                pass.failed = true;
            }
        }
    }
    if is_meta {
        pass.metas.insert(number);
    }
    Ok(())
}

/// Close each open top meta issue (the parent of a meta issue) whose every
/// sub-issue has a legitimate closer.
fn reconcile_top_metas(cwd: &Path, pass: &mut Pass, default_branch: &str) -> Result<(), String> {
    let sub_metas = gh_json(
        cwd,
        &[
            "issue",
            "list",
            "--label",
            SUB_META_LABEL,
            "--state",
            "all",
            "--limit",
            "1000",
            "--json",
            "number",
        ],
    )
    .unwrap_or(Value::Null);
    let mut candidates: BTreeSet<u64> = pass.metas.clone();
    candidates.extend(
        sub_metas
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|i| i.get("number").and_then(Value::as_u64)),
    );
    if candidates.is_empty() {
        return Ok(());
    }
    let numbers: Vec<u64> = candidates.into_iter().collect();
    let parents = graphql(cwd, &parent_query(&numbers))?;
    let tops: BTreeSet<u64> = parents_from_graphql(&parents, &numbers)
        .into_iter()
        .filter(|&(_, _, open)| open)
        .map(|(_, parent, _)| parent)
        .collect();
    for top in tops {
        let mut children = Vec::new();
        for s in fetch_sub_issues(cwd, top)? {
            let landed = !s.open && closer_landed(&fetch_closer(cwd, s.number, default_branch)?, default_branch);
            children.push(ChildClose {
                number: s.number,
                open: s.open,
                landed,
            });
            if s.open {
                break;
            }
        }
        if top_meta_closable(&children) {
            let cite = format!(
                "Closed by `clud grind reconcile`: every sub-issue is closed by a merge into \
                 `{default_branch}`."
            );
            if !apply_all(cwd, top, &[Action::Close { cite }]) {
                pass.failed = true;
            }
        }
    }
    Ok(())
}

/// Reconcile every `grind:on-feature` issue in the repo at `cwd`.
/// Returns exit code 0 on success, 1 if any issue failed to reconcile.
pub fn run(cwd: &Path) -> Result<i32, String> {
    let repo = gh_json(cwd, &["repo", "view", "--json", "defaultBranchRef"])?;
    let default_branch = repo
        .pointer("/defaultBranchRef/name")
        .and_then(Value::as_str)
        .ok_or("could not resolve the default branch")?
        .to_string();
    let issues = gh_json(
        cwd,
        &[
            "issue",
            "list",
            "--label",
            ON_FEATURE_LABEL,
            "--state",
            "all",
            "--limit",
            "1000",
            "--json",
            "number,state,labels",
        ],
    )?;
    let now = now_secs();
    let labelled: BTreeSet<u64> = issues
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|i| i.get("number").and_then(Value::as_u64))
        .collect();
    let mut pass = Pass::default();
    for issue in issues.as_array().into_iter().flatten() {
        if let Err(e) = reconcile_issue(cwd, &mut pass, issue, &labelled, &default_branch, now) {
            eprintln!("{e}");
            pass.failed = true;
        }
    }
    if let Err(e) = reconcile_top_metas(cwd, &mut pass, &default_branch) {
        eprintln!("top meta issues: {e}");
        pass.failed = true;
    }
    if !pass.open_features.is_empty() {
        println!("open feature PRs (their fixes have not reached `{default_branch}`):");
        for (pr, waiting) in &pass.open_features {
            let (branch, describe) = pass
                .features
                .get(pr)
                .map(|f| (f.branch.as_str(), f.describe.as_str()))
                .unwrap_or(("?", "?"));
            let issues: Vec<String> = waiting.iter().map(|n| format!("#{n}")).collect();
            println!(
                "  feature PR #{pr} ({branch}): {describe}; waiting: {}",
                issues.join(", ")
            );
        }
    }
    if !pass.stale.is_empty() {
        println!("stale feature PRs:");
        for (pr, why) in &pass.stale {
            println!("  STALE feature PR #{pr}: {why}");
        }
    }
    Ok(if pass.failed { 1 } else { 0 })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    const NOW: i64 = 1_790_000_000; // 2026-09-21

    fn closer_json(closer: Value) -> Value {
        json!({"data":{"repository":{"issue":{"timelineItems":{"nodes":[{"closer": closer}]}}}}})
    }

    fn facts(open: bool, closer: Closer, feature: FeaturePrState) -> IssueFacts {
        IssueFacts {
            number: 7,
            open,
            closer,
            feature_pr: 812,
            feature,
            is_meta: false,
            recovery_noted: false,
            stale_noted_at: None,
            now: NOW,
        }
    }

    fn open_pr() -> Value {
        json!({"state":"OPEN","mergedAt":null,"baseRefName":"main",
               "updatedAt":"2026-09-20T00:00:00Z","mergeable":"MERGEABLE",
               "headRefName":"grind/x-1","isDraft":true,"reviewDecision":""})
    }

    fn healthy() -> FeaturePrState {
        feature_from_pr_json(&open_pr(), NOW, true)
    }

    fn reopens(acts: &[Action]) -> bool {
        matches!(acts.first(), Some(Action::Reopen { .. }))
            && acts.iter().any(|a| matches!(
                a,
                Action::Comment { body } if body.contains(&reconcile_tag("reopen", 812))
            ))
    }

    /// The state after applying `acts` to `f` (what GitHub would show next pass).
    fn after(mut f: IssueFacts, acts: &[Action], labelled: &mut bool) -> IssueFacts {
        for a in acts {
            match a {
                Action::Reopen { .. } => {
                    f.open = true;
                    f.closer = Closer::None;
                }
                Action::Close { .. } => f.open = false,
                Action::RemoveLabel => *labelled = false,
                Action::Comment { body } => {
                    if body.contains(&reconcile_tag("unmerged", f.feature_pr)) {
                        f.recovery_noted = true;
                    }
                    if body.contains(&reconcile_tag("stale", f.feature_pr)) {
                        f.stale_noted_at = Some(f.now);
                    }
                }
                Action::ReportStale { .. } => {}
            }
        }
        f
    }

    // C1
    #[test]
    fn c1_closer_pr_merged_into_default_leaves_closed_and_unlabels() {
        let (closer, _) = closer_from_graphql(&closer_json(
            json!({"__typename":"PullRequest","merged":true,"baseRefName":"main"}),
        ));
        let acts = decide(&facts(false, closer, healthy()), "main");
        assert_eq!(acts, vec![Action::RemoveLabel]);
    }

    // C2
    #[test]
    fn c2_closer_pr_merged_into_non_default_reopens_and_comments() {
        let (closer, _) = closer_from_graphql(&closer_json(
            json!({"__typename":"PullRequest","merged":true,"baseRefName":"grind/x-1"}),
        ));
        let acts = decide(&facts(false, closer, healthy()), "main");
        assert!(reopens(&acts), "{acts:?}");
        assert!(!acts.contains(&Action::RemoveLabel));
    }

    // C3
    #[test]
    fn c3_closed_by_hand_reopens_and_comments() {
        let (closer, oid) = closer_from_graphql(&closer_json(Value::Null));
        assert_eq!((closer.clone(), oid), (Closer::None, None));
        let empty = json!({"data":{"repository":{"issue":{"timelineItems":{"nodes":[]}}}}});
        assert_eq!(closer_from_graphql(&empty).0, Closer::None);
        let acts = decide(&facts(false, closer, healthy()), "main");
        assert!(reopens(&acts), "{acts:?}");
        assert!(format!("{acts:?}").contains("closed by hand"));
    }

    // C4
    #[test]
    fn c4_closer_commit_on_default_is_landed() {
        assert!(commit_on_default(&json!({"status":"behind"})));
        assert!(commit_on_default(&json!({"status":"identical"})));
        let acts = decide(
            &facts(false, Closer::Commit { on_default: true }, healthy()),
            "main",
        );
        assert_eq!(acts, vec![Action::RemoveLabel]);
    }

    // C5
    #[test]
    fn c5_closer_commit_not_on_default_reopens() {
        let (closer, oid) =
            closer_from_graphql(&closer_json(json!({"__typename":"Commit","oid":"abc"})));
        assert_eq!(oid.as_deref(), Some("abc"));
        assert_eq!(closer, Closer::Commit { on_default: false });
        for status in ["ahead", "diverged"] {
            assert!(!commit_on_default(&json!({ "status": status })));
        }
        let acts = decide(
            &facts(false, Closer::Commit { on_default: false }, healthy()),
            "main",
        );
        assert!(reopens(&acts), "{acts:?}");
    }

    // C6
    #[test]
    fn c6_closer_is_judged_against_the_current_default_branch() {
        // Closed by a PR merged into `master`; the default is now `main`.
        let closer = Closer::PullRequest {
            merged: true,
            base_ref: "master".into(),
        };
        assert!(reopens(&decide(&facts(false, closer.clone(), healthy()), "main")));
        assert_eq!(
            decide(&facts(false, closer, healthy()), "master"),
            vec![Action::RemoveLabel]
        );
        let unmerged = Closer::PullRequest {
            merged: false,
            base_ref: "main".into(),
        };
        assert!(reopens(&decide(&facts(false, unmerged, healthy()), "main")));
    }

    // C7
    #[test]
    fn c7_feature_closed_unmerged_reopens_unlabels_and_cites_ref() {
        let pr = json!({"state":"CLOSED","mergedAt":null,"baseRefName":"main",
                        "updatedAt":"2026-09-20T00:00:00Z","mergeable":"UNKNOWN",
                        "headRefName":"grind/x-1"});
        let feature = feature_from_pr_json(&pr, NOW, false);
        assert_eq!(feature, FeaturePrState::ClosedUnmerged);
        for open in [false, true] {
            let acts = decide(&facts(open, Closer::None, feature.clone()), "main");
            assert_eq!(matches!(acts[0], Action::Reopen { .. }), !open, "{acts:?}");
            assert_eq!(acts.last(), Some(&Action::RemoveLabel), "{acts:?}");
            assert!(acts.iter().any(|a| matches!(
                a,
                Action::Comment { body } if body.contains("refs/pull/812/head")
            )));
        }
    }

    // C8
    #[test]
    fn c8_feature_merged_to_default_closes_open_issue_then_unlabels() {
        let pr = json!({"state":"MERGED","mergedAt":"2026-09-20T00:00:00Z",
                        "baseRefName":"main","updatedAt":"2026-09-20T00:00:00Z",
                        "mergeable":"UNKNOWN","headRefName":"grind/x-1"});
        let feature = feature_from_pr_json(&pr, NOW, false);
        let acts = decide(&facts(true, Closer::None, feature.clone()), "main");
        assert!(matches!(&acts[0], Action::Close { cite } if cite.contains("#812")));
        assert_eq!(acts.last(), Some(&Action::RemoveLabel));
        // Already closed by the merge: only the label goes.
        let closer = Closer::PullRequest {
            merged: true,
            base_ref: "main".into(),
        };
        assert_eq!(
            decide(&facts(false, closer, feature), "main"),
            vec![Action::RemoveLabel]
        );
    }

    #[test]
    fn feature_merged_to_non_default_keeps_label_reopens_and_reports() {
        let feature = FeaturePrState::Merged {
            base_ref: "develop".into(),
        };
        let acts = decide(&facts(false, Closer::None, feature), "main");
        assert!(reopens(&acts), "{acts:?}");
        assert!(!acts.contains(&Action::RemoveLabel));
        assert!(acts
            .iter()
            .any(|a| matches!(a, Action::ReportStale { why } if why.contains("develop"))));
    }

    // C9
    #[test]
    fn c9_stale_is_reported_and_commented_once_per_window() {
        let mut pr = open_pr();
        pr["updatedAt"] = json!("2026-08-01T00:00:00Z");
        let stale = feature_from_pr_json(&pr, NOW, true);
        assert_eq!(stale, FeaturePrState::Open { stale: true });
        let mut conflicting = open_pr();
        conflicting["mergeable"] = json!("CONFLICTING");
        assert_eq!(
            feature_from_pr_json(&conflicting, NOW, true),
            FeaturePrState::Open { stale: true }
        );
        assert_eq!(
            feature_from_pr_json(&open_pr(), NOW, false),
            FeaturePrState::Open { stale: true }
        );

        // A child is reported, never commented.
        let acts = decide(&facts(true, Closer::None, stale.clone()), "main");
        assert!(matches!(acts.as_slice(), [Action::ReportStale { .. }]), "{acts:?}");

        // The meta issue gets one comment per staleness window.
        let mut meta = facts(true, Closer::None, stale);
        meta.is_meta = true;
        let acts = decide(&meta, "main");
        assert!(acts.iter().any(|a| matches!(
            a,
            Action::Comment { body } if body.contains(&reconcile_tag("stale", 812))
        )));
        meta.stale_noted_at = Some(NOW - 86_400);
        let acts = decide(&meta, "main");
        assert!(matches!(acts.as_slice(), [Action::ReportStale { .. }]), "{acts:?}");
        meta.stale_noted_at = Some(NOW - STALE_AFTER_SECS - 1);
        assert!(decide(&meta, "main")
            .iter()
            .any(|a| matches!(a, Action::Comment { .. })));
    }

    // C10, C11
    #[test]
    fn c10_c11_top_meta_closes_only_when_every_child_landed() {
        let child = |number, open, landed| ChildClose {
            number,
            open,
            landed,
        };
        assert!(top_meta_closable(&[
            child(2, false, true),
            child(3, false, true)
        ]));
        // C11: a sub-meta issue still open.
        assert!(!top_meta_closable(&[
            child(2, false, true),
            child(3, true, false)
        ]));
        // Closed, but by something that did not land the fix.
        assert!(!top_meta_closable(&[
            child(2, false, true),
            child(3, false, false)
        ]));
        assert!(!top_meta_closable(&[]));
    }

    #[test]
    fn parents_parse_from_a_batched_query() {
        let v = json!({"data":{"repository":{
            "i5":{"parent":{"number":1,"state":"OPEN"}},
            "i6":{"parent":null},
            "i7":{"parent":{"number":2,"state":"CLOSED"}}}}});
        assert_eq!(
            parents_from_graphql(&v, &[5, 6, 7, 8]),
            vec![(5, 1, true), (7, 2, false)]
        );
        assert!(parents_from_graphql(&json!({"data":{}}), &[5]).is_empty());
        let q = parent_query(&[5, 7]);
        assert!(q.contains("i5:issue(number:5){parent{number state}}"), "{q}");
        assert!(!q.contains("closer") && !q.contains("ClosedEvent"), "{q}");
    }

    // C12
    #[test]
    fn c12_every_row_is_idempotent() {
        let stale = FeaturePrState::Open { stale: true };
        let cases = [
            facts(false, Closer::None, healthy()),
            facts(false, Closer::Commit { on_default: false }, healthy()),
            facts(true, Closer::None, FeaturePrState::ClosedUnmerged),
            facts(false, Closer::None, FeaturePrState::ClosedUnmerged),
            facts(
                true,
                Closer::None,
                FeaturePrState::Merged {
                    base_ref: "main".into(),
                },
            ),
            IssueFacts {
                is_meta: true,
                ..facts(true, Closer::None, stale)
            },
        ];
        for f in cases {
            let mut labelled = true;
            let first = decide(&f, "main");
            let next = after(f.clone(), &first, &mut labelled);
            let second = if labelled {
                decide(&next, "main")
            } else {
                Vec::new()
            };
            let changes = |acts: &[Action]| {
                acts.iter()
                    .filter(|a| !matches!(a, Action::ReportStale { .. }))
                    .count()
            };
            assert_eq!(changes(&second), 0, "{f:?}: {first:?} then {second:?}");
        }
    }

    #[test]
    fn healthy_open_issue_is_a_no_op() {
        assert_eq!(healthy(), FeaturePrState::Open { stale: false });
        assert!(decide(&facts(true, Closer::None, healthy()), "main").is_empty());
    }

    #[test]
    fn open_features_are_described_for_the_report() {
        assert_eq!(describe_open_feature(&open_pr()), "draft");
        let mut waiting = open_pr();
        waiting["isDraft"] = json!(false);
        waiting["reviewDecision"] = json!("REVIEW_REQUIRED");
        assert_eq!(describe_open_feature(&waiting), "ready, waiting for review");
        waiting["mergeable"] = json!("CONFLICTING");
        assert!(describe_open_feature(&waiting).ends_with("conflicting"));
    }

    #[test]
    fn unlabelled_child_closed_by_hand_mid_run_is_reopened() {
        let v = json!([
            {"number": 11, "state": "closed", "state_reason": "completed",
             "closed_at": "2026-09-20T00:00:00Z", "labels": []},
            {"number": 12, "state": "open"},
            {"number": 13, "state": "CLOSED", "state_reason": "not_planned"}
        ]);
        let subs = sub_issues_from_json(&v);
        assert_eq!(subs.len(), 3);
        assert!(!subs[0].open && subs[1].open && !subs[2].open);
        let created = parse_iso8601_utc("2026-09-19T00:00:00Z");
        let reopen = decide_unlabelled_child(&subs[0], &Closer::None, 812, created, "main");
        assert!(reopens(&reopen), "{reopen:?}");
        // Open, not planned, closed before the run, or closed by a merge into main.
        assert!(decide_unlabelled_child(&subs[1], &Closer::None, 812, created, "main").is_empty());
        assert!(decide_unlabelled_child(&subs[2], &Closer::None, 812, created, "main").is_empty());
        let later = parse_iso8601_utc("2026-09-21T00:00:00Z");
        assert!(decide_unlabelled_child(&subs[0], &Closer::None, 812, later, "main").is_empty());
        let landed = Closer::PullRequest {
            merged: true,
            base_ref: "main".into(),
        };
        assert!(decide_unlabelled_child(&subs[0], &landed, 812, created, "main").is_empty());
    }

    #[test]
    fn tagged_comments_are_found_by_kind_and_pr() {
        let comments = vec![
            CommentFacts {
                body: format!("x\n{}", reconcile_tag("stale", 812)),
                created_at: Some(NOW - 10),
            },
            CommentFacts {
                body: reconcile_tag("stale", 900),
                created_at: Some(NOW),
            },
            CommentFacts {
                body: reconcile_tag("unmerged", 812),
                created_at: None,
            },
        ];
        assert_eq!(last_tagged(&comments, "stale", 812, NOW), Some(NOW - 10));
        assert_eq!(last_tagged(&comments, "unmerged", 812, NOW), Some(NOW));
        assert_eq!(last_tagged(&comments, "reopen", 812, NOW), None);
        // A reconcile tag is never mistaken for a grind marker.
        assert_eq!(parse_marker(&reconcile_tag("stale", 812)), None);
    }

    #[test]
    fn marker_parses_and_last_wins() {
        let a = "hi <!-- grind:v1 feature-pr=#812 branch=grind/foo-r1 goal-pr=#805 run=r1 -->";
        let b = "<!-- grind:v1 feature-pr=#900 branch=grind/foo-r2 run=r2 -->";
        assert_eq!(
            parse_marker(a),
            Some(Marker {
                feature_pr: 812,
                branch: "grind/foo-r1".into(),
                goal_pr: Some(805),
                run: "r1".into(),
            })
        );
        let m = last_marker([a, "no marker", b]).unwrap();
        assert_eq!(m.feature_pr, 900);
        assert_eq!(m.goal_pr, None);
        assert_eq!(parse_marker(&format!("{a}\n{b}")).unwrap().run, "r2");
        assert_eq!(parse_marker("<!-- grind:v1 branch=x run=y -->"), None);
        // The bare sub-meta marker is not a feature marker.
        assert_eq!(parse_marker("<!-- grind:v1 -->"), None);
    }

    // S6: two runs on the same meta issue carry distinct branches and run ids.
    #[test]
    fn two_runs_on_one_meta_have_distinct_markers() {
        let r1 = "<!-- grind:v1 feature-pr=#101 branch=grind/meta-100-1f3a goal-pr=#102 run=1f3a -->";
        let r2 = "<!-- grind:v1 feature-pr=#201 branch=grind/meta-100-9c0d goal-pr=#202 run=9c0d -->";
        let (a, b) = (parse_marker(r1).unwrap(), parse_marker(r2).unwrap());
        assert_ne!(a.branch, b.branch);
        assert_ne!(a.run, b.run);
        assert_eq!(last_marker([r1, r2]).unwrap().feature_pr, 201);
    }

    #[test]
    fn iso8601_parses() {
        assert_eq!(parse_iso8601_utc("1970-01-02T00:00:01Z"), Some(86_401));
    }
}
