//! `clud grind reconcile` — never lose an issue in feature-branch mode (#1393 §3).
//!
//! Issues a feature-branch grind run fixed carry the `grind:on-feature` label and
//! a `<!-- grind:v1 ... -->` marker comment naming the feature PR. GitHub only
//! auto-closes an issue when its fix lands on the default branch, so an issue
//! can be closed by the wrong thing (a PR merged into a non-default branch, a
//! commit that never reached the default branch) or left dangling (feature PR
//! closed unmerged). `reconcile` walks every labelled issue and repairs it.
//!
//! The module is split in two:
//! * a **pure decision layer** (`parse_marker`, `closer_from_graphql`,
//!   `feature_from_pr_json`, `decide`) that is unit-tested against recorded
//!   JSON, and
//! * a **thin gh I/O layer** (`run`) that fetches facts and applies actions via
//!   `running_process` (never `std::process`).

use std::path::Path;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use running_process::ReadStatus;
use serde_json::Value;

use crate::subprocess::ManagedSubprocess;
use crate::win_creation_flags::invisible_helper_creationflags;

/// The label a feature-branch run puts on every issue it fixed.
pub const ON_FEATURE_LABEL: &str = "grind:on-feature";

/// A feature PR untouched for this long is reported as stale.
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
/// is present.
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

/// What closed the issue, from GraphQL `ClosedEvent.closer`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Closer {
    /// Closed by hand (or not closed at all).
    None,
    PullRequest { merged: bool, base_ref: String },
    Commit { on_default: bool },
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
    pub is_meta: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Action {
    Reopen { reason: String },
    RemoveLabel,
    Close { cite: String },
    Comment { body: String },
    ReportStale,
}

/// Decide the repair actions for one labelled issue. Pure and idempotent: an
/// open issue whose feature PR is open and healthy yields no actions.
pub fn decide(facts: &IssueFacts, default_branch: &str) -> Vec<Action> {
    let n = facts.feature_pr;
    match &facts.feature {
        FeaturePrState::Merged { base_ref } if base_ref == default_branch => {
            let mut out = vec![Action::RemoveLabel];
            if facts.open {
                out.push(Action::Close {
                    cite: format!("fixed by feature PR #{n}, merged into `{default_branch}`"),
                });
            }
            out
        }
        FeaturePrState::ClosedUnmerged => {
            let mut out = Vec::new();
            if !facts.open {
                out.push(Action::Reopen {
                    reason: format!("feature PR #{n} was closed without merging"),
                });
            }
            out.push(Action::RemoveLabel);
            out.push(Action::Comment {
                body: format!(
                    "Feature PR #{n} was closed unmerged: the fix is not on \
                     `{default_branch}`; its commits remain at refs/pull/{n}/head."
                ),
            });
            out
        }
        // Merged into a non-default branch, or still open: not landed.
        FeaturePrState::Merged { .. } | FeaturePrState::Open { .. } => {
            let mut out = Vec::new();
            if !facts.open && !closer_landed(&facts.closer, default_branch) {
                let reason = closer_reason(&facts.closer, default_branch);
                out.push(Action::Reopen {
                    reason: reason.clone(),
                });
                out.push(Action::Comment {
                    body: format!(
                        "Reopened by `clud grind reconcile`: {reason}. The fix is \
                         tracked by feature PR #{n} and has not reached `{default_branch}`."
                    ),
                });
            }
            if let FeaturePrState::Open { stale: true } = facts.feature {
                out.push(Action::ReportStale);
                if facts.is_meta {
                    out.push(Action::Comment {
                        body: format!(
                            "Feature PR #{n} is stale (no activity in 14 days, \
                             conflicting, or its branch is gone)."
                        ),
                    });
                }
            }
            out
        }
    }
}

fn closer_landed(closer: &Closer, default_branch: &str) -> bool {
    match closer {
        Closer::PullRequest { merged, base_ref } => *merged && base_ref == default_branch,
        Closer::Commit { on_default } => *on_default,
        Closer::None => false,
    }
}

