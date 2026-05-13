//! `clud gc` — tracked-entry garbage collection (issue #110).
//!
//! Background: Claude Code creates per-agent git worktrees under
//! `.claude/worktrees/agent-<id>/` whenever a subagent runs with
//! `isolation: "worktree"`. Over a long debugging session these accumulate
//! across repos and across `clud` invocations, and the existing
//! `--clean-worktrees` flag only knows about the current repo. This module
//! adds a per-user SQLite registry of every tracked entry, plus three CLI
//! handlers (`list`, `purge`, `reconcile`).
//!
//! Schema lives in `tracked_entries`; the `kind` column is generic so
//! future kinds (caches, daemon state) drop in without a migration.
//!
//! The DB also gets watched by a background `WorktreeScanner` thread,
//! spawned from `main.rs` for the lifetime of a normal `clud` launch.
//! It polls `.claude/worktrees/` every ~2 seconds and upserts any new
//! `agent-*` directory it spots. Cancellation is cooperative via an
//! `Arc<AtomicBool>`; `Drop` joins the thread.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use clap::CommandFactory;
use rusqlite::{params, Connection, OpenFlags};

use crate::args::{Args, GcSubcommand};
use crate::session_registry::{LivenessProbe, OsLivenessProbe};
use crate::worktrees;

/// Env-var override for the DB path. Mirrors `CLUD_SESSION_DB` in
/// `session_registry.rs`. Tests set this to a tempdir.
pub const ENV_DATA_DB: &str = "CLUD_DATA_DB";

/// Errors surfaced by `gc.rs`. Narrow on purpose — the CLI handlers
/// stringify these and log; nothing branches on the variant.
#[derive(Debug)]
pub enum GcError {
    NoDefaultPath,
    Io(String),
    Sql(String),
}

impl std::fmt::Display for GcError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NoDefaultPath => write!(f, "no default clud data-db path could be resolved"),
            Self::Io(m) => write!(f, "gc i/o error: {m}"),
            Self::Sql(m) => write!(f, "gc sql error: {m}"),
        }
    }
}

impl std::error::Error for GcError {}

impl From<rusqlite::Error> for GcError {
    fn from(e: rusqlite::Error) -> Self {
        Self::Sql(e.to_string())
    }
}

/// One row from the `tracked_entries` table.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TrackedEntry {
    pub id: i64,
    pub kind: String,
    pub path: String,
    pub repo_root: Option<String>,
    pub branch: Option<String>,
    pub agent_id: Option<String>,
    pub created_unix: i64,
    pub last_seen_unix: i64,
}

/// Inputs for `upsert_entry`. Keeps the API ergonomic — `created_unix`
/// is only used on INSERT (the upsert preserves the original creation
/// time on conflict).
#[derive(Debug, Clone)]
pub struct UpsertInput {
    pub kind: String,
    pub path: String,
    pub repo_root: Option<String>,
    pub branch: Option<String>,
    pub agent_id: Option<String>,
    pub now_unix: i64,
}

/// SQLite-backed registry of tracked entries.
pub struct Registry {
    conn: Mutex<Connection>,
}

impl Registry {
    /// Open at the default path (honors `CLUD_DATA_DB`).
    pub fn open_default() -> Result<Self, GcError> {
        let path = match std::env::var_os(ENV_DATA_DB) {
            Some(v) => PathBuf::from(v),
            None => default_data_db_path()?,
        };
        Self::open_at(&path)
    }

    /// Open the registry at `path`, creating parent dirs + bootstrapping
    /// the schema if needed.
    pub fn open_at(path: &Path) -> Result<Self, GcError> {
        if let Some(parent) = path.parent() {
            if !parent.as_os_str().is_empty() {
                std::fs::create_dir_all(parent)
                    .map_err(|e| GcError::Io(format!("create_dir_all({:?}): {e}", parent)))?;
            }
        }
        let conn = Connection::open_with_flags(
            path,
            OpenFlags::SQLITE_OPEN_READ_WRITE | OpenFlags::SQLITE_OPEN_CREATE,
        )?;
        // Mirror the proven pragma combo from session_registry.rs.
        conn.busy_timeout(Duration::from_secs(5))?;
        conn.pragma_update(None, "journal_mode", "WAL")?;
        conn.pragma_update(None, "synchronous", "NORMAL")?;
        Self::bootstrap_schema(&conn)?;
        Ok(Self {
            conn: Mutex::new(conn),
        })
    }

