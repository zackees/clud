//! Issue #183: in-process HTTP dashboard.
//!
//! Binds a second loopback `tiny_http::Server` alongside the IPC TCP
//! listener (`daemon/server.rs`). Serves three routes:
//!
//! - `GET /` / `GET /index.html` — the embedded single-page dashboard.
//! - `GET /state.json` — one consolidated JSON document with daemon meta,
//!   live sessions, GC tracked entries, repo visits, and aggregate stats.
//! - `POST /gc/purge` — body `{id?, kind?}`; delegates to the existing
//!   `GcOp::Purge` IPC op and returns `{removed, skipped}`.
//!
//! Loopback-only, no authentication — matches the trust model of the
//! existing JSON IPC listener.

use std::collections::HashMap;
use std::fs;
use std::io::{self, Read};
use std::net::TcpStream;
use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::thread;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use tiny_http::{Header, Method, Request, Response, Server};

use super::gc_service::{GcRequestMsg, RegistryMsg, WORKER_REPLY_TIMEOUT};
use super::io_helpers::read_json_file;
use super::memory_service::MemoryService;
use super::paths::{daemon_info_path, sessions_dir};
use super::process_utils::pid_is_alive;
use super::types::{
    CtrlCProfile, DaemonInfo, GcOp, GcReply, ListRow, RepoVisit, SessionKind, SessionSnapshot,
};
use crate::ctrl_c_track::{self, CtrlCEvent};
use crate::memory::embedder::EmbedderTrait;
use crate::session_registry::LiveSession;

/// Supplier of live session-registry rows. Injected at the dashboard
/// boundary so production wires in the redb-backed reader while unit
/// tests pass a no-op stub. This avoids env-var coupling between
/// parallel tests in `daemon::http::tests` (issue #190 follow-up: the
/// initial implementation that read `CLUD_SESSION_DB` directly inside
/// `build_dashboard_state` raced with `build_state_with_empty_state_dir_returns_zeros`
/// on macOS x86 CI).
pub(super) type LiveSessionsProvider =
    std::sync::Arc<dyn Fn() -> Vec<LiveSession> + Send + Sync + 'static>;

/// Production provider: reads the redb session registry under the
/// cross-process advisory lock. Errors are swallowed so a registry
/// hiccup never blanks the dashboard for sessions that *do* have
/// JSON snapshots.
pub(super) fn default_live_sessions_provider() -> LiveSessionsProvider {
    std::sync::Arc::new(|| {
        crate::session_registry::list_live_sessions_under_lock().unwrap_or_default()
    })
}

/// Bundled single-page dashboard. Vanilla JS, no build step. Polls
/// `/state.json` every 5s and renders the three tabs (Sessions / GC /
/// Repos) plus per-row and per-kind purge controls.
const DASHBOARD_HTML: &str = include_str!("../../assets/dashboard/index.html");

/// Hard cap on a POST request body so a misbehaving client can't OOM the
/// daemon. The purge payload is two short JSON fields; 16 KiB is generous.
const MAX_REQUEST_BODY_BYTES: usize = 16 * 1024;

/// Aggregate document returned by `GET /state.json`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DashboardState {
    pub daemon: DaemonStateView,
    pub sessions: Vec<SessionView>,
    pub gc: Vec<ListRow>,
    pub repos: Vec<RepoVisit>,
    /// Recent cross-path Ctrl+C exit events. Each entry is one CLI
    /// process that observed Ctrl+C and recorded the elapsed wall-clock
    /// time from observation to process-exit. Capped at
    /// [`ctrl_c_track::DASHBOARD_EVENT_LIMIT`], newest first.
    #[serde(default)]
    pub ctrl_c_events: Vec<CtrlCEvent>,
    pub stats: Stats,
}

/// Meta about the daemon serving this dashboard.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DaemonStateView {
    pub pid: u32,
    pub ipc_port: u16,
    pub dashboard_port: Option<u16>,
    pub started_at_unix: i64,
    pub now_unix: i64,
    pub uptime_secs: u64,
    pub version: String,
}

/// Public-safe projection of `SessionSnapshot` — drops the *_pid fields.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionView {
    pub id: String,
    pub kind: String,
    pub name: Option<String>,
    pub cwd: Option<String>,
    pub created_at: Option<u64>,
    pub detachable: bool,
    pub background: bool,
    pub attachable: bool,
    pub repeat_interval_secs: Option<u64>,
    pub repeat_next_run_at: Option<u64>,
    pub repeat_running: bool,
    pub exit_code: Option<i32>,
    pub worker_port: u16,
    pub live: bool,
    pub ctrl_c: Option<CtrlCProfileView>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CtrlCProfileView {
    pub cli_pid: Option<u32>,
    pub cli_observed_at_ms: Option<u64>,
    pub cli_handoff_at_ms: Option<u64>,
    pub cli_return_ready_at_ms: Option<u64>,
    pub cli_handoff_ms: Option<u64>,
    pub daemon_received_at_ms: Option<u64>,
    pub daemon_kill_started_at_ms: Option<u64>,
    pub daemon_kill_finished_at_ms: Option<u64>,
    pub daemon_kill_ms: Option<u64>,
    pub fast_path: bool,
}

/// Counts derived from the rest of the document.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Stats {
    pub session_count: usize,
    pub live_session_count: usize,
    pub gc_count: usize,
    pub gc_by_kind: HashMap<String, usize>,
    pub repo_count: usize,
}

/// Body of `POST /gc/purge`.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct PurgeRequest {
    #[serde(default)]
    pub id: Option<i64>,
    #[serde(default)]
    pub kind: Option<String>,
}

/// Response body of `POST /gc/purge`. The synchronous per-row delete
/// (`{id: N}`) populates `removed`; the bulk async purge (no `id`)
/// populates `dispatched`. `skipped` is always the count of candidates
/// the worker filtered out as live or non-purgeable.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct PurgeResponse {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub removed: Option<usize>,
    /// Issue #268: tasks handed to the parallel purge pool. The
    /// matching filesystem removals and redb row deletes happen
    /// asynchronously; poll `/state.json` to watch counts drop.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dispatched: Option<usize>,
    pub skipped: usize,
}

