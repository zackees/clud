//! Phase 2 (#1743): merged collections, frozen listings and targeted
//! invalidation, against a fake GitHub that answers each query (`since`,
//! `created`, `branch`, `per_page`, `page`, `If-None-Match`) the way the
//! REST API does. Every merged answer is compared with the fake's own full
//! answer to the caller's exact URL.

use super::merged::RECONCILE_MS;
use super::*;
use crate::gh_broker::collection::{format_ts, parse_ts, split_query};
use std::sync::atomic::{AtomicU64, Ordering};

#[derive(Clone)]
struct Comment {
    id: u64,
    created: i64,
    updated: i64,
    body: String,
}

#[derive(Clone)]
struct Run {
    id: u64,
    created: i64,
    updated: i64,
    status: &'static str,
    branch: &'static str,
}

#[derive(Clone)]
struct Step {
    status: &'static str,
    completed_at: Option<i64>,
}

#[derive(Default)]
struct Server {
    comments: Vec<Comment>,
    runs: Vec<Run>,
    jobs: Vec<Step>,
    checks: Vec<Step>,
    /// Status of the single run object `repos/o/r/actions/runs/7`.
    run7: &'static str,
    /// Bodies served verbatim for an exact endpoint.
    raw: HashMap<String, Vec<u8>>,
    /// Every upstream request: endpoint and `If-None-Match`.
    log: Vec<(String, Option<String>)>,
}

fn decode(value: &str) -> String {
    value
        .replace("%3A", ":")
        .replace("%3E", ">")
        .replace("%3D", "=")
}

fn page_of<T: Clone>(items: &[T], params: &HashMap<&str, &str>) -> (Vec<T>, bool) {
    let per_page: usize = params.get("per_page").map_or(30, |v| v.parse().unwrap());
    let page: usize = params.get("page").map_or(1, |v| v.parse().unwrap());
    let start = (page - 1) * per_page;
    let end = (start + per_page).min(items.len());
    let shown = items.get(start..end).unwrap_or_default().to_vec();
    (shown, end < items.len())
}

fn steps_json(steps: &[Step]) -> String {
    steps
        .iter()
        .enumerate()
        .map(|(i, s)| {
            let at = s
                .completed_at
                .map_or("null".to_string(), |at| format!("\"{}\"", format_ts(at)));
            format!(
                r#"{{"id":{i},"status":"{}","completed_at":{at}}}"#,
                s.status
            )
        })
        .collect::<Vec<_>>()
        .join(",")
}

