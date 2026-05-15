//! GC daemon — owns the redb registry exclusively (issue #135 Phase 1).
//!
//! # Single-owner invariant
//!
//! `~/.clud/data.redb` is held by **exactly one process**: the GC daemon.
//! All other access — `clud gc list`, `clud gc purge`, the in-process
//! `WorktreeScanner` — goes through the IPC protocol defined in this
//! module. The daemon's accept thread parses inbound JSON, packages a
//! `GcRequest`, sends it on an `mpsc::Sender<GcRequest>`, and awaits a
//! `GcReply` on a per-request `oneshot` channel (we approximate this with
//! a `mpsc::sync_channel(1)`). A single **registry worker thread** owns
//! the `Registry` handle and is the sole reader/writer of the redb file.
//!
//! Why: redb's locking is intra-process; multi-process access is undefined
//! and risks corruption. Funneling every op through one worker thread
//! also eliminates the entire class of read/write-conflict bugs.
//!
//! # Protocol
//!
//! JSON-over-loopback-TCP, one request per connection, one response back,
//! versioned envelope: `{"v": 1, "op": "...", ...}`. See `GcRequestEnvelope`
//! and `GcReplyEnvelope`.
//!
//! # Auto-spawn
//!
//! `ensure_running()` is idempotent: it reads `~/.clud/state/gc-daemon.info`,
//! probes the PID via `OsLivenessProbe`, and if dead, spawns
//! `clud __gc-daemon --state-dir ~/.clud/state` detached via
//! `trampoline::spawn_detached_self` with `invisible_helper_creationflags()`
//! on Windows. After spawn we poll for the info file to appear and the TCP
//! port to accept connections, up to 5 seconds.

use std::io::{self, BufRead, BufReader, Write};
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Arc};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

use crate::gc::{extract_pid_from_lock_reason, reconcile_dir, InsertInput, Registry, TrackedEntry};
use crate::session_registry::{LivenessProbe, OsLivenessProbe};
use crate::trampoline;
use crate::worktrees;

/// Env var to disable the auto-spawn. Mirrors the `--no-daemon` flag.
pub const ENV_NO_DAEMON: &str = "CLUD_NO_DAEMON";

/// Env var to override the state directory (PID + port info file).
pub const ENV_GC_STATE_DIR: &str = "CLUD_GC_STATE_DIR";

const PROTOCOL_VERSION: u32 = 1;
const SPAWN_WAIT: Duration = Duration::from_secs(5);
const POLL_INTERVAL: Duration = Duration::from_millis(50);

// ---------- daemon info file ----------

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DaemonInfo {
    pub pid: u32,
    pub port: u16,
}

/// Resolve the default state directory: `~/.clud/state` (or `CLUD_GC_STATE_DIR`).
pub fn default_state_dir() -> io::Result<PathBuf> {
    if let Ok(p) = std::env::var(ENV_GC_STATE_DIR) {
        return Ok(PathBuf::from(p));
    }
    let home = dirs::home_dir()
        .ok_or_else(|| io::Error::other("no home directory; cannot resolve clud state dir"))?;
    Ok(home.join(".clud").join("state"))
}

fn info_path(state_dir: &Path) -> PathBuf {
    state_dir.join("gc-daemon.info")
}

fn read_info(state_dir: &Path) -> io::Result<DaemonInfo> {
    let bytes = std::fs::read(info_path(state_dir))?;
    serde_json::from_slice(&bytes).map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))
}

fn write_info_atomic(state_dir: &Path, info: &DaemonInfo) -> io::Result<()> {
    std::fs::create_dir_all(state_dir)?;
    let final_path = info_path(state_dir);
    let tmp_path = final_path.with_extension("info.tmp");
    let bytes = serde_json::to_vec(info)
        .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e.to_string()))?;
    std::fs::write(&tmp_path, &bytes)?;
    // Cross-platform atomic rename. On Windows, rename to an existing
    // destination fails — clear first.
    let _ = std::fs::remove_file(&final_path);
    std::fs::rename(&tmp_path, &final_path)?;
    Ok(())
}

// ---------- versioned wire protocol ----------

#[derive(Debug, Serialize, Deserialize)]
struct GcRequestEnvelope {
    #[serde(default)]
    v: u32,
    #[serde(flatten)]
    op: GcRequestOp,
}