/// Spawn the dashboard's HTTP listener in a background thread.
/// Returns the bound port (or `None` if the listener could not be brought
/// up — logged once and the daemon continues without a dashboard).
pub(super) fn spawn_dashboard(
    state_dir: PathBuf,
    gc_tx: Option<mpsc::Sender<RegistryMsg>>,
    ipc_port: u16,
    started_at_unix: i64,
    live_sessions_provider: LiveSessionsProvider,
    memory: Option<std::sync::Arc<MemoryService>>,
) -> Option<u16> {
    let server = match Server::http("127.0.0.1:0") {
        Ok(s) => s,
        Err(err) => {
            eprintln!("[clud] note: dashboard listener failed to bind: {err}");
            return None;
        }
    };
    let port = match server.server_addr().to_ip() {
        Some(addr) => addr.port(),
        None => {
            eprintln!("[clud] note: dashboard listener has no IPv4 address");
            return None;
        }
    };
    let res = thread::Builder::new()
        .name("clud-dashboard-http".to_string())
        .spawn(move || {
            run_dashboard_loop(
                server,
                state_dir,
                gc_tx,
                ipc_port,
                started_at_unix,
                live_sessions_provider,
                memory,
            )
        });
    match res {
        Ok(_) => Some(port),
        Err(err) => {
            eprintln!("[clud] note: dashboard thread spawn failed: {err}");
            None
        }
    }
}

fn run_dashboard_loop(
    server: Server,
    state_dir: PathBuf,
    gc_tx: Option<mpsc::Sender<RegistryMsg>>,
    ipc_port: u16,
    started_at_unix: i64,
    live_sessions_provider: LiveSessionsProvider,
    memory: Option<std::sync::Arc<MemoryService>>,
) {
    for request in server.incoming_requests() {
        let method = request.method().clone();
        let url = request.url().to_string();
        let path = url.split('?').next().unwrap_or(&url).to_string();
        match (method, path.as_str()) {
            (Method::Get, "/") | (Method::Get, "/index.html") => {
                respond_html(request, 200, DASHBOARD_HTML.as_bytes());
            }
            (Method::Get, "/state.json") => {
                handle_state(
                    request,
                    &state_dir,
                    gc_tx.as_ref(),
                    ipc_port,
                    started_at_unix,
                    live_sessions_provider.as_ref(),
                );
            }
            (Method::Post, "/gc/purge") => {
                handle_purge(request, gc_tx.as_ref());
            }
            // Issue #261: scaffolding for the memory dashboard routes.
            // Bodies are stubs in this PR; the real reads (recent
            // memories, BM25 + KNN hybrid search, tier counts) land in
            // #263 alongside the dashboard JS.
            (Method::Get, "/memory/recent") => {
                handle_memory_recent(request, memory.as_ref());
            }
            (Method::Get, "/memory/search") => {
                handle_memory_search(request, memory.as_ref());
            }
            (Method::Get, "/memory/stats") => {
                handle_memory_stats(request, memory.as_ref());
            }
            (Method::Post, "/memory/save") => {
                handle_memory_save(request, memory.as_ref());
            }
            (m, p) if m == Method::Post && p.starts_with("/memory/forget/") => {
                let id = p.trim_start_matches("/memory/forget/").to_string();
                handle_memory_forget(request, &id, memory.as_ref());
            }
            _ => {
                respond_text(request, 404, b"not found");
            }
        }
    }
}

// ---------- route handlers ----------

fn handle_state(
    request: Request,
    state_dir: &Path,
    gc_tx: Option<&mpsc::Sender<RegistryMsg>>,
    ipc_port: u16,
    started_at_unix: i64,
    live_sessions_provider: &(dyn Fn() -> Vec<LiveSession> + Send + Sync),
) {
    let live_sessions = live_sessions_provider();
    match build_dashboard_state(state_dir, gc_tx, ipc_port, started_at_unix, live_sessions) {
        Ok(state) => match serde_json::to_vec(&state) {
            Ok(bytes) => respond_json(request, 200, &bytes),
            Err(err) => respond_json(
                request,
                500,
                json_error_bytes(&format!("serialize state failed: {err}")).as_slice(),
            ),
        },
        Err(err) => {
            respond_json(request, 500, json_error_bytes(&err.to_string()).as_slice());
        }
    }
}

fn handle_purge(mut request: Request, gc_tx: Option<&mpsc::Sender<RegistryMsg>>) {
    let body = match read_body(&mut request) {
        Ok(b) => b,
        Err(err) => {
            respond_json(
                request,
                400,
                json_error_bytes(&format!("read body failed: {err}")).as_slice(),
            );
            return;
        }
    };
    let payload: PurgeRequest = if body.is_empty() {
        PurgeRequest::default()
    } else {
        match serde_json::from_slice(&body) {
            Ok(p) => p,
            Err(err) => {
                respond_json(
                    request,
                    400,
                    json_error_bytes(&format!("invalid JSON: {err}")).as_slice(),
                );
                return;
            }
        }
    };

    let Some(tx) = gc_tx else {
        respond_json(
            request,
            503,
            json_error_bytes("gc registry unavailable").as_slice(),
        );
        return;
    };

    // Route the request: per-row delete uses the surgical `DeleteById`
    // IPC op so the on-disk and registry-row removal target exactly the
    // requested row regardless of how many siblings share its kind. The
    // bulk per-kind / per-age path keeps using `Purge`.
    let op = match payload.id {
        Some(id) => GcOp::DeleteById { id },
        None => GcOp::Purge {
            duration: None,
            kind: payload.kind.clone(),
            dry_run: false,
        },
    };

    match send_gc_op(tx, op) {
        Ok(reply) => respond_purge_reply(request, reply),
        Err(err) => respond_json(request, 500, json_error_bytes(&err).as_slice()),
    }
}

fn respond_purge_reply(request: Request, reply: GcReply) {
    match reply {
        GcReply::PurgeOk { removed, skipped } => {
            let body = serde_json::to_vec(&PurgeResponse {
                removed: Some(removed),
                dispatched: None,
                skipped,
            })
            .unwrap_or_else(|_| b"{}".to_vec());
            respond_json(request, 200, &body);
        }
        GcReply::PurgeStarted {
            dispatched,
            skipped,
        } => {
            let body = serde_json::to_vec(&PurgeResponse {
                removed: None,
                dispatched: Some(dispatched),
                skipped,
            })
            .unwrap_or_else(|_| b"{}".to_vec());
            respond_json(request, 200, &body);
        }
        GcReply::Error { message } => {
            respond_json(request, 500, json_error_bytes(&message).as_slice());
        }
        other => {
            respond_json(
                request,
                500,
                json_error_bytes(&format!("unexpected reply: {other:?}")).as_slice(),
            );
        }
    }
}

