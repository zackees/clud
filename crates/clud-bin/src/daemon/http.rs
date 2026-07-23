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

use std::collections::{BTreeMap, HashMap, VecDeque};
use std::fs;
use std::io::{self, Read};
use std::net::TcpStream;
use std::path::{Path, PathBuf};
use std::sync::{mpsc, Arc, Mutex};
use std::thread;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use tiny_http::{Header, Method, Request, Response, Server};

use super::gc_service::{GcRequestMsg, RegistryMsg, WORKER_REPLY_TIMEOUT};
use super::io_helpers::read_json_file;
use super::paths::{daemon_info_path, sessions_dir};
use super::process_utils::pid_is_alive;
use super::types::{
    CtrlCProfile, DaemonInfo, GcOp, GcReply, ListRow, RepoVisit, SessionKind, SessionSnapshot,
};
use crate::ctrl_c_track::{self, CtrlCEvent};
use crate::launch_log::{self, LaunchRecord};
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

/// Test-only public entry point: spawn the dashboard HTTP listener for
/// telemetry-only scenarios (no GC backend). Integration tests under
/// `tests/telemetry_endpoint.rs` use this to wire up the server without
/// taking on the `gc_service::RegistryMsg` type that the full
/// `spawn_dashboard` signature otherwise leaks.
pub fn spawn_dashboard_telemetry_only(
    state_dir: PathBuf,
    ipc_port: u16,
    started_at_unix: i64,
    telemetry: TelemetryStore,
) -> Option<u16> {
    let live_provider: LiveSessionsProvider = std::sync::Arc::new(Vec::new);
    let tool_telemetry = ToolTelemetryStore::new();
    spawn_dashboard(
        state_dir,
        None,
        ipc_port,
        started_at_unix,
        live_provider,
        telemetry,
        tool_telemetry,
    )
}

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

/// Issue #469 (beta): per-PID cap on telemetry entries. A runaway logger
/// can't grow this past N — oldest entries get dropped first.
const TELEMETRY_PER_PID_CAP: usize = 500;
const TOOL_TELEMETRY_CAP: usize = 1000;

/// Issue #469 — one telemetry record submitted by `clud log`. Mirrors
/// `log_event::TelemetryPayload` plus the daemon-added receive timestamp.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TelemetryEntry {
    pub parent_pid: u32,
    pub time_ms: u64,
    pub received_at_ms: u64,
    pub cmd: String,
    pub cwd: String,
    pub env: BTreeMap<String, String>,
}

/// `POST /telemetry/log` body — same shape as the entry minus the
/// server-side `received_at_ms` timestamp (daemon assigns it).
#[derive(Debug, Clone, Deserialize)]
pub struct TelemetryIngest {
    pub parent_pid: u32,
    pub time_ms: u64,
    pub cmd: String,
    pub cwd: String,
    #[serde(default)]
    pub env: BTreeMap<String, String>,
}

/// Compact per-PID view returned inside `/state.json` — totals only, so
/// the polled summary stays bounded regardless of entry count. The
/// per-entry detail (with envs) lives behind `/telemetry/by-pid/<pid>`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TelemetryPidSummary {
    pub parent_pid: u32,
    pub entry_count: usize,
    pub last_at_ms: u64,
}

/// Full per-PID payload returned by `GET /telemetry/by-pid/<pid>`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TelemetryPidDetail {
    pub parent_pid: u32,
    pub entries: Vec<TelemetryEntry>,
}

/// In-memory telemetry sink shared between the HTTP listener and any
/// other daemon component that wants to read it. Lifetime = daemon
/// lifetime; restart wipes it (persistence is a follow-up).
#[derive(Debug, Default, Clone)]
pub struct TelemetryStore {
    inner: Arc<Mutex<TelemetryStoreInner>>,
}

#[derive(Debug, Default)]
struct TelemetryStoreInner {
    by_pid: HashMap<u32, VecDeque<TelemetryEntry>>,
}

impl TelemetryStore {
    pub fn new() -> Self {
        Self::default()
    }