    fn bootstrap_schema(conn: &Connection) -> Result<(), GcError> {
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS tracked_entries (
                id              INTEGER PRIMARY KEY AUTOINCREMENT,
                kind            TEXT NOT NULL,
                path            TEXT NOT NULL,
                repo_root       TEXT,
                branch          TEXT,
                agent_id        TEXT,
                created_unix    INTEGER NOT NULL,
                last_seen_unix  INTEGER NOT NULL,
                UNIQUE(kind, path)
            );
            CREATE INDEX IF NOT EXISTS idx_tracked_kind_created
                ON tracked_entries(kind, created_unix);
            CREATE TABLE IF NOT EXISTS schema_version (version INTEGER PRIMARY KEY);
            INSERT OR IGNORE INTO schema_version (version) VALUES (1);",
        )?;
        Ok(())
    }

    /// Insert a new entry or refresh `last_seen_unix` on an existing one
    /// keyed by `(kind, path)`. `created_unix` is preserved on conflict.
    pub fn upsert_entry(&self, input: &UpsertInput) -> Result<(), GcError> {
        let conn = self.conn.lock().unwrap();
        with_busy_retry(|| {
            conn.execute(
                "INSERT INTO tracked_entries
                    (kind, path, repo_root, branch, agent_id, created_unix, last_seen_unix)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?6)
                 ON CONFLICT(kind, path) DO UPDATE SET
                    last_seen_unix = excluded.last_seen_unix,
                    repo_root = COALESCE(excluded.repo_root, tracked_entries.repo_root),
                    branch = COALESCE(excluded.branch, tracked_entries.branch),
                    agent_id = COALESCE(excluded.agent_id, tracked_entries.agent_id)",
                params![
                    input.kind,
                    input.path,
                    input.repo_root,
                    input.branch,
                    input.agent_id,
                    input.now_unix,
                ],
            )
        })?;
        Ok(())
    }

    /// Return every row, newest first. Optionally filter by kind.
    pub fn list(&self, filter_kind: Option<&str>) -> Result<Vec<TrackedEntry>, GcError> {
        let conn = self.conn.lock().unwrap();
        let rows = match filter_kind {
            Some(k) => {
                let mut stmt = conn.prepare(
                    "SELECT id, kind, path, repo_root, branch, agent_id, created_unix, last_seen_unix
                     FROM tracked_entries
                     WHERE kind = ?1
                     ORDER BY created_unix DESC",
                )?;
                let iter = stmt.query_map(params![k], row_to_entry)?;
                iter.collect::<rusqlite::Result<Vec<_>>>()?
            }
            None => {
                let mut stmt = conn.prepare(
                    "SELECT id, kind, path, repo_root, branch, agent_id, created_unix, last_seen_unix
                     FROM tracked_entries
                     ORDER BY created_unix DESC",
                )?;
                let iter = stmt.query_map([], row_to_entry)?;
                iter.collect::<rusqlite::Result<Vec<_>>>()?
            }
        };
        Ok(rows)
    }

    /// Fetch rows whose `created_unix` is strictly less than `cutoff`.
    /// Optionally filter by kind.
    pub fn select_older_than(
        &self,
        cutoff: i64,
        filter_kind: Option<&str>,
    ) -> Result<Vec<TrackedEntry>, GcError> {
        let conn = self.conn.lock().unwrap();
        let rows = match filter_kind {
            Some(k) => {
                let mut stmt = conn.prepare(
                    "SELECT id, kind, path, repo_root, branch, agent_id, created_unix, last_seen_unix
                     FROM tracked_entries
                     WHERE kind = ?1 AND created_unix < ?2
                     ORDER BY created_unix ASC",
                )?;
                let iter = stmt.query_map(params![k, cutoff], row_to_entry)?;
                iter.collect::<rusqlite::Result<Vec<_>>>()?
            }
            None => {
                let mut stmt = conn.prepare(
                    "SELECT id, kind, path, repo_root, branch, agent_id, created_unix, last_seen_unix
                     FROM tracked_entries
                     WHERE created_unix < ?1
                     ORDER BY created_unix ASC",
                )?;
                let iter = stmt.query_map(params![cutoff], row_to_entry)?;
                iter.collect::<rusqlite::Result<Vec<_>>>()?
            }
        };
        Ok(rows)
    }

    /// Delete a single entry by id, inside its own retry-wrapped transaction.
    /// Per-entry isolation means a stuck row never blocks neighbors.
    pub fn delete(&self, id: i64) -> Result<(), GcError> {
        let conn = self.conn.lock().unwrap();
        with_busy_retry(|| conn.execute("DELETE FROM tracked_entries WHERE id = ?1", params![id]))?;
        Ok(())
    }

    /// Count rows. Mostly for tests.
    pub fn count(&self) -> Result<u64, GcError> {
        let conn = self.conn.lock().unwrap();
        let n: i64 = conn.query_row("SELECT COUNT(*) FROM tracked_entries", [], |r| r.get(0))?;
        Ok(n as u64)
    }
}