impl Server {
    /// The REST answer to `endpoint`: status, body, `Link` next.
    fn respond(&self, endpoint: &str) -> (u16, Vec<u8>, bool) {
        if let Some(body) = self.raw.get(endpoint) {
            return (200, body.clone(), false);
        }
        let (path, pairs) = split_query(endpoint).unwrap();
        let params: HashMap<&str, &str> = pairs.into_iter().collect();
        let sha = "0123456789abcdef0123456789abcdef01234567";
        match path {
            "repos/o/r/issues/5/comments" => {
                let since = params.get("since").map(|v| parse_ts(&decode(v)).unwrap());
                let mut items: Vec<&Comment> = self
                    .comments
                    .iter()
                    .filter(|c| since.is_none_or(|since| c.updated >= since))
                    .collect();
                items.sort_by_key(|c| c.id);
                let (shown, more) = page_of(&items, &params);
                let body = shown
                    .iter()
                    .map(|c| {
                        format!(
                            r#"{{"id":{},"node_id":"IC_{}","body":"{}","created_at":"{}","updated_at":"{}"}}"#,
                            c.id,
                            c.id,
                            c.body,
                            format_ts(c.created),
                            format_ts(c.updated)
                        )
                    })
                    .collect::<Vec<_>>()
                    .join(",");
                (200, format!("[{body}]").into_bytes(), more)
            }
            "repos/o/r/actions/runs" => {
                let created = params
                    .get("created")
                    .map(|v| parse_ts(decode(v).strip_prefix(">=").unwrap()).unwrap());
                let mut items: Vec<&Run> = self
                    .runs
                    .iter()
                    .filter(|r| params.get("branch").is_none_or(|b| r.branch == *b))
                    .filter(|r| created.is_none_or(|at| r.created >= at))
                    .collect();
                items.sort_by(|a, b| b.created.cmp(&a.created).then(b.id.cmp(&a.id)));
                let (shown, more) = page_of(&items, &params);
                let body = shown
                    .iter()
                    .map(|r| {
                        format!(
                            r#"{{"id":{},"head_branch":"{}","status":"{}","created_at":"{}","updated_at":"{}"}}"#,
                            r.id,
                            r.branch,
                            r.status,
                            format_ts(r.created),
                            format_ts(r.updated)
                        )
                    })
                    .collect::<Vec<_>>()
                    .join(",");
                let body = format!(
                    r#"{{"total_count":{},"workflow_runs":[{body}]}}"#,
                    items.len()
                );
                (200, body.into_bytes(), more)
            }
            "repos/o/r/actions/runs/7/jobs" => {
                let body = format!(
                    r#"{{"total_count":{},"jobs":[{}]}}"#,
                    self.jobs.len(),
                    steps_json(&self.jobs)
                );
                (200, body.into_bytes(), false)
            }
            "repos/o/r/actions/runs/7" => (
                200,
                format!(r#"{{"id":7,"status":"{}"}}"#, self.run7).into_bytes(),
                false,
            ),
            _ if path == format!("repos/o/r/commits/{sha}/check-runs") => {
                let body = format!(
                    r#"{{"total_count":{},"check_runs":[{}]}}"#,
                    self.checks.len(),
                    steps_json(&self.checks)
                );
                (200, body.into_bytes(), false)
            }
            _ => (404, b"{\"message\":\"Not Found\"}".to_vec(), false),
        }
    }
}

struct FakeGitHub(Arc<Mutex<Server>>);

impl Upstream for FakeGitHub {
    fn fetch(&self, request: &UpstreamRequest<'_>) -> Result<Response, String> {
        let mut server = self.0.lock().unwrap();
        server.log.push((
            request.endpoint.to_string(),
            request.if_none_match.map(str::to_string),
        ));
        let (status, body, more) = server.respond(request.endpoint);
        let digest = Sha256::digest(&body);
        let etag = format!(
            "\"{:02x}{:02x}{:02x}{:02x}\"",
            digest[0], digest[1], digest[2], digest[3]
        );
        let mut headers = vec![
            (
                "Content-Type".to_string(),
                "application/json; charset=utf-8".to_string(),
            ),
            ("Etag".to_string(), etag.clone()),
            ("X-Ratelimit-Remaining".to_string(), "4000".to_string()),
        ];
        if more {
            headers.push((
                "Link".to_string(),
                "<https://api.github.com/x?page=2>; rel=\"next\"".to_string(),
            ));
        }
        if status == 200 && request.if_none_match == Some(etag.as_str()) {
            return Ok(Response {
                status: 304,
                headers,
                body: Arc::new(Vec::new()),
            });
        }
        Ok(Response {
            status,
            headers,
            body: Arc::new(body),
        })
    }
}

struct World {
    _dir: tempfile::TempDir,
    broker: GhBroker,
    server: Arc<Mutex<Server>>,
    now: Arc<AtomicU64>,
}

fn ts(text: &str) -> i64 {
    parse_ts(text).unwrap()
}

fn world(server: Server) -> World {
    let dir = tempfile::tempdir().unwrap();
    let server = Arc::new(Mutex::new(server));
    let now = Arc::new(AtomicU64::new(
        u64::try_from(ts("2026-10-02T12:00:00Z")).unwrap() * 1000,
    ));
    let clock_now = Arc::clone(&now);
    let broker = GhBroker::new(
        dir.path().join(crate::gh_broker::STORE_FILE),
        Box::new(FakeGitHub(Arc::clone(&server))),
        Box::new(move || clock_now.load(Ordering::SeqCst)),
    );
    World {
        _dir: dir,
        broker,
        server,
        now,
    }
}

impl World {
    fn read(&self, endpoint: &str) -> Vec<u8> {
        let read = BrokerRead {
            gh: Path::new("/usr/bin/gh"),
            endpoint,
            hostname: None,
            env: &[],
            session_id: None,
            fresh: false,
        };
        self.broker.read(&read).unwrap().body.to_vec()
    }

