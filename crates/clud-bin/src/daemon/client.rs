use std::fs::{self, OpenOptions};
use std::io::{self, BufRead, BufReader, Write};
use std::net::{Shutdown, TcpStream};
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use fs4::fs_std::FileExt;
use sysinfo::Signal;

use crate::gc::InsertInput;
use crate::trampoline;

use super::io_helpers::read_json_file;
use super::paths::{
    daemon_info_path, daemon_lock_path, session_snapshot_path, sessions_dir, spec_path, specs_dir,
};
use super::process_utils::{pid_is_alive, signal_process_tree};
use super::types::{
    CtrlCProfile, DaemonInfo, DaemonRequest, DaemonResponse, GcOp, GcReply, GcWatchRoot, ListRow,
    ProcTreeSnapshot, RepoVisit, SessionSnapshot, WorkerClientMessage,
};
use super::wire_prost::{
    daemon_wire_format_from_env, decode_daemon_response_line, encode_daemon_request_line,
    encode_worker_client_line, DaemonWireFormat, WireError,
};

/// Idempotent best-effort daemon spawn (issue #135). Always called via
/// `main.rs`; the session daemon is now an always-on background service.
///
/// 1. Fast path: read the info file, probe its PID + port; return if up.
/// 2. Slow path: acquire `<state_dir>/daemon.lock` (issue #138 bringup
///    serialization), clean stale state, re-probe under the lock, and
///    only spawn `clud __daemon --state-dir <state_dir>` detached if a
///    sibling didn't bring the daemon up while we waited. Lock releases
///    when this function returns.
///
/// Visible to `main.rs` (the `clud` binary) so it can call this during
/// early startup. `pub` rather than `pub(crate)` because the binary is
/// a separate crate within the package.
pub fn ensure_daemon(state_dir: &Path) -> io::Result<()> {
    fs::create_dir_all(state_dir)?;
    if let Some(info) = probe_existing(state_dir) {
        if daemon_version_matches(&info) {
            return Ok(());
        }
        // Issue #192: stale daemon from a prior clud version. Kill it
        // under the bringup lock so a fresh `__daemon` (with the current
        // binary's dashboard + registry-merge code) takes over.
        let _bringup_lock = acquire_bringup_lock(state_dir)?;
        if let Some(info) = probe_existing(state_dir) {
            if !daemon_version_matches(&info) {
                replace_stale_daemon(state_dir, &info)?;
            } else {
                return Ok(());
            }
        }
        return spawn_and_await_daemon(state_dir);
    }

    let _bringup_lock = acquire_bringup_lock(state_dir)?;
    cleanup_stale_state(state_dir);
    // Re-probe under the lock: a sibling may have spawned while we waited.
    if let Some(info) = probe_existing(state_dir) {
        if daemon_version_matches(&info) {
            return Ok(());
        }
        replace_stale_daemon(state_dir, &info)?;
    }
    spawn_and_await_daemon(state_dir)
}

fn spawn_and_await_daemon(state_dir: &Path) -> io::Result<()> {
    let args = vec![
        "__daemon".to_string(),
        "--state-dir".to_string(),
        state_dir.to_string_lossy().to_string(),
    ];
    trampoline::spawn_detached_self(&args)?;

    let started = Instant::now();
    let our_pid = std::process::id();
    loop {
        if let Some(info) = probe_existing(state_dir) {
            // Make sure we didn't read a stale info file from before the spawn.
            if info.pid != our_pid && daemon_version_matches(&info) {
                return Ok(());
            }
        }
        if started.elapsed() > Duration::from_secs(5) {
            return Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "timed out waiting for daemon startup",
            ));
        }
        thread::sleep(Duration::from_millis(25));
    }
}

pub(super) fn probe_existing(state_dir: &Path) -> Option<DaemonInfo> {
    let info = read_json_file::<DaemonInfo>(&daemon_info_path(state_dir)).ok()?;
    if !pid_is_alive(info.pid) {
        return None;
    }
    if TcpStream::connect(("127.0.0.1", info.port)).is_ok() {
        Some(info)
    } else {
        None
    }
}

/// Issue #192: returns true when the running daemon was built from the
/// same `CARGO_PKG_VERSION` as this binary. `None` here means the daemon
/// was started by clud <= 2.0.14 (pre-fix daemons never wrote a `version`
/// field), so treat as a mismatch — they predate the registry-merge
/// dashboard fix and should be replaced.
fn daemon_version_matches(info: &DaemonInfo) -> bool {
    info.version.as_deref() == Some(env!("CARGO_PKG_VERSION"))
}