// ---------- memory route stubs (issue #261) ----------
//
// All five routes are wired to the live `Arc<MemoryService>` so #263 can
// fill in the bodies in a follow-up without touching this file's match
// arms. On `memory.is_none()` (memory subsystem failed to start) every
// route returns 503 — the daemon keeps serving session + GC traffic.

fn handle_memory_recent(request: Request, memory: Option<&std::sync::Arc<MemoryService>>) {
    if memory.is_none() {
        respond_json(
            request,
            503,
            json_error_bytes("memory subsystem unavailable").as_slice(),
        );
        return;
    }
    // TODO(#263): pull recent rows from `MemoryService::store` and
    // render them as `[{id, tier, content, created_at_ms}]`. Empty array
    // until then so the dashboard's JS can already poll the route.
    respond_json(request, 200, b"[]");
}

fn handle_memory_search(request: Request, memory: Option<&std::sync::Arc<MemoryService>>) {
    if memory.is_none() {
        respond_json(
            request,
            503,
            json_error_bytes("memory subsystem unavailable").as_slice(),
        );
        return;
    }
    // TODO(#263): parse `?q=` + `?k=`, run hybrid search via embedder +
    // store + lexical, return `[{id, tier, content, score}]`.
    respond_json(request, 200, b"[]");
}

fn handle_memory_stats(request: Request, memory: Option<&std::sync::Arc<MemoryService>>) {
    let Some(svc) = memory else {
        respond_json(
            request,
            503,
            json_error_bytes("memory subsystem unavailable").as_slice(),
        );
        return;
    };
    // The embedder name is cheap and useful even before #263 ships, so
    // populate it now. Counts stay at 0 until #263 wires them up.
    let embedder_status =
        <crate::memory::Embedder as EmbedderTrait>::name(svc.embedder.as_ref()).to_string();
    let body = serde_json::json!({
        "tier_counts": {
            "working": 0,
            "episodic": 0,
            "semantic": 0,
        },
        "embedder_status": embedder_status,
        "consolidate_interval_ms": svc.consolidate_interval_ms,
    });
    let bytes = serde_json::to_vec(&body).unwrap_or_else(|_| b"{}".to_vec());
    respond_json(request, 200, &bytes);
}

fn handle_memory_save(request: Request, memory: Option<&std::sync::Arc<MemoryService>>) {
    if memory.is_none() {
        respond_json(
            request,
            503,
            json_error_bytes("memory subsystem unavailable").as_slice(),
        );
        return;
    }
    // TODO(#263): parse `{content, tier, session_id?, scope_key?}`,
    // embed, store, lexical-upsert, return `{id}`. Stub returns 501 so
    // the dashboard's save form gets a deterministic error until then.
    respond_json(
        request,
        501,
        json_error_bytes("memory save not implemented in this build (see #263)").as_slice(),
    );
}

fn handle_memory_forget(
    request: Request,
    _id: &str,
    memory: Option<&std::sync::Arc<MemoryService>>,
) {
    if memory.is_none() {
        respond_json(
            request,
            503,
            json_error_bytes("memory subsystem unavailable").as_slice(),
        );
        return;
    }
    // TODO(#263): validate id, call `SqliteStore::delete` +
    // `LexicalIndex::delete`, return `{deleted: true}`.
    respond_json(
        request,
        501,
        json_error_bytes("memory forget not implemented in this build (see #263)").as_slice(),
    );
}

// ---------- state aggregation ----------

fn build_dashboard_state(
    state_dir: &Path,
    gc_tx: Option<&mpsc::Sender<RegistryMsg>>,
    ipc_port: u16,
    started_at_unix: i64,
    live_sessions: Vec<LiveSession>,
) -> Result<DashboardState, String> {
    let now_unix = current_unix();

    let mut sessions = read_session_views(state_dir).unwrap_or_default();
    // Issue #190: surface direct-runner sessions (default `clud` invocation
    // path) by reading the redb session registry. The on-disk snapshot
    // files are only written by the centralized daemon worker, so without
    // this merge the dashboard would render "no sessions recorded" even
    // while a foreground `clud` is clearly running. The caller — typically
    // `handle_state` via `default_live_sessions_provider` — does the
    // actual registry read so tests can inject mock data without env-var
    // entanglement.
    merge_registry_sessions(&mut sessions, live_sessions);
    let live_session_count = sessions.iter().filter(|s| s.live).count();

    let gc_rows = match gc_tx {
        Some(tx) => match send_gc_op(tx, GcOp::List { kind: None }) {
            Ok(GcReply::ListOk { rows }) => rows,
            Ok(GcReply::Error { message }) => return Err(format!("gc.list failed: {message}")),
            Ok(other) => return Err(format!("gc.list unexpected reply: {other:?}")),
            Err(err) => return Err(err),
        },
        None => Vec::new(),
    };

    let repos = match gc_tx {
        Some(tx) => match send_gc_op(tx, GcOp::ListRepoVisits) {
            Ok(GcReply::RepoVisitsOk { rows }) => rows,
            Ok(GcReply::Error { message }) => {
                return Err(format!("gc.list_repo_visits failed: {message}"));
            }
            Ok(other) => {
                return Err(format!("gc.list_repo_visits unexpected reply: {other:?}"));
            }
            Err(err) => return Err(err),
        },
        None => Vec::new(),
    };

    let mut gc_by_kind: HashMap<String, usize> = HashMap::new();
    for row in &gc_rows {
        *gc_by_kind.entry(row.kind.clone()).or_insert(0) += 1;
    }

    let ctrl_c_events =
        ctrl_c_track::read_recent_events(state_dir, ctrl_c_track::DASHBOARD_EVENT_LIMIT);

    let stats = Stats {
        session_count: sessions.len(),
        live_session_count,
        gc_count: gc_rows.len(),
        gc_by_kind,
        repo_count: repos.len(),
    };

    Ok(DashboardState {
        daemon: DaemonStateView {
            pid: std::process::id(),
            ipc_port,
            dashboard_port: read_dashboard_port(state_dir).ok().flatten(),
            started_at_unix,
            now_unix,
            uptime_secs: (now_unix - started_at_unix).max(0) as u64,
            version: env!("CARGO_PKG_VERSION").to_string(),
        },
        sessions,
        gc: gc_rows,
        repos,
        ctrl_c_events,
        stats,
    })
}