    /// What GitHub itself returns for `endpoint` right now.
    fn full(&self, endpoint: &str) -> Vec<u8> {
        self.server.lock().unwrap().respond(endpoint).1
    }

    fn log(&self) -> Vec<(String, Option<String>)> {
        self.server.lock().unwrap().log.clone()
    }

    fn requests(&self) -> Vec<String> {
        self.log()
            .into_iter()
            .map(|(endpoint, _)| endpoint)
            .collect()
    }

    fn advance(&self, ms: u64) {
        self.now.fetch_add(ms, Ordering::SeqCst);
    }

    fn ledger(&self) -> Vec<(String, u32, Option<u32>)> {
        self.broker
            .ledger()
            .unwrap()
            .into_iter()
            .map(|e| (e.outcome, e.upstream_requests, e.changed))
            .collect()
    }

    fn server(&self) -> std::sync::MutexGuard<'_, Server> {
        self.server.lock().unwrap()
    }
}

fn comment(id: u64, created: &str) -> Comment {
    Comment {
        id,
        created: ts(created),
        updated: ts(created),
        body: format!("c{id}"),
    }
}

fn three_comments() -> Server {
    Server {
        comments: vec![
            comment(1, "2026-10-02T10:00:00Z"),
            comment(2, "2026-10-02T10:01:00Z"),
            comment(3, "2026-10-02T10:02:00Z"),
        ],
        ..Server::default()
    }
}

const COMMENTS: &str = "repos/o/r/issues/5/comments";

fn entry(outcome: &str, requests: u32, changed: Option<u32>) -> (String, u32, Option<u32>) {
    (outcome.to_string(), requests, changed)
}

#[test]
fn a_comment_reread_fetches_only_the_since_delta_and_equals_a_full_fetch() {
    let w = world(three_comments());
    assert_eq!(w.read(COMMENTS), w.full(COMMENTS));
    {
        let mut server = w.server();
        server.comments.push(comment(4, "2026-10-02T11:00:00Z"));
        server.comments[1].updated = ts("2026-10-02T11:30:00Z");
        server.comments[1].body = "edited".into();
    }
    w.advance(DEFAULT_TTL_MS);
    let merged = w.read(COMMENTS);
    assert_eq!(merged, w.full(COMMENTS));
    assert!(String::from_utf8_lossy(&merged).contains("edited"));
    // Seeded once, then bounded by the newest updated_at seen minus 5 s.
    assert_eq!(
        w.requests(),
        [
            "repos/o/r/issues/5/comments?per_page=100",
            "repos/o/r/issues/5/comments?since=2026-10-02T10%3A01%3A55Z&per_page=100",
        ]
    );
    // Within the TTL, any page size is cut from the merged membership.
    for endpoint in [
        "repos/o/r/issues/5/comments?per_page=2",
        "/repos/o/r/issues/5/comments?page=1&per_page=100",
    ] {
        assert_eq!(w.read(endpoint), w.full(endpoint.trim_start_matches('/')));
    }
    assert_eq!(w.requests().len(), 2);
    assert_eq!(
        w.ledger(),
        [
            entry("full", 1, None),
            entry("incremental", 1, Some(2)),
            entry("cache", 0, None),
            entry("cache", 0, None),
        ]
    );
}

