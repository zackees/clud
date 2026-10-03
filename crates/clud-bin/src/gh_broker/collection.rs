//! Incremental rewrites and merged collections (#1743, phase 2).
//!
//! A collection read (issue or PR comments, review comments, a workflow-run
//! list) is keyed by its path and the caller's filters, not by its exact URL.
//! The broker keeps the collection's members and brings them up to date with
//! the narrowest upstream query (`since=` or `created=>=`, bounded by the
//! newest timestamp it has *seen*, minus a 5 s overlap), then rebuilds the
//! response the caller's own query would have returned.
//!
//! Everything here is pure: parsing the caller's endpoint into a [`Plan`],
//! the upstream URLs, parsing pages into [`Member`]s with their exact bytes,
//! the upsert, and rendering. A shape this module cannot reproduce exactly
//! returns `None`, and the broker answers the read the phase-1 way (an ETag
//! on the exact URL).

use serde::{Deserialize, Serialize};
use serde_json::value::RawValue;

/// Overlap below the high-water mark, for clock skew between GitHub's
/// writers. Duplicates it returns are removed by id.
pub const OVERLAP_SECS: i64 = 5;
/// Page size of every upstream query the broker makes.
pub const UPSTREAM_PER_PAGE: usize = 100;
/// GitHub's page size when the caller sets none.
pub const DEFAULT_PER_PAGE: usize = 30;
/// A comment collection with more pages than this is not merged.
pub const MAX_SEED_PAGES: usize = 10;
/// An incremental query needing more pages than this reseeds instead.
pub const MAX_DELTA_PAGES: usize = 5;
/// Newest runs kept per run-list key: the most a caller's page can show.
pub const MAX_RUN_MEMBERS: usize = 100;
/// Check runs must have been complete this long before a key freezes:
/// another app or a later workflow can still add a check run to the commit.
pub const CHECKS_SETTLE_SECS: i64 = 300;

/// Run-list filters that name immutable properties of a run, so a merged
/// membership stays exact. `status` and `created` are not among them: a
/// run's status changes, and `created` is the broker's own rewrite.
const RUN_FILTERS: &[&str] = &[
    "actor",
    "branch",
    "event",
    "head_sha",
    "check_suite_id",
    "exclude_pull_requests",
];

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Kind {
    /// `repos/{o}/{r}/issues/{n}/comments`: an array, id ascending.
    IssueComments,
    /// `repos/{o}/{r}/pulls/{n}/comments`: an array, id ascending.
    ReviewComments,
    /// `repos/{o}/{r}/actions/runs`: `{"total_count","workflow_runs"}`,
    /// newest `created_at` first.
    Runs,
}

impl Kind {
    fn is_comments(self) -> bool {
        matches!(self, Kind::IssueComments | Kind::ReviewComments)
    }
}

/// A collection read the broker can answer by merging.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Plan {
    pub kind: Kind,
    /// The path without a leading `/` or a query.
    pub path: String,
    /// The caller's filters that select the membership, sorted, values as
    /// written (still percent-encoded).
    pub filters: Vec<(String, String)>,
    /// The caller's page size.
    pub per_page: usize,
}

/// Split an endpoint into its path and query pairs. `None` for a fragment,
/// an empty key or a repeated key.
pub(crate) fn split_query(endpoint: &str) -> Option<(&str, Vec<(&str, &str)>)> {
    let endpoint = endpoint.trim_start_matches('/');
    if endpoint.contains('#') {
        return None;
    }
    let (path, query) = endpoint.split_once('?').unwrap_or((endpoint, ""));
    let mut params: Vec<(&str, &str)> = Vec::new();
    for pair in query.split('&').filter(|pair| !pair.is_empty()) {
        let (key, value) = pair.split_once('=').unwrap_or((pair, ""));
        if key.is_empty() || params.iter().any(|(seen, _)| *seen == key) {
            return None;
        }
        params.push((key, value));
    }
    Some((path, params))
}

pub(crate) fn is_number(word: &str) -> bool {
    !word.is_empty() && word.len() <= 20 && word.bytes().all(|b| b.is_ascii_digit())
}