fn read_session_views(state_dir: &Path) -> io::Result<Vec<SessionView>> {
    let mut out = Vec::new();
    let dir = sessions_dir(state_dir);
    let entries = match fs::read_dir(&dir) {
        Ok(it) => it,
        Err(err) if err.kind() == io::ErrorKind::NotFound => return Ok(out),
        Err(err) => return Err(err),
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().and_then(|ext| ext.to_str()) != Some("json") {
            continue;
        }
        let Ok(snap) = read_json_file::<SessionSnapshot>(&path) else {
            continue;
        };
        let live = snap.exit_code.is_none() && pid_is_alive(snap.worker_pid);
        out.push(SessionView {
            id: snap.id,
            kind: match snap.kind {
                SessionKind::Subprocess => "subprocess".to_string(),
                SessionKind::Pty => "pty".to_string(),
            },
            name: snap.name,
            cwd: snap.cwd,
            created_at: snap.created_at,
            detachable: snap.detachable,
            background: snap.background,
            attachable: snap.attachable,
            repeat_interval_secs: snap.repeat_interval_secs,
            repeat_next_run_at: snap.repeat_next_run_at,
            repeat_running: snap.repeat_running,
            exit_code: snap.exit_code,
            worker_port: snap.worker_port,
            live,
            ctrl_c: snap.ctrl_c.map(ctrl_c_profile_view),
        });
    }
    // Newest first.
    out.sort_by(|a, b| b.created_at.unwrap_or(0).cmp(&a.created_at.unwrap_or(0)));
    Ok(out)
}

/// Merge live rows from the redb session registry into the dashboard's
/// session list (issue #190). Direct-runner `clud` invocations never
/// produce a `SessionSnapshot` JSON file but do register themselves in
/// the redb registry for the fork-bomb cap, so the registry is the only
/// place where they're visible. `live_sessions` is provided by the
/// caller — production wires in the real registry reader; tests pass
/// `Vec::new()` (or seeded data) to avoid env-var racing across the
/// `daemon::http::tests` module.
fn merge_registry_sessions(sessions: &mut Vec<SessionView>, live_sessions: Vec<LiveSession>) {
    for row in live_sessions {
        let id = format!("direct-{}", row.pid);
        sessions.push(SessionView {
            id,
            kind: "direct".to_string(),
            // Surface the backend selection (`claude` / `codex`) under the
            // session name column so users can tell which agent each
            // direct-runner row corresponds to.
            name: row.backend.clone(),
            cwd: row.cwd,
            // `started_unix` is seconds; snapshot rows use milliseconds.
            // Convert so the dashboard's age formatter renders both the
            // same way without a per-kind unit-toggle.
            created_at: Some((row.started_unix.max(0) as u64) * 1000),
            detachable: false,
            background: false,
            attachable: false,
            repeat_interval_secs: None,
            repeat_next_run_at: None,
            repeat_running: false,
            exit_code: None,
            worker_port: 0,
            // The registry already filtered by OS PID liveness probe.
            live: true,
            ctrl_c: None,
        });
    }

    // Newest first across the merged list.
    sessions.sort_by(|a, b| b.created_at.unwrap_or(0).cmp(&a.created_at.unwrap_or(0)));
}

fn ctrl_c_profile_view(profile: CtrlCProfile) -> CtrlCProfileView {
    CtrlCProfileView {
        cli_pid: profile.cli_pid,
        cli_observed_at_ms: profile.cli_observed_at_ms,
        cli_handoff_at_ms: profile.cli_handoff_at_ms,
        cli_return_ready_at_ms: profile.cli_return_ready_at_ms,
        cli_handoff_ms: profile.cli_handoff_ms,
        daemon_received_at_ms: profile.daemon_received_at_ms,
        daemon_kill_started_at_ms: profile.daemon_kill_started_at_ms,
        daemon_kill_finished_at_ms: profile.daemon_kill_finished_at_ms,
        daemon_kill_ms: profile.daemon_kill_ms,
        fast_path: profile.fast_path,
    }
}

// ---------- IPC plumbing ----------

fn send_gc_op(tx: &mpsc::Sender<RegistryMsg>, op: GcOp) -> Result<GcReply, String> {
    let (reply_tx, reply_rx) = mpsc::sync_channel::<GcReply>(1);
    tx.send(RegistryMsg::Op(GcRequestMsg { op, reply_tx }))
        .map_err(|_| "gc registry worker stopped".to_string())?;
    reply_rx
        .recv_timeout(WORKER_REPLY_TIMEOUT)
        .map_err(|_| "gc registry worker timed out".to_string())
}

// ---------- public helpers for the `clud ui` CLI ----------

/// Read the daemon-info file and return its dashboard port, if present.
/// `Ok(None)` distinguishes "daemon is up but the dashboard listener
/// didn't bind on this run" from "daemon hasn't even been started".
pub fn read_dashboard_port(state_dir: &Path) -> io::Result<Option<u16>> {
    let info = read_json_file::<DaemonInfo>(&daemon_info_path(state_dir))?;
    Ok(info.dashboard_port)
}

/// Re-export the typed info read by the `clud ui` CLI. Kept narrow so the
/// CLI layer doesn't depend on the (internal) `DaemonInfo` struct.
pub fn read_dashboard_info(state_dir: &Path) -> io::Result<DashboardInfo> {
    let info = read_json_file::<DaemonInfo>(&daemon_info_path(state_dir))?;
    Ok(DashboardInfo {
        pid: info.pid,
        ipc_port: info.port,
        dashboard_port: info.dashboard_port,
    })
}

/// Public view of `daemon.json` used by the `clud ui` CLI.
#[derive(Debug, Clone)]
pub struct DashboardInfo {
    pub pid: u32,
    pub ipc_port: u16,
    pub dashboard_port: Option<u16>,
}

pub fn dashboard_url_from_info(port: u16) -> String {
    format!("http://127.0.0.1:{port}/")
}

/// Fetch `/state.json` from the running dashboard. Used by `clud ui --json`.
pub fn fetch_state_json(port: u16) -> io::Result<String> {
    use std::io::Write;
    let mut stream = TcpStream::connect(("127.0.0.1", port))?;
    stream.set_read_timeout(Some(Duration::from_secs(5)))?;
    stream.set_write_timeout(Some(Duration::from_secs(5)))?;
    let req = "GET /state.json HTTP/1.0\r\nHost: localhost\r\nConnection: close\r\n\r\n";
    stream.write_all(req.as_bytes())?;
    stream.flush()?;
    let mut buf = Vec::with_capacity(4096);
    stream.read_to_end(&mut buf)?;
    // Split off the HTTP headers; we only return the body.
    let body_start = find_body_start(&buf).ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidData,
            "dashboard response had no headers terminator",
        )
    })?;
    let body = &buf[body_start..];
    String::from_utf8(body.to_vec())
        .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e.to_string()))
}