#[test]
fn an_unchanged_delta_repeats_its_url_and_costs_a_free_304() {
    let w = world(three_comments());
    w.read(COMMENTS);
    w.advance(DEFAULT_TTL_MS);
    w.read(COMMENTS);
    w.advance(DEFAULT_TTL_MS);
    assert_eq!(w.read(COMMENTS), w.full(COMMENTS));
    let log = w.log();
    assert_eq!(log[1].0, log[2].0);
    assert!(log[2].1.is_some(), "the repeat carries If-None-Match");
    assert_eq!(
        w.ledger(),
        [
            entry("full", 1, None),
            entry("incremental", 1, Some(0)),
            entry("304", 1, Some(0)),
        ]
    );
}

#[test]
fn a_multi_page_collection_is_seeded_in_full_and_pages_are_emulated() {
    let mut server = Server::default();
    for id in 1..=150 {
        server.comments.push(comment(
            id,
            &format_ts(ts("2026-10-01T00:00:00Z") + id as i64 * 60),
        ));
    }
    let w = world(server);
    for endpoint in [
        "repos/o/r/issues/5/comments?per_page=100",
        COMMENTS,
        "repos/o/r/issues/5/comments?per_page=7&page=1",
    ] {
        assert_eq!(w.read(endpoint), w.full(endpoint), "{endpoint}");
    }
    assert_eq!(
        w.requests(),
        [
            "repos/o/r/issues/5/comments?per_page=100",
            "repos/o/r/issues/5/comments?per_page=100&page=2",
        ]
    );
}

#[test]
fn a_query_the_merge_cannot_reproduce_reads_the_exact_url() {
    let mut server = Server::default();
    for id in 1..=40 {
        server.comments.push(comment(
            id,
            &format_ts(ts("2026-10-01T00:00:00Z") + id as i64),
        ));
    }
    let w = world(server);
    let endpoint = "repos/o/r/issues/5/comments?page=2";
    assert_eq!(w.read(endpoint), w.full(endpoint));
    assert_eq!(w.requests(), [endpoint]);
    assert_eq!(w.ledger(), [entry("full", 1, None)]);
}

fn run(id: u64, created: &str, status: &'static str, branch: &'static str) -> Run {
    Run {
        id,
        created: ts(created),
        updated: ts(created),
        status,
        branch,
    }
}

#[test]
fn a_run_list_merges_new_and_finishing_runs_under_the_callers_filters() {
    let w = world(Server {
        runs: vec![
            run(100, "2026-10-02T09:00:00Z", "completed", "main"),
            run(101, "2026-10-02T10:00:00Z", "in_progress", "main"),
            run(102, "2026-10-02T10:30:00Z", "completed", "main"),
            run(103, "2026-10-02T10:40:00Z", "completed", "dev"),
        ],
        ..Server::default()
    });
    let endpoint = "repos/o/r/actions/runs?per_page=3&branch=main";
    assert_eq!(w.read(endpoint), w.full(endpoint));
    {
        let mut server = w.server();
        server.runs[1].status = "completed";
        server.runs[1].updated = ts("2026-10-02T11:00:00Z");
        server
            .runs
            .push(run(104, "2026-10-02T11:10:00Z", "queued", "main"));
    }
    w.advance(RUNS_TTL_MS);
    let merged = w.read(endpoint);
    assert_eq!(merged, w.full(endpoint));
    assert!(String::from_utf8_lossy(&merged).starts_with("{\"total_count\":4,"));
    // The bound reaches back to the oldest run that had not finished, so one
    // query refreshes it and finds the new run.
    assert_eq!(
        w.requests(),
        [
            "repos/o/r/actions/runs?branch=main&per_page=100",
            "repos/o/r/actions/runs?branch=main&created=%3E%3D2026-10-02T09%3A59%3A55Z&per_page=100",
        ]
    );
    assert_eq!(
        w.ledger(),
        [entry("full", 1, None), entry("incremental", 1, Some(2))]
    );
    // Once nothing is live the bound is the newest run seen.
    w.server().runs[4].status = "completed";
    w.advance(RUNS_TTL_MS);
    w.read(endpoint);
    w.advance(RUNS_TTL_MS);
    assert_eq!(w.read(endpoint), w.full(endpoint));
    assert_eq!(
        w.requests()[3],
        "repos/o/r/actions/runs?branch=main&created=%3E%3D2026-10-02T11%3A09%3A55Z&per_page=100"
    );
}