#[allow(clippy::enum_variant_names)] // gc.* prefix is the IPC contract
#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
enum GcRequestOp {
    #[serde(rename = "gc.list")]
    GcList {
        #[serde(default)]
        kind: Option<String>,
    },
    #[serde(rename = "gc.purge")]
    GcPurge {
        /// Duration string (e.g. `"7d"`) or `null` to purge ALL non-live-locked entries.
        #[serde(default)]
        duration: Option<String>,
        #[serde(default)]
        kind: Option<String>,
        #[serde(default)]
        dry_run: bool,
    },
    #[serde(rename = "gc.reconcile")]
    GcReconcile { repo_root: String },
    #[serde(rename = "gc.insert")]
    GcInsert {
        kind: String,
        path: String,
        #[serde(default)]
        repo_root: Option<String>,
        #[serde(default)]
        branch: Option<String>,
        #[serde(default)]
        agent_id: Option<String>,
        #[serde(default)]
        created_unix: Option<i64>,
    },
}

#[derive(Debug, Serialize, Deserialize)]
struct GcReplyEnvelope {
    v: u32,
    #[serde(flatten)]
    body: GcReplyBody,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
enum GcReplyBody {
    #[serde(rename = "gc.list.ok")]
    GcListOk { rows: Vec<ListRow> },
    #[serde(rename = "gc.purge.ok")]
    GcPurgeOk { removed: usize, skipped: usize },
    #[serde(rename = "gc.reconcile.ok")]
    GcReconcileOk { inserted: usize },
    #[serde(rename = "gc.insert.ok")]
    GcInsertOk { id: i64 },
    #[serde(rename = "gc.insert.skipped")]
    GcInsertSkipped,
    #[serde(rename = "error")]
    Error { message: String },
}

/// Public row shape returned by `gc.list`. Stable JSON schema for the CLI.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ListRow {
    pub id: i64,
    pub kind: String,
    pub path: String,
    pub repo_root: Option<String>,
    pub branch: Option<String>,
    pub agent_id: Option<String>,
    pub created_unix: i64,
    pub live_locked: bool,
}

// ---------- registry worker thread ----------

/// One request passed from the accept thread to the registry worker.
struct GcRequestMsg {
    op: GcRequestOp,
    reply_tx: mpsc::SyncSender<GcReplyBody>,
}

fn spawn_registry_worker() -> io::Result<mpsc::Sender<GcRequestMsg>> {
    let registry = Registry::open_default().map_err(|e| io::Error::other(e.to_string()))?;
    let (tx, rx) = mpsc::channel::<GcRequestMsg>();
    thread::Builder::new()
        .name("clud-gc-registry-worker".to_string())
        .spawn(move || run_worker_loop(registry, rx))?;
    Ok(tx)
}

fn run_worker_loop(registry: Registry, rx: mpsc::Receiver<GcRequestMsg>) {
    while let Ok(msg) = rx.recv() {
        let reply = process_op(&registry, msg.op);
        // If the requester hung up before we replied, just drop. The
        // worker keeps running.
        let _ = msg.reply_tx.send(reply);
    }
}

