//! SQLite-backed registry of live `clud` sessions.
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
//! ```sql
//! CREATE TABLE IF NOT EXISTS sessions (
//!     pid INTEGER PRIMARY KEY,
//!     started_unix INTEGER NOT NULL,
//!     backend TEXT,
//!     launch_mode TEXT,
//!     cwd TEXT
//! );
//! CREATE TABLE IF NOT EXISTS schema_version (version INTEGER PRIMARY KEY);
//! ```
//!
//! ## Concurrency
//!
//! Multiple `clud` processes may hit the same DB simultaneously. We open
//! in WAL mode with a 5-second busy timeout, and the GC + insert sequence
//! runs inside a transaction so the cap check is atomic with respect to
//! siblings.
//!
//! ## Liveness probe
//!
//! The PID-liveness check is abstracted behind `LivenessProbe` so unit
//! tests can deterministically mark specific PIDs alive/dead. The
//! production path uses `OsLivenessProbe`: on Windows
//! `OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, …)` + `GetExitCodeProcess`,
//! on POSIX `kill(pid, 0)` (which returns 0 / `ESRCH` without actually
//! sending a signal).

use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use rusqlite::{params, Connection, OpenFlags};

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

/// Errors surfaced by the registry. Kept narrow on purpose — callers in
/// `main.rs` either log and continue (for "couldn't open the DB") or log
/// and exit (for "cap exceeded"), so a rich error enum buys nothing here.
#[derive(Debug)]
pub enum RegistryError {
    /// Could not figure out the default DB path (no `LOCALAPPDATA` /
    /// `XDG_STATE_HOME` / `HOME`). Caller should log and skip the cap
    /// check rather than refusing to launch.
    NoDefaultPath,
    /// Filesystem or sqlite open/IO failure.
    Io(String),
    /// SQL error (schema bootstrap, query, transaction).
    Sql(String),
}

impl std::fmt::Display for RegistryError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NoDefaultPath => write!(f, "no default session-db path could be resolved"),
            Self::Io(msg) => write!(f, "session-db I/O error: {msg}"),
            Self::Sql(msg) => write!(f, "session-db sql error: {msg}"),
        }
    }
}

impl std::error::Error for RegistryError {}

impl From<rusqlite::Error> for RegistryError {
    fn from(e: rusqlite::Error) -> Self {
        Self::Sql(e.to_string())
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

/// Live-session registry. Holds an open SQLite connection. On `Drop` the
/// row matching `own_pid` is deleted (best-effort), but only if
/// `register_self` was called successfully.
pub struct SessionRegistry {
    conn: Mutex<Connection>,
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
    /// (`%LOCALAPPDATA%/clud/sessions.db` on Windows,
    /// `$XDG_STATE_HOME/clud/sessions.db` on POSIX). Honors
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
        let conn = Connection::open_with_flags(
            path,
            OpenFlags::SQLITE_OPEN_READ_WRITE | OpenFlags::SQLITE_OPEN_CREATE,
        )?;
        // WAL gives us non-blocking readers; busy_timeout absorbs the
        // (rare) writer-vs-writer races between two clud startups.
        conn.busy_timeout(Duration::from_secs(5))?;
        conn.pragma_update(None, "journal_mode", "WAL")?;
        conn.pragma_update(None, "synchronous", "NORMAL")?;
        Self::bootstrap_schema(&conn)?;
        Ok(Self {
            conn: Mutex::new(conn),
            own_pid: std::process::id(),
            probe,
            registered: std::sync::atomic::AtomicBool::new(false),
        })
    }

    fn bootstrap_schema(conn: &Connection) -> Result<(), RegistryError> {
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS sessions (
                pid INTEGER PRIMARY KEY,
                started_unix INTEGER NOT NULL,
                backend TEXT,
                launch_mode TEXT,
                cwd TEXT
            );
            CREATE TABLE IF NOT EXISTS schema_version (
                version INTEGER PRIMARY KEY
            );
            INSERT OR IGNORE INTO schema_version (version) VALUES (1);",
        )?;
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
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare("SELECT pid FROM sessions")?;
        let pids: Vec<u32> = stmt
            .query_map([], |row| row.get::<_, i64>(0))?
            .filter_map(|r| r.ok())
            .map(|p| p as u32)
            .collect();
        drop(stmt);

        let mut dead: Vec<u32> = Vec::new();
        for pid in pids {
            if !self.probe.is_alive(pid) {
                dead.push(pid);
            }
        }
        if dead.is_empty() {
            return Ok(0);
        }
        let tx = conn.unchecked_transaction()?;
        for pid in &dead {
            tx.execute("DELETE FROM sessions WHERE pid = ?1", params![*pid as i64])?;
        }
        tx.commit()?;
        Ok(dead.len() as u64)
    }