#[test]
fn a_body_the_merge_cannot_rebuild_falls_back_to_the_exact_url() {
    let pretty = b"[\n  {\"id\": 1}\n]".to_vec();
    let mut server = Server::default();
    server.raw.insert(
        "repos/o/r/issues/5/comments?per_page=100".into(),
        pretty.clone(),
    );
    server.raw.insert(COMMENTS.into(), pretty.clone());
    let w = world(server);
    assert_eq!(w.read(COMMENTS), pretty);
    assert_eq!(
        w.requests(),
        ["repos/o/r/issues/5/comments?per_page=100", COMMENTS]
    );
    // The collection is marked unmergeable: later reads go straight to the
    // phase-1 path, which is within its TTL here.
    assert_eq!(w.read(COMMENTS), pretty);
    assert_eq!(w.requests().len(), 2);
    assert_eq!(
        w.ledger(),
        [entry("full", 2, None), entry("cache", 0, None)]
    );
}

#[test]
fn deletions_drop_out_at_reconciliation_and_after_a_write_that_names_the_issue() {
    let w = world(three_comments());
    w.read(COMMENTS);
    w.server().comments.remove(1);
    w.advance(DEFAULT_TTL_MS);
    // `since=` cannot see a deletion.
    assert_ne!(w.read(COMMENTS), w.full(COMMENTS));
    w.advance(RECONCILE_MS);
    assert_eq!(w.read(COMMENTS), w.full(COMMENTS));
    // The next reconciliation of an unchanged collection is a free 304.
    w.advance(RECONCILE_MS);
    assert_eq!(w.read(COMMENTS), w.full(COMMENTS));
    let log = w.log();
    assert_eq!(log[2].0, "repos/o/r/issues/5/comments?per_page=100");
    assert_eq!(log[3].0, log[2].0);
    assert!(log[3].1.is_some());
    // A write naming the issue reseeds inside the TTL; one naming another
    // issue does not.
    w.server().comments.remove(0);
    w.broker
        .invalidate_tags(&["o/r#num:6".to_string()])
        .unwrap();
    w.advance(1);
    assert_ne!(w.read(COMMENTS), w.full(COMMENTS));
    w.broker.invalidate_tags(&["*#num:5".to_string()]).unwrap();
    w.advance(1);
    assert_eq!(w.read(COMMENTS), w.full(COMMENTS));
    assert_eq!(
        w.ledger()
            .into_iter()
            .map(|(outcome, _, _)| outcome)
            .collect::<Vec<_>>(),
        ["full", "incremental", "full", "304", "cache", "full"]
    );
}

#[test]
fn a_deleted_object_a_delta_added_is_dropped_at_reconciliation() {
    let w = world(three_comments());
    w.read(COMMENTS);
    w.server().comments.push(comment(4, "2026-10-02T11:00:00Z"));
    w.advance(DEFAULT_TTL_MS);
    assert_eq!(w.read(COMMENTS), w.full(COMMENTS));
    // Deleting it makes the collection byte-identical to the seed: the
    // seed's ETag must not be sent, or GitHub would answer 304.
    w.server().comments.pop();
    w.advance(RECONCILE_MS);
    assert_eq!(w.read(COMMENTS), w.full(COMMENTS));
    let log = w.log();
    assert_eq!(log[2].0, "repos/o/r/issues/5/comments?per_page=100");
    assert_eq!(log[2].1, None);
}

const JOBS: &str = "repos/o/r/actions/runs/7/jobs";

fn done(at: &str) -> Step {
    Step {
        status: "completed",
        completed_at: Some(ts(at)),
    }
}

