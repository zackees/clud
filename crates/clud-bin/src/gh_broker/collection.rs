//! Merged collections (#1743).
//!
//! A collection read (issue or PR comments, review comments, a workflow-run
//! list) is keyed by its path and the caller's filters, not by its exact URL.
//! The broker keeps the collection as the upstream pages it last received,
//! each with its ETag, and brings it up to date by re-sending every page
//! conditionally: a `304` (free) proves the page is byte-identical, a `200`
//! replaces it. The response the caller's own query would have returned is
//! rebuilt from those pages.
//!
//! Everything here is pure: parsing the caller's endpoint into a [`Plan`],
//! the upstream URLs, parsing pages into [`Member`]s with their exact bytes,
//! ordering checks, the change count, and rendering. A shape this module cannot reproduce exactly
//! returns `None`, and the broker answers the read the phase-1 way (an ETag
//! on the exact URL).

use serde::{Deserialize, Serialize};
use serde_json::value::RawValue;

/// Page size of every upstream comment query, and the widest run list.
pub const UPSTREAM_PER_PAGE: usize = 100;
/// GitHub's page size when the caller sets none.
pub const DEFAULT_PER_PAGE: usize = 30;
/// A comment collection with more pages than this is not merged.
pub const MAX_SEED_PAGES: usize = 10;
/// Upstream page sizes of a run list. A run list keeps only its newest
/// page, as wide as the widest caller needs, so a `per_page=5` reader of a
/// busy repo does not re-download 100 runs (about 1.2 MB) on every change.
pub const RUN_WIDTHS: &[usize] = &[10, 30, UPSTREAM_PER_PAGE];

/// Run-list filters that name immutable properties of a run, so a merged
/// membership stays exact. `status` and `created` are not among them: a
/// run's status changes, and a `created` window is a different membership.
/// The planner refuses both, and the read takes the exact-URL path.
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

    /// Upstream page `page` of the collection at page size `width`: the
    /// caller's filters, no bound, so the answer is the whole truth and its
    /// ETag can prove the stored copy current.
    pub fn seed_url(&self, page: usize, width: usize) -> String {
        let mut query: Vec<String> = Vec::new();
        if !self.filters.is_empty() {
            query.push(self.filter_query());
        }
        query.push(format!("per_page={width}"));
        if page > 1 {
            query.push(format!("page={page}"));
        }
        format!("{}?{}", self.path, query.join("&"))
    }

    /// The upstream page size this caller needs: comments always page
    /// through at 100; a run list keeps one page, the narrowest of
    /// [`RUN_WIDTHS`] that covers the caller's `per_page`.
    pub fn width(&self) -> usize {
        if self.kind.is_comments() {
            return UPSTREAM_PER_PAGE;
        }
        RUN_WIDTHS
            .iter()
            .copied()
            .find(|width| *width >= self.per_page)
            .unwrap_or(UPSTREAM_PER_PAGE)
    }

    /// Upstream pages a collection may span: comments up to
    /// [`MAX_SEED_PAGES`], a run list only its newest page.
    pub fn max_pages(&self) -> usize {
        if self.kind.is_comments() {
            MAX_SEED_PAGES
        } else {
            1
        }
    }
}

/// One collection object: the fields the merge needs, and its exact bytes.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Member {
    pub id: u64,
    pub created_at: i64,
    pub raw: String,
}

/// One parsed upstream page.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Page {
    pub members: Vec<Member>,
    pub total_count: Option<u64>,
}

/// The fields every member must carry. `updated_at` is required only so a
/// shape the merge was not built for is refused.
#[derive(Deserialize)]
struct Probe {
    id: u64,
    created_at: String,
    updated_at: String,
}

#[derive(Deserialize)]
struct RunsBody {
    total_count: u64,
    workflow_runs: Vec<Box<RawValue>>,
}

fn member(raw: &RawValue) -> Option<Member> {
    let probe: Probe = serde_json::from_str(raw.get()).ok()?;
    parse_ts(&probe.updated_at)?;
    Some(Member {
        id: probe.id,
        created_at: parse_ts(&probe.created_at)?,
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
            members: raws.iter().map(|raw| member(raw)).collect::<Option<_>>()?,
            total_count: None,
        }
    } else {
        let runs: RunsBody = serde_json::from_slice(body).ok()?;
        Page {
            members: runs
                .workflow_runs
                .iter()
                .map(|raw| member(raw))
                .collect::<Option<_>>()?,
            total_count: Some(runs.total_count),
        }
    };
    let rebuilt = render(kind, &page.members, page.total_count, usize::MAX);
    (rebuilt == body).then_some(page)
}

/// Whether `members` are in the endpoint's natural order ([`sort`]) with
/// no id twice. A page, or a membership joined from pages, that breaks it
/// is not served: GitHub sent another order than the merge assumes, or the
/// pages were fetched across a change and do not fit together.
pub fn in_natural_order(kind: Kind, members: &[Member]) -> bool {
    let mut sorted = members.to_vec();
    sort(kind, &mut sorted);
    let mut ids: Vec<u64> = members.iter().map(|m| m.id).collect();
    ids.sort_unstable();
    ids.dedup();
    sorted == members && ids.len() == members.len()
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

/// Whether a response says another page follows.
pub fn has_next_page(link: Option<&str>) -> bool {
    link.is_some_and(|link| link.contains("rel=\"next\""))
}

/// One upstream page as last received: its ETag, members and headers.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct StoredPage {
    /// Sent as `If-None-Match` on the next refresh; a `304` keeps the page.
    pub etag: Option<String>,
    pub members: Vec<Member>,
    /// Run lists: GitHub's `total_count` for the caller's filters.
    pub total_count: Option<u64>,
    /// Response headers, minus transfer and paging ones.
    pub headers: Vec<(String, String)>,
}

/// A collection's durable state, one redb row per key.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct CollectionState {
    /// Upstream pages in order. Their members, joined, are the membership
    /// in the endpoint's natural order.
    pub pages: Vec<StoredPage>,
    /// The upstream page size the pages were fetched at.
    pub width: usize,
    /// Start of the last upstream refresh (Unix ms).
    pub fetched_at_ms: u64,
    /// Set when the collection cannot be merged (too many pages, a body
    /// the merge cannot rebuild); reads use the exact-URL path until the
    /// retry interval has passed.
    pub unmergeable_at_ms: Option<u64>,
}

impl CollectionState {
    /// The membership: every page's members, in order.
    pub fn members(&self) -> Vec<Member> {
        self.pages
            .iter()
            .flat_map(|page| page.members.iter().cloned())
            .collect()
    }
}

/// Objects added or changed (new id, or other bytes) and objects removed,
/// from `before` to `after`.
pub fn diff(before: &[Member], after: &[Member]) -> (u32, u32) {
    let old: std::collections::HashMap<u64, &str> =
        before.iter().map(|m| (m.id, m.raw.as_str())).collect();
    let new: std::collections::HashSet<u64> = after.iter().map(|m| m.id).collect();
    let changed = after
        .iter()
        .filter(|m| old.get(&m.id) != Some(&m.raw.as_str()))
        .count();
    let removed = before.iter().filter(|m| !new.contains(&m.id)).count();
    (
        u32::try_from(changed).unwrap_or(u32::MAX),
        u32::try_from(removed).unwrap_or(u32::MAX),
    )
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