/// Issue #192: terminate a stale daemon (and its worker tree) and delete
/// its `daemon.json` so a fresh daemon can take over. Best-effort — if
/// the kill races with the daemon's own exit, the file may already be
/// gone. Held by the caller under `acquire_bringup_lock` so only one
/// upgrade attempt runs at a time.
fn replace_stale_daemon(state_dir: &Path, info: &DaemonInfo) -> io::Result<()> {
    eprintln!(
        "[clud] restarting daemon: running {} != binary {}",
        info.version.as_deref().unwrap_or("<pre-2.0.15>"),
        env!("CARGO_PKG_VERSION"),
    );
    signal_process_tree(info.pid, Signal::Term);
    let deadline = Instant::now() + Duration::from_secs(2);
    while pid_is_alive(info.pid) && Instant::now() < deadline {
        thread::sleep(Duration::from_millis(50));
    }
    if pid_is_alive(info.pid) {
        signal_process_tree(info.pid, Signal::Kill);
        let deadline = Instant::now() + Duration::from_secs(2);
        while pid_is_alive(info.pid) && Instant::now() < deadline {
            thread::sleep(Duration::from_millis(50));
        }
    }
    // Remove the stale info file so `probe_existing` doesn't return it
    // again during the spawn-await loop.
    let _ = fs::remove_file(daemon_info_path(state_dir));
    Ok(())
}

fn acquire_bringup_lock(state_dir: &Path) -> io::Result<fs::File> {
    fs::create_dir_all(state_dir)?;
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(daemon_lock_path(state_dir))?;
    FileExt::lock_exclusive(&file)?;
    Ok(file)
}

pub(super) fn send_daemon_request(
    state_dir: &Path,
    request: &DaemonRequest,
) -> io::Result<DaemonResponse> {
    // Fast path: running-process broker v1 frame lane (Hello-skip via
    // the daemon identity sidecar). Any miss — `RUNNING_PROCESS_DISABLE=1`,
    // no sidecar, connect/wire failure — falls through to the legacy TCP
    // wire below, which remains the authoritative path.
    if let Some(response) = super::rp_broker::try_send_via_frame_lane(state_dir, request) {
        return Ok(response);
    }
    let info = read_json_file::<DaemonInfo>(&daemon_info_path(state_dir))?;
    let mut stream = TcpStream::connect(("127.0.0.1", info.port))?;
    write_daemon_request(
        &mut stream,
        request,
        daemon_wire_format_from_env().map_err(wire_error_to_io)?,
    )?;
    let mut reader = BufReader::new(stream);
    let mut line = String::new();
    let bytes = reader.read_line(&mut line)?;
    if bytes == 0 || line.trim().is_empty() {
        return Err(io::Error::new(
            io::ErrorKind::UnexpectedEof,
            "daemon closed connection without replying",
        ));
    }
    decode_daemon_response_line(&line).map_err(wire_error_to_io)
}

pub fn daemon_client_metrics(state_dir: &Path) -> io::Result<(u32, f32)> {
    match send_daemon_request(state_dir, &DaemonRequest::Metrics)? {
        DaemonResponse::Metrics { pid, cpu_pct } => Ok((pid, cpu_pct)),
        DaemonResponse::Error { message } => Err(io::Error::other(message)),
        response => Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("unexpected daemon response: {response:?}"),
        )),
    }
}

pub(super) fn daemon_client_proc_snapshot(
    state_dir: &Path,
    include_dead_since_ms: u64,
) -> io::Result<ProcTreeSnapshot> {
    match send_daemon_request(
        state_dir,
        &DaemonRequest::ProcSnapshot {
            include_dead_since_ms,
        },
    )? {
        DaemonResponse::ProcSnapshot { snapshot } => Ok(snapshot),
        DaemonResponse::Error { message } => Err(io::Error::other(message)),
        response => Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("unexpected daemon response: {response:?}"),
        )),
    }
}

pub(super) fn request_session_termination(
    state_dir: &Path,
    session_id: &str,
) -> io::Result<SessionSnapshot> {
    match send_daemon_request(
        state_dir,
        &DaemonRequest::Terminate {
            session_id: session_id.to_string(),
        },
    )? {
        DaemonResponse::Terminated { session } => Ok(session),
        DaemonResponse::Error { message } => Err(io::Error::other(message)),
        response => Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("unexpected daemon response: {response:?}"),
        )),
    }
}