fn process_op(registry: &Registry, op: GcRequestOp) -> GcReplyBody {
    match op {
        GcRequestOp::GcList { kind } => match registry.list(kind.as_deref()) {
            Ok(rows) => {
                let live_locks = collect_live_lock_paths();
                let out: Vec<ListRow> = rows
                    .into_iter()
                    .map(|r| ListRow {
                        live_locked: r.kind == "worktree" && live_locks.contains(&r.path),
                        id: r.id,
                        kind: r.kind,
                        path: r.path,
                        repo_root: r.repo_root,
                        branch: r.branch,
                        agent_id: r.agent_id,
                        created_unix: r.created_unix,
                    })
                    .collect();
                GcReplyBody::GcListOk { rows: out }
            }
            Err(e) => GcReplyBody::Error {
                message: e.to_string(),
            },
        },

        GcRequestOp::GcPurge {
            duration,
            kind,
            dry_run,
        } => {
            // Step 1: pick candidates. `None` duration -> ALL.
            let candidates_res = match &duration {
                Some(d) => match worktrees::parse_duration(d) {
                    Ok(dur) => {
                        let cutoff = now_unix().saturating_sub(dur.as_secs() as i64);
                        registry.select_older_than(cutoff, kind.as_deref())
                    }
                    Err(e) => {
                        return GcReplyBody::Error {
                            message: format!("invalid duration: {e}"),
                        };
                    }
                },
                None => registry.list(kind.as_deref()),
            };
            let candidates: Vec<TrackedEntry> = match candidates_res {
                Ok(v) => v,
                Err(e) => {
                    return GcReplyBody::Error {
                        message: e.to_string(),
                    };
                }
            };
            // Step 2: skip live-locked worktrees.
            let live_locks = collect_live_lock_paths();
            let (purgeable, skipped): (Vec<_>, Vec<_>) = candidates
                .into_iter()
                .partition(|c| !(c.kind == "worktree" && live_locks.contains(&c.path)));
            if dry_run {
                return GcReplyBody::GcPurgeOk {
                    removed: purgeable.len(),
                    skipped: skipped.len(),
                };
            }
            let mut removed = 0usize;
            for entry in &purgeable {
                if remove_entry_and_delete_row(registry, entry).is_ok() {
                    removed += 1;
                }
            }
            GcReplyBody::GcPurgeOk {
                removed,
                skipped: skipped.len(),
            }
        }

        GcRequestOp::GcReconcile { repo_root } => {
            let root = PathBuf::from(&repo_root);
            let watch_dir = root.join(".claude").join("worktrees");
            match reconcile_dir(registry, &watch_dir, Some(&root)) {
                Ok(res) => GcReplyBody::GcReconcileOk {
                    inserted: res.inserted,
                },
                Err(e) => GcReplyBody::Error {
                    message: e.to_string(),
                },
            }
        }

        GcRequestOp::GcInsert {
            kind,
            path,
            repo_root,
            branch,
            agent_id,
            created_unix,
        } => {
            let input = InsertInput {
                kind,
                path,
                repo_root,
                branch,
                agent_id,
                now_unix: created_unix.unwrap_or_else(now_unix),
            };
            match registry.insert_if_new(&input) {
                Ok(()) => GcReplyBody::GcInsertOk { id: 0 },
                Err(e) => GcReplyBody::Error {
                    message: e.to_string(),
                },
            }
        }
    }
}

fn now_unix() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

fn collect_live_lock_paths() -> std::collections::HashSet<String> {
    let mut out = std::collections::HashSet::new();
    let probe = OsLivenessProbe;
    let main_root = match worktrees::locate_main_repo_root() {
        Ok(p) => p,
        Err(_) => return out,
    };
    let raw = match worktrees::run_git(&main_root, &["worktree", "list", "--porcelain"]) {
        Ok(s) => s,
        Err(_) => return out,
    };
    let entries = worktrees::parse_worktree_porcelain(&raw);
    for e in entries {
        if !e.locked {
            continue;
        }
        let Some(reason) = e.locked_reason.as_deref() else {
            continue;
        };
        let Some(pid) = extract_pid_from_lock_reason(reason) else {
            continue;
        };
        if probe.is_alive(pid) {
            out.insert(e.path.to_string_lossy().to_string());
        }
    }
    out
}

fn remove_entry_and_delete_row(registry: &Registry, entry: &TrackedEntry) -> Result<(), String> {
    if entry.kind == "worktree" {
        let main_root = entry.repo_root.clone().unwrap_or_else(|| ".".to_string());
        let git_result = worktrees::run_git(
            Path::new(&main_root),
            &["worktree", "remove", "--force", &entry.path],
        );
        if git_result.is_err() {
            let dir = Path::new(&entry.path);
            if dir.exists() {
                std::fs::remove_dir_all(dir).map_err(|e| e.to_string())?;
            }
        }
    } else {
        let p = Path::new(&entry.path);
        if p.exists() {
            std::fs::remove_dir_all(p).map_err(|e| e.to_string())?;
        }
    }
    registry.delete(entry.id).map_err(|e| e.to_string())
}

// ---------- daemon entry point ----------