fn closer_reason(closer: &Closer, default_branch: &str) -> String {
    match closer {
        Closer::PullRequest { merged: true, base_ref } => {
            format!("closed by a PR merged into `{base_ref}`, not `{default_branch}`")
        }
        Closer::PullRequest { .. } => "closed by an unmerged PR".to_string(),
        Closer::Commit { .. } => format!("closed by a commit not on `{default_branch}`"),
        Closer::None => "closed by hand before the fix landed".to_string(),
    }
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

/// Map `gh pr view --json state,mergedAt,baseRefName,updatedAt,mergeable,headRefName`.
/// `branch_exists` and `now_secs` feed the staleness rule.
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
            ReadStatus::Line(line) => buf.extend_from_slice(&line),
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

fn closer_query(number: u64) -> String {
    format!(
        "query($owner:String!,$repo:String!){{repository(owner:$owner,name:$repo){{\
         issue(number:{number}){{timelineItems(itemTypes:CLOSED_EVENT,last:1){{nodes{{\
         ... on ClosedEvent{{closer{{__typename ... on PullRequest{{merged baseRefName}} \
         ... on Commit{{oid}}}}}}}}}}}}}}}}"
    )
}

fn now_secs() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

fn gather_facts(
    cwd: &Path,
    issue: &Value,
    default_branch: &str,
) -> Result<Option<IssueFacts>, String> {
    let Some(number) = issue.get("number").and_then(Value::as_u64) else {
        return Ok(None);
    };
    let n = number.to_string();
    let open = issue.get("state").and_then(Value::as_str) == Some("OPEN");
    let is_meta = issue
        .get("labels")
        .and_then(Value::as_array)
        .is_some_and(|ls| {
            ls.iter().any(|l| {
                l.get("name")
                    .and_then(Value::as_str)
                    .is_some_and(|s| s.eq_ignore_ascii_case("meta"))
            })
        });
    let comments = gh_json(cwd, &["issue", "view", &n, "--json", "comments"])?;
    let bodies: Vec<&str> = comments
        .get("comments")
        .and_then(Value::as_array)
        .map(|a| {
            a.iter()
                .filter_map(|c| c.get("body").and_then(Value::as_str))
                .collect()
        })
        .unwrap_or_default();
    let Some(marker) = last_marker(bodies) else {
        eprintln!("#{number}: labelled {ON_FEATURE_LABEL} but has no grind marker; skipping");
        return Ok(None);
    };

    let closer = if open {
        Closer::None
    } else {
        let q = format!("query={}", closer_query(number));
        let g = gh_json(
            cwd,
            &[
                "api", "graphql", "-F", "owner={owner}", "-F", "repo={repo}", "-f", &q,
            ],
        )?;
        match closer_from_graphql(&g) {
            (Closer::Commit { .. }, Some(oid)) => {
                let path = format!("repos/{{owner}}/{{repo}}/compare/{default_branch}...{oid}");
                let on_default = gh_json(cwd, &["api", &path])
                    .map(|c| commit_on_default(&c))
                    .unwrap_or(false);
                Closer::Commit { on_default }
            }
            (c, _) => c,
        }
    };

    let pr = marker.feature_pr.to_string();
    let prj = gh_json(
        cwd,
        &[
            "pr",
            "view",
            &pr,
            "--json",
            "state,mergedAt,baseRefName,updatedAt,mergeable,headRefName",
        ],
    )?;
    let head = prj
        .get("headRefName")
        .and_then(Value::as_str)
        .unwrap_or(marker.branch.as_str())
        .to_string();
    let branch_path = format!("repos/{{owner}}/{{repo}}/branches/{head}");
    let branch_exists = matches!(gh(cwd, &["api", &branch_path]), Ok((0, _)));
    let feature = feature_from_pr_json(&prj, now_secs(), branch_exists);

    Ok(Some(IssueFacts {
        number,
        open,
        closer,
        feature_pr: marker.feature_pr,
        feature,
        is_meta,
    }))
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
        Action::ReportStale => {
            println!("#{number}: STALE feature PR");
            return Ok(());
        }
    };
    println!("{line}");
    let (code, _) = gh(cwd, &args)?;
    if code != 0 {
        return Err(format!("#{number}: `gh {}` exited {code}", args.join(" ")));
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
    let mut failed = false;
    let mut stale = Vec::new();
    for issue in issues.as_array().into_iter().flatten() {
        let facts = match gather_facts(cwd, issue, &default_branch) {
            Ok(Some(f)) => f,
            Ok(None) => continue,
            Err(e) => {
                eprintln!("{e}");
                failed = true;
                continue;
            }
        };
        for action in decide(&facts, &default_branch) {
            if action == Action::ReportStale {
                stale.push((facts.number, facts.feature_pr));
            }
            if let Err(e) = apply(cwd, facts.number, &action) {
                eprintln!("{e}");
                failed = true;
            }
        }
    }
    if !stale.is_empty() {
        println!("stale feature PRs:");
        for (issue, pr) in &stale {
            println!("  #{issue} -> feature PR #{pr}");
        }
    }
    Ok(if failed { 1 } else { 0 })
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
        }
    }

    fn open_pr() -> Value {
        json!({"state":"OPEN","mergedAt":null,"baseRefName":"main",
               "updatedAt":"2026-09-20T00:00:00Z","mergeable":"MERGEABLE",
               "headRefName":"grind/x-1"})
    }

    #[test]
    fn closer_pr_merged_into_non_default_reopens() {
        let (closer, _) = closer_from_graphql(&closer_json(
            json!({"__typename":"PullRequest","merged":true,"baseRefName":"grind/x-1"}),
        ));
        let feature = feature_from_pr_json(&open_pr(), NOW, true);
        let acts = decide(&facts(false, closer, feature), "main");
        assert!(matches!(acts[0], Action::Reopen { .. }), "{acts:?}");
        assert!(!acts.contains(&Action::RemoveLabel));
    }

    #[test]
    fn closer_commit_not_on_default_reopens() {
        let (closer, oid) =
            closer_from_graphql(&closer_json(json!({"__typename":"Commit","oid":"abc"})));
        assert_eq!(oid.as_deref(), Some("abc"));
        let on_default = commit_on_default(&json!({"status":"ahead"}));
        assert!(!on_default);
        assert_eq!(closer, Closer::Commit { on_default: false });
        let feature = feature_from_pr_json(&open_pr(), NOW, true);
        let acts = decide(&facts(false, Closer::Commit { on_default }, feature), "main");
        assert!(matches!(acts[0], Action::Reopen { .. }), "{acts:?}");
    }

    #[test]
    fn closer_commit_on_default_is_landed() {
        assert!(commit_on_default(&json!({"status":"behind"})));
        assert!(commit_on_default(&json!({"status":"identical"})));
        let feature = feature_from_pr_json(&open_pr(), NOW, true);
        let acts = decide(
            &facts(false, Closer::Commit { on_default: true }, feature),
            "main",
        );
        assert!(acts.is_empty(), "{acts:?}");
    }

    #[test]
    fn closer_pr_merged_into_default_does_not_reopen() {
        let (closer, _) = closer_from_graphql(&closer_json(
            json!({"__typename":"PullRequest","merged":true,"baseRefName":"main"}),
        ));
        let feature = feature_from_pr_json(&open_pr(), NOW, true);
        let acts = decide(&facts(false, closer, feature), "main");
        assert!(
            !acts.iter().any(|a| matches!(a, Action::Reopen { .. })),
            "{acts:?}"
        );
    }

    #[test]
    fn feature_closed_unmerged_reopens_unlabels_and_cites_ref() {
        let pr = json!({"state":"CLOSED","mergedAt":null,"baseRefName":"main",
                        "updatedAt":"2026-09-20T00:00:00Z","mergeable":"UNKNOWN",
                        "headRefName":"grind/x-1"});
        let feature = feature_from_pr_json(&pr, NOW, true);
        assert_eq!(feature, FeaturePrState::ClosedUnmerged);
        let acts = decide(&facts(false, Closer::None, feature), "main");
        assert!(matches!(acts[0], Action::Reopen { .. }));
        assert!(acts.contains(&Action::RemoveLabel));
        assert!(acts.iter().any(|a| matches!(
            a,
            Action::Comment { body } if body.contains("refs/pull/812/head")
        )));
    }

    #[test]
    fn feature_merged_to_default_closes_open_issue_and_unlabels() {
        let pr = json!({"state":"MERGED","mergedAt":"2026-09-20T00:00:00Z",
                        "baseRefName":"main","updatedAt":"2026-09-20T00:00:00Z",
                        "mergeable":"UNKNOWN","headRefName":"grind/x-1"});
        let feature = feature_from_pr_json(&pr, NOW, false);
        let acts = decide(&facts(true, Closer::None, feature), "main");
        assert!(acts.contains(&Action::RemoveLabel));
        assert!(acts.iter().any(|a| matches!(a, Action::Close { .. })));
    }

    #[test]
    fn feature_merged_to_non_default_keeps_label_and_reopens() {
        let feature = FeaturePrState::Merged {
            base_ref: "develop".into(),
        };
        let acts = decide(&facts(false, Closer::None, feature), "main");
        assert!(matches!(acts[0], Action::Reopen { .. }));
        assert!(!acts.contains(&Action::RemoveLabel));
    }

    #[test]
    fn open_stale_reports_stale() {
        let mut pr = open_pr();
        pr["updatedAt"] = json!("2026-08-01T00:00:00Z");
        let feature = feature_from_pr_json(&pr, NOW, true);
        assert_eq!(feature, FeaturePrState::Open { stale: true });
        let acts = decide(&facts(true, Closer::None, feature.clone()), "main");
        assert_eq!(acts, vec![Action::ReportStale]);

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

        let mut meta = facts(true, Closer::None, feature);
        meta.is_meta = true;
        let acts = decide(&meta, "main");
        assert_eq!(acts.len(), 2);
    }

    #[test]
    fn healthy_open_issue_is_a_no_op() {
        let feature = feature_from_pr_json(&open_pr(), NOW, true);
        assert_eq!(feature, FeaturePrState::Open { stale: false });
        assert!(decide(&facts(true, Closer::None, feature), "main").is_empty());
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
    }

    #[test]
    fn iso8601_parses() {
        assert_eq!(parse_iso8601_utc("1970-01-02T00:00:01Z"), Some(86_401));
    }
}