fn row_to_entry(row: &rusqlite::Row<'_>) -> rusqlite::Result<TrackedEntry> {
    Ok(TrackedEntry {
        id: row.get(0)?,
        kind: row.get(1)?,
        path: row.get(2)?,
        repo_root: row.get(3)?,
        branch: row.get(4)?,
        agent_id: row.get(5)?,
        created_unix: row.get(6)?,
        last_seen_unix: row.get(7)?,
    })
}

/// Bounded exponential backoff on SQLITE_BUSY / SQLITE_LOCKED. The
/// `busy_timeout(5s)` pragma absorbs most contention; this gives us
/// explicit retry for the long-tail (WAL checkpoint stalls, etc.).
pub fn with_busy_retry<F, T>(mut op: F) -> rusqlite::Result<T>
where
    F: FnMut() -> rusqlite::Result<T>,
{
    let backoffs_ms = [50u64, 100, 200, 400, 800];
    let mut last_err: Option<rusqlite::Error> = None;
    for (attempt, delay) in std::iter::once(0u64)
        .chain(backoffs_ms.iter().copied())
        .enumerate()
    {
        if attempt > 0 {
            std::thread::sleep(Duration::from_millis(delay));
        }
        match op() {
            Ok(v) => return Ok(v),
            Err(e) => {
                if !is_busy_or_locked(&e) {
                    return Err(e);
                }
                last_err = Some(e);
            }
        }
    }
    Err(last_err.expect("loop ran at least once"))
}

fn is_busy_or_locked(e: &rusqlite::Error) -> bool {
    matches!(
        e.sqlite_error_code(),
        Some(rusqlite::ErrorCode::DatabaseBusy) | Some(rusqlite::ErrorCode::DatabaseLocked)
    )
}

/// Resolve the default DB path: `~/.clud/data.db`. Mirrors
/// `~/.soldr/data.db`. `CLUD_DATA_DB` overrides.
pub fn default_data_db_path() -> Result<PathBuf, GcError> {
    let home = dirs::home_dir().ok_or(GcError::NoDefaultPath)?;
    Ok(home.join(".clud").join("data.db"))
}

fn now_unix() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

// ---------- lock-reason pid extraction ----------

/// Extract the pid from a git-worktree lock reason string emitted by
/// Claude Code, e.g. `"claude agent agent-abf (pid 12345)"`. Returns
/// `None` for anything that doesn't match the `pid <digits>` pattern.
pub fn extract_pid_from_lock_reason(reason: &str) -> Option<u32> {
    // Find the `pid ` substring and take the run of ASCII digits that
    // follows. No regex — keeps the dep graph tiny.
    let idx = reason.find("pid ")?;
    let rest = &reason[idx + 4..];
    let digits: String = rest.chars().take_while(|c| c.is_ascii_digit()).collect();
    if digits.is_empty() {
        return None;
    }
    digits.parse().ok()
}

// ---------- reconcile ----------

/// Walk `.claude/worktrees/` in the *current* repo and upsert any
/// agent-* subdirectory we find. Returns the number of new entries
/// (i.e. rows that didn't previously exist).
pub fn run_reconcile(registry: &Registry) -> Result<usize, GcError> {
    let main_root = worktrees::locate_main_repo_root().map_err(GcError::Io)?;
    let watch_dir = main_root.join(".claude").join("worktrees");
    reconcile_dir(registry, &watch_dir, Some(&main_root)).map(|res| res.inserted)
}

#[derive(Debug, Default, Clone, Copy)]
pub struct ScanResult {
    pub inserted: usize,
    pub refreshed: usize,
}