    /// Append one entry. Trims the per-PID ring buffer to `TELEMETRY_PER_PID_CAP`
    /// (drop-oldest).
    pub fn push(&self, entry: TelemetryEntry) {
        let mut guard = self.inner.lock().expect("telemetry store poisoned");
        let dq = guard.by_pid.entry(entry.parent_pid).or_default();
        dq.push_back(entry);
        while dq.len() > TELEMETRY_PER_PID_CAP {
            dq.pop_front();
        }
    }

    /// Per-PID summary keyed by parent_pid, sorted by last activity desc.
    pub fn summary(&self) -> Vec<TelemetryPidSummary> {
        let guard = self.inner.lock().expect("telemetry store poisoned");
        let mut rows: Vec<_> = guard
            .by_pid
            .iter()
            .map(|(pid, dq)| {
                let last_at_ms = dq.back().map(|e| e.received_at_ms).unwrap_or(0);
                TelemetryPidSummary {
                    parent_pid: *pid,
                    entry_count: dq.len(),
                    last_at_ms,
                }
            })
            .collect();
        rows.sort_by(|a, b| b.last_at_ms.cmp(&a.last_at_ms));
        rows
    }

    /// Full per-PID detail or `None` if the PID has no entries.
    pub fn detail(&self, pid: u32) -> Option<TelemetryPidDetail> {
        let guard = self.inner.lock().expect("telemetry store poisoned");
        guard.by_pid.get(&pid).map(|dq| TelemetryPidDetail {
            parent_pid: pid,
            entries: dq.iter().cloned().collect(),
        })
    }
}