/// Fire-and-forget handoff: ask the daemon to kill these process trees
/// on a background thread so the CLI can return from a Ctrl+C teardown
/// immediately. Returns `true` if the daemon acked the handoff. On
/// failure the caller is expected to fall back to a synchronous kill
/// (best behavior: same as before this op existed).
///
/// Uses tight read/write timeouts so a wedged daemon never blocks the
/// CLI for more than ~250ms total — the entire point of this call is
/// sub-100ms latency on Ctrl+C. Errors are logged at most once via the
/// returned bool; the caller decides whether to surface them.
pub fn try_handoff_kill_to_daemon(state_dir: &Path, pids: &[u32], reason: Option<&str>) -> bool {
    if pids.is_empty() {
        return true;
    }
    let info = match read_json_file::<DaemonInfo>(&daemon_info_path(state_dir)) {
        Ok(info) => info,
        Err(_) => return false,
    };
    let mut stream = match TcpStream::connect_timeout(
        &std::net::SocketAddr::from(([127, 0, 0, 1], info.port)),
        Duration::from_millis(150),
    ) {
        Ok(s) => s,
        Err(_) => return false,
    };
    let _ = stream.set_read_timeout(Some(Duration::from_millis(150)));
    let _ = stream.set_write_timeout(Some(Duration::from_millis(150)));
    let request = DaemonRequest::AdoptKill {
        pids: pids.to_vec(),
        reason: reason.map(|s| s.to_string()),
    };
    let Ok(format) = daemon_wire_format_from_env() else {
        return false;
    };
    if write_daemon_request(&mut stream, &request, format).is_err() {
        return false;
    }
    // We could parse the ack here, but the wire contract guarantees the
    // daemon spawns its worker before replying; receiving any bytes back
    // means our PIDs are queued.
    let mut reader = BufReader::new(stream);
    let mut line = String::new();
    matches!(reader.read_line(&mut line), Ok(n) if n > 0)
}

/// Fire-and-forget: ask the daemon to sweep dead-originator CLUD orphans.
/// Called from the foreground clud's exit hook. Tight 150ms read/write
/// timeouts (mirrors `try_handoff_kill_to_daemon`) so a wedged daemon
/// can't drag out CLI shutdown. Returns `true` when the ack arrives.
///
/// Failure modes (daemon down, version skew, network hiccup) all degrade
/// silently — the daemon will still catch any dead-originator orphans on
/// its next periodic sweep (`gc_service`-adjacent path), and the next
/// `clud slay` invocation does the synchronous version.
pub fn try_request_orphan_reap(state_dir: &Path) -> bool {
    let info = match read_json_file::<DaemonInfo>(&daemon_info_path(state_dir)) {
        Ok(info) => info,
        Err(_) => return false,
    };
    let mut stream = match TcpStream::connect_timeout(
        &std::net::SocketAddr::from(([127, 0, 0, 1], info.port)),
        Duration::from_millis(150),
    ) {
        Ok(s) => s,
        Err(_) => return false,
    };
    let _ = stream.set_read_timeout(Some(Duration::from_millis(150)));
    let _ = stream.set_write_timeout(Some(Duration::from_millis(150)));
    let Ok(format) = daemon_wire_format_from_env() else {
        return false;
    };
    if write_daemon_request(&mut stream, &DaemonRequest::ReapOrphans, format).is_err() {
        return false;
    }
    let mut reader = BufReader::new(stream);
    let mut line = String::new();
    matches!(reader.read_line(&mut line), Ok(n) if n > 0)
}

pub(super) fn request_session_interrupt(
    state_dir: &Path,
    session_id: &str,
    profile: CtrlCProfile,
) -> io::Result<SessionSnapshot> {
    match send_daemon_request(
        state_dir,
        &DaemonRequest::Interrupt {
            session_id: session_id.to_string(),
            profile,
        },
    )? {
        DaemonResponse::Interrupted { session } => Ok(session),
        DaemonResponse::Error { message } => Err(io::Error::other(message)),
        response => Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("unexpected daemon response: {response:?}"),
        )),
    }
}