/// Run the GC daemon: bind a loopback TCP port, write the info file,
/// spin up the registry worker, and serve requests forever.
///
/// Used by `main.rs` when dispatching the hidden `__gc-daemon` subcommand.
pub fn run_daemon(state_dir: &Path) -> i32 {
    if let Err(err) = std::fs::create_dir_all(state_dir) {
        eprintln!("[clud] gc-daemon: failed to create state dir: {err}");
        return 1;
    }
    let listener = match TcpListener::bind(("127.0.0.1", 0)) {
        Ok(l) => l,
        Err(e) => {
            eprintln!("[clud] gc-daemon: bind failed: {e}");
            return 1;
        }
    };
    let port = match listener.local_addr() {
        Ok(a) => a.port(),
        Err(e) => {
            eprintln!("[clud] gc-daemon: local_addr failed: {e}");
            return 1;
        }
    };
    let worker_tx = match spawn_registry_worker() {
        Ok(tx) => tx,
        Err(e) => {
            eprintln!("[clud] gc-daemon: worker spawn failed: {e}");
            return 1;
        }
    };
    let info = DaemonInfo {
        pid: std::process::id(),
        port,
    };
    if let Err(e) = write_info_atomic(state_dir, &info) {
        eprintln!("[clud] gc-daemon: cannot persist info: {e}");
        return 1;
    }
    let shutdown = Arc::new(AtomicBool::new(false));
    for stream_res in listener.incoming() {
        if shutdown.load(Ordering::SeqCst) {
            break;
        }
        let Ok(stream) = stream_res else { continue };
        let tx = worker_tx.clone();
        thread::spawn(move || {
            let _ = handle_connection(stream, tx);
        });
    }
    0
}

fn handle_connection(
    mut stream: TcpStream,
    worker_tx: mpsc::Sender<GcRequestMsg>,
) -> io::Result<()> {
    let mut reader = BufReader::new(stream.try_clone()?);
    let mut line = String::new();
    if reader.read_line(&mut line)? == 0 {
        return Ok(());
    }
    let envelope: GcRequestEnvelope = match serde_json::from_str(line.trim()) {
        Ok(e) => e,
        Err(err) => {
            let reply = GcReplyEnvelope {
                v: PROTOCOL_VERSION,
                body: GcReplyBody::Error {
                    message: format!("invalid request: {err}"),
                },
            };
            write_reply(&mut stream, &reply)?;
            return Ok(());
        }
    };
    let (reply_tx, reply_rx) = mpsc::sync_channel::<GcReplyBody>(1);
    if worker_tx
        .send(GcRequestMsg {
            op: envelope.op,
            reply_tx,
        })
        .is_err()
    {
        let reply = GcReplyEnvelope {
            v: PROTOCOL_VERSION,
            body: GcReplyBody::Error {
                message: "registry worker not running".to_string(),
            },
        };
        write_reply(&mut stream, &reply)?;
        return Ok(());
    }
    let body = reply_rx
        .recv_timeout(Duration::from_secs(30))
        .unwrap_or(GcReplyBody::Error {
            message: "registry worker timed out".to_string(),
        });
    let reply = GcReplyEnvelope {
        v: PROTOCOL_VERSION,
        body,
    };
    write_reply(&mut stream, &reply)
}

fn write_reply(stream: &mut TcpStream, reply: &GcReplyEnvelope) -> io::Result<()> {
    let mut bytes = serde_json::to_vec(reply)
        .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e.to_string()))?;
    bytes.push(b'\n');
    stream.write_all(&bytes)?;
    stream.flush()
}

// ---------- ensure-running helper ----------

/// Handle returned from `ensure_running()`. Holds the daemon's PID and port.
#[derive(Debug, Clone)]
pub struct DaemonHandle {
    pub pid: u32,
    pub port: u16,
}

/// Idempotent best-effort daemon spawn.
///
/// 1. Honor `CLUD_NO_DAEMON=1` — return an error without spawning.
/// 2. Read `~/.clud/state/gc-daemon.info`; if its PID is alive and the
///    port accepts connections, return it.
/// 3. Otherwise spawn `clud __gc-daemon --state-dir <state_dir>` detached
///    and poll for readiness up to 5 seconds.
pub fn ensure_running() -> io::Result<DaemonHandle> {
    if std::env::var_os(ENV_NO_DAEMON)
        .map(|v| v == "1" || v.eq_ignore_ascii_case("true"))
        .unwrap_or(false)
    {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "gc daemon auto-spawn disabled by CLUD_NO_DAEMON",
        ));
    }
    let state_dir = default_state_dir()?;
    if let Some(h) = probe_existing(&state_dir) {
        return Ok(h);
    }
    // Spawn detached.
    let args = vec![
        "__gc-daemon".to_string(),
        "--state-dir".to_string(),
        state_dir.to_string_lossy().to_string(),
    ];
    trampoline::spawn_detached_self(&args)?;

    // Poll until the info file is fresh AND the TCP port responds.
    let started = Instant::now();
    let our_pid = std::process::id();
    loop {
        if let Some(h) = probe_existing(&state_dir) {
            // Make sure we didn't read a stale info file from before the spawn.
            if h.pid != our_pid {
                return Ok(h);
            }
        }
        if started.elapsed() > SPAWN_WAIT {
            return Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "timed out waiting for gc daemon startup",
            ));
        }
        thread::sleep(POLL_INTERVAL);
    }
}

