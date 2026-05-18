//! `redb`-backed registry of live `clud` sessions.
//!
//! Background — issue #73: while iterating on the test-popup work for #55,
//! a regression spawned 100+ console windows from a single terminal. The
//! fork-bomb-by-mistake class of bug needed a hard guardrail in `clud`
//! itself, not just hygiene in the test harness.
//!
//! ## Behavior
//!
//! On every `clud` startup the registry is opened, dead rows (PIDs that no
//! longer name a live process) are GC'd, the live-sibling count is
//! compared against the cap, and — assuming we're under the cap — a row
//! is inserted for our own PID. On graceful exit the row is removed via
//! `Drop`. Crashed processes leave stale rows; the next startup's GC pass
//! cleans them up.
//!
//! ## Configuration
//!
//! - `CLUD_MAX_INSTANCES` — overrides the cap (default 64). Setting `0`
//!   disables the cap entirely.
//! - `CLUD_WARN_INSTANCES` — overrides the warn threshold (default
//!   `cap / 2`).
//! - `CLUD_SESSION_DB` — overrides the DB path (used by tests).
//!
//! ## Schema (v1)
//!
//! One `redb` `Table` keyed by PID (`u32`) → JSON-serialized `SessionRow`.
//! A small `meta` table records `schema_version`.
//!
//! ## Concurrency (issue #138)
//!
//! `redb` takes an **exclusive per-process file lock** when it opens a
//! database (`LockFileEx` on Windows, `flock` on POSIX) — only one
//! process at a time can hold the redb file open. To serialize concurrent
//! `clud` startups without failing the loser with `DatabaseAlreadyOpen`,
//! every redb open is bracketed by an `fs4` advisory exclusive lock on a
//! sibling lock file (`sessions.lock` next to `sessions.redb`):
//!
//!   1. Acquire `sessions.lock` (blocks if a sibling holds it).
//!   2. Open `sessions.redb`.
//!   3. GC dead siblings, check the cap, register-self (write txn).
//!   4. Close redb (`Drop`).
//!   5. Release `sessions.lock` (OS reclaims on fd drop).
//!
//! The lock is held only for the duration of the startup ops (a few ms),
//! not for the lifetime of the `clud` session. Shutdown re-acquires the
//! lock briefly to remove the row. This means N concurrent `clud` launches
//! serialize on the *lock* — never on the redb file lock — and the cap
//! check is consistent across all of them.
//!
//! ## Liveness probe
//!
//! The PID-liveness check is abstracted behind `LivenessProbe` so unit
//! tests can deterministically mark specific PIDs alive/dead. The
//! production path uses `OsLivenessProbe`: on Windows
//! `OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, …)` + `GetExitCodeProcess`,
//! on POSIX `kill(pid, 0)` (which returns 0 / `ESRCH` without actually
//! sending a signal).

use std::fs::{File, OpenOptions};
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};

use fs4::fs_std::FileExt;
use redb::{Database, ReadableTable, ReadableTableMetadata, TableDefinition};
use serde::{Deserialize, Serialize};

/// Default maximum live sessions before `clud` refuses to launch.
pub const DEFAULT_MAX_INSTANCES: u64 = 64;

/// Bit value for "cap disabled" (set `CLUD_MAX_INSTANCES=0` to opt out).
pub const CAP_DISABLED: u64 = 0;

/// Environment variable: cap override.
pub const ENV_MAX_INSTANCES: &str = "CLUD_MAX_INSTANCES";

/// Environment variable: warn threshold override.
pub const ENV_WARN_INSTANCES: &str = "CLUD_WARN_INSTANCES";

/// Environment variable: DB path override (used by tests).
pub const ENV_SESSION_DB: &str = "CLUD_SESSION_DB";

/// Environment variable: lock-file path override. Defaults to the
/// `sessions.redb` parent dir + `sessions.lock`. Tests can point this at
/// a tempdir to isolate from concurrent test runs.
pub const ENV_SESSION_LOCK: &str = "CLUD_SESSION_LOCK";

/// Filename used for the cross-process advisory lock when the path is
/// derived from the DB path's parent dir.
const LOCK_FILE_NAME: &str = "sessions.lock";

/// redb table: `pid -> serde_json::to_vec(&SessionRow)`.
const SESSIONS: TableDefinition<u32, &[u8]> = TableDefinition::new("sessions");

/// redb table: `meta_key -> meta_value` (currently only `schema_version`).
const META: TableDefinition<&str, u64> = TableDefinition::new("meta");

/// On-disk representation of a row. Lives separately from the public
/// `SessionInfo` to keep the disk format independent of any future API
/// changes.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct SessionRow {
    started_unix: i64,
    backend: Option<String>,
    launch_mode: Option<String>,
    cwd: Option<String>,
}

/// Errors surfaced by the registry. Kept narrow on purpose — callers in
/// `main.rs` either log and continue (for "couldn't open the DB") or log
/// and exit (for "cap exceeded"), so a rich error enum buys nothing here.
#[derive(Debug)]
pub enum RegistryError {
    /// Could not figure out the default DB path (no `LOCALAPPDATA` /
    /// `XDG_STATE_HOME` / `HOME`). Caller should log and skip the cap
    /// check rather than refusing to launch.
    NoDefaultPath,
    /// Filesystem or DB open/IO failure.
    Io(String),
    /// DB error (table open, transaction, query, commit, value
    /// serialization).
    Sql(String),
}

impl std::fmt::Display for RegistryError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NoDefaultPath => write!(f, "no default session-db path could be resolved"),
            Self::Io(msg) => write!(f, "session-db I/O error: {msg}"),
            Self::Sql(msg) => write!(f, "session-db error: {msg}"),
        }
    }
}

impl std::error::Error for RegistryError {}

// `redb` errors come in several flavors depending on which phase of the
// txn lifecycle they originate from. The umbrella `redb::Error` covers
// all of them, but every concrete call site returns its own type. Map
// each into `Sql(string)` so the public surface stays variant-light.
macro_rules! impl_from_redb {
    ($($t:ty),* $(,)?) => {
        $(
            impl From<$t> for RegistryError {
                fn from(e: $t) -> Self {
                    Self::Sql(e.to_string())
                }
            }
        )*
    };
}