fn per_page(value: &str) -> Option<usize> {
    if !is_number(value) {
        return None;
    }
    let n: usize = value.parse().ok()?;
    (1..=UPSTREAM_PER_PAGE).contains(&n).then_some(n)
}

/// The merge plan for `endpoint`, or `None` when the caller's query asks for
/// something a merged membership cannot reproduce exactly (a later page, a
/// sort order, a caller `since`, a status filter, an unknown parameter).
pub fn plan(endpoint: &str) -> Option<Plan> {
    let (path, params) = split_query(endpoint)?;
    let segments: Vec<&str> = path.split('/').collect();
    if segments.iter().any(|s| s.is_empty()) {
        return None;
    }
    let kind = match segments.as_slice() {
        ["repos", _, _, "issues", n, "comments"] if is_number(n) => Kind::IssueComments,
        ["repos", _, _, "pulls", n, "comments"] if is_number(n) => Kind::ReviewComments,
        ["repos", _, _, "actions", "runs"] => Kind::Runs,
        _ => return None,
    };
    let mut page_size = DEFAULT_PER_PAGE;
    let mut filters = Vec::new();
    for (key, value) in params {
        match key {
            "per_page" => page_size = per_page(value)?,
            "page" if value == "1" => {}
            _ if kind == Kind::Runs && RUN_FILTERS.contains(&key) && !value.is_empty() => {
                filters.push((key.to_string(), value.to_string()));
            }
            _ => return None,
        }
    }
    filters.sort();
    Some(Plan {
        kind,
        path: path.to_string(),
        filters,
        per_page: page_size,
    })
}

/// `2026-10-02T23:18:20Z` -> `2026-10-02T23%3A18%3A20Z`.
fn encode_ts(ts: &str) -> String {
    ts.replace(':', "%3A")
}

impl Plan {
    /// Path plus filters: the membership's identity, independent of the
    /// caller's page size.
    pub fn label(&self) -> String {
        let mut label = self.path.clone();
        if !self.filters.is_empty() {
            label.push('?');
            label.push_str(&self.filter_query());
        }
        label
    }

    fn filter_query(&self) -> String {
        self.filters
            .iter()
            .map(|(k, v)| format!("{k}={v}"))
            .collect::<Vec<_>>()
            .join("&")
    }

    fn url(&self, extra: Option<String>, page: usize) -> String {
        let mut query: Vec<String> = Vec::new();
        if !self.filters.is_empty() {
            query.push(self.filter_query());
        }
        query.extend(extra);
        query.push(format!("per_page={UPSTREAM_PER_PAGE}"));
        if page > 1 {
            query.push(format!("page={page}"));
        }
        format!("{}?{}", self.path, query.join("&"))
    }

    /// The full collection, page `page` (comments page through all of it;
    /// a run list keeps only its newest page).
    pub fn seed_url(&self, page: usize) -> String {
        self.url(None, page)
    }

    /// Everything at or after `bound` (`YYYY-MM-DDTHH:MM:SSZ`).
    pub fn delta_url(&self, bound: &str, page: usize) -> String {
        let bound = encode_ts(bound);
        let extra = if self.kind.is_comments() {
            format!("since={bound}")
        } else {
            format!("created=%3E%3D{bound}")
        };
        self.url(Some(extra), page)
    }

    /// Whether a seed pages past page 1. A run list keeps its newest page.
    pub fn seeds_all_pages(&self) -> bool {
        self.kind.is_comments()
    }
}

/// One collection object: the fields the merge needs, and its exact bytes.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Member {
    pub id: u64,
    pub created_at: i64,
    pub updated_at: i64,
    /// Workflow runs only.
    pub status: Option<String>,
    pub raw: String,
}

/// One parsed upstream page.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Page {
    pub members: Vec<Member>,
    pub total_count: Option<u64>,
}

#[derive(Deserialize)]
struct Probe {
    id: u64,
    created_at: String,
    updated_at: String,
    #[serde(default)]
    status: Option<String>,
}