/// Walk `watch_dir` and upsert each immediate subdir whose name starts
/// with `agent-`. Returns counts of inserted-vs-refreshed rows.
pub fn reconcile_dir(
    registry: &Registry,
    watch_dir: &Path,
    repo_root: Option<&Path>,
) -> Result<ScanResult, GcError> {
    let mut res = ScanResult::default();
    let entries = match std::fs::read_dir(watch_dir) {
        Ok(it) => it,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(res),
        Err(e) => return Err(GcError::Io(format!("read_dir({:?}): {e}", watch_dir))),
    };
    for entry in entries.flatten() {
        let ft = match entry.file_type() {
            Ok(t) => t,
            Err(_) => continue,
        };
        if !ft.is_dir() {
            continue;
        }
        let name = entry.file_name();
        let name_str = match name.to_str() {
            Some(s) => s,
            None => continue,
        };
        if !name_str.starts_with("agent-") {
            continue;
        }
        let path = entry.path();
        let path_str = path.to_string_lossy().to_string();

        // Was this a new row or a refresh? Decide via a count probe so the
        // upsert can stay a single statement.
        let existed_before = registry_has_entry(registry, "worktree", &path_str)?;

        let branch = best_effort_branch(&path);
        let input = UpsertInput {
            kind: "worktree".to_string(),
            path: path_str,
            repo_root: repo_root.map(|p| p.to_string_lossy().to_string()),
            branch,
            agent_id: Some(name_str.to_string()),
            now_unix: now_unix(),
        };
        registry.upsert_entry(&input)?;
        if existed_before {
            res.refreshed += 1;
        } else {
            res.inserted += 1;
        }
    }
    Ok(res)
}

fn registry_has_entry(registry: &Registry, kind: &str, path: &str) -> Result<bool, GcError> {
    let conn = registry.conn.lock().unwrap();
    let n: i64 = conn.query_row(
        "SELECT COUNT(*) FROM tracked_entries WHERE kind = ?1 AND path = ?2",
        params![kind, path],
        |r| r.get(0),
    )?;
    Ok(n > 0)
}

fn best_effort_branch(path: &Path) -> Option<String> {
    worktrees::run_git(path, &["rev-parse", "--abbrev-ref", "HEAD"])
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty() && s != "HEAD")
}

// ---------- background scanner thread ----------

/// Polling scanner that watches a `.claude/worktrees/` directory and
/// inserts new agent-* subdirs into the registry as they appear.
/// Cancels cooperatively via `Arc<AtomicBool>`.
pub struct WorktreeScanner {
    cancel: Arc<AtomicBool>,
    handle: Option<JoinHandle<()>>,
}

impl WorktreeScanner {
    /// Spawn a scanner watching the *current* repo's `.claude/worktrees/`.
    /// Returns `None` if the registry can't be opened or the repo root
    /// can't be located — the caller logs and continues.
    pub fn maybe_spawn() -> Option<Self> {
        let registry = match Registry::open_default() {
            Ok(r) => Arc::new(r),
            Err(e) => {
                eprintln!("[clud] warning: gc registry unavailable: {e}");
                return None;
            }
        };
        let main_root = match worktrees::locate_main_repo_root() {
            Ok(p) => p,
            Err(_) => {
                // Not inside a git repo (e.g. running `clud` from /tmp).
                // No worktrees to scan — skip spawning.
                return None;
            }
        };
        let watch_dir = main_root.join(".claude").join("worktrees");
        Some(Self::spawn(registry, watch_dir, Some(main_root)))
    }

    /// Explicit spawn. Tests pass a custom watch dir + tempdir-backed
    /// registry. `scan_interval` is hardcoded to ~2s via the
    /// 20-chunk × 100ms sleep loop.
    pub fn spawn(registry: Arc<Registry>, watch_dir: PathBuf, repo_root: Option<PathBuf>) -> Self {
        let cancel = Arc::new(AtomicBool::new(false));
        let cancel_t = cancel.clone();
        let handle = std::thread::Builder::new()
            .name("clud-gc-scanner".to_string())
            .spawn(move || run_scanner_loop(registry, watch_dir, repo_root, cancel_t))
            .expect("spawn scanner thread");
        Self {
            cancel,
            handle: Some(handle),
        }
    }

    /// Signal cancellation and wait for the worker thread to exit.
    pub fn cancel(&mut self) {
        self.cancel.store(true, Ordering::SeqCst);
        if let Some(h) = self.handle.take() {
            let _ = h.join();
        }
    }
}

impl Drop for WorktreeScanner {
    fn drop(&mut self) {
        self.cancel();
    }
}