#[test]
fn finished_jobs_freeze_until_a_write_names_their_run() {
    let w = world(Server {
        jobs: vec![done("2026-10-02T11:00:00Z"), done("2026-10-02T11:05:00Z")],
        run7: "completed",
        ..Server::default()
    });
    assert_eq!(w.read(JOBS), w.full(JOBS));
    // The run itself was checked once, through the broker.
    assert_eq!(w.requests(), [JOBS, "repos/o/r/actions/runs/7"]);
    w.advance(100 * RUNS_TTL_MS);
    w.read(JOBS);
    w.broker.invalidate_tags(&["run:8".to_string()]).unwrap();
    w.broker
        .invalidate_tags(&["*#num:7".to_string(), scope::OTHER.to_string()])
        .unwrap();
    w.advance(1);
    w.read(JOBS);
    assert_eq!(w.requests().len(), 2, "frozen: no upstream request");
    // `gh run rerun 7` names the run: revalidate (a free 304 here).
    w.broker.invalidate_tags(&["run:7".to_string()]).unwrap();
    w.advance(1);
    assert_eq!(w.read(JOBS), w.full(JOBS));
    assert_eq!(w.requests().len(), 4);
    assert!(w.log()[2].1.is_some());
    // A write the shim could not classify thaws it too.
    w.broker.invalidate().unwrap();
    w.advance(1);
    w.read(JOBS);
    assert_eq!(w.requests().len(), 6);
}

#[test]
fn jobs_of_a_run_still_in_progress_do_not_freeze() {
    let w = world(Server {
        jobs: vec![done("2026-10-02T11:00:00Z")],
        run7: "in_progress",
        ..Server::default()
    });
    w.read(JOBS);
    w.advance(RUNS_TTL_MS);
    w.read(JOBS);
    assert_eq!(
        w.requests(),
        [
            JOBS,
            "repos/o/r/actions/runs/7",
            JOBS,
            "repos/o/r/actions/runs/7"
        ]
    );
    let unfinished = world(Server {
        jobs: vec![
            done("2026-10-02T11:00:00Z"),
            Step {
                status: "in_progress",
                completed_at: None,
            },
        ],
        run7: "completed",
        ..Server::default()
    });
    unfinished.read(JOBS);
    unfinished.advance(RUNS_TTL_MS);
    unfinished.read(JOBS);
    // Not all jobs completed: the run is never even asked for.
    assert_eq!(unfinished.requests(), [JOBS, JOBS]);
}

#[test]
fn check_runs_freeze_only_after_they_have_settled() {
    let checks = "repos/o/r/commits/0123456789abcdef0123456789abcdef01234567/check-runs";
    let w = world(Server {
        // Completed one minute before the first read.
        checks: vec![done("2026-10-02T11:59:00Z")],
        ..Server::default()
    });
    assert_eq!(w.read(checks), w.full(checks));
    w.advance(DEFAULT_TTL_MS);
    w.read(checks);
    assert_eq!(
        w.requests().len(),
        2,
        "a just-finished commit can still gain checks"
    );
    w.advance(5 * 60 * 1000);
    w.read(checks);
    w.advance(100 * DEFAULT_TTL_MS);
    w.read(checks);
    assert_eq!(
        w.requests().len(),
        3,
        "settled: frozen after the third read"
    );
    let outcomes: Vec<String> = w.ledger().into_iter().map(|e| e.0).collect();
    assert_eq!(outcomes, ["full", "304", "304", "cache"]);
}

#[test]
fn the_invalidate_route_takes_valid_tags_or_falls_back_to_global() {
    let w = world(Server::default());
    let store = w.broker.store().unwrap();
    let run7 = ["run:7".to_string()];
    let other = ["run:8".to_string()];
    w.advance(1);
    assert_eq!(w.broker.handle_invalidate(br#"{"tags":["run:7"]}"#).0, 200);
    let stamp = store.stale_after(&run7).unwrap();
    assert!(stamp > 0);
    assert_eq!(store.stale_after(&other).unwrap(), 0);
    for body in [
        &b"{}"[..],
        br#"{"tags":["bad tag"]}"#,
        br#"{"tags":[]}"#,
        b"junk",
    ] {
        w.advance(1);
        assert_eq!(w.broker.handle_invalidate(body).0, 200);
        assert!(store.stale_after(&other).unwrap() > stamp, "{body:?}");
    }
}