/// Ask the daemon to terminate and wait for its pid to exit. Returns the
/// daemon pid that was stopped. If the running daemon predates the shutdown
/// IPC and drops the connection on the unknown request, fall back to killing
/// the recorded pid tree directly; that is the version-skew state this
/// recovery path is meant to repair.
pub(super) fn request_daemon_shutdown(state_dir: &Path) -> io::Result<u32> {
    let info = read_json_file::<DaemonInfo>(&daemon_info_path(state_dir))?;
    let recorded_pid = info.pid;
    if !pid_is_alive(recorded_pid) {
        let _ = fs::remove_file(daemon_info_path(state_dir));
        return Err(io::Error::new(
            io::ErrorKind::NotFound,
            format!("daemon pid {recorded_pid} is not running"),
        ));
    }

    let pid = match send_daemon_request(state_dir, &DaemonRequest::Shutdown) {
        Ok(DaemonResponse::ShutdownAck { pid }) => pid,
        Ok(DaemonResponse::Error { message }) => return Err(io::Error::other(message)),
        Ok(response) => {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!("unexpected daemon response: {response:?}"),
            ));
        }
        Err(err) if is_old_daemon_signature(&err) => {
            eprintln!(
                "[clud] daemon pid {recorded_pid} does not support shutdown IPC; terminating it directly"
            );
            signal_process_tree(recorded_pid, Signal::Term);
            thread::sleep(Duration::from_millis(150));
            if pid_is_alive(recorded_pid) {
                signal_process_tree(recorded_pid, Signal::Kill);
            }
            recorded_pid
        }
        Err(err) => return Err(err),
    };

    let deadline = Instant::now() + Duration::from_secs(10);
    while pid_is_alive(pid) {
        if Instant::now() >= deadline {
            return Err(io::Error::new(
                io::ErrorKind::TimedOut,
                format!("daemon pid {pid} did not exit within 10s after shutdown"),
            ));
        }
        thread::sleep(Duration::from_millis(50));
    }

    let _ = fs::remove_file(daemon_info_path(state_dir));
    Ok(pid)
}

fn is_old_daemon_signature(err: &io::Error) -> bool {
    matches!(
        err.kind(),
        io::ErrorKind::UnexpectedEof
            | io::ErrorKind::ConnectionReset
            | io::ErrorKind::ConnectionAborted
    )
}

fn write_daemon_request(
    stream: &mut TcpStream,
    request: &DaemonRequest,
    format: DaemonWireFormat,
) -> io::Result<()> {
    let bytes = encode_daemon_request_line(request, format).map_err(wire_error_to_io)?;
    stream.write_all(&bytes)?;
    stream.flush()
}

fn wire_error_to_io(err: WireError) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, err)
}

pub(super) fn send_worker_message(
    writer: &Arc<Mutex<TcpStream>>,
    message: &WorkerClientMessage,
    format: DaemonWireFormat,
) -> io::Result<()> {
    let mut guard = writer.lock().expect("writer mutex poisoned");
    write_worker_message(&mut guard, message, format)
}

pub(super) fn write_worker_message(
    stream: &mut TcpStream,
    message: &WorkerClientMessage,
    format: DaemonWireFormat,
) -> io::Result<()> {
    let bytes = encode_worker_client_line(message, format).map_err(wire_error_to_io)?;
    stream.write_all(&bytes)?;
    stream.flush()
}

pub(super) fn shutdown_worker_connection(writer: &Arc<Mutex<TcpStream>>) -> io::Result<()> {
    let guard = writer.lock().expect("writer mutex poisoned");
    guard.shutdown(Shutdown::Both)
}

pub(super) fn cleanup_stale_state(state_dir: &Path) {
    // Clean stale session files: mark sessions whose worker is dead.
    if let Ok(entries) = fs::read_dir(sessions_dir(state_dir)) {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().and_then(|ext| ext.to_str()) != Some("json") {
                continue;
            }
            let Ok(mut session) = read_json_file::<SessionSnapshot>(&path) else {
                continue;
            };
            if session.exit_code.is_some() {
                continue;
            }
            if !pid_is_alive(session.worker_pid) {
                session.exit_code = Some(137);
                session.background = false;
                let _ = super::io_helpers::write_json_file(&path, &session);
            }
        }
    }

    // Clean dangling spec files: specs with no corresponding session snapshot
    // that are older than 10 seconds (grace period for worker startup).
    if let Ok(entries) = fs::read_dir(specs_dir(state_dir)) {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().and_then(|ext| ext.to_str()) != Some("json") {
                continue;
            }
            let session_id = path
                .file_stem()
                .and_then(|name| name.to_str())
                .unwrap_or("");
            let snapshot_path = session_snapshot_path(state_dir, session_id);
            if snapshot_path.exists() {
                continue;
            }
            // Only remove if the spec is old enough (worker may still be starting).
            let is_stale = path
                .metadata()
                .and_then(|m| m.modified())
                .map(|modified| modified.elapsed().unwrap_or_default() > Duration::from_secs(10))
                .unwrap_or(true);
            if is_stale {
                let _ = fs::remove_file(&path);
            }
        }
    }

    // Clean stale daemon.json if it refers to a dead process.
    let daemon_path = daemon_info_path(state_dir);
    if let Ok(info) = read_json_file::<DaemonInfo>(&daemon_path) {
        if !pid_is_alive(info.pid) {
            let _ = fs::remove_file(&daemon_path);
        }
    }
}