fn run_scanner_loop(
    registry: Arc<Registry>,
    watch_dir: PathBuf,
    repo_root: Option<PathBuf>,
    cancel: Arc<AtomicBool>,
) {
    let repo_root_ref = repo_root.as_deref();
    let mut last_error_kind: Option<String> = None;
    while !cancel.load(Ordering::SeqCst) {
        match reconcile_dir(&registry, &watch_dir, repo_root_ref) {
            Ok(_) => last_error_kind = None,
            Err(e) => {
                // Dedupe noisy errors — log once per distinct kind.
                let key = format!("{:?}", &e);
                if last_error_kind.as_deref() != Some(&key) {
                    eprintln!("[clud] warning: gc scanner: {e}");
                    last_error_kind = Some(key);
                }
            }
        }
        // Interruptible sleep: 20 × 100ms = ~2s, but cancellable within
        // 100ms.
        for _ in 0..20 {
            if cancel.load(Ordering::SeqCst) {
                return;
            }
            std::thread::sleep(Duration::from_millis(100));
        }
    }
}

// ---------- CLI handlers ----------

/// Dispatch a `clud gc` invocation. Returns the process exit code.
pub fn run(sub: Option<GcSubcommand>) -> i32 {
    match sub {
        None => print_help_and_exit_zero(),
        Some(GcSubcommand::List) => cmd_list(),
        Some(GcSubcommand::Purge {
            duration,
            dry_run,
            yes,
            kind,
        }) => cmd_purge(&duration, dry_run, yes, kind.as_deref()),
        Some(GcSubcommand::Reconcile) => cmd_reconcile(),
    }
}

fn print_help_and_exit_zero() -> i32 {
    let mut top = Args::command();
    match top.find_subcommand_mut("gc") {
        Some(gc) => {
            let _ = gc.print_help();
            println!();
            0
        }
        None => {
            eprintln!("error: gc subcommand definition missing (internal bug)");
            2
        }
    }
}

fn open_registry_or_log() -> Option<Registry> {
    match Registry::open_default() {
        Ok(r) => Some(r),
        Err(e) => {
            eprintln!("error: {e}");
            None
        }
    }
}

fn cmd_list() -> i32 {
    let Some(registry) = open_registry_or_log() else {
        return 1;
    };
    let rows = match registry.list(None) {
        Ok(v) => v,
        Err(e) => {
            eprintln!("error: list failed: {e}");
            return 1;
        }
    };
    print_table(&rows);
    0
}

fn cmd_reconcile() -> i32 {
    let Some(registry) = open_registry_or_log() else {
        return 1;
    };
    match run_reconcile(&registry) {
        Ok(n) => {
            println!(
                "reconcile: {n} new entr{}",
                if n == 1 { "y" } else { "ies" }
            );
            0
        }
        Err(e) => {
            eprintln!("error: reconcile failed: {e}");
            1
        }
    }
}

fn cmd_purge(duration: &str, dry_run: bool, yes: bool, kind_filter: Option<&str>) -> i32 {
    let dur = match worktrees::parse_duration(duration) {
        Ok(d) => d,
        Err(e) => {
            eprintln!("error: invalid duration: {e}");
            return 2;
        }
    };
    let Some(registry) = open_registry_or_log() else {
        return 1;
    };
    // Run reconcile inline so the purge sees the latest worktrees.
    if let Err(e) = run_reconcile(&registry) {
        eprintln!("[clud] warning: pre-purge reconcile failed: {e}");
    }
    let cutoff = now_unix().saturating_sub(dur.as_secs() as i64);
    let mut candidates = match registry.select_older_than(cutoff, kind_filter) {
        Ok(v) => v,
        Err(e) => {
            eprintln!("error: select failed: {e}");
            return 1;
        }
    };

    // Apply liveness filter for worktree-kind rows: if the worktree is
    // git-locked with `pid <N>` in its reason and the pid is alive, skip.
    let live_locks = collect_live_lock_paths();
    let mut skipped_live: Vec<TrackedEntry> = Vec::new();
    candidates.retain(|c| {
        if c.kind == "worktree" && live_locks.contains(&c.path) {
            skipped_live.push(c.clone());
            false
        } else {
            true
        }
    });

    if candidates.is_empty() {
        if !skipped_live.is_empty() {
            println!(
                "purge: no removable entries (skipped {} with live agent pid)",
                skipped_live.len()
            );
        } else {
            println!("purge: no entries older than {}", duration);
        }
        return 0;
    }

    println!(
        "purge plan ({} candidate(s) older than {duration}):",
        candidates.len()
    );
    for c in &candidates {
        let age = age_of(c);
        println!(
            "  remove  [{}]  {}  (age {}, agent {})",
            c.kind,
            c.path,
            worktrees::fmt_age(age),
            c.agent_id.as_deref().unwrap_or("-"),
        );
    }
    if !skipped_live.is_empty() {
        println!("skipped ({}, live agent pid):", skipped_live.len());
        for s in &skipped_live {
            println!("  skip    [{}]  {}", s.kind, s.path);
        }
    }

    if dry_run {
        println!("\n--dry-run: no changes made.");
        return 0;
    }

    if !yes && !confirm_interactive(candidates.len()) {
        println!("aborted.");
        return 0;
    }

    let mut removed = 0usize;
    let mut failed = 0usize;
    for c in &candidates {
        if let Err(e) = remove_entry_and_delete_row(&registry, c) {
            eprintln!("error: failed to remove {}: {e}", c.path);
            failed += 1;
        } else {
            removed += 1;
        }
    }
    println!(
        "summary: {removed} removed, {skipped} skipped, {failed} failed.",
        skipped = skipped_live.len(),
    );
    if failed > 0 {
        1
    } else {
        0
    }
}