fn find_body_start(buf: &[u8]) -> Option<usize> {
    buf.windows(4)
        .position(|w| w == b"\r\n\r\n")
        .map(|i| i + 4)
        .or_else(|| buf.windows(2).position(|w| w == b"\n\n").map(|i| i + 2))
}

// ---------- tiny_http helpers ----------

fn respond_html(request: Request, status: u16, body: &[u8]) {
    let response = Response::from_data(body.to_vec())
        .with_status_code(status)
        .with_header(html_content_type())
        .with_header(no_cache_header());
    let _ = request.respond(response);
}

fn respond_json(request: Request, status: u16, body: &[u8]) {
    let response = Response::from_data(body.to_vec())
        .with_status_code(status)
        .with_header(json_content_type())
        .with_header(no_cache_header());
    let _ = request.respond(response);
}

fn respond_text(request: Request, status: u16, body: &[u8]) {
    let response = Response::from_data(body.to_vec())
        .with_status_code(status)
        .with_header(text_content_type())
        .with_header(no_cache_header());
    let _ = request.respond(response);
}

fn html_content_type() -> Header {
    Header::from_bytes(&b"Content-Type"[..], &b"text/html; charset=utf-8"[..])
        .expect("static content-type header")
}

fn json_content_type() -> Header {
    Header::from_bytes(&b"Content-Type"[..], &b"application/json"[..])
        .expect("static content-type header")
}

fn text_content_type() -> Header {
    Header::from_bytes(&b"Content-Type"[..], &b"text/plain; charset=utf-8"[..])
        .expect("static content-type header")
}

fn no_cache_header() -> Header {
    Header::from_bytes(&b"Cache-Control"[..], &b"no-store"[..]).expect("static cache header")
}

fn read_body(request: &mut Request) -> io::Result<Vec<u8>> {
    let mut buf = Vec::new();
    request
        .as_reader()
        .take(MAX_REQUEST_BODY_BYTES as u64)
        .read_to_end(&mut buf)?;
    Ok(buf)
}

fn json_error_bytes(message: &str) -> Vec<u8> {
    let payload = serde_json::json!({ "error": message });
    serde_json::to_vec(&payload).unwrap_or_else(|_| b"{\"error\":\"unknown\"}".to_vec())
}