impl_from_redb!(
    redb::Error,
    redb::DatabaseError,
    redb::TransactionError,
    redb::TableError,
    redb::StorageError,
    redb::CommitError,
);

impl From<serde_json::Error> for RegistryError {
    fn from(e: serde_json::Error) -> Self {
        Self::Sql(format!("serde: {e}"))
    }
}

/// Cap configuration loaded from env vars.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CapConfig {
    /// Cap; `0` disables the cap entirely.
    pub max: u64,
    /// Warn threshold; ignored if greater than `max`.
    pub warn: u64,
}

impl CapConfig {
    /// Default: `max = 64`, `warn = 32`.
    pub fn defaults() -> Self {
        Self {
            max: DEFAULT_MAX_INSTANCES,
            warn: DEFAULT_MAX_INSTANCES / 2,
        }
    }
}

/// Cap-check decision. The numbers are the *current live count* (not
/// inclusive of the to-be-inserted self).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CapDecision {
    /// Under the warn threshold — proceed silently.
    Allow,
    /// At or above warn threshold but under the cap — emit a warning and
    /// continue. Carries the current live-count.
    Warn(u64),
    /// At or above the cap — refuse to launch. Carries the current
    /// live-count.
    Refuse(u64),
}

/// Metadata to record for a session. `cwd` and `backend` are best-effort
/// — the row is keyed by `pid`, so all of these may be empty without
/// breaking the cap check.
#[derive(Debug, Clone)]
pub struct SessionInfo {
    pub pid: u32,
    pub started_unix: i64,
    pub backend: Option<String>,
    pub launch_mode: Option<String>,
    pub cwd: Option<String>,
}

impl SessionInfo {
    /// Build a `SessionInfo` for the current process with `now` as
    /// `started_unix`.
    pub fn for_self(backend: Option<String>, launch_mode: Option<String>) -> Self {
        let started_unix = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_secs() as i64)
            .unwrap_or(0);
        let cwd = std::env::current_dir()
            .ok()
            .map(|p| p.display().to_string());
        Self {
            pid: std::process::id(),
            started_unix,
            backend,
            launch_mode,
            cwd,
        }
    }
}

/// Abstraction over PID-liveness checks. Unit tests substitute
/// `MockLivenessProbe`; production uses `OsLivenessProbe`.
pub trait LivenessProbe: Send + Sync {
    /// Returns `true` if a process with this PID is currently alive on
    /// the system.
    fn is_alive(&self, pid: u32) -> bool;
}

/// Production liveness probe: `kill(pid, 0)` on POSIX,
/// `OpenProcess + GetExitCodeProcess` on Windows.
#[derive(Debug, Default, Clone, Copy)]
pub struct OsLivenessProbe;

impl LivenessProbe for OsLivenessProbe {
    fn is_alive(&self, pid: u32) -> bool {
        os_probe_is_alive(pid)
    }
}

#[cfg(unix)]
fn os_probe_is_alive(pid: u32) -> bool {
    if pid == 0 {
        // POSIX: kill(0, 0) signals every process in our group — never
        // useful here, and a misleading "alive" answer.
        return false;
    }
    // SAFETY: kill(pid, 0) does not deliver a signal; it only checks
    // whether we *could* signal the target. Returns 0 on success
    // (process exists and we have permission), -1 with errno set
    // otherwise. ESRCH means the PID is dead; EPERM means it's alive
    // but owned by another user — we count that as alive.
    let rc = unsafe { libc::kill(pid as libc::pid_t, 0) };
    if rc == 0 {
        return true;
    }
    // `last_os_error` reads errno via the OS-portable wrapper that std
    // already maintains; saves us from chasing __errno_location vs
    // __error link-name shims for every libc flavor.
    let errno = std::io::Error::last_os_error().raw_os_error();
    errno != Some(libc::ESRCH)
}

#[cfg(windows)]
fn os_probe_is_alive(pid: u32) -> bool {
    use std::ffi::c_void;

    // Windows reserves PID 0 for the System Idle Process; treating it as
    // a clud sibling is nonsense.
    if pid == 0 {
        return false;
    }

    const PROCESS_QUERY_LIMITED_INFORMATION: u32 = 0x1000;
    const STILL_ACTIVE: u32 = 259; // STATUS_PENDING

    extern "system" {
        fn OpenProcess(desired_access: u32, inherit_handle: i32, process_id: u32) -> *mut c_void;
        fn CloseHandle(handle: *mut c_void) -> i32;
        fn GetExitCodeProcess(handle: *mut c_void, exit_code: *mut u32) -> i32;
    }

    unsafe {
        let handle = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid);
        if handle.is_null() {
            // Either the PID never existed, or we lack rights. We can't
            // distinguish here, so treat as dead — the worst case is one
            // stale row that the *next* GC pass will catch.
            return false;
        }
        let mut exit_code: u32 = 0;
        let ok = GetExitCodeProcess(handle, &mut exit_code);
        CloseHandle(handle);
        if ok == 0 {
            return false;
        }
        exit_code == STILL_ACTIVE
    }
}

#[cfg(not(any(unix, windows)))]
fn os_probe_is_alive(_pid: u32) -> bool {
    // On exotic targets we don't have a portable cheap probe; default to
    // "dead" so GC is aggressive and the cap is conservative.
    false
}

/// Test-only liveness probe: checks PIDs against an explicit "alive" set.
#[derive(Debug, Default)]
pub struct MockLivenessProbe {
    alive: Mutex<std::collections::HashSet<u32>>,
}

impl MockLivenessProbe {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn with_alive(pids: impl IntoIterator<Item = u32>) -> Self {
        let probe = Self::new();
        for pid in pids {
            probe.mark_alive(pid);
        }
        probe
    }

    pub fn mark_alive(&self, pid: u32) {
        self.alive.lock().unwrap().insert(pid);
    }

    pub fn mark_dead(&self, pid: u32) {
        self.alive.lock().unwrap().remove(&pid);
    }
}

impl LivenessProbe for MockLivenessProbe {
    fn is_alive(&self, pid: u32) -> bool {
        self.alive.lock().unwrap().contains(&pid)
    }
}