fn probe_existing(state_dir: &Path) -> Option<DaemonHandle> {
    let info = read_info(state_dir).ok()?;
    let probe = OsLivenessProbe;
    if !probe.is_alive(info.pid) {
        return None;
    }
    if TcpStream::connect(("127.0.0.1", info.port)).is_ok() {
        Some(DaemonHandle {
            pid: info.pid,
            port: info.port,
        })
    } else {
        None
    }
}

// ---------- CLI-side IPC client ----------

/// Connect to the daemon and send one request, returning the reply body.
/// Auto-spawns the daemon on first failure.
fn send_request(op: GcRequestOp) -> io::Result<GcReplyBody> {
    let handle = ensure_running()?;
    let mut stream = TcpStream::connect(("127.0.0.1", handle.port))?;
    let envelope = GcRequestEnvelope {
        v: PROTOCOL_VERSION,
        op,
    };
    let mut bytes = serde_json::to_vec(&envelope)
        .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e.to_string()))?;
    bytes.push(b'\n');
    stream.write_all(&bytes)?;
    stream.flush()?;
    let mut reader = BufReader::new(stream);
    let mut line = String::new();
    reader.read_line(&mut line)?;
    let reply: GcReplyEnvelope = serde_json::from_str(line.trim())
        .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e.to_string()))?;
    Ok(reply.body)
}

/// `gc.list` — fetch every tracked row.
pub fn client_list(kind: Option<&str>) -> io::Result<Vec<ListRow>> {
    match send_request(GcRequestOp::GcList {
        kind: kind.map(String::from),
    })? {
        GcReplyBody::GcListOk { rows } => Ok(rows),
        GcReplyBody::Error { message } => Err(io::Error::other(message)),
        other => Err(io::Error::other(format!("unexpected reply: {other:?}"))),
    }
}

/// `gc.purge` — purge entries. `duration = None` -> purge all non-live-locked.
pub fn client_purge(
    duration: Option<&str>,
    kind: Option<&str>,
    dry_run: bool,
) -> io::Result<(usize, usize)> {
    match send_request(GcRequestOp::GcPurge {
        duration: duration.map(String::from),
        kind: kind.map(String::from),
        dry_run,
    })? {
        GcReplyBody::GcPurgeOk { removed, skipped } => Ok((removed, skipped)),
        GcReplyBody::Error { message } => Err(io::Error::other(message)),
        other => Err(io::Error::other(format!("unexpected reply: {other:?}"))),
    }
}

/// `gc.reconcile` — walk the given repo's `.claude/worktrees/` and insert
/// new agent-* subdirs. Returns the number of newly-inserted rows.
pub fn client_reconcile(repo_root: &Path) -> io::Result<usize> {
    match send_request(GcRequestOp::GcReconcile {
        repo_root: repo_root.to_string_lossy().to_string(),
    })? {
        GcReplyBody::GcReconcileOk { inserted } => Ok(inserted),
        GcReplyBody::Error { message } => Err(io::Error::other(message)),
        other => Err(io::Error::other(format!("unexpected reply: {other:?}"))),
    }
}

/// `gc.insert` — insert a single row if not already present.
pub fn client_insert(input: &InsertInput) -> io::Result<()> {
    match send_request(GcRequestOp::GcInsert {
        kind: input.kind.clone(),
        path: input.path.clone(),
        repo_root: input.repo_root.clone(),
        branch: input.branch.clone(),
        agent_id: input.agent_id.clone(),
        created_unix: Some(input.now_unix),
    })? {
        GcReplyBody::GcInsertOk { .. } | GcReplyBody::GcInsertSkipped => Ok(()),
        GcReplyBody::Error { message } => Err(io::Error::other(message)),
        other => Err(io::Error::other(format!("unexpected reply: {other:?}"))),
    }
}