fn current_unix() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::daemon::gc_service::spawn_registry_worker_with;
    use crate::daemon::types::{CtrlCProfile, SessionKind, SessionSnapshot};
    use crate::gc::Registry;
    use std::io::Write;

    fn write_fake_session(state_dir: &Path, id: &str, snap: SessionSnapshot) {
        let dir = state_dir.join("sessions");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join(format!("{id}.json"));
        std::fs::write(&path, serde_json::to_vec_pretty(&snap).unwrap()).unwrap();
    }

    fn fake_snapshot(id: &str, name: &str, cwd: &str) -> SessionSnapshot {
        SessionSnapshot {
            id: id.to_string(),
            kind: SessionKind::Pty,
            cwd: Some(cwd.to_string()),
            name: Some(name.to_string()),
            created_at: Some(500),
            detachable: false,
            background: false,
            attachable: true,
            repeat_interval_secs: None,
            repeat_next_run_at: None,
            repeat_running: false,
            // Sensitive fields — the SessionView should drop these.
            daemon_pid: 1,
            // A PID this unlikely to be alive forces live=false in tests.
            worker_pid: 4_000_000_000,
            worker_port: 12345,
            root_pid: None,
            exit_code: None,
            ctrl_c: None,
        }
    }

    #[test]
    fn dashboard_url_format() {
        assert_eq!(dashboard_url_from_info(54321), "http://127.0.0.1:54321/");
    }

    #[test]
    fn purge_request_defaults_when_body_is_empty() {
        let parsed: PurgeRequest = serde_json::from_str("{}").unwrap();
        assert!(parsed.id.is_none());
        assert!(parsed.kind.is_none());
    }

    #[test]
    fn purge_request_round_trips_kind_filter() {
        let json = r#"{"kind":"worktree"}"#;
        let parsed: PurgeRequest = serde_json::from_str(json).unwrap();
        assert_eq!(parsed.kind.as_deref(), Some("worktree"));
    }

    #[test]
    fn dashboard_html_asset_loads() {
        // Sanity check: the embedded asset compiled in. Tests pulled from
        // disk would mask a missing `include_str!`.
        assert!(DASHBOARD_HTML.contains("clud"));
    }

    #[test]
    fn find_body_start_after_crlf_crlf() {
        let raw = b"HTTP/1.0 200 OK\r\nContent-Type: application/json\r\n\r\n{\"x\":1}";
        let idx = find_body_start(raw).unwrap();
        assert_eq!(&raw[idx..], b"{\"x\":1}");
    }

    /// Shared "no live sessions" provider for the tests below that pre-date
    /// issue #190 — they don't care about the registry merge and would
    /// otherwise have to fight the global `CLUD_SESSION_DB` env-var.
    fn empty_live_provider() -> super::LiveSessionsProvider {
        std::sync::Arc::new(Vec::new)
    }

    #[test]
    fn build_state_with_empty_state_dir_returns_zeros() {
        let dir = tempfile::tempdir().unwrap();
        let state = build_dashboard_state(dir.path(), None, 9999, 100, Vec::new()).expect("build");
        assert_eq!(state.stats.session_count, 0);
        assert_eq!(state.stats.live_session_count, 0);
        assert_eq!(state.stats.gc_count, 0);
        assert_eq!(state.stats.repo_count, 0);
        assert_eq!(state.daemon.ipc_port, 9999);
        assert_eq!(state.daemon.started_at_unix, 100);
        assert_eq!(state.daemon.version, env!("CARGO_PKG_VERSION"));
    }

    /// Issue #190: direct-runner `clud` invocations only show up in the
    /// redb session registry, not as JSON snapshots on disk. The dashboard
    /// must merge those rows in so the Sessions tab isn't perpetually
    /// empty for users who never use `--detach` / `--experimental-daemon-centralized`.
    ///
    /// Inject a synthetic `LiveSession` directly so this test can run in
    /// parallel with the rest of the suite — no env-var fiddling, no
    /// cross-test races on `CLUD_SESSION_DB`.
    #[test]
    fn build_state_surfaces_direct_runner_registry_rows() {
        let dir = tempfile::tempdir().unwrap();
        let live = vec![LiveSession {
            pid: 4242,
            started_unix: 1_700_000_000,
            backend: Some("claude".to_string()),
            launch_mode: Some("subprocess".to_string()),
            cwd: Some("/dev/repo".to_string()),
        }];

        let state = build_dashboard_state(dir.path(), None, 9999, 100, live).expect("build");
        let direct: Vec<_> = state
            .sessions
            .iter()
            .filter(|s| s.kind == "direct")
            .collect();
        assert_eq!(
            direct.len(),
            1,
            "registry-backed direct session should appear; got {:?}",
            state.sessions
        );
        assert_eq!(direct[0].id, "direct-4242");
        assert_eq!(direct[0].name.as_deref(), Some("claude"));
        assert_eq!(direct[0].cwd.as_deref(), Some("/dev/repo"));
        assert!(direct[0].live);
        assert_eq!(direct[0].worker_port, 0);
        // The live-session count in the stats must include direct sessions
        // — that's what the dashboard header displays.
        assert_eq!(state.stats.live_session_count, 1);
    }

    #[test]
    fn build_state_includes_session_snapshots() {
        let dir = tempfile::tempdir().unwrap();
        write_fake_session(
            dir.path(),
            "sess-a",
            fake_snapshot("sess-a", "test", "/dev/foo"),
        );

        let state = build_dashboard_state(dir.path(), None, 9999, 100, Vec::new()).expect("build");
        assert_eq!(state.sessions.len(), 1);
        assert_eq!(state.sessions[0].id, "sess-a");
        assert_eq!(state.sessions[0].name.as_deref(), Some("test"));
        assert_eq!(state.sessions[0].cwd.as_deref(), Some("/dev/foo"));
        assert_eq!(state.sessions[0].kind, "pty");
        // Unlikely-PID worker should be reported as not live.
        assert!(!state.sessions[0].live);

        // SessionView must not expose `daemon_pid` / `worker_pid` / `root_pid`.
        let json = serde_json::to_value(&state.sessions[0]).unwrap();
        assert!(json.get("daemon_pid").is_none());
        assert!(json.get("worker_pid").is_none());
        assert!(json.get("root_pid").is_none());
    }

    #[test]
    fn build_state_includes_ctrl_c_events_when_present() {
        use crate::ctrl_c_track::{events_dir, CtrlCEvent, InvocationKind};
        let dir = tempfile::tempdir().unwrap();
        let edir = events_dir(dir.path());
        std::fs::create_dir_all(&edir).unwrap();
        for i in 0..3u64 {
            let event = CtrlCEvent {
                pid: 1_000 + i as u32,
                observed_at_ms: 1_700_000_000_000 + i * 1000,
                exit_at_ms: 1_700_000_000_500 + i * 1000,
                elapsed_ms: 500 + i,
                kind: InvocationKind::Direct,
                exit_code: 130,
                cwd: Some(format!("/tmp/a{i}")),
            };
            let path = edir.join(format!("{:013}-{}.json", event.exit_at_ms, event.pid));
            std::fs::write(&path, serde_json::to_vec(&event).unwrap()).unwrap();
        }
        let state = build_dashboard_state(dir.path(), None, 9999, 100, Vec::new()).expect("build");
        assert_eq!(state.ctrl_c_events.len(), 3);
        // Newest first
        assert_eq!(state.ctrl_c_events[0].exit_at_ms, 1_700_000_000_500 + 2_000);
        assert_eq!(state.ctrl_c_events[2].exit_at_ms, 1_700_000_000_500);
    }

    #[test]
    fn build_state_includes_ctrl_c_profile() {
        let dir = tempfile::tempdir().unwrap();
        let mut snap = fake_snapshot("sess-ctrl-c", "interrupt", "/dev/ctrl-c");
        snap.ctrl_c = Some(CtrlCProfile {
            cli_pid: Some(777),
            cli_observed_at_ms: Some(10_000),
            cli_handoff_at_ms: Some(10_025),
            cli_return_ready_at_ms: Some(10_025),
            cli_handoff_ms: Some(25),
            daemon_received_at_ms: Some(10_026),
            daemon_kill_started_at_ms: Some(10_026),
            daemon_kill_finished_at_ms: Some(10_090),
            daemon_kill_ms: Some(64),
            fast_path: true,
        });
        write_fake_session(dir.path(), "sess-ctrl-c", snap);

        let state = build_dashboard_state(dir.path(), None, 9999, 100, Vec::new()).expect("build");
        let profile = state.sessions[0].ctrl_c.as_ref().expect("profile");
        assert_eq!(profile.cli_handoff_ms, Some(25));
        assert_eq!(profile.daemon_kill_ms, Some(64));
        assert!(profile.fast_path);
    }

    #[test]
    fn end_to_end_state_endpoint_returns_all_three_kinds() {
        let dir = tempfile::tempdir().unwrap();
        let db_path = dir.path().join("e2e.redb");

        // Seed: one session.
        write_fake_session(
            dir.path(),
            "sess-x",
            fake_snapshot("sess-x", "fix", "/dev/repo"),
        );

        // Seed: one GC row + one repo visit.
        let registry = Registry::open_at(&db_path).expect("open registry");
        let gc_tx = spawn_registry_worker_with(registry).expect("worker");
        let (rx_t, rx) = mpsc::sync_channel::<GcReply>(1);
        gc_tx
            .send(RegistryMsg::Op(GcRequestMsg {
                op: GcOp::Insert {
                    kind: "worktree".to_string(),
                    path: "/tmp/wt-x".to_string(),
                    repo_root: Some("/dev/repo".to_string()),
                    branch: Some("feat/x".to_string()),
                    agent_id: Some("agent-x".to_string()),
                    created_unix: Some(1000),
                },
                reply_tx: rx_t,
            }))
            .unwrap();
        let _ = rx.recv_timeout(Duration::from_secs(2)).unwrap();

        let (rx_t, rx) = mpsc::sync_channel::<GcReply>(1);
        gc_tx
            .send(RegistryMsg::Op(GcRequestMsg {
                op: GcOp::RecordRepoVisit {
                    repo_root: "/dev/repo".to_string(),
                    cwd: "/dev/repo".to_string(),
                    now_unix: Some(2000),
                },
                reply_tx: rx_t,
            }))
            .unwrap();
        let _ = rx.recv_timeout(Duration::from_secs(2)).unwrap();

        // Spawn the actual HTTP server.
        let port = spawn_dashboard(
            dir.path().to_path_buf(),
            Some(gc_tx.clone()),
            9999,
            100,
            empty_live_provider(),
            None,
        )
        .expect("dashboard spawned");

        // Hit /state.json.
        let body = fetch_state_json(port).expect("fetch state");
        let state: DashboardState = serde_json::from_str(&body).expect("parse");
        assert_eq!(state.stats.session_count, 1);
        assert_eq!(state.stats.gc_count, 1);
        assert_eq!(state.stats.repo_count, 1);
        assert_eq!(state.sessions[0].name.as_deref(), Some("fix"));
        assert_eq!(state.gc[0].kind, "worktree");
        assert_eq!(state.repos[0].repo_root, "/dev/repo");
        assert_eq!(state.repos[0].run_count, 1);

        // Hit GET / and confirm the HTML asset is served.
        let html_body = fetch_path(port, "GET", "/", None).expect("fetch root");
        assert!(html_body.contains("clud dashboard"));
    }

    #[test]
    fn end_to_end_purge_kind_round_trip_mutates_registry() {
        let dir = tempfile::tempdir().unwrap();
        let db_path = dir.path().join("purge.redb");

        let registry = Registry::open_at(&db_path).expect("open registry");
        let gc_tx = spawn_registry_worker_with(registry).expect("worker");
        for p in ["/tmp/p-a", "/tmp/p-b"] {
            let (rx_t, rx) = mpsc::sync_channel::<GcReply>(1);
            gc_tx
                .send(RegistryMsg::Op(GcRequestMsg {
                    op: GcOp::Insert {
                        kind: "cache".to_string(),
                        path: p.to_string(),
                        repo_root: None,
                        branch: None,
                        agent_id: None,
                        created_unix: Some(1000),
                    },
                    reply_tx: rx_t,
                }))
                .unwrap();
            let _ = rx.recv_timeout(Duration::from_secs(2)).unwrap();
        }

        let port = spawn_dashboard(
            dir.path().to_path_buf(),
            Some(gc_tx.clone()),
            9999,
            100,
            empty_live_provider(),
            None,
        )
        .expect("dashboard spawned");

        // POST /gc/purge {"kind":"cache"} — bulk async purge.
        let body = fetch_path(
            port,
            "POST",
            "/gc/purge",
            Some(r#"{"kind":"cache"}"#.to_string()),
        )
        .expect("purge");
        let resp: PurgeResponse = serde_json::from_str(&body).expect("parse");
        // Issue #268: bulk purge replies `dispatched`, not `removed`.
        // The two entries point at /tmp/p-a and /tmp/p-b, which do not
        // exist on disk → `remove_dir_all` short-circuits to Ok and the
        // worker drops the redb rows once the completions land.
        assert_eq!(resp.dispatched, Some(2));
        assert_eq!(resp.removed, None);
        assert_eq!(resp.skipped, 0);

        // Async deletes land slightly after the HTTP response — poll
        // until the registry shrinks rather than racing against it.
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        loop {
            let state_body = fetch_state_json(port).expect("re-fetch state");
            let state: DashboardState = serde_json::from_str(&state_body).expect("parse state");
            if state.stats.gc_count == 0 {
                break;
            }
            if std::time::Instant::now() >= deadline {
                panic!(
                    "registry never drained after bulk purge (gc_count={})",
                    state.stats.gc_count
                );
            }
            std::thread::sleep(Duration::from_millis(25));
        }
    }

    /// Per-row Delete button on the dashboard: `POST /gc/purge {id: N}`
    /// must remove exactly the targeted row even when other rows share
    /// its `kind`. Replaces the earlier "single row of a kind" workaround
    /// that returned a 500 in this case.
    #[test]
    fn end_to_end_per_row_delete_only_targets_requested_id() {
        let dir = tempfile::tempdir().unwrap();
        let db_path = dir.path().join("delete-by-id.redb");

        let registry = Registry::open_at(&db_path).expect("open registry");
        let gc_tx = spawn_registry_worker_with(registry).expect("worker");

        // Three siblings of the same kind in a tempdir.
        let paths: Vec<String> = ["e1", "e2", "e3"]
            .iter()
            .map(|name| {
                let p = dir.path().join(name);
                std::fs::create_dir_all(&p).unwrap();
                p.to_string_lossy().to_string()
            })
            .collect();
        for p in &paths {
            let (rx_t, rx) = mpsc::sync_channel::<GcReply>(1);
            gc_tx
                .send(RegistryMsg::Op(GcRequestMsg {
                    op: GcOp::Insert {
                        kind: "cache".to_string(),
                        path: p.clone(),
                        repo_root: None,
                        branch: None,
                        agent_id: None,
                        created_unix: Some(1000),
                    },
                    reply_tx: rx_t,
                }))
                .unwrap();
            let _ = rx.recv_timeout(Duration::from_secs(2)).unwrap();
        }

        let port = spawn_dashboard(
            dir.path().to_path_buf(),
            Some(gc_tx.clone()),
            9999,
            100,
            empty_live_provider(),
            None,
        )
        .expect("dashboard spawned");

        // Fetch /state.json to get the assigned ids.
        let state_body = fetch_state_json(port).expect("fetch state");
        let state: DashboardState = serde_json::from_str(&state_body).expect("parse");
        let middle = state
            .gc
            .iter()
            .find(|r| r.path == paths[1])
            .expect("middle row");

        // POST /gc/purge {"id": <middle.id>}
        let body = fetch_path(
            port,
            "POST",
            "/gc/purge",
            Some(format!(r#"{{"id":{}}}"#, middle.id)),
        )
        .expect("delete");
        let resp: PurgeResponse = serde_json::from_str(&body).expect("parse");
        // Per-row Delete uses the synchronous `DeleteById` path —
        // response shape stays `removed`, not `dispatched`.
        assert_eq!(resp.removed, Some(1));
        assert_eq!(resp.dispatched, None);
        assert_eq!(resp.skipped, 0);

        // The two siblings must survive.
        let after = fetch_state_json(port).expect("re-fetch state");
        let after_state: DashboardState = serde_json::from_str(&after).expect("parse");
        let surviving: Vec<&str> = after_state.gc.iter().map(|r| r.path.as_str()).collect();
        assert_eq!(after_state.gc.len(), 2);
        assert!(surviving.contains(&paths[0].as_str()));
        assert!(surviving.contains(&paths[2].as_str()));
        assert!(!surviving.contains(&paths[1].as_str()));

        // On-disk deletion happened for the targeted row only.
        assert!(!std::path::Path::new(&paths[1]).exists());
        assert!(std::path::Path::new(&paths[0]).exists());
        assert!(std::path::Path::new(&paths[2]).exists());
    }

    /// Tiny HTTP/1.0 client for tests. Connect, send a request, read the
    /// body. Avoids pulling in a real HTTP client dep just for tests.
    fn fetch_path(port: u16, method: &str, path: &str, body: Option<String>) -> io::Result<String> {
        fetch_path_with_status(port, method, path, body).map(|(_, body)| body)
    }

    /// Same as `fetch_path` but also returns the HTTP status code from the
    /// status line — required by the issue #261 stub-route tests that
    /// assert 503 / 501 / 200 outcomes.
    fn fetch_path_with_status(
        port: u16,
        method: &str,
        path: &str,
        body: Option<String>,
    ) -> io::Result<(u16, String)> {
        let mut stream = TcpStream::connect(("127.0.0.1", port))?;
        stream.set_read_timeout(Some(Duration::from_secs(5)))?;
        stream.set_write_timeout(Some(Duration::from_secs(5)))?;
        let mut req =
            format!("{method} {path} HTTP/1.0\r\nHost: localhost\r\nConnection: close\r\n",);
        if let Some(b) = &body {
            req.push_str(&format!(
                "Content-Type: application/json\r\nContent-Length: {}\r\n\r\n{}",
                b.len(),
                b
            ));
        } else {
            req.push_str("\r\n");
        }
        stream.write_all(req.as_bytes())?;
        stream.flush()?;
        let mut buf = Vec::with_capacity(4096);
        stream.read_to_end(&mut buf)?;
        let status_line_end = buf
            .windows(2)
            .position(|w| w == b"\r\n")
            .or_else(|| buf.windows(1).position(|w| w == b"\n"))
            .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "no status line"))?;
        let status_line = std::str::from_utf8(&buf[..status_line_end])
            .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e.to_string()))?;
        let status: u16 = status_line
            .split_whitespace()
            .nth(1)
            .and_then(|s| s.parse().ok())
            .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "no status code"))?;
        let body_start = find_body_start(&buf)
            .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "no header terminator"))?;
        let body = String::from_utf8(buf[body_start..].to_vec())
            .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e.to_string()))?;
        Ok((status, body))
    }

    // ---------- issue #261: memory-route stub coverage ----------

    /// With `memory: None`, every memory route must return 503 with a
    /// JSON error body. The daemon keeps serving session + GC traffic.
    #[test]
    fn memory_routes_return_503_when_subsystem_disabled() {
        let dir = tempfile::tempdir().unwrap();
        let port = spawn_dashboard(
            dir.path().to_path_buf(),
            None,
            9999,
            100,
            empty_live_provider(),
            None,
        )
        .expect("dashboard spawned");

        for (method, path) in [
            ("GET", "/memory/recent"),
            ("GET", "/memory/search?q=foo"),
            ("GET", "/memory/stats"),
        ] {
            let (status, body) = fetch_path_with_status(port, method, path, None).expect("fetch");
            assert_eq!(status, 503, "{method} {path}");
            assert!(body.contains("memory subsystem unavailable"), "body={body}");
        }
        let (status, _) =
            fetch_path_with_status(port, "POST", "/memory/save", Some("{}".to_string()))
                .expect("save");
        assert_eq!(status, 503);
        let (status, _) =
            fetch_path_with_status(port, "POST", "/memory/forget/abc", Some("{}".to_string()))
                .expect("forget");
        assert_eq!(status, 503);
    }

    /// With a live `MemoryService`, the stub bodies #263 will replace
    /// are wired through: stats returns 200 with the documented JSON
    /// shape; recent/search return 200 with an empty array; save/forget
    /// return 501 until #263 implements them.
    #[test]
    fn memory_routes_serve_stubs_when_subsystem_present() {
        // Force the embedder to `Disabled` so the test doesn't try to
        // download a fastembed model.
        let saved: Vec<(&str, Option<String>)> = [
            "CLUD_MEMORY_EMBEDDER",
            "CLUD_MEMORY_EMBEDDER_PROVIDER",
            "CLUD_MEMORY_EMBEDDER_URL",
            "CLUD_MEMORY_EMBEDDER_API_KEY",
            "CLUD_MEMORY_EMBEDDER_MODEL",
        ]
        .iter()
        .map(|k| (*k, std::env::var(*k).ok()))
        .collect();
        for (k, _) in &saved {
            unsafe {
                std::env::remove_var(k);
            }
        }
        unsafe {
            std::env::set_var("CLUD_MEMORY_EMBEDDER", "disabled");
        }

        let dir = tempfile::tempdir().unwrap();
        let svc = crate::daemon::memory_service::spawn_memory_service(dir.path())
            .expect("memory service");
        let svc = std::sync::Arc::new(svc);

        let port = spawn_dashboard(
            dir.path().to_path_buf(),
            None,
            9999,
            100,
            empty_live_provider(),
            Some(svc.clone()),
        )
        .expect("dashboard spawned");

        let (status, body) =
            fetch_path_with_status(port, "GET", "/memory/stats", None).expect("stats");
        assert_eq!(status, 200);
        let parsed: serde_json::Value = serde_json::from_str(&body).expect("json");
        assert!(parsed.get("tier_counts").is_some(), "body={body}");
        assert!(parsed.get("embedder_status").is_some(), "body={body}");

        let (status, body) =
            fetch_path_with_status(port, "GET", "/memory/recent", None).expect("recent");
        assert_eq!(status, 200);
        assert_eq!(body.trim(), "[]");

        let (status, body) =
            fetch_path_with_status(port, "GET", "/memory/search?q=foo", None).expect("search");
        assert_eq!(status, 200);
        assert_eq!(body.trim(), "[]");

        let (status, _) =
            fetch_path_with_status(port, "POST", "/memory/save", Some("{}".to_string()))
                .expect("save");
        assert_eq!(status, 501);

        let (status, _) =
            fetch_path_with_status(port, "POST", "/memory/forget/abc", Some("{}".to_string()))
                .expect("forget");
        assert_eq!(status, 501);

        svc.shutdown();

        // Restore env.
        for (k, v) in saved {
            match v {
                Some(val) => unsafe { std::env::set_var(k, val) },
                None => unsafe { std::env::remove_var(k) },
            }
        }
    }
}