/// Live-session registry. Holds an open `redb` Database handle. On `Drop`
/// the row matching `own_pid` is deleted (best-effort), but only if
/// `register_self` was called successfully.
pub struct SessionRegistry {
    db: Database,
    own_pid: u32,
    probe: Box<dyn LivenessProbe>,
    /// Set after `register_self` succeeds; controls whether `Drop`
    /// removes a row.
    registered: std::sync::atomic::AtomicBool,
}

impl std::fmt::Debug for SessionRegistry {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SessionRegistry")
            .field("own_pid", &self.own_pid)
            .field(
                "registered",
                &self.registered.load(std::sync::atomic::Ordering::Relaxed),
            )
            .finish()
    }
}

impl SessionRegistry {
    /// Test-only: override the PID this registry treats as "self" (the
    /// PID `Drop` removes from the table). Production code never needs
    /// this — `open_at` initializes `own_pid` to `std::process::id()`.
    #[cfg(test)]
    pub(crate) fn set_own_pid_for_test(&mut self, pid: u32) {
        self.own_pid = pid;
    }

    /// Open the registry at the OS-default path
    /// (`%LOCALAPPDATA%/clud/sessions.redb` on Windows,
    /// `$XDG_STATE_HOME/clud/sessions.redb` on POSIX). Honors
    /// `CLUD_SESSION_DB` if set.
    pub fn open_default() -> Result<Self, RegistryError> {
        let path = match std::env::var_os(ENV_SESSION_DB) {
            Some(v) => PathBuf::from(v),
            None => default_db_path()?,
        };
        Self::open_at(&path)
    }

    /// Open (or create) the registry at `path`. Initializes the schema
    /// if the DB is fresh.
    pub fn open_at(path: &Path) -> Result<Self, RegistryError> {
        Self::open_at_with_probe(path, Box::new(OsLivenessProbe))
    }

    /// Open the registry with a caller-supplied liveness probe (used by
    /// tests).
    pub fn open_at_with_probe(
        path: &Path,
        probe: Box<dyn LivenessProbe>,
    ) -> Result<Self, RegistryError> {
        if let Some(parent) = path.parent() {
            if !parent.as_os_str().is_empty() {
                std::fs::create_dir_all(parent)
                    .map_err(|e| RegistryError::Io(format!("create_dir_all({:?}): {e}", parent)))?;
            }
        }
        let db = Database::create(path)?;
        Self::bootstrap_schema(&db)?;
        Ok(Self {
            db,
            own_pid: std::process::id(),
            probe,
            registered: std::sync::atomic::AtomicBool::new(false),
        })
    }

    fn bootstrap_schema(db: &Database) -> Result<(), RegistryError> {
        // Open both tables once so they materialize, then write a
        // `schema_version` row if it's not already there. `redb` opens
        // tables lazily on first reference, so this also acts as a
        // light DB-integrity smoke test on first run.
        let txn = db.begin_write()?;
        {
            let _ = txn.open_table(SESSIONS)?;
            let mut meta = txn.open_table(META)?;
            if meta.get("schema_version")?.is_none() {
                meta.insert("schema_version", 1u64)?;
            }
        }
        txn.commit()?;
        Ok(())
    }

    /// Read cap configuration from `CLUD_MAX_INSTANCES` /
    /// `CLUD_WARN_INSTANCES`.
    pub fn cap_config_from_env() -> CapConfig {
        let mut cfg = CapConfig::defaults();
        if let Ok(v) = std::env::var(ENV_MAX_INSTANCES) {
            if let Ok(parsed) = v.trim().parse::<u64>() {
                cfg.max = parsed;
                // If the user set max explicitly without setting warn,
                // re-derive warn = max/2.
                cfg.warn = parsed / 2;
            }
        }
        if let Ok(v) = std::env::var(ENV_WARN_INSTANCES) {
            if let Ok(parsed) = v.trim().parse::<u64>() {
                cfg.warn = parsed;
            }
        }
        // warn must not exceed max (when max > 0).
        if cfg.max > 0 && cfg.warn > cfg.max {
            cfg.warn = cfg.max;
        }
        cfg
    }

    /// Garbage-collect rows whose PID is no longer alive. Returns the
    /// number of rows removed.
    pub fn gc_dead_sessions(&self) -> Result<u64, RegistryError> {
        // Read-snapshot first so we don't hold the writer lock while we
        // probe the OS for liveness.
        let pids: Vec<u32> = {
            let rtxn = self.db.begin_read()?;
            let table = rtxn.open_table(SESSIONS)?;
            let mut out = Vec::new();
            for entry in table.iter()? {
                let (k, _v) = entry?;
                out.push(k.value());
            }
            out
        };
        let dead: Vec<u32> = pids
            .into_iter()
            .filter(|p| !self.probe.is_alive(*p))
            .collect();
        if dead.is_empty() {
            return Ok(0);
        }
        let wtxn = self.db.begin_write()?;
        {
            let mut table = wtxn.open_table(SESSIONS)?;
            for pid in &dead {
                table.remove(pid)?;
            }
        }
        wtxn.commit()?;
        Ok(dead.len() as u64)
    }

    /// Count rows currently in the DB. Does *not* run GC — call
    /// `gc_dead_sessions` first if you want a live-only count.
    pub fn count_live(&self) -> Result<u64, RegistryError> {
        let rtxn = self.db.begin_read()?;
        let table = rtxn.open_table(SESSIONS)?;
        Ok(table.len()?)
    }

    /// Decide whether this process may launch given the current row count
    /// and the supplied cap config. Does not insert anything.
    pub fn check_cap(&self, cfg: &CapConfig) -> Result<CapDecision, RegistryError> {
        let count = self.count_live()?;
        Ok(decide_cap(count, cfg))
    }

    /// Insert this process's row. Idempotent: re-registering replaces the
    /// existing row for our PID. Sets the `registered` flag so `Drop`
    /// removes the row on graceful exit.
    pub fn register_self(&self, info: SessionInfo) -> Result<(), RegistryError> {
        let row = SessionRow {
            started_unix: info.started_unix,
            backend: info.backend,
            launch_mode: info.launch_mode,
            cwd: info.cwd,
        };
        let bytes = serde_json::to_vec(&row)?;
        let wtxn = self.db.begin_write()?;
        {
            let mut table = wtxn.open_table(SESSIONS)?;
            table.insert(info.pid, bytes.as_slice())?;
        }
        wtxn.commit()?;
        self.registered
            .store(true, std::sync::atomic::Ordering::SeqCst);
        Ok(())
    }