#[derive(Deserialize)]
struct RunsBody {
    total_count: u64,
    workflow_runs: Vec<Box<RawValue>>,
}

fn member(raw: &RawValue, kind: Kind) -> Option<Member> {
    let probe: Probe = serde_json::from_str(raw.get()).ok()?;
    if kind == Kind::Runs && probe.status.is_none() {
        return None;
    }
    Some(Member {
        id: probe.id,
        created_at: parse_ts(&probe.created_at)?,
        updated_at: parse_ts(&probe.updated_at)?,
        status: if kind == Kind::Runs {
            probe.status
        } else {
            None
        },
        raw: raw.get().to_string(),
    })
}

/// Parse one upstream page. `None` unless the page re-renders to exactly
/// the bytes GitHub sent: the merge only ever reorders whole objects, so a
/// body it cannot rebuild byte for byte (whitespace, an extra key, an
/// unexpected shape) is never merged.
pub fn parse_page(kind: Kind, body: &[u8]) -> Option<Page> {
    let page = if kind.is_comments() {
        let raws: Vec<Box<RawValue>> = serde_json::from_slice(body).ok()?;
        Page {
            members: raws
                .iter()
                .map(|raw| member(raw, kind))
                .collect::<Option<_>>()?,
            total_count: None,
        }
    } else {
        let runs: RunsBody = serde_json::from_slice(body).ok()?;
        Page {
            members: runs
                .workflow_runs
                .iter()
                .map(|raw| member(raw, kind))
                .collect::<Option<_>>()?,
            total_count: Some(runs.total_count),
        }
    };
    let rebuilt = render(kind, &page.members, page.total_count, usize::MAX);
    (rebuilt == body).then_some(page)
}

/// Whether a full-fetch page is in the order [`sort`] produces. The merge
/// re-sorts every answer into that order, so a seed page GitHub sent in
/// another order means a merged answer would differ from a full fetch.
/// Incremental pages are not checked: GitHub orders a `created`-filtered run
/// list differently (same-second runs by workflow), and the merge re-sorts.
pub fn in_natural_order(kind: Kind, members: &[Member]) -> bool {
    let mut sorted = members.to_vec();
    sort(kind, &mut sorted);
    sorted == members
}

/// The endpoint's natural order: comments by id ascending; runs newest
/// `created_at` first, then id descending.
pub fn sort(kind: Kind, members: &mut [Member]) {
    if kind.is_comments() {
        members.sort_by_key(|m| m.id);
    } else {
        members.sort_by(|a, b| {
            b.created_at
                .cmp(&a.created_at)
                .then_with(|| b.id.cmp(&a.id))
        });
    }
}

/// Upsert `incoming` into `members` by id; the copy with the newer
/// `updated_at` wins, and on a tie the later fetch wins (its non-timestamped
/// fields, such as reaction counts, are newer). Returns how many objects
/// were added or changed, and the ids that were new. Leaves `members` in the
/// endpoint's natural order.
pub fn upsert(kind: Kind, members: &mut Vec<Member>, incoming: Vec<Member>) -> (u32, Vec<u64>) {
    let mut index: std::collections::HashMap<u64, usize> =
        members.iter().enumerate().map(|(i, m)| (m.id, i)).collect();
    let mut changed = 0u32;
    let mut added = Vec::new();
    for object in incoming {
        match index.get(&object.id) {
            Some(&i) => {
                let stored = &mut members[i];
                if object.updated_at >= stored.updated_at && object.raw != stored.raw {
                    *stored = object;
                    changed += 1;
                }
            }
            None => {
                index.insert(object.id, members.len());
                added.push(object.id);
                members.push(object);
                changed += 1;
            }
        }
    }
    sort(kind, members);
    (changed, added)
}