#[allow(dead_code)]
pub(super) fn remove_spec_file(state_dir: &Path, session_id: &str) {
    let _ = fs::remove_file(spec_path(state_dir, session_id));
}

// ---------- GC IPC client wrappers (issue #135) ----------
//
// Thin convenience layer around `send_daemon_request` for the GC ops the
// session daemon now serves (replacing the standalone `gc_daemon`
// process). Auto-spawn the daemon on first use so the CLI works the
// same way it did against gc_daemon: `clud gc list` from a cold start
// brings the daemon up.

fn send_gc(state_dir: &Path, op: GcOp) -> io::Result<GcReply> {
    ensure_daemon(state_dir)?;
    match send_daemon_request(state_dir, &DaemonRequest::Gc { payload: op })? {
        DaemonResponse::Gc { reply } => Ok(reply),
        DaemonResponse::Error { message } => Err(io::Error::other(message)),
        other => Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("unexpected daemon response: {other:?}"),
        )),
    }
}

/// `gc.list` — fetch every tracked row.
pub fn gc_client_list(state_dir: &Path, kind: Option<&str>) -> io::Result<Vec<ListRow>> {
    match send_gc(
        state_dir,
        GcOp::List {
            kind: kind.map(String::from),
        },
    )? {
        GcReply::ListOk { rows } => Ok(rows),
        GcReply::Error { message } => Err(io::Error::other(message)),
        other => Err(io::Error::other(format!("unexpected gc reply: {other:?}"))),
    }
}

/// Outcome of a `gc.purge` IPC call. Bulk non-dry-run purges fan out
/// to the daemon's parallel purge pool and return as
/// `Started { dispatched, skipped }`; dry-run and the per-row
/// `DeleteById` paths complete synchronously and return
/// `Completed { removed, skipped }` (#268).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GcPurgeOutcome {
    Completed { removed: usize, skipped: usize },
    Started { dispatched: usize, skipped: usize },
}

/// `gc.purge` — purge entries. `duration = None` -> purge all non-live-locked.
pub fn gc_client_purge(
    state_dir: &Path,
    duration: Option<&str>,
    kind: Option<&str>,
    dry_run: bool,
) -> io::Result<GcPurgeOutcome> {
    match send_gc(
        state_dir,
        GcOp::Purge {
            duration: duration.map(String::from),
            kind: kind.map(String::from),
            dry_run,
        },
    )? {
        GcReply::PurgeOk { removed, skipped } => Ok(GcPurgeOutcome::Completed { removed, skipped }),
        GcReply::PurgeStarted {
            dispatched,
            skipped,
        } => Ok(GcPurgeOutcome::Started {
            dispatched,
            skipped,
        }),
        GcReply::Error { message } => Err(io::Error::other(message)),
        other => Err(io::Error::other(format!("unexpected gc reply: {other:?}"))),
    }
}

/// `gc.reconcile` — walk the given repo's `.claude/worktrees/` and insert
/// new agent-* subdirs. Returns the number of newly-inserted rows.
pub fn gc_client_reconcile(state_dir: &Path, repo_root: &Path) -> io::Result<usize> {
    match send_gc(
        state_dir,
        GcOp::Reconcile {
            repo_root: repo_root.to_string_lossy().to_string(),
        },
    )? {
        GcReply::ReconcileOk { inserted } => Ok(inserted),
        GcReply::Error { message } => Err(io::Error::other(message)),
        other => Err(io::Error::other(format!("unexpected gc reply: {other:?}"))),
    }
}

/// `gc.insert` — insert a single row if not already present.
pub fn gc_client_insert(state_dir: &Path, input: &InsertInput) -> io::Result<()> {
    match send_gc(
        state_dir,
        GcOp::Insert {
            kind: input.kind.clone(),
            path: input.path.clone(),
            repo_root: input.repo_root.clone(),
            branch: input.branch.clone(),
            agent_id: input.agent_id.clone(),
            created_unix: Some(input.now_unix),
        },
    )? {
        GcReply::InsertOk { .. } => Ok(()),
        GcReply::Error { message } => Err(io::Error::other(message)),
        other => Err(io::Error::other(format!("unexpected gc reply: {other:?}"))),
    }
}