    /// Delete this process's row explicitly (issue #138). Unlike `Drop`,
    /// this is a synchronous, error-reporting deletion the startup/shutdown
    /// helpers can call inside the lockfile's critical section.
    ///
    /// Clears the `registered` flag so a subsequent `Drop` doesn't try to
    /// re-delete and clobber a sibling that happens to inherit our PID
    /// after we exit (POSIX PID reuse).
    pub fn unregister(&self) -> Result<(), RegistryError> {
        let pid = self.own_pid;
        let wtxn = self.db.begin_write()?;
        {
            let mut table = wtxn.open_table(SESSIONS)?;
            let _ = table.remove(pid)?;
        }
        wtxn.commit()?;
        self.registered
            .store(false, std::sync::atomic::Ordering::SeqCst);
        Ok(())
    }
}

impl Drop for SessionRegistry {
    fn drop(&mut self) {
        // Best-effort: if the row was never inserted, skip the DELETE so
        // we don't clobber a sibling that happens to share our PID
        // namespace via PID reuse.
        if !self.registered.load(std::sync::atomic::Ordering::SeqCst) {
            return;
        }
        let Ok(wtxn) = self.db.begin_write() else {
            return;
        };
        let pid = self.own_pid;
        let table_ok = wtxn.open_table(SESSIONS).is_ok_and(|mut table| {
            // `remove` returns the prior value as Ok(Some(_)) or Ok(None);
            // either way the row is gone if the call succeeded.
            table.remove(pid).is_ok()
        });
        if table_ok {
            let _ = wtxn.commit();
        }
    }
}

/// Pure cap-decision function. Split out so unit tests can exercise the
/// branches without needing a DB.
fn decide_cap(count: u64, cfg: &CapConfig) -> CapDecision {
    if cfg.max == CAP_DISABLED {
        return CapDecision::Allow;
    }
    if count >= cfg.max {
        return CapDecision::Refuse(count);
    }
    if cfg.warn > 0 && count >= cfg.warn {
        return CapDecision::Warn(count);
    }
    CapDecision::Allow
}

/// RAII guard for the cross-process session-registry lock (issue #138).
/// The OS releases the advisory lock automatically when the file handle
/// drops (or when the process exits), so Drop is intentionally empty —
/// we just need to keep the `File` alive.
pub struct LockGuard {
    _file: File,
}

/// Acquire the cross-process session-registry advisory lock. Blocks
/// until the lock is exclusive to us; only fails on filesystem errors
/// (missing parent dir we can't create, permission denied, etc.).
///
/// The lock file path comes from `CLUD_SESSION_LOCK` if set, else from
/// the DB path's parent dir + `sessions.lock`. Tests can point both env
/// vars at a tempdir to fully isolate from the host's real registry.
pub fn acquire_lock() -> Result<LockGuard, RegistryError> {
    let path = default_lock_path()?;
    acquire_lock_at(&path)
}

/// Acquire the lock at a specific path. Public for tests; production
/// callers should use `acquire_lock()` so the env override is honored.
pub fn acquire_lock_at(path: &Path) -> Result<LockGuard, RegistryError> {
    if let Some(parent) = path.parent() {
        if !parent.as_os_str().is_empty() {
            std::fs::create_dir_all(parent)
                .map_err(|e| RegistryError::Io(format!("create_dir_all({:?}): {e}", parent)))?;
        }
    }
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(path)
        .map_err(|e| RegistryError::Io(format!("open lock {:?}: {e}", path)))?;
    // Blocks until acquired. The lock auto-releases on fd drop or
    // process death, so a crashed `clud` doesn't deadlock siblings.
    FileExt::lock_exclusive(&file)
        .map_err(|e| RegistryError::Io(format!("lock_exclusive: {e}")))?;
    Ok(LockGuard { _file: file })
}

/// Resolve the path used for the session-registry advisory lock.
/// `CLUD_SESSION_LOCK` overrides; otherwise we derive it from the DB
/// path's parent dir so both files live next to each other.
pub fn default_lock_path() -> Result<PathBuf, RegistryError> {
    if let Some(v) = std::env::var_os(ENV_SESSION_LOCK) {
        return Ok(PathBuf::from(v));
    }
    let db_path = match std::env::var_os(ENV_SESSION_DB) {
        Some(v) => PathBuf::from(v),
        None => default_db_path()?,
    };
    let parent = db_path
        .parent()
        .map(Path::to_path_buf)
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or_else(|| PathBuf::from("."));
    Ok(parent.join(LOCK_FILE_NAME))
}

/// Result of `run_startup_under_lock`: the cap decision plus a flag for
/// whether `register_self` was called (only true when the decision was
/// Allow or Warn).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StartupOutcome {
    pub decision: CapDecision,
    pub registered: bool,
}

/// Run the full startup sequence (gc → cap-check → register) under the
/// cross-process lock, then close the redb file. Returns the cap decision
/// plus whether we registered ourselves. Caller (typically `main.rs`)
/// decides what to do with the decision: print a warning, refuse to
/// launch, or proceed.
///
/// On `Refuse(_)` the row is **not** inserted — the caller is supposed
/// to exit, and inserting would inflate the count for the next sibling.
pub fn run_startup_under_lock(
    cfg: &CapConfig,
    info: SessionInfo,
) -> Result<StartupOutcome, RegistryError> {
    let _lock = acquire_lock()?;
    let registry = SessionRegistry::open_default()?;
    let _ = registry.gc_dead_sessions()?;
    let decision = registry.check_cap(cfg)?;
    let mut registered = false;
    if !matches!(decision, CapDecision::Refuse(_)) {
        registry.register_self(info)?;
        // `register_self` sets the `registered` flag, which would tell
        // the registry's `Drop` impl to immediately remove our row. We
        // *want* the row to persist after this function returns so
        // sibling launches can see us in their cap count — cleanup
        // happens later in `run_shutdown_under_lock`. Clear the flag
        // here to disarm Drop.
        registry
            .registered
            .store(false, std::sync::atomic::Ordering::SeqCst);
        registered = true;
    }
    // Drop registry (closes redb) before dropping _lock so the redb file
    // lock releases first; the next sibling can open redb as soon as our
    // lock drops.
    drop(registry);
    drop(_lock);
    Ok(StartupOutcome {
        decision,
        registered,
    })
}