/// One `clud tool` invocation reported by the lightweight launcher.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolCallEntry {
    pub id: String,
    pub name: String,
    pub start_time_ms: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub end_time_ms: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub exit_code: Option<i32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stderr_tail: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ToolEventIngest {
    pub event: String,
    pub id: String,
    pub name: String,
    pub start_time_ms: u64,
    #[serde(default)]
    pub end_time_ms: Option<u64>,
    #[serde(default)]
    pub exit_code: Option<i32>,
    #[serde(default)]
    pub stderr_tail: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolAggregateBucket {
    pub label: String,
    pub start_ms: u64,
    pub end_ms: u64,
    pub total: usize,
    pub success: usize,
    pub failed: usize,
    pub running: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolTelemetryView {
    pub entries: Vec<ToolCallEntry>,
    pub aggregate: Vec<ToolAggregateBucket>,
}

#[derive(Debug, Default, Clone)]
pub struct ToolTelemetryStore {
    inner: Arc<Mutex<ToolTelemetryStoreInner>>,
}

#[derive(Debug, Default)]
struct ToolTelemetryStoreInner {
    entries: VecDeque<ToolCallEntry>,
}

#[derive(Debug, Clone)]
struct DashboardTelemetryStores {
    telemetry: TelemetryStore,
    tool_telemetry: ToolTelemetryStore,
}

impl ToolTelemetryStore {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn push_event(&self, event: ToolEventIngest) {
        let mut guard = self.inner.lock().expect("tool telemetry store poisoned");
        match event.event.as_str() {
            "start" => {
                if let Some(existing) = guard.entries.iter_mut().find(|entry| entry.id == event.id)
                {
                    existing.name = event.name;
                    existing.start_time_ms = event.start_time_ms;
                    return;
                }
                guard.entries.push_back(ToolCallEntry {
                    id: event.id,
                    name: event.name,
                    start_time_ms: event.start_time_ms,
                    end_time_ms: None,
                    exit_code: None,
                    stderr_tail: None,
                });
                while guard.entries.len() > TOOL_TELEMETRY_CAP {
                    guard.entries.pop_front();
                }
            }
            "finish" => {
                if let Some(existing) = guard.entries.iter_mut().find(|entry| entry.id == event.id)
                {
                    existing.name = event.name;
                    existing.start_time_ms = event.start_time_ms;
                    existing.end_time_ms = event.end_time_ms;
                    existing.exit_code = event.exit_code;
                    existing.stderr_tail = event.stderr_tail;
                } else {
                    guard.entries.push_back(ToolCallEntry {
                        id: event.id,
                        name: event.name,
                        start_time_ms: event.start_time_ms,
                        end_time_ms: event.end_time_ms,
                        exit_code: event.exit_code,
                        stderr_tail: event.stderr_tail,
                    });
                }
                while guard.entries.len() > TOOL_TELEMETRY_CAP {
                    guard.entries.pop_front();
                }
            }
            _ => {}
        }
    }

    pub fn view(&self) -> ToolTelemetryView {
        self.view_at(current_unix_millis())
    }

    fn view_at(&self, now_ms: u64) -> ToolTelemetryView {
        let guard = self.inner.lock().expect("tool telemetry store poisoned");
        let mut entries: Vec<_> = guard.entries.iter().cloned().collect();
        entries.sort_by(|a, b| b.start_time_ms.cmp(&a.start_time_ms));
        ToolTelemetryView {
            aggregate: tool_aggregate_at(&entries, now_ms),
            entries,
        }
    }
}

fn tool_aggregate_at(entries: &[ToolCallEntry], now_ms: u64) -> Vec<ToolAggregateBucket> {
    let mut buckets = Vec::new();
    push_tool_bucket(
        &mut buckets,
        "last 10s",
        now_ms.saturating_sub(10_000),
        now_ms,
    );
    push_tool_bucket(
        &mut buckets,
        "10-20s",
        now_ms.saturating_sub(20_000),
        now_ms.saturating_sub(10_000),
    );
    push_tool_bucket(
        &mut buckets,
        "20-30s",
        now_ms.saturating_sub(30_000),
        now_ms.saturating_sub(20_000),
    );
    for minute in 1..=10 {
        let end_ms = now_ms.saturating_sub(30_000 + ((minute - 1) * 60_000));
        let start_ms = end_ms.saturating_sub(60_000);
        push_tool_bucket(&mut buckets, &format!("{minute}m"), start_ms, end_ms);
    }

    for entry in entries {
        if entry.start_time_ms < now_ms.saturating_sub(10 * 60_000) || entry.start_time_ms > now_ms
        {
            continue;
        }
        if let Some(bucket) = buckets.iter_mut().find(|bucket| {
            entry.start_time_ms >= bucket.start_ms && entry.start_time_ms < bucket.end_ms
        }) {
            bucket.total += 1;
            match entry.exit_code {
                Some(0) => bucket.success += 1,
                Some(_) => bucket.failed += 1,
                None => bucket.running += 1,
            }
        }
    }
    buckets
}

fn push_tool_bucket(
    buckets: &mut Vec<ToolAggregateBucket>,
    label: &str,
    start_ms: u64,
    end_ms: u64,
) {
    buckets.push(ToolAggregateBucket {
        label: label.to_string(),
        start_ms,
        end_ms,
        total: 0,
        success: 0,
        failed: 0,
        running: 0,
    });
}

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
    /// Cached daemon process sample, consumed by the Processes dashboard tab.
    #[serde(default)]
    pub process_tree: serde_json::Value,
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
    pub source: String,
    pub backend: Option<String>,
    pub launch_mode: Option<String>,
    pub name: Option<String>,
    pub cwd: Option<String>,
    pub repo_root: Option<String>,
    pub command: Vec<String>,
    pub clud_argv: Vec<String>,
    pub clud_pid: Option<u32>,
    pub created_at: Option<u64>,
    pub exited_at: Option<u64>,
    pub duration_ms: Option<u64>,
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
    telemetry: TelemetryStore,
    tool_telemetry: ToolTelemetryStore,
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
                DashboardTelemetryStores {
                    telemetry,
                    tool_telemetry,
                },
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
    stores: DashboardTelemetryStores,
) {
    for request in server.incoming_requests() {
        let method = request.method().clone();
        let url = request.url().to_string();
        let path = url.split('?').next().unwrap_or(&url).to_string();
        // Telemetry detail route — `/telemetry/by-pid/<u32>`. Matched
        // first so the catch-all SPA fallback below never claims it.
        if method == Method::Get {
            if let Some(rest) = path.strip_prefix("/telemetry/by-pid/") {
                handle_telemetry_detail(request, rest, &stores.telemetry);
                continue;
            }
        }
        match (method, path.as_str()) {
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
            // Issue #471: telemetry summary lives at its own URL now
            // (was previously bundled into `/state.json#telemetry`).
            (Method::Get, "/telemetry") => {
                handle_telemetry_summary(request, &stores.telemetry);
            }
            (Method::Get, "/tools") => {
                handle_tools_summary(request, &stores.tool_telemetry);
            }
            (Method::Post, "/gc/purge") => {
                handle_purge(request, gc_tx.as_ref());
            }
            (Method::Post, "/telemetry/log") => {
                handle_telemetry_log(request, &stores.telemetry);
            }
            (Method::Post, "/tools/event") => {
                handle_tool_event(request, &stores.tool_telemetry);
            }
            // Any other GET is an SPA route — serve the dashboard so the
            // History-API router takes over (refresh + deep-links).
            (Method::Get, _) => {
                respond_html(request, 200, DASHBOARD_HTML.as_bytes());
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

/// Issue #471: per-PID summary list at its own URL. Returns the same
/// `Vec<TelemetryPidSummary>` shape that the bundled
/// `/state.json#telemetry` field used to carry — no behavior change
/// for the SPA's existing render code beyond the fetch destination.
fn handle_telemetry_summary(request: Request, telemetry: &TelemetryStore) {
    let summary = telemetry.summary();
    match serde_json::to_vec(&summary) {
        Ok(bytes) => respond_json(request, 200, &bytes),
        Err(err) => respond_json(
            request,
            500,
            json_error_bytes(&format!("serialize failed: {err}")).as_slice(),
        ),
    }
}

fn handle_telemetry_log(mut request: Request, telemetry: &TelemetryStore) {
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
    let payload: TelemetryIngest = match serde_json::from_slice(&body) {
        Ok(p) => p,
        Err(err) => {
            respond_json(
                request,
                400,
                json_error_bytes(&format!("invalid JSON: {err}")).as_slice(),
            );
            return;
        }
    };
    let received_at_ms = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0);
    telemetry.push(TelemetryEntry {
        parent_pid: payload.parent_pid,
        time_ms: payload.time_ms,
        received_at_ms,
        cmd: payload.cmd,
        cwd: payload.cwd,
        env: payload.env,
    });
    respond_json(request, 200, b"{}");
}

fn handle_tools_summary(request: Request, tool_telemetry: &ToolTelemetryStore) {
    let view = tool_telemetry.view();
    match serde_json::to_vec(&view) {
        Ok(bytes) => respond_json(request, 200, &bytes),
        Err(err) => respond_json(
            request,
            500,
            json_error_bytes(&format!("serialize failed: {err}")).as_slice(),
        ),
    }
}

fn handle_tool_event(mut request: Request, tool_telemetry: &ToolTelemetryStore) {
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
    let payload: ToolEventIngest = match serde_json::from_slice(&body) {
        Ok(p) => p,
        Err(err) => {
            respond_json(
                request,
                400,
                json_error_bytes(&format!("invalid JSON: {err}")).as_slice(),
            );
            return;
        }
    };
    tool_telemetry.push_event(payload);
    respond_json(request, 200, b"{}");
}

fn handle_telemetry_detail(request: Request, pid_str: &str, telemetry: &TelemetryStore) {
    let pid: u32 = match pid_str.parse() {
        Ok(p) => p,
        Err(_) => {
            respond_json(
                request,
                400,
                json_error_bytes(&format!("invalid pid: {pid_str}")).as_slice(),
            );
            return;
        }
    };
    let detail = telemetry.detail(pid).unwrap_or(TelemetryPidDetail {
        parent_pid: pid,
        entries: Vec::new(),
    });
    match serde_json::to_vec(&detail) {
        Ok(bytes) => respond_json(request, 200, &bytes),
        Err(err) => respond_json(
            request,
            500,
            json_error_bytes(&format!("serialize failed: {err}")).as_slice(),
        ),
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
    merge_launch_records(&mut sessions, launch_log::read_recent(state_dir));
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

    // The daemon RPC returns the sampler's cached snapshot; this HTTP worker
    // never does an expensive process-table scan of its own.
    let mut process_tree = super::client::daemon_client_proc_snapshot(state_dir, 0)
        .ok()
        .and_then(|snapshot| serde_json::to_value(snapshot).ok())
        .unwrap_or(serde_json::Value::Null);
    let cwd_by_session: HashMap<String, String> = sessions
        .iter()
        .filter_map(|session| session.cwd.clone().map(|cwd| (session.id.clone(), cwd)))
        .collect();
    if let Some(rows) = process_tree.get_mut("rows").and_then(serde_json::Value::as_array_mut) {
        for row in rows {
            let cwd = row
                .get("session_id")
                .and_then(serde_json::Value::as_str)
                .and_then(|id| cwd_by_session.get(id))
                .cloned()
                .unwrap_or_else(|| "-".to_string());
            row["cwd"] = serde_json::Value::String(cwd);
        }
    }

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
        process_tree,
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
            source: "daemon".to_string(),
            backend: snap.backend,
            launch_mode: snap.launch_mode,
            name: snap.name,
            cwd: snap.cwd,
            repo_root: snap.repo_root,
            command: snap.command,
            clud_argv: Vec::new(),
            clud_pid: None,
            created_at: snap.created_at,
            exited_at: snap.exited_at,
            duration_ms: match (snap.created_at, snap.exited_at) {
                (Some(start), Some(end)) => Some(end.saturating_sub(start)),
                _ => None,
            },
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
        if sessions
            .iter()
            .any(|session| session.live && session.clud_pid == Some(row.pid))
        {
            continue;
        }
        let id = format!("direct-{}", row.pid);
        sessions.push(SessionView {
            id,
            kind: "direct".to_string(),
            source: "registry".to_string(),
            backend: row.backend.clone(),
            launch_mode: row.launch_mode.clone(),
            // Surface the backend selection (`claude` / `codex`) under the
            // session name column so users can tell which agent each
            // direct-runner row corresponds to.
            name: row.backend.clone(),
            cwd: row.cwd,
            repo_root: None,
            command: Vec::new(),
            clud_argv: Vec::new(),
            clud_pid: Some(row.pid),
            // `started_unix` is seconds; snapshot rows use milliseconds.
            // Convert so the dashboard's age formatter renders both the
            // same way without a per-kind unit-toggle.
            created_at: Some((row.started_unix.max(0) as u64) * 1000),
            exited_at: None,
            duration_ms: None,
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

fn merge_launch_records(sessions: &mut Vec<SessionView>, records: Vec<LaunchRecord>) {
    for record in records {
        let live = record.exit_code.is_none() && pid_is_alive(record.clud_pid);
        let duration_ms = record.duration_ms();
        sessions.push(SessionView {
            id: format!("launch-{}", record.id),
            kind: record.source.clone(),
            source: record.source,
            backend: Some(record.backend.clone()),
            launch_mode: Some(record.launch_mode.clone()),
            name: Some(record.backend),
            cwd: record.cwd,
            repo_root: record.repo_root,
            command: record.command,
            clud_argv: record.clud_argv,
            clud_pid: Some(record.clud_pid),
            created_at: Some(record.launched_at_ms),
            exited_at: record.exited_at_ms,
            duration_ms,
            detachable: false,
            background: false,
            attachable: false,
            repeat_interval_secs: None,
            repeat_next_run_at: None,
            repeat_running: false,
            exit_code: record.exit_code,
            worker_port: 0,
            live,
            ctrl_c: None,
        });
    }
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

fn current_unix_millis() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests;