/// Rebuild the body GitHub returns for the first `per_page` members.
pub fn render(
    kind: Kind,
    members: &[Member],
    total_count: Option<u64>,
    per_page: usize,
) -> Vec<u8> {
    let shown = &members[..members.len().min(per_page)];
    let mut out = Vec::new();
    if !kind.is_comments() {
        out.extend_from_slice(
            format!(
                "{{\"total_count\":{},\"workflow_runs\":",
                total_count.unwrap_or(0)
            )
            .as_bytes(),
        );
    }
    out.push(b'[');
    for (i, m) in shown.iter().enumerate() {
        if i > 0 {
            out.push(b',');
        }
        out.extend_from_slice(m.raw.as_bytes());
    }
    out.push(b']');
    if !kind.is_comments() {
        out.push(b'}');
    }
    out
}

/// The lower bound of the next incremental query: the high-water mark
/// minus the overlap. Comments: the newest `updated_at` seen (an edit moves
/// it). Runs: the newest `created_at` seen, or the oldest `live` run's
/// `created_at` if that is older, so the same one query also refreshes every
/// run that has not finished. `None` for an empty membership (reseed).
pub fn delta_bound(
    kind: Kind,
    members: &[Member],
    live: impl Fn(&Member) -> bool,
) -> Option<String> {
    let mark = if kind.is_comments() {
        members.iter().map(|m| m.updated_at).max()?
    } else {
        let newest = members.iter().map(|m| m.created_at).max()?;
        members
            .iter()
            .filter(|m| live(m))
            .map(|m| m.created_at)
            .min()
            .map_or(newest, |oldest_live| oldest_live.min(newest))
    };
    Some(format_ts(mark - OVERLAP_SECS))
}

/// Whether a response says another page follows.
pub fn has_next_page(link: Option<&str>) -> bool {
    link.is_some_and(|link| link.contains("rel=\"next\""))
}

/// A collection's durable state, one redb row per key.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct CollectionState {
    pub members: Vec<Member>,
    /// Run lists: GitHub's `total_count` for the caller's filters.
    pub total_count: Option<u64>,
    /// The highest id ever seen; a run with a higher id is new.
    pub max_id: u64,
    /// Headers of the newest upstream page, minus transfer and paging ones.
    pub headers: Vec<(String, String)>,
    /// ETag of a single-page seed, for a free `304` reconciliation.
    pub seed_etag: Option<String>,
    /// The last incremental URL and its ETag: an unchanged bound repeats the
    /// URL, and a `304` costs nothing.
    pub delta_url: Option<String>,
    pub delta_etag: Option<String>,
    /// Start of the last upstream refresh (Unix ms).
    pub fetched_at_ms: u64,
    /// Start of the last full fetch (seed or reconciliation).
    pub reconciled_at_ms: u64,
    /// Set when the collection cannot be merged (too many pages, a body
    /// the merge cannot rebuild); reads use the exact-URL path until a
    /// reconciliation interval has passed.
    pub unmergeable_at_ms: Option<u64>,
}

/// Headers kept on a rebuilt response: not paging, validators or length,
/// which describe one upstream transfer rather than the merged body.
pub fn kept_headers(headers: &[(String, String)]) -> Vec<(String, String)> {
    headers
        .iter()
        .filter(|(name, _)| {
            !["link", "etag", "last-modified", "content-length"]
                .iter()
                .any(|skip| name.eq_ignore_ascii_case(skip))
        })
        .cloned()
        .collect()
}

/// A single-object listing that can freeze once everything in it is done.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Freeze {
    /// `repos/{o}/{r}/actions/runs/{id}/jobs`: frozen once every job and
    /// the run itself are completed.
    Jobs {
        owner: String,
        repo: String,
        run_id: String,
    },
    /// `repos/{o}/{r}/commits/{sha}/check-runs`: frozen once every check run
    /// has been completed for [`CHECKS_SETTLE_SECS`]. Only a full 40-hex
    /// SHA; a branch name moves.
    CheckRuns,
}