/// Best-effort removal of a worktree (or arbitrary path) followed by the
/// DELETE FROM tracked_entries. Each entry's removal + DELETE is its own
/// transaction so a stuck row doesn't block the others.
fn remove_entry_and_delete_row(registry: &Registry, entry: &TrackedEntry) -> Result<(), String> {
    if entry.kind == "worktree" {
        // Try git first; fall back to plain rm -rf only if the dir still
        // exists after git's refusal.
        let main_root = entry.repo_root.clone().unwrap_or_else(|| ".".to_string());
        let git_result = worktrees::run_git(
            Path::new(&main_root),
            &["worktree", "remove", "--force", &entry.path],
        );
        match git_result {
            Ok(_) => {}
            Err(e) => {
                let dir = Path::new(&entry.path);
                if dir.exists() {
                    std::fs::remove_dir_all(dir)
                        .map_err(|fs_err| format!("git({e}); fs({fs_err})"))?;
                }
                // If the dir is already gone we treat the git failure as
                // a no-op: the row is stale.
            }
        }
    } else {
        // Non-worktree kinds: best-effort directory delete.
        let p = Path::new(&entry.path);
        if p.exists() {
            std::fs::remove_dir_all(p).map_err(|e| format!("remove_dir_all: {e}"))?;
        }
    }
    registry.delete(entry.id).map_err(|e| e.to_string())?;
    Ok(())
}

/// Build the set of worktree paths whose `locked` line names a still-live
/// pid. We probe `git worktree list --porcelain` in the current repo,
/// parse each entry's lock reason, and run the embedded pid through the
/// production `OsLivenessProbe`.
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

fn print_table(rows: &[TrackedEntry]) {
    if rows.is_empty() {
        println!("(no tracked entries)");
        return;
    }
    let now = now_unix();
    let kind_w = rows.iter().map(|r| r.kind.len()).max().unwrap_or(0).max(4);
    let agent_w = rows
        .iter()
        .map(|r| r.agent_id.as_deref().unwrap_or("-").len())
        .max()
        .unwrap_or(0)
        .max(5);
    println!(
        "{:<kind_w$}  {:>6}  {:<agent_w$}  {:<20}  PATH",
        "KIND",
        "AGE",
        "AGENT",
        "BRANCH",
        kind_w = kind_w,
        agent_w = agent_w,
    );
    for r in rows {
        let age = Duration::from_secs((now - r.created_unix).max(0) as u64);
        println!(
            "{:<kind_w$}  {:>6}  {:<agent_w$}  {:<20}  {}",
            r.kind,
            worktrees::fmt_age(age),
            r.agent_id.as_deref().unwrap_or("-"),
            r.branch.as_deref().unwrap_or("-"),
            r.path,
            kind_w = kind_w,
            agent_w = agent_w,
        );
    }
}

fn age_of(r: &TrackedEntry) -> Duration {
    Duration::from_secs((now_unix() - r.created_unix).max(0) as u64)
}