/// Path the daemon writes its `(pid, port)` JSON to. Public for tests.
pub fn info_file_path(state_dir: &Path) -> PathBuf {
    info_path(state_dir)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::gc::ENV_DATA_DB;
    use std::sync::Mutex;

    // ENV_DATA_DB is process-global; serialize so two test threads
    // never race to open the same redb file concurrently.
    static TEST_LOCK: Mutex<()> = Mutex::new(());

    /// Spawn a daemon in-process by binding a port directly, then run the
    /// registry worker inline. Returns `(port, _join_handle)` plus a
    /// guard that holds `TEST_LOCK` for the test's lifetime.
    fn spawn_test_daemon(
        db_path: &Path,
    ) -> (
        u16,
        mpsc::Sender<GcRequestMsg>,
        std::sync::MutexGuard<'static, ()>,
    ) {
        let guard = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        // Open the registry at the requested path so tests share data via
        // the standard ENV override.
        std::env::set_var(ENV_DATA_DB, db_path);
        let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let port = listener.local_addr().unwrap().port();
        let tx = spawn_registry_worker().unwrap();
        let tx_for_thread = tx.clone();
        thread::spawn(move || {
            for stream_res in listener.incoming() {
                let Ok(stream) = stream_res else { continue };
                let tx2 = tx_for_thread.clone();
                thread::spawn(move || {
                    let _ = handle_connection(stream, tx2);
                });
            }
        });
        (port, tx, guard)
    }

    fn send_to_port(port: u16, op: GcRequestOp) -> GcReplyBody {
        let mut stream = TcpStream::connect(("127.0.0.1", port)).unwrap();
        let env = GcRequestEnvelope {
            v: PROTOCOL_VERSION,
            op,
        };
        let mut bytes = serde_json::to_vec(&env).unwrap();
        bytes.push(b'\n');
        stream.write_all(&bytes).unwrap();
        stream.flush().unwrap();
        let mut reader = BufReader::new(stream);
        let mut line = String::new();
        reader.read_line(&mut line).unwrap();
        let reply: GcReplyEnvelope = serde_json::from_str(line.trim()).unwrap();
        reply.body
    }

    #[test]
    fn round_trip_insert_then_list() {
        let dir = tempfile::tempdir().unwrap();
        let db_path = dir.path().join("test.redb");
        let (port, _tx, _g) = spawn_test_daemon(&db_path);
        // Insert a row.
        let resp = send_to_port(
            port,
            GcRequestOp::GcInsert {
                kind: "worktree".to_string(),
                path: "/tmp/test-a".to_string(),
                repo_root: Some("/tmp/repo".to_string()),
                branch: Some("main".to_string()),
                agent_id: Some("agent-abc".to_string()),
                created_unix: Some(100),
            },
        );
        assert!(matches!(
            resp,
            GcReplyBody::GcInsertOk { .. } | GcReplyBody::GcInsertSkipped
        ));
        // List should show one row.
        let resp = send_to_port(port, GcRequestOp::GcList { kind: None });
        match resp {
            GcReplyBody::GcListOk { rows } => {
                assert_eq!(rows.len(), 1);
                assert_eq!(rows[0].path, "/tmp/test-a");
                assert_eq!(rows[0].agent_id.as_deref(), Some("agent-abc"));
            }
            _ => panic!("unexpected reply"),
        }
    }

    #[test]
    fn purge_with_no_duration_removes_all_non_live() {
        let dir = tempfile::tempdir().unwrap();
        let db_path = dir.path().join("purge-all.redb");
        let (port, _tx, _g) = spawn_test_daemon(&db_path);
        // Insert two non-worktree rows so liveness probe doesn't gate them.
        send_to_port(
            port,
            GcRequestOp::GcInsert {
                kind: "cache".to_string(),
                path: "/tmp/c1".to_string(),
                repo_root: None,
                branch: None,
                agent_id: None,
                created_unix: Some(100),
            },
        );
        send_to_port(
            port,
            GcRequestOp::GcInsert {
                kind: "cache".to_string(),
                path: "/tmp/c2".to_string(),
                repo_root: None,
                branch: None,
                agent_id: None,
                created_unix: Some(100),
            },
        );
        // Purge with `duration: null` should purge both (paths don't
        // exist on disk, but remove_entry_and_delete_row tolerates that).
        let resp = send_to_port(
            port,
            GcRequestOp::GcPurge {
                duration: None,
                kind: None,
                dry_run: false,
            },
        );
        match resp {
            GcReplyBody::GcPurgeOk { removed, skipped } => {
                assert_eq!(removed, 2);
                assert_eq!(skipped, 0);
            }
            _ => panic!("unexpected reply"),
        }
        // List should be empty now.
        let resp = send_to_port(port, GcRequestOp::GcList { kind: None });
        match resp {
            GcReplyBody::GcListOk { rows } => assert!(rows.is_empty()),
            _ => panic!("unexpected reply"),
        }
    }

    #[test]
    fn purge_dry_run_does_not_modify_db() {
        let dir = tempfile::tempdir().unwrap();
        let db_path = dir.path().join("purge-dry.redb");
        let (port, _tx, _g) = spawn_test_daemon(&db_path);
        send_to_port(
            port,
            GcRequestOp::GcInsert {
                kind: "cache".to_string(),
                path: "/tmp/dry".to_string(),
                repo_root: None,
                branch: None,
                agent_id: None,
                created_unix: Some(100),
            },
        );
        let resp = send_to_port(
            port,
            GcRequestOp::GcPurge {
                duration: None,
                kind: None,
                dry_run: true,
            },
        );
        match resp {
            GcReplyBody::GcPurgeOk {
                removed,
                skipped: _,
            } => assert_eq!(removed, 1),
            _ => panic!("unexpected reply"),
        }
        // Row should still exist.
        let resp = send_to_port(port, GcRequestOp::GcList { kind: None });
        match resp {
            GcReplyBody::GcListOk { rows } => assert_eq!(rows.len(), 1),
            _ => panic!("unexpected reply"),
        }
    }

    #[test]
    fn list_filter_by_kind() {
        let dir = tempfile::tempdir().unwrap();
        let db_path = dir.path().join("filter.redb");
        let (port, _tx, _g) = spawn_test_daemon(&db_path);
        send_to_port(
            port,
            GcRequestOp::GcInsert {
                kind: "worktree".to_string(),
                path: "/tmp/wt".to_string(),
                repo_root: None,
                branch: None,
                agent_id: None,
                created_unix: Some(100),
            },
        );
        send_to_port(
            port,
            GcRequestOp::GcInsert {
                kind: "cache".to_string(),
                path: "/tmp/ca".to_string(),
                repo_root: None,
                branch: None,
                agent_id: None,
                created_unix: Some(100),
            },
        );
        let resp = send_to_port(
            port,
            GcRequestOp::GcList {
                kind: Some("worktree".to_string()),
            },
        );
        match resp {
            GcReplyBody::GcListOk { rows } => {
                assert_eq!(rows.len(), 1);
                assert_eq!(rows[0].kind, "worktree");
            }
            _ => panic!("unexpected reply"),
        }
    }

    #[test]
    fn protocol_version_in_reply() {
        let dir = tempfile::tempdir().unwrap();
        let db_path = dir.path().join("ver.redb");
        let (port, _tx, _g) = spawn_test_daemon(&db_path);
        // Send a raw request and check `v` is in the JSON.
        let mut stream = TcpStream::connect(("127.0.0.1", port)).unwrap();
        stream
            .write_all(b"{\"v\":1,\"op\":\"gc.list\",\"kind\":null}\n")
            .unwrap();
        stream.flush().unwrap();
        let mut reader = BufReader::new(stream);
        let mut line = String::new();
        reader.read_line(&mut line).unwrap();
        let parsed: serde_json::Value = serde_json::from_str(line.trim()).unwrap();
        assert_eq!(parsed.get("v").and_then(|v| v.as_u64()), Some(1));
        assert_eq!(
            parsed.get("op").and_then(|o| o.as_str()),
            Some("gc.list.ok")
        );
    }

    #[test]
    fn info_atomic_write_creates_file() {
        let dir = tempfile::tempdir().unwrap();
        let info = DaemonInfo {
            pid: 12345,
            port: 9999,
        };
        write_info_atomic(dir.path(), &info).unwrap();
        let read = read_info(dir.path()).unwrap();
        assert_eq!(read.pid, 12345);
        assert_eq!(read.port, 9999);
    }
}