/// Whether `endpoint` is a listing that may freeze. Only the unfiltered
/// listing (page size and `filter` aside); a status or name filter can make
/// "everything completed" vacuously true.
pub fn freeze_kind(endpoint: &str) -> Option<Freeze> {
    let (path, params) = split_query(endpoint)?;
    let plain = params.iter().all(|(key, value)| match *key {
        "per_page" => per_page(value).is_some(),
        "page" => *value == "1",
        "filter" => matches!(*value, "latest" | "all"),
        _ => false,
    });
    if !plain {
        return None;
    }
    let segments: Vec<&str> = path.split('/').collect();
    match segments.as_slice() {
        ["repos", owner, repo, "actions", "runs", id, "jobs"] if is_number(id) => {
            Some(Freeze::Jobs {
                owner: owner.to_string(),
                repo: repo.to_string(),
                run_id: id.to_string(),
            })
        }
        ["repos", _, _, "commits", sha, "check-runs"]
            if sha.len() == 40 && sha.bytes().all(|b| b.is_ascii_hexdigit()) =>
        {
            Some(Freeze::CheckRuns)
        }
        _ => None,
    }
}

#[derive(Deserialize)]
struct Listing {
    total_count: u64,
    #[serde(default)]
    jobs: Option<Vec<StatusProbe>>,
    #[serde(default)]
    check_runs: Option<Vec<StatusProbe>>,
}

#[derive(Deserialize)]
struct StatusProbe {
    status: String,
    #[serde(default)]
    completed_at: Option<String>,
}

/// For a non-empty, single-page listing whose every entry is `completed`,
/// the newest `completed_at` (Unix seconds). `None` otherwise.
pub fn all_completed(body: &[u8]) -> Option<i64> {
    let listing: Listing = serde_json::from_slice(body).ok()?;
    let entries = listing.jobs.or(listing.check_runs)?;
    if entries.is_empty() || listing.total_count != entries.len() as u64 {
        return None;
    }
    let mut newest = i64::MIN;
    for entry in &entries {
        if entry.status != "completed" {
            return None;
        }
        let at = parse_ts(entry.completed_at.as_deref()?)?;
        newest = newest.max(at);
    }
    Some(newest)
}

/// Whether a single workflow run object is `completed`.
pub fn run_completed(body: &[u8]) -> bool {
    #[derive(Deserialize)]
    struct Run {
        status: String,
    }
    serde_json::from_slice::<Run>(body).is_ok_and(|run| run.status == "completed")
}

/// Parse GitHub's `YYYY-MM-DDTHH:MM:SSZ` into Unix seconds.
pub fn parse_ts(ts: &str) -> Option<i64> {
    let b = ts.as_bytes();
    if b.len() != 20
        || b[4] != b'-'
        || b[7] != b'-'
        || b[10] != b'T'
        || b[13] != b':'
        || b[16] != b':'
        || b[19] != b'Z'
    {
        return None;
    }
    let num = |range: std::ops::Range<usize>| -> Option<i64> {
        let part = ts.get(range)?;
        if !part.bytes().all(|c| c.is_ascii_digit()) {
            return None;
        }
        part.parse().ok()
    };
    let (y, mo, d) = (num(0..4)?, num(5..7)?, num(8..10)?);
    let (h, mi, s) = (num(11..13)?, num(14..16)?, num(17..19)?);
    if !(1..=12).contains(&mo) || !(1..=31).contains(&d) || h > 23 || mi > 59 || s > 60 {
        return None;
    }
    Some(days_from_civil(y, mo, d) * 86_400 + h * 3_600 + mi * 60 + s)
}

/// Unix seconds -> `YYYY-MM-DDTHH:MM:SSZ`.
pub fn format_ts(secs: i64) -> String {
    let days = secs.div_euclid(86_400);
    let rem = secs.rem_euclid(86_400);
    let (y, m, d) = civil_from_days(days);
    format!(
        "{y:04}-{m:02}-{d:02}T{:02}:{:02}:{:02}Z",
        rem / 3_600,
        rem % 3_600 / 60,
        rem % 60
    )
}

/// Days since 1970-01-01 (proleptic Gregorian; H. Hinnant's algorithm).
fn days_from_civil(y: i64, m: i64, d: i64) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let mp = (m + 9) % 12;
    let doy = (153 * mp + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

fn civil_from_days(days: i64) -> (i64, i64, i64) {
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + i64::from(m <= 2);
    (y, m, d)
}

#[cfg(test)]
mod tests;