/// Register daemon-owned GC discovery roots without delaying foreground
/// startup. An old daemon closes the stream on the unknown operation; all
/// transport/version-skew failures intentionally degrade to no discovery for
/// this client rather than breaking the backend launch (#545/#546).
pub fn try_register_gc_watch(state_dir: &Path, roots: &[GcWatchRoot]) -> bool {
    if roots.is_empty() {
        return true;
    }
    let info = match read_json_file::<DaemonInfo>(&daemon_info_path(state_dir)) {
        Ok(info) => info,
        Err(_) => return false,
    };
    let Ok(format) = daemon_wire_format_from_env() else {
        return false;
    };
    for root in roots {
        let mut stream = match TcpStream::connect_timeout(
            &std::net::SocketAddr::from(([127, 0, 0, 1], info.port)),
            Duration::from_millis(150),
        ) {
            Ok(stream) => stream,
            Err(_) => return false,
        };
        let _ = stream.set_read_timeout(Some(Duration::from_millis(150)));
        let _ = stream.set_write_timeout(Some(Duration::from_millis(150)));
        let request = DaemonRequest::Gc {
            payload: GcOp::Watch {
                kind: root.kind.clone(),
                watch_dir: root.watch_dir.clone(),
                repo_root: root.repo_root.clone(),
            },
        };
        if write_daemon_request(&mut stream, &request, format).is_err() {
            return false;
        }
        let mut reader = BufReader::new(stream);
        let mut line = String::new();
        if !matches!(reader.read_line(&mut line), Ok(n) if n > 0) {
            return false;
        }
    }
    true
}

/// Issue #183: upsert a `repo_visits` row. Called by `clud` startup
/// when CWD is inside a git repo. Errors are swallowed by the caller —
/// failing to record a visit must never block a launch.
pub fn gc_client_record_repo_visit(
    state_dir: &Path,
    repo_root: &Path,
    cwd: &Path,
) -> io::Result<()> {
    match send_gc(
        state_dir,
        GcOp::RecordRepoVisit {
            repo_root: repo_root.to_string_lossy().to_string(),
            cwd: cwd.to_string_lossy().to_string(),
            now_unix: None,
        },
    )? {
        GcReply::RepoVisitOk => Ok(()),
        GcReply::Error { message } => Err(io::Error::other(message)),
        other => Err(io::Error::other(format!("unexpected gc reply: {other:?}"))),
    }
}