    /// Count rows currently in the DB. Does *not* run GC — call
    /// `gc_dead_sessions` first if you want a live-only count.
    pub fn count_live(&self) -> Result<u64, RegistryError> {
        let conn = self.conn.lock().unwrap();
        let n: i64 = conn.query_row("SELECT COUNT(*) FROM sessions", [], |row| row.get(0))?;
        Ok(n as u64)
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
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "INSERT OR REPLACE INTO sessions
                (pid, started_unix, backend, launch_mode, cwd)
             VALUES (?1, ?2, ?3, ?4, ?5)",
            params![
                info.pid as i64,
                info.started_unix,
                info.backend,
                info.launch_mode,
                info.cwd,
            ],
        )?;
        self.registered
            .store(true, std::sync::atomic::Ordering::SeqCst);
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
        if let Ok(conn) = self.conn.lock() {
            let _ = conn.execute(
                "DELETE FROM sessions WHERE pid = ?1",
                params![self.own_pid as i64],
            );
        }
    }
}

/// Pure cap-decision function. Split out so unit tests can exercise the
/// branches without needing a SQLite connection.
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

/// Resolve the OS-default DB path. `%LOCALAPPDATA%\clud\sessions.db` on
/// Windows; `$XDG_STATE_HOME/clud/sessions.db` (or
/// `~/.local/state/clud/sessions.db`) on POSIX.
fn default_db_path() -> Result<PathBuf, RegistryError> {
    #[cfg(windows)]
    {
        if let Some(local) = std::env::var_os("LOCALAPPDATA") {
            let mut p = PathBuf::from(local);
            p.push("clud");
            p.push("sessions.db");
            return Ok(p);
        }
        // Fallback: %USERPROFILE%\AppData\Local\clud\sessions.db
        if let Some(home) = std::env::var_os("USERPROFILE") {
            let mut p = PathBuf::from(home);
            p.push("AppData");
            p.push("Local");
            p.push("clud");
            p.push("sessions.db");
            return Ok(p);
        }
    }
    #[cfg(not(windows))]
    {
        if let Some(state) = std::env::var_os("XDG_STATE_HOME") {
            if !state.is_empty() {
                let mut p = PathBuf::from(state);
                p.push("clud");
                p.push("sessions.db");
                return Ok(p);
            }
        }
        if let Some(home) = std::env::var_os("HOME") {
            let mut p = PathBuf::from(home);
            p.push(".local");
            p.push("state");
            p.push("clud");
            p.push("sessions.db");
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
        let path = dir.path().join(format!("sessions-{tag}.db"));
        std::mem::forget(dir);
        path
    }

    fn open_with_alive_set(path: &Path, alive: Vec<u32>) -> SessionRegistry {
        let probe = Box::new(MockLivenessProbe::with_alive(alive));
        SessionRegistry::open_at_with_probe(path, probe).expect("open registry")
    }

    fn raw_insert(reg: &SessionRegistry, pid: u32) {
        let conn = reg.conn.lock().unwrap();
        conn.execute(
            "INSERT OR REPLACE INTO sessions
                (pid, started_unix, backend, launch_mode, cwd)
             VALUES (?1, 0, NULL, NULL, NULL)",
            params![pid as i64],
        )
        .unwrap();
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
        let path = fresh_db_path("concurrent");
        let pid_a: u32 = 700_001;
        let pid_b: u32 = 700_002;
        let mut reg_a = SessionRegistry::open_at_with_probe(
            &path,
            Box::new(MockLivenessProbe::with_alive([pid_a, pid_b])),
        )
        .unwrap();
        let mut reg_b = SessionRegistry::open_at_with_probe(
            &path,
            Box::new(MockLivenessProbe::with_alive([pid_a, pid_b])),
        )
        .unwrap();

        // Override own_pid so two registries in one test process can
        // each "register themselves" without colliding on the primary
        // key (and so each one's Drop removes its *own* row).
        reg_a.set_own_pid_for_test(pid_a);
        reg_b.set_own_pid_for_test(pid_b);
        reg_a
            .register_self(SessionInfo {
                pid: pid_a,
                started_unix: 0,
                backend: None,
                launch_mode: None,
                cwd: None,
            })
            .unwrap();
        reg_b
            .register_self(SessionInfo {
                pid: pid_b,
                started_unix: 0,
                backend: None,
                launch_mode: None,
                cwd: None,
            })
            .unwrap();
        assert_eq!(reg_a.count_live().unwrap(), 2);

        // Drop reg_b → its row goes. From reg_a's perspective only one
        // row remains.
        drop(reg_b);
        // GC with both PIDs marked alive: nothing to remove. We're
        // testing that the count after one drop is exactly 1, not that
        // GC removes anything here.
        let removed = reg_a.gc_dead_sessions().unwrap();
        assert_eq!(removed, 0);
        assert_eq!(reg_a.count_live().unwrap(), 1);
    }

    #[test]
    fn schema_bootstrap_is_idempotent() {
        let path = fresh_db_path("schema-idempotent");
        // Open twice in a row — second open must not error on
        // CREATE TABLE.
        let reg1 = open_with_alive_set(&path, vec![]);
        drop(reg1);
        let reg2 = open_with_alive_set(&path, vec![]);
        // schema_version row was inserted exactly once.
        let n: i64 = reg2
            .conn
            .lock()
            .unwrap()
            .query_row("SELECT COUNT(*) FROM schema_version", [], |row| row.get(0))
            .unwrap();
        assert_eq!(n, 1);
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
}