fn confirm_interactive(n: usize) -> bool {
    use std::io::{self, Write};
    print!("remove {n} entry/entries? [y/N] ");
    let _ = io::stdout().flush();
    let mut line = String::new();
    if io::stdin().read_line(&mut line).is_err() {
        return false;
    }
    matches!(line.trim().to_ascii_lowercase().as_str(), "y" | "yes")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::AtomicU32;

    fn fresh_db_path(tag: &str) -> PathBuf {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join(format!("data-{tag}.db"));
        std::mem::forget(dir);
        path
    }

    fn fresh_registry(tag: &str) -> Registry {
        let path = fresh_db_path(tag);
        Registry::open_at(&path).expect("open registry")
    }

    fn upsert(reg: &Registry, kind: &str, path: &str, now: i64) {
        reg.upsert_entry(&UpsertInput {
            kind: kind.to_string(),
            path: path.to_string(),
            repo_root: None,
            branch: None,
            agent_id: None,
            now_unix: now,
        })
        .expect("upsert");
    }

    #[test]
    fn schema_bootstraps_on_first_open() {
        let path = fresh_db_path("bootstrap");
        let _r1 = Registry::open_at(&path).expect("first open");
        let _r2 = Registry::open_at(&path).expect("reopen");
        // Reopening on a populated db must not error out.
    }

    #[test]
    fn upsert_then_list_round_trips() {
        let reg = fresh_registry("rt");
        upsert(&reg, "worktree", "/tmp/a", 100);
        upsert(&reg, "worktree", "/tmp/b", 200);
        let rows = reg.list(None).expect("list");
        assert_eq!(rows.len(), 2);
        // ORDER BY created_unix DESC → /tmp/b first.
        assert_eq!(rows[0].path, "/tmp/b");
        assert_eq!(rows[1].path, "/tmp/a");
    }

    #[test]
    fn upsert_preserves_created_unix_on_conflict() {
        let reg = fresh_registry("upsert-conflict");
        upsert(&reg, "worktree", "/tmp/a", 100);
        upsert(&reg, "worktree", "/tmp/a", 500);
        let rows = reg.list(None).unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].created_unix, 100);
        assert_eq!(rows[0].last_seen_unix, 500);
    }

    #[test]
    fn purge_respects_kind_filter() {
        let reg = fresh_registry("kind-filter");
        upsert(&reg, "worktree", "/tmp/wt-1", 100);
        upsert(&reg, "worktree", "/tmp/wt-2", 100);
        upsert(&reg, "cache", "/tmp/cache-1", 100);
        let cutoff = 500;
        // Filter to worktrees only.
        let older = reg.select_older_than(cutoff, Some("worktree")).unwrap();
        assert_eq!(older.len(), 2);
        assert!(older.iter().all(|r| r.kind == "worktree"));
        // No filter: all 3.
        let older_all = reg.select_older_than(cutoff, None).unwrap();
        assert_eq!(older_all.len(), 3);
    }

    #[test]
    fn delete_removes_one_row() {
        let reg = fresh_registry("delete");
        upsert(&reg, "worktree", "/tmp/a", 100);
        upsert(&reg, "worktree", "/tmp/b", 100);
        let rows = reg.list(None).unwrap();
        assert_eq!(rows.len(), 2);
        reg.delete(rows[0].id).unwrap();
        assert_eq!(reg.count().unwrap(), 1);
    }

    #[test]
    fn retry_succeeds_after_simulated_busy() {
        // Closure returns SQLITE_BUSY twice then Ok.
        let attempts = AtomicU32::new(0);
        let r: rusqlite::Result<i32> = with_busy_retry(|| {
            let n = attempts.fetch_add(1, Ordering::SeqCst);
            if n < 2 {
                Err(rusqlite::Error::SqliteFailure(
                    rusqlite::ffi::Error::new(rusqlite::ffi::SQLITE_BUSY),
                    None,
                ))
            } else {
                Ok(42)
            }
        });
        assert_eq!(r.unwrap(), 42);
        assert_eq!(attempts.load(Ordering::SeqCst), 3);
    }

    #[test]
    fn retry_propagates_non_busy_error() {
        let attempts = AtomicU32::new(0);
        let r: rusqlite::Result<i32> = with_busy_retry(|| {
            attempts.fetch_add(1, Ordering::SeqCst);
            Err(rusqlite::Error::QueryReturnedNoRows)
        });
        assert!(r.is_err());
        // Non-busy errors must not retry.
        assert_eq!(attempts.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn extract_pid_parses_claude_format() {
        assert_eq!(
            extract_pid_from_lock_reason("claude agent agent-abf (pid 12345)"),
            Some(12345)
        );
    }

    #[test]
    fn extract_pid_handles_no_match() {
        assert_eq!(extract_pid_from_lock_reason("manual lock by user"), None);
        assert_eq!(extract_pid_from_lock_reason("pid "), None);
        assert_eq!(extract_pid_from_lock_reason("pid abc"), None);
    }

    #[test]
    fn extract_pid_handles_trailing_text() {
        assert_eq!(
            extract_pid_from_lock_reason("agent (pid 999) running"),
            Some(999)
        );
    }

    #[test]
    fn reconcile_dir_inserts_agent_subdirs() {
        let reg = fresh_registry("reconcile-dir");
        let dir = tempfile::tempdir().unwrap();
        let watch = dir.path().to_path_buf();
        std::fs::create_dir_all(watch.join("agent-abc")).unwrap();
        std::fs::create_dir_all(watch.join("agent-def")).unwrap();
        // Non-agent dir is ignored.
        std::fs::create_dir_all(watch.join("not-an-agent")).unwrap();
        // File at top level is ignored.
        std::fs::write(watch.join("README"), b"hi").unwrap();
        let res = reconcile_dir(&reg, &watch, None).unwrap();
        assert_eq!(res.inserted, 2);
        assert_eq!(res.refreshed, 0);
        let rows = reg.list(None).unwrap();
        assert_eq!(rows.len(), 2);
        assert!(rows.iter().all(|r| r.kind == "worktree"));
        assert!(rows.iter().all(|r| r
            .agent_id
            .as_deref()
            .map(|a| a.starts_with("agent-"))
            .unwrap_or(false)));
    }

    #[test]
    fn reconcile_dir_handles_missing_watch_dir() {
        let reg = fresh_registry("reconcile-missing");
        let dir = tempfile::tempdir().unwrap();
        let watch = dir.path().join("does-not-exist");
        let res = reconcile_dir(&reg, &watch, None).unwrap();
        assert_eq!(res.inserted, 0);
        assert_eq!(reg.count().unwrap(), 0);
    }

    #[test]
    fn reconcile_dir_is_idempotent() {
        let reg = fresh_registry("reconcile-idem");
        let dir = tempfile::tempdir().unwrap();
        let watch = dir.path().to_path_buf();
        std::fs::create_dir_all(watch.join("agent-abc")).unwrap();
        let first = reconcile_dir(&reg, &watch, None).unwrap();
        assert_eq!(first.inserted, 1);
        let before = reg.list(None).unwrap()[0].clone();
        // Sleep enough that now_unix() advances at least one second.
        std::thread::sleep(Duration::from_millis(1100));
        let second = reconcile_dir(&reg, &watch, None).unwrap();
        assert_eq!(second.inserted, 0);
        assert_eq!(second.refreshed, 1);
        let after = reg.list(None).unwrap()[0].clone();
        assert_eq!(after.created_unix, before.created_unix);
        assert!(after.last_seen_unix >= before.last_seen_unix);
    }

    #[test]
    fn scanner_inserts_new_worktree_within_one_cycle() {
        let path = fresh_db_path("scanner-insert");
        let reg = Arc::new(Registry::open_at(&path).unwrap());
        let dir = tempfile::tempdir().unwrap();
        let watch = dir.path().to_path_buf();
        std::fs::create_dir_all(&watch).unwrap();
        let mut scanner = WorktreeScanner::spawn(reg.clone(), watch.clone(), None);
        // Create after spawn, give the loop one cycle to notice.
        std::thread::sleep(Duration::from_millis(300));
        std::fs::create_dir_all(watch.join("agent-fresh")).unwrap();
        // Up to 2 full cycles (~4s) before we declare failure — generous
        // for slow CI hosts.
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        loop {
            if reg.count().unwrap() >= 1 {
                break;
            }
            if std::time::Instant::now() > deadline {
                panic!("scanner did not pick up agent-fresh within 5s");
            }
            std::thread::sleep(Duration::from_millis(150));
        }
        scanner.cancel();
        let rows = reg.list(None).unwrap();
        assert!(rows.iter().any(|r| r.path.ends_with("agent-fresh")));
    }

    #[test]
    fn scanner_cancels_promptly() {
        let path = fresh_db_path("scanner-cancel");
        let reg = Arc::new(Registry::open_at(&path).unwrap());
        let dir = tempfile::tempdir().unwrap();
        let mut scanner = WorktreeScanner::spawn(reg, dir.path().to_path_buf(), None);
        let start = std::time::Instant::now();
        scanner.cancel();
        let elapsed = start.elapsed();
        // The chunked sleep wakes every 100ms; cancellation should be
        // observed well before one full 2s scan cycle. Allow some slack
        // for slow CI runners.
        assert!(
            elapsed < Duration::from_secs(1),
            "cancel took too long: {elapsed:?}"
        );
    }
}