/// Issue #183: enumerate the `repo_visits` table for the dashboard /
/// `clud ui --json` payload.
pub fn gc_client_list_repo_visits(state_dir: &Path) -> io::Result<Vec<RepoVisit>> {
    match send_gc(state_dir, GcOp::ListRepoVisits)? {
        GcReply::RepoVisitsOk { rows } => Ok(rows),
        GcReply::Error { message } => Err(io::Error::other(message)),
        other => Err(io::Error::other(format!("unexpected gc reply: {other:?}"))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::TcpListener;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::mpsc;

    /// Issue #192: a daemon whose `daemon.json` reports the same version
    /// as the spawning binary must NOT be restarted. This is the steady-
    /// state case for every `ensure_daemon` call after the first launch.
    #[test]
    fn daemon_version_matches_current_binary() {
        let info = DaemonInfo {
            pid: 1,
            port: 0,
            dashboard_port: None,
            version: Some(env!("CARGO_PKG_VERSION").to_string()),
        };
        assert!(daemon_version_matches(&info));
    }

    /// A daemon whose `daemon.json` reports a different version is stale
    /// (likely a leftover from an in-place upgrade). `ensure_daemon` must
    /// see this as a mismatch so the upgrade path replaces it.
    #[test]
    fn daemon_version_mismatch_when_versions_differ() {
        let info = DaemonInfo {
            pid: 1,
            port: 0,
            dashboard_port: None,
            version: Some("0.0.0-not-the-current".to_string()),
        };
        assert!(!daemon_version_matches(&info));
    }

    /// `daemon.json` files written by clud <= 2.0.14 omit the `version`
    /// field entirely. Treat them as stale so they're swept away on the
    /// next launch — those daemons predate the registry-merge dashboard
    /// fix (#190) and would keep reporting zero sessions.
    #[test]
    fn daemon_version_mismatch_when_field_absent() {
        let info = DaemonInfo {
            pid: 1,
            port: 0,
            dashboard_port: None,
            version: None,
        };
        assert!(!daemon_version_matches(&info));
    }

    fn write_daemon_info(state_dir: &Path, pid: u32, port: u16) {
        fs::create_dir_all(state_dir).unwrap();
        let info = DaemonInfo {
            pid,
            port,
            dashboard_port: None,
            version: Some(env!("CARGO_PKG_VERSION").to_string()),
        };
        super::super::io_helpers::write_json_file(&daemon_info_path(state_dir), &info).unwrap();
    }

    fn write_unfinished_session(state_dir: &Path, id: &str) {
        fs::create_dir_all(sessions_dir(state_dir)).unwrap();
        let session = SessionSnapshot {
            id: id.to_string(),
            kind: super::super::types::SessionKind::Subprocess,
            backend: Some("codex".to_string()),
            launch_mode: Some("subprocess".to_string()),
            repo_root: None,
            command: vec!["codex".to_string()],
            cwd: None,
            name: None,
            created_at: Some(1),
            detachable: false,
            background: false,
            attachable: false,
            repeat_interval_secs: None,
            repeat_next_run_at: None,
            repeat_running: false,
            daemon_pid: 1,
            worker_pid: u32::MAX,
            worker_port: 0,
            root_pid: None,
            exit_code: None,
            exited_at: None,
            ctrl_c: None,
        };
        super::super::io_helpers::write_json_file(&session_snapshot_path(state_dir, id), &session)
            .unwrap();
    }

    fn spawn_silent_peer() -> (u16, Arc<AtomicBool>) {
        let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let port = listener.local_addr().unwrap().port();
        let saw_request = Arc::new(AtomicBool::new(false));
        let saw_request_thread = Arc::clone(&saw_request);

        thread::spawn(move || {
            if let Ok((stream, _)) = listener.accept() {
                let mut reader = BufReader::new(stream);
                let mut line = String::new();
                let _ = reader.read_line(&mut line);
                if !line.is_empty() {
                    saw_request_thread.store(true, Ordering::SeqCst);
                }
            }
        });

        (port, saw_request)
    }

    #[test]
    fn try_register_gc_watch_is_silent_when_daemon_is_unreachable() {
        let state = tempfile::tempdir().unwrap();
        let roots = [GcWatchRoot {
            kind: "worktree".to_string(),
            watch_dir: state.path().join("worktrees").to_string_lossy().to_string(),
            repo_root: None,
        }];
        assert!(!try_register_gc_watch(state.path(), &roots));
    }

    fn spawn_shutdown_ack_peer() -> (u16, mpsc::Receiver<String>) {
        let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let port = listener.local_addr().unwrap().port();
        let (line_tx, line_rx) = mpsc::channel();

        thread::spawn(move || {
            if let Ok((mut stream, _)) = listener.accept() {
                let mut reader = BufReader::new(stream.try_clone().unwrap());
                let mut line = String::new();
                let _ = reader.read_line(&mut line);
                let _ = line_tx.send(line.clone());
                let (_, format) =
                    super::super::wire_prost::decode_daemon_request_line(&line).unwrap();
                let response = DaemonResponse::ShutdownAck { pid: 4242 };
                let bytes =
                    super::super::wire_prost::encode_daemon_response_line(&response, format)
                        .unwrap();
                stream.write_all(&bytes).unwrap();
                stream.flush().unwrap();
            }
        });

        (port, line_rx)
    }

    #[test]
    fn send_daemon_request_translates_silent_peer_to_unexpected_eof() {
        let tmp = tempfile::tempdir().unwrap();
        let (port, saw_request) = spawn_silent_peer();
        write_daemon_info(tmp.path(), std::process::id(), port);

        let err = send_daemon_request(tmp.path(), &DaemonRequest::Shutdown)
            .expect_err("silent peer must not produce a daemon response");
        assert_eq!(err.kind(), io::ErrorKind::UnexpectedEof);
        assert!(
            !err.to_string().contains("EOF while parsing a value"),
            "must not surface the raw serde_json EOF message: {err}"
        );

        for _ in 0..20 {
            if saw_request.load(Ordering::SeqCst) {
                break;
            }
            thread::sleep(Duration::from_millis(25));
        }
        assert!(
            saw_request.load(Ordering::SeqCst),
            "stub peer should have observed the request before closing"
        );
    }

    #[test]
    fn ensure_daemon_fast_path_skips_stale_state_cleanup() {
        let tmp = tempfile::tempdir().unwrap();
        let (port, _saw_request) = spawn_silent_peer();
        write_daemon_info(tmp.path(), std::process::id(), port);
        write_unfinished_session(tmp.path(), "stale-session");

        ensure_daemon(tmp.path()).expect("reachable daemon should satisfy fast path");

        let session: SessionSnapshot =
            read_json_file(&session_snapshot_path(tmp.path(), "stale-session")).unwrap();
        assert_eq!(
            session.exit_code, None,
            "steady-state ensure_daemon must not scan and mutate session files"
        );
    }

    #[test]
    fn send_daemon_request_defaults_to_prost_wire() {
        let _guard = EnvGuard::unset(super::super::wire_prost::ENV_DAEMON_WIRE);
        let tmp = tempfile::tempdir().unwrap();
        let (port, line_rx) = spawn_shutdown_ack_peer();
        write_daemon_info(tmp.path(), std::process::id(), port);

        let response = send_daemon_request(tmp.path(), &DaemonRequest::Shutdown).unwrap();
        assert!(matches!(
            response,
            DaemonResponse::ShutdownAck { pid: 4242 }
        ));
        let line = line_rx.recv_timeout(Duration::from_secs(1)).unwrap();
        assert!(line.starts_with("CLUD-FRAME/1 434c5544 "));
    }

    #[test]
    fn send_daemon_request_uses_legacy_json_wire_when_requested() {
        let _guard = EnvGuard::set(super::super::wire_prost::ENV_DAEMON_WIRE, "json");
        let tmp = tempfile::tempdir().unwrap();
        let (port, line_rx) = spawn_shutdown_ack_peer();
        write_daemon_info(tmp.path(), std::process::id(), port);

        let response = send_daemon_request(tmp.path(), &DaemonRequest::Shutdown).unwrap();
        assert!(matches!(
            response,
            DaemonResponse::ShutdownAck { pid: 4242 }
        ));
        let line = line_rx.recv_timeout(Duration::from_secs(1)).unwrap();
        assert!(line.starts_with(r#"{"op":"shutdown"}"#));
    }

    #[test]
    fn is_old_daemon_signature_recognizes_connection_drop_variants() {
        assert!(is_old_daemon_signature(&io::Error::new(
            io::ErrorKind::UnexpectedEof,
            "x"
        )));
        assert!(is_old_daemon_signature(&io::Error::new(
            io::ErrorKind::ConnectionReset,
            "x"
        )));
        assert!(is_old_daemon_signature(&io::Error::new(
            io::ErrorKind::ConnectionAborted,
            "x"
        )));
        assert!(!is_old_daemon_signature(&io::Error::new(
            io::ErrorKind::NotFound,
            "x"
        )));
        assert!(!is_old_daemon_signature(&io::Error::new(
            io::ErrorKind::TimedOut,
            "x"
        )));
    }

    #[test]
    fn request_daemon_shutdown_treats_dead_pid_as_not_found() {
        let tmp = tempfile::tempdir().unwrap();
        write_daemon_info(tmp.path(), u32::MAX, 9);

        let err = request_daemon_shutdown(tmp.path())
            .expect_err("dead daemon pid should be treated as absent");
        assert_eq!(err.kind(), io::ErrorKind::NotFound);
        assert!(
            !daemon_info_path(tmp.path()).exists(),
            "stale daemon.json should be removed"
        );
    }

    struct EnvGuard {
        key: &'static str,
        prior: Option<String>,
        _lock: std::sync::MutexGuard<'static, ()>,
    }

    impl EnvGuard {
        fn lock() -> std::sync::MutexGuard<'static, ()> {
            static LOCK: std::sync::OnceLock<std::sync::Mutex<()>> = std::sync::OnceLock::new();
            LOCK.get_or_init(|| std::sync::Mutex::new(()))
                .lock()
                .unwrap_or_else(|poison| poison.into_inner())
        }

        fn set(key: &'static str, value: &str) -> Self {
            let lock = Self::lock();
            let prior = std::env::var(key).ok();
            std::env::set_var(key, value);
            Self {
                key,
                prior,
                _lock: lock,
            }
        }

        fn unset(key: &'static str) -> Self {
            let lock = Self::lock();
            let prior = std::env::var(key).ok();
            std::env::remove_var(key);
            Self {
                key,
                prior,
                _lock: lock,
            }
        }
    }

    impl Drop for EnvGuard {
        fn drop(&mut self) {
            match self.prior.take() {
                Some(value) => std::env::set_var(self.key, value),
                None => std::env::remove_var(self.key),
            }
        }
    }
}