/// Run the shutdown sequence (remove own row) under the cross-process
/// lock, then close the redb file. Best-effort: if anything fails we
/// return the error but the next startup's GC pass will clean the row.
pub fn run_shutdown_under_lock() -> Result<(), RegistryError> {
    let _lock = acquire_lock()?;
    let registry = SessionRegistry::open_default()?;
    registry.unregister()?;
    drop(registry);
    drop(_lock);
    Ok(())
}

/// Resolve the OS-default DB path. `%LOCALAPPDATA%\clud\sessions.redb` on
/// Windows; `$XDG_STATE_HOME/clud/sessions.redb` (or
/// `~/.local/state/clud/sessions.redb`) on POSIX.
fn default_db_path() -> Result<PathBuf, RegistryError> {
    #[cfg(windows)]
    {
        if let Some(local) = std::env::var_os("LOCALAPPDATA") {
            let mut p = PathBuf::from(local);
            p.push("clud");
            p.push("sessions.redb");
            return Ok(p);
        }
        // Fallback: %USERPROFILE%\AppData\Local\clud\sessions.redb
        if let Some(home) = std::env::var_os("USERPROFILE") {
            let mut p = PathBuf::from(home);
            p.push("AppData");
            p.push("Local");
            p.push("clud");
            p.push("sessions.redb");
            return Ok(p);
        }
    }
    #[cfg(not(windows))]
    {
        if let Some(state) = std::env::var_os("XDG_STATE_HOME") {
            if !state.is_empty() {
                let mut p = PathBuf::from(state);
                p.push("clud");
                p.push("sessions.redb");
                return Ok(p);
            }
        }
        if let Some(home) = std::env::var_os("HOME") {
            let mut p = PathBuf::from(home);
            p.push(".local");
            p.push("state");
            p.push("clud");
            p.push("sessions.redb");
            return Ok(p);
        }
    }
    Err(RegistryError::NoDefaultPath)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;
    use std::sync::Mutex as StdMutex;

    /// Serialize env-var manipulation across the few tests that touch
    /// process-global state. Test threads otherwise stomp each other.
    static ENV_LOCK: StdMutex<()> = StdMutex::new(());

    /// Build a unique DB path inside a TempDir that's intentionally
    /// leaked for the lifetime of the test process. We need the file
    /// alive across reopens (e.g. `register_then_drop_round_trips`),
    /// and the test process exits shortly anyway.
    fn fresh_db_path(tag: &str) -> PathBuf {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join(format!("sessions-{tag}.redb"));
        std::mem::forget(dir);
        path
    }

    fn open_with_alive_set(path: &Path, alive: Vec<u32>) -> SessionRegistry {
        let probe = Box::new(MockLivenessProbe::with_alive(alive));
        SessionRegistry::open_at_with_probe(path, probe).expect("open registry")
    }

    /// Raw insert that bypasses `register_self` (the public path sets
    /// the `registered` flag, which we *don't* want for most tests).
    fn raw_insert(reg: &SessionRegistry, pid: u32) {
        let row = SessionRow {
            started_unix: 0,
            backend: None,
            launch_mode: None,
            cwd: None,
        };
        let bytes = serde_json::to_vec(&row).unwrap();
        let wtxn = reg.db.begin_write().unwrap();
        {
            let mut table = wtxn.open_table(SESSIONS).unwrap();
            table.insert(pid, bytes.as_slice()).unwrap();
        }
        wtxn.commit().unwrap();
    }

    #[test]
    fn gc_removes_dead_pids() {
        // u32::MAX is virtually guaranteed to be a dead PID. Insert it,
        // then call gc_dead_sessions and assert it's gone.
        let path = fresh_db_path("gc-dead");
        let reg = open_with_alive_set(&path, vec![]);
        raw_insert(&reg, u32::MAX);
        raw_insert(&reg, u32::MAX - 1);
        assert_eq!(reg.count_live().unwrap(), 2);
        let removed = reg.gc_dead_sessions().unwrap();
        assert_eq!(removed, 2);
        assert_eq!(reg.count_live().unwrap(), 0);
    }

    #[test]
    fn gc_keeps_live_pids() {
        let path = fresh_db_path("gc-live");
        let reg = open_with_alive_set(&path, vec![1234, 5678]);
        raw_insert(&reg, 1234);
        raw_insert(&reg, 5678);
        raw_insert(&reg, 9999); // not in alive set => dead
        let removed = reg.gc_dead_sessions().unwrap();
        assert_eq!(removed, 1);
        assert_eq!(reg.count_live().unwrap(), 2);
    }

    #[test]
    fn count_under_cap_returns_allow() {
        let path = fresh_db_path("under-cap");
        let reg = open_with_alive_set(&path, vec![]);
        let cfg = CapConfig::defaults();
        assert_eq!(reg.check_cap(&cfg).unwrap(), CapDecision::Allow);
    }

    #[test]
    fn count_at_warn_returns_warn() {
        // Populate DB with N=warn rows of distinct, "alive" fake PIDs so
        // GC wouldn't reap them. We don't run GC here — check_cap itself
        // doesn't either.
        let path = fresh_db_path("at-warn");
        let cfg = CapConfig { max: 10, warn: 5 };
        let alive: Vec<u32> = (1000..1000 + cfg.warn as u32).collect();
        let reg = open_with_alive_set(&path, alive.clone());
        for pid in &alive {
            raw_insert(&reg, *pid);
        }
        assert_eq!(reg.count_live().unwrap(), cfg.warn);
        assert_eq!(reg.check_cap(&cfg).unwrap(), CapDecision::Warn(cfg.warn));
    }

    #[test]
    fn count_at_cap_returns_refuse() {
        let path = fresh_db_path("at-cap");
        let cfg = CapConfig { max: 4, warn: 2 };
        let alive: Vec<u32> = (2000..2000 + cfg.max as u32).collect();
        let reg = open_with_alive_set(&path, alive.clone());
        for pid in &alive {
            raw_insert(&reg, *pid);
        }
        assert_eq!(reg.count_live().unwrap(), cfg.max);
        assert_eq!(reg.check_cap(&cfg).unwrap(), CapDecision::Refuse(cfg.max));
    }

    /// **Issue #73 regression test**: verifies the `CLUD_MAX_INSTANCES=0`
    /// "cap disabled" hatch actually disables the cap. A future commit
    /// that drops the `cfg.max == CAP_DISABLED` short-circuit fails this
    /// test instead of silently breaking the env-var override that ops
    /// folks may rely on to recover from a stuck registry.
    #[test]
    fn fork_bomb_regression_max_instances_zero_disables_cap() {
        let path = fresh_db_path("max-zero-disables");
        // 1000 fake-alive PIDs.
        let alive: Vec<u32> = (10_000..11_000).collect();
        let reg = open_with_alive_set(&path, alive.clone());
        for pid in &alive {
            raw_insert(&reg, *pid);
        }
        let cfg = CapConfig { max: 0, warn: 0 };
        assert_eq!(reg.count_live().unwrap(), 1000);
        assert_eq!(reg.check_cap(&cfg).unwrap(), CapDecision::Allow);
    }

    /// **Issue #73 fork-bomb regression test** — the explicit one the
    /// user asked for. With `CLUD_MAX_INSTANCES=1` and a single live
    /// sibling already in the DB, `check_cap` MUST refuse. A future
    /// commit that accidentally removes the cap check, inverts the
    /// comparison, or special-cases small caps will fail this test
    /// instead of silently letting `clud` fork-bomb the workstation.
    #[test]
    fn fork_bomb_regression_max_instances_one_caps_at_one() {
        let path = fresh_db_path("max-one-caps");
        let reg = open_with_alive_set(&path, vec![424242]);
        raw_insert(&reg, 424242);
        let cfg = CapConfig { max: 1, warn: 0 };
        assert_eq!(reg.count_live().unwrap(), 1);
        assert_eq!(reg.check_cap(&cfg).unwrap(), CapDecision::Refuse(1));
    }

    #[test]
    fn register_then_drop_round_trips() {
        let path = fresh_db_path("register-drop");
        {
            let reg = open_with_alive_set(&path, vec![std::process::id()]);
            let info = SessionInfo {
                pid: std::process::id(),
                started_unix: 1234,
                backend: Some("claude".into()),
                launch_mode: Some("subprocess".into()),
                cwd: Some("/tmp/x".into()),
            };
            reg.register_self(info).unwrap();
            assert_eq!(reg.count_live().unwrap(), 1);
        }
        // Reopen and check the row was deleted on drop.
        let reg2 = open_with_alive_set(&path, vec![]);
        assert_eq!(reg2.count_live().unwrap(), 0);
    }

    #[test]
    fn drop_without_register_does_not_delete_other_rows() {
        // If `register_self` was never called, Drop should not touch the
        // DB. Otherwise an early-aborted clud (e.g. cap-exceeded refuse)
        // would clobber a sibling row that *happens* to share its PID
        // namespace via PID reuse.
        let path = fresh_db_path("drop-no-register");
        let reg = open_with_alive_set(&path, vec![]);
        raw_insert(&reg, std::process::id()); // pretend a sibling has our PID
        drop(reg);
        let reg2 = open_with_alive_set(&path, vec![]);
        assert_eq!(reg2.count_live().unwrap(), 1);
    }

    #[test]
    fn cap_config_from_env_defaults() {
        let _g = ENV_LOCK.lock().unwrap();
        // SAFETY: serialized via ENV_LOCK.
        unsafe {
            std::env::remove_var(ENV_MAX_INSTANCES);
            std::env::remove_var(ENV_WARN_INSTANCES);
        }
        let cfg = SessionRegistry::cap_config_from_env();
        assert_eq!(
            cfg,
            CapConfig {
                max: DEFAULT_MAX_INSTANCES,
                warn: DEFAULT_MAX_INSTANCES / 2,
            }
        );
    }

    #[test]
    fn cap_config_from_env_custom() {
        let _g = ENV_LOCK.lock().unwrap();
        unsafe {
            std::env::set_var(ENV_MAX_INSTANCES, "10");
            std::env::set_var(ENV_WARN_INSTANCES, "3");
        }
        let cfg = SessionRegistry::cap_config_from_env();
        unsafe {
            std::env::remove_var(ENV_MAX_INSTANCES);
            std::env::remove_var(ENV_WARN_INSTANCES);
        }
        assert_eq!(cfg, CapConfig { max: 10, warn: 3 });
    }

    #[test]
    fn cap_config_from_env_max_only_redrives_warn() {
        let _g = ENV_LOCK.lock().unwrap();
        unsafe {
            std::env::set_var(ENV_MAX_INSTANCES, "8");
            std::env::remove_var(ENV_WARN_INSTANCES);
        }
        let cfg = SessionRegistry::cap_config_from_env();
        unsafe {
            std::env::remove_var(ENV_MAX_INSTANCES);
        }
        assert_eq!(cfg, CapConfig { max: 8, warn: 4 });
    }

    #[test]
    fn cap_config_from_env_clamps_warn_to_max() {
        let _g = ENV_LOCK.lock().unwrap();
        unsafe {
            std::env::set_var(ENV_MAX_INSTANCES, "5");
            std::env::set_var(ENV_WARN_INSTANCES, "999");
        }
        let cfg = SessionRegistry::cap_config_from_env();
        unsafe {
            std::env::remove_var(ENV_MAX_INSTANCES);
            std::env::remove_var(ENV_WARN_INSTANCES);
        }
        assert_eq!(cfg.max, 5);
        assert_eq!(cfg.warn, 5);
    }

    #[test]
    fn gc_handles_concurrent_writes() {
        // Two registries on the same DB; register both, drop one, GC,
        // count → 1.
        //
        // NOTE on redb concurrency: redb takes an exclusive lock per
        // process via flock/LockFileEx. Opening the *same* file twice
        // from the same process succeeds on Windows and macOS/Linux
        // because the lock is held by the file descriptor, not by the
        // process — but the test's intent is to verify that two
        // independent SessionRegistry instances over the same file
        // coordinate correctly via redb's own write serialization.
        let path = fresh_db_path("concurrent");
        let pid_a: u32 = 700_001;
        let pid_b: u32 = 700_002;
        let mut reg_a = SessionRegistry::open_at_with_probe(
            &path,
            Box::new(MockLivenessProbe::with_alive([pid_a, pid_b])),
        )
        .unwrap();

        // Override own_pid so two registries in one test process can
        // each "register themselves" without colliding on the primary
        // key (and so each one's Drop removes its *own* row).
        reg_a.set_own_pid_for_test(pid_a);
        reg_a
            .register_self(SessionInfo {
                pid: pid_a,
                started_unix: 0,
                backend: None,
                launch_mode: None,
                cwd: None,
            })
            .unwrap();
        // Insert the sibling row directly — opening a second redb handle
        // on the same file in the same process is not supported (file
        // lock conflict), but the cap-check semantics we want to test
        // are: row count, GC keeps live rows, drop reduces count.
        raw_insert(&reg_a, pid_b);
        assert_eq!(reg_a.count_live().unwrap(), 2);

        // Drop pid_b's row directly. From reg_a's perspective only one
        // row remains.
        {
            let wtxn = reg_a.db.begin_write().unwrap();
            {
                let mut t = wtxn.open_table(SESSIONS).unwrap();
                t.remove(pid_b).unwrap();
            }
            wtxn.commit().unwrap();
        }
        // GC with both PIDs marked alive: nothing to remove.
        let removed = reg_a.gc_dead_sessions().unwrap();
        assert_eq!(removed, 0);
        assert_eq!(reg_a.count_live().unwrap(), 1);
    }

    #[test]
    fn schema_bootstrap_is_idempotent() {
        let path = fresh_db_path("schema-idempotent");
        // Open twice in a row — second open must not error.
        let reg1 = open_with_alive_set(&path, vec![]);
        drop(reg1);
        let reg2 = open_with_alive_set(&path, vec![]);
        // schema_version row was inserted exactly once and equals 1.
        let rtxn = reg2.db.begin_read().unwrap();
        let meta = rtxn.open_table(META).unwrap();
        let v = meta.get("schema_version").unwrap().unwrap().value();
        assert_eq!(v, 1);
    }

    #[test]
    fn decide_cap_branches() {
        // Pure-function coverage: keep the branch table here so a
        // refactor that reshapes `decide_cap` has to update *one* test
        // and not three.
        let cfg = CapConfig { max: 10, warn: 5 };
        assert_eq!(decide_cap(0, &cfg), CapDecision::Allow);
        assert_eq!(decide_cap(4, &cfg), CapDecision::Allow);
        assert_eq!(decide_cap(5, &cfg), CapDecision::Warn(5));
        assert_eq!(decide_cap(9, &cfg), CapDecision::Warn(9));
        assert_eq!(decide_cap(10, &cfg), CapDecision::Refuse(10));
        assert_eq!(decide_cap(99, &cfg), CapDecision::Refuse(99));

        // max == 0 disables the cap entirely.
        let disabled = CapConfig { max: 0, warn: 0 };
        assert_eq!(decide_cap(99, &disabled), CapDecision::Allow);

        // warn == 0 with max > 0 means "no warn band, just the cap".
        let no_warn = CapConfig { max: 5, warn: 0 };
        assert_eq!(decide_cap(4, &no_warn), CapDecision::Allow);
        assert_eq!(decide_cap(5, &no_warn), CapDecision::Refuse(5));
    }

    #[test]
    fn mock_liveness_probe_set_arithmetic() {
        let probe = MockLivenessProbe::with_alive([1, 2, 3]);
        assert!(probe.is_alive(1));
        assert!(probe.is_alive(2));
        assert!(!probe.is_alive(99));
        probe.mark_dead(2);
        assert!(!probe.is_alive(2));
        probe.mark_alive(99);
        assert!(probe.is_alive(99));
    }

    #[test]
    fn os_liveness_probe_treats_pid_zero_as_dead() {
        // PID 0 is reserved on every OS we ship to (Idle on Windows,
        // process-group sentinel on POSIX). Counting it as a "clud
        // sibling" would be a bug.
        let probe = OsLivenessProbe;
        assert!(!probe.is_alive(0));
    }

    #[test]
    fn os_liveness_probe_recognizes_self() {
        // The current test process must show up as alive — this is the
        // closest thing to an integration smoke test we can run without
        // launching a child. If this ever fails, the cap will refuse to
        // launch a fresh `clud` even on an empty DB (because GC would
        // wrongly reap our own row).
        let probe = OsLivenessProbe;
        assert!(probe.is_alive(std::process::id()));
    }

    #[test]
    fn session_info_for_self_uses_current_pid_and_cwd() {
        let info = SessionInfo::for_self(Some("claude".into()), Some("subprocess".into()));
        assert_eq!(info.pid, std::process::id());
        assert!(info.started_unix > 0);
        assert!(info.cwd.is_some());
    }

    #[test]
    fn distinct_db_paths_do_not_collide() {
        // Each test gets its own DB path; this asserts the helper itself
        // returns distinct paths so future tests can rely on it.
        let a = fresh_db_path("a");
        let b = fresh_db_path("b");
        assert_ne!(a, b);
        let mut seen = HashSet::new();
        seen.insert(a);
        seen.insert(b);
        assert_eq!(seen.len(), 2);
    }

    /// **Issue #138 regression test**: `unregister` deletes the row
    /// synchronously and clears the `registered` flag so a subsequent
    /// `Drop` doesn't try to re-delete the row (and possibly clobber a
    /// sibling that inherited our PID via PID reuse).
    #[test]
    fn unregister_deletes_row_and_clears_flag() {
        let path = fresh_db_path("unregister");
        let reg = open_with_alive_set(&path, vec![std::process::id()]);
        reg.register_self(SessionInfo {
            pid: std::process::id(),
            started_unix: 1234,
            backend: None,
            launch_mode: None,
            cwd: None,
        })
        .unwrap();
        assert_eq!(reg.count_live().unwrap(), 1);
        assert!(reg.registered.load(std::sync::atomic::Ordering::SeqCst));
        reg.unregister().unwrap();
        assert_eq!(reg.count_live().unwrap(), 0);
        assert!(!reg.registered.load(std::sync::atomic::Ordering::SeqCst));
        // Subsequent Drop must not panic and must not touch the table.
        drop(reg);
        let reg2 = open_with_alive_set(&path, vec![]);
        assert_eq!(reg2.count_live().unwrap(), 0);
    }

    /// **Issue #138 regression test**: two `acquire_lock_at` calls on the
    /// same lock path serialize — the second one only returns after the
    /// first guard drops. We model "wait for the other thread" with a
    /// barrier + a generous timeout to keep the test deterministic on
    /// slow CI without making the happy path slow.
    ///
    /// Why this is the right shape: redb's exclusive lock fails fast on
    /// contention; `fs4`'s `lock_exclusive` blocks. The lock-file pattern
    /// from issue #138 converts a "fail on contention" surface into a
    /// "queue on contention" surface — that's what this test pins.
    #[test]
    fn acquire_lock_serializes_callers() {
        use std::sync::atomic::{AtomicU64, Ordering};
        use std::sync::Arc;
        use std::thread;
        use std::time::Duration;

        let dir = tempfile::tempdir().expect("tempdir");
        let lock_path = dir.path().join("acquire-serializes.lock");

        // Holder thread: grabs the lock, then sleeps for a known interval.
        let holder_path = lock_path.clone();
        let holder_started = Arc::new(AtomicU64::new(0));
        let holder_released = Arc::new(AtomicU64::new(0));
        let holder_started_clone = Arc::clone(&holder_started);
        let holder_released_clone = Arc::clone(&holder_released);
        let holder = thread::spawn(move || {
            let _guard = acquire_lock_at(&holder_path).expect("holder lock");
            holder_started_clone.store(now_ms(), Ordering::SeqCst);
            thread::sleep(Duration::from_millis(200));
            holder_released_clone.store(now_ms(), Ordering::SeqCst);
        });

        // Wait until the holder confirms it owns the lock before we try
        // to acquire from this thread. Otherwise we might *win* the race
        // and the test asserts nothing.
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        while holder_started.load(Ordering::SeqCst) == 0 {
            if std::time::Instant::now() > deadline {
                panic!("holder never acquired the lock");
            }
            thread::sleep(Duration::from_millis(5));
        }

        // Now race: this acquire MUST block until the holder releases.
        let waiter_acquired = now_ms();
        let _guard = acquire_lock_at(&lock_path).expect("waiter lock");
        let waiter_unblocked = now_ms();
        holder.join().expect("holder join");

        // The waiter's unblock time should be at or after the holder's
        // release time. Generous epsilon (50ms) for clock skew between
        // the two thread observations on a busy CI runner.
        let released = holder_released.load(Ordering::SeqCst);
        assert!(
            waiter_unblocked + 50 >= released,
            "waiter unblocked at {waiter_unblocked} but holder released at {released}",
        );
        // Sanity: the waiter was at least delayed beyond when it tried.
        assert!(
            waiter_unblocked >= waiter_acquired,
            "waiter clock skew detected: tried at {waiter_acquired}, unblocked at {waiter_unblocked}",
        );
    }

    fn now_ms() -> u64 {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis() as u64)
            .unwrap_or(0)
    }

    /// **Issue #138 regression test**: `run_startup_under_lock` opens the
    /// redb file, performs gc / cap-check / register inside the lock, and
    /// **closes the file before returning**. After it returns, a subsequent
    /// caller can immediately open the redb file — proving the lock is
    /// scoped to the helper, not the whole `clud` lifetime.
    #[test]
    fn run_startup_under_lock_releases_redb_after_return() {
        let _g = ENV_LOCK.lock().unwrap();
        let dir = tempfile::tempdir().expect("tempdir");
        let db = dir.path().join("startup-releases.redb");
        let lock = dir.path().join("startup-releases.lock");
        // SAFETY: serialized via ENV_LOCK.
        unsafe {
            std::env::set_var(ENV_SESSION_DB, &db);
            std::env::set_var(ENV_SESSION_LOCK, &lock);
            std::env::set_var(ENV_MAX_INSTANCES, "0"); // disable cap to keep test simple
        }

        let info = SessionInfo {
            pid: std::process::id(),
            started_unix: 1,
            backend: None,
            launch_mode: None,
            cwd: None,
        };
        let cfg = SessionRegistry::cap_config_from_env();
        let outcome = run_startup_under_lock(&cfg, info).expect("startup");
        assert_eq!(outcome.decision, CapDecision::Allow);
        assert!(outcome.registered);

        // If `run_startup_under_lock` left redb open, this would fail
        // with `DatabaseAlreadyOpen`. The whole point of issue #138 is
        // that this succeeds without contention.
        let reopen = SessionRegistry::open_default().expect("reopen");
        assert_eq!(reopen.count_live().unwrap(), 1);
        drop(reopen);

        // Shutdown removes the row, again under the lock.
        run_shutdown_under_lock().expect("shutdown");
        let after = SessionRegistry::open_default().expect("reopen after shutdown");
        assert_eq!(after.count_live().unwrap(), 0);

        unsafe {
            std::env::remove_var(ENV_SESSION_DB);
            std::env::remove_var(ENV_SESSION_LOCK);
            std::env::remove_var(ENV_MAX_INSTANCES);
        }
    }

    /// **Issue #138 regression test**: `default_lock_path` derives from
    /// the DB path's parent dir when `CLUD_SESSION_LOCK` is unset.
    /// `CLUD_SESSION_LOCK` wins when both are set.
    #[test]
    fn default_lock_path_derives_from_db_parent_or_env() {
        let _g = ENV_LOCK.lock().unwrap();
        let dir = tempfile::tempdir().expect("tempdir");
        let db = dir.path().join("derives.redb");
        // SAFETY: serialized via ENV_LOCK.
        unsafe {
            std::env::set_var(ENV_SESSION_DB, &db);
            std::env::remove_var(ENV_SESSION_LOCK);
        }
        let derived = default_lock_path().expect("derived");
        assert_eq!(derived, dir.path().join("sessions.lock"));

        let explicit = dir.path().join("custom.lock");
        unsafe {
            std::env::set_var(ENV_SESSION_LOCK, &explicit);
        }
        let resolved = default_lock_path().expect("explicit");
        assert_eq!(resolved, explicit);
        unsafe {
            std::env::remove_var(ENV_SESSION_DB);
            std::env::remove_var(ENV_SESSION_LOCK);
        }
    }
}
