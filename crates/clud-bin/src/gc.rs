//! `clud gc` — tracked-entry garbage collection (issue #110).
//!
//! Background: Claude Code creates per-agent git worktrees under
//! `.claude/worktrees/agent-<id>/` whenever a subagent runs with
//! `isolation: "worktree"`. Over a long debugging session these accumulate
//! across repos and across `clud` invocations, and the existing
//! `--clean-worktrees` flag only knows about the current repo. This module
//! adds a per-user `redb` registry of every tracked entry, plus three CLI
//! handlers (`list`, `purge`, `reconcile`).
//!
//! Storage lives in a `tracked_entries` redb table keyed by `(kind, path)`
//! whose value is a JSON-serialized row. The `kind` field is generic so
//! future kinds (caches, daemon state) drop in without a migration.
//!
//! The DB also gets watched by a background `WorktreeScanner` thread,
//! spawned from `main.rs` for the lifetime of a normal `clud` launch.
//! It polls `.claude/worktrees/` every ~2 seconds and inserts any new
//! `agent-*` directory it spots. **Existing rows are left alone** —
//! the scanner is insert-only, no write churn on every cycle.
//! Cancellation is cooperative via an `Arc<AtomicBool>`; `Drop` joins
//! the thread.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread::JoinHandle;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use clap::CommandFactory;
use redb::{Database, ReadableTable, ReadableTableMetadata, TableDefinition};
use serde::{Deserialize, Serialize};

use crate::args::{Args, GcSubcommand};
use crate::worktrees;

/// Env-var override for the DB path. Mirrors `CLUD_SESSION_DB` in
/// `session_registry.rs`. Tests set this to a tempdir.
pub const ENV_DATA_DB: &str = "CLUD_DATA_DB";

/// redb table: `(kind, path) -> serde_json::to_vec(&TrackedRow)`.
const TRACKED: TableDefinition<(&str, &str), &[u8]> = TableDefinition::new("tracked_entries");

/// redb table: `meta_key -> u64`. Holds `schema_version` and the
/// monotonic `next_id` counter used to mint `TrackedEntry.id` values.
const META: TableDefinition<&str, u64> = TableDefinition::new("meta");

const META_SCHEMA_VERSION: &str = "schema_version";
const META_NEXT_ID: &str = "next_id";

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
            Self::Sql(m) => write!(f, "gc db error: {m}"),
        }
    }
}

impl std::error::Error for GcError {}

macro_rules! impl_from_redb {
    ($($t:ty),* $(,)?) => {
        $(
            impl From<$t> for GcError {
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

impl From<serde_json::Error> for GcError {
    fn from(e: serde_json::Error) -> Self {
        Self::Sql(format!("serde: {e}"))
    }
}

/// On-disk row, stored as serde_json bytes under the `(kind, path)` key.
/// Kept separate from the public `TrackedEntry` so the file format can
/// evolve independently of the API.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct TrackedRow {
    /// Monotonic id assigned at insert time. Stable across reopens so
    /// `gc list` / `gc purge` keep referring to the same row.
    id: i64,
    repo_root: Option<String>,
    branch: Option<String>,
    agent_id: Option<String>,
    created_unix: i64,
}

/// One entry from the `tracked_entries` table.
///
/// **NOTE on `last_seen_unix`**: that field used to live here and on the
/// SQLite schema, but `WorktreeScanner` was upserting it every ~2s with
/// no consumer (purge filters on `created_unix`). It was removed when the
/// scanner switched from upsert to insert-on-first-detection.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TrackedEntry {
    pub id: i64,
    pub kind: String,
    pub path: String,
    pub repo_root: Option<String>,
    pub branch: Option<String>,
    pub agent_id: Option<String>,
    pub created_unix: i64,
}

/// Inputs for `insert_if_new`. `now_unix` is used as `created_unix` on
/// first insertion; a no-op repeat insertion leaves the original row
/// (and its `created_unix`) untouched.
#[derive(Debug, Clone)]
pub struct InsertInput {
    pub kind: String,
    pub path: String,
    pub repo_root: Option<String>,
    pub branch: Option<String>,
    pub agent_id: Option<String>,
    pub now_unix: i64,
}

/// `redb`-backed registry of tracked entries.
///
/// `redb::Database` serializes writers at the file level, so we hold a
/// single `Database` handle and rely on its own concurrency control. The
/// `next_id` counter lives in the `META` table and is read+bumped inside
/// the same `WriteTransaction` as the insert, so two concurrent
/// `insert_if_new` callers can never mint duplicate ids.
pub struct Registry {
    db: Database,
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
        let db = Database::create(path)?;
        Self::bootstrap_schema(&db)?;
        Ok(Self { db })
    }

    fn bootstrap_schema(db: &Database) -> Result<(), GcError> {
        let txn = db.begin_write()?;
        {
            let _ = txn.open_table(TRACKED)?;
            let mut meta = txn.open_table(META)?;
            if meta.get(META_SCHEMA_VERSION)?.is_none() {
                meta.insert(META_SCHEMA_VERSION, 1u64)?;
            }
            if meta.get(META_NEXT_ID)?.is_none() {
                meta.insert(META_NEXT_ID, 1u64)?;
            }
        }
        txn.commit()?;
        Ok(())
    }

    /// Insert a new entry keyed by `(kind, path)`. **No-op if a row with
    /// the same `(kind, path)` already exists** — the existing row is
    /// left exactly as-is (no field updates, no write). This is the
    /// scanner-friendly contract: `WorktreeScanner` calls this every
    /// cycle, and we want to avoid ~0.5 writes/sec of pure churn.
    pub fn insert_if_new(&self, input: &InsertInput) -> Result<(), GcError> {
        let wtxn = self.db.begin_write()?;
        {
            let mut table = wtxn.open_table(TRACKED)?;
            let key = (input.kind.as_str(), input.path.as_str());
            if table.get(key)?.is_some() {
                // Row already present — explicit no-op. Drop the txn
                // without committing.
                drop(table);
                drop(wtxn);
                return Ok(());
            }
            // Mint a new id from META[next_id].
            let mut meta = wtxn.open_table(META)?;
            let cur = meta.get(META_NEXT_ID)?.map(|g| g.value()).unwrap_or(1);
            meta.insert(META_NEXT_ID, cur + 1)?;
            drop(meta);

            let row = TrackedRow {
                id: cur as i64,
                repo_root: input.repo_root.clone(),
                branch: input.branch.clone(),
                agent_id: input.agent_id.clone(),
                created_unix: input.now_unix,
            };
            let bytes = serde_json::to_vec(&row)?;
            table.insert(key, bytes.as_slice())?;
        }
        wtxn.commit()?;
        Ok(())
    }

    /// Return every row, newest first. Optionally filter by kind.
    pub fn list(&self, filter_kind: Option<&str>) -> Result<Vec<TrackedEntry>, GcError> {
        let mut rows = self.collect_all(filter_kind)?;
        // ORDER BY created_unix DESC.
        rows.sort_by(|a, b| b.created_unix.cmp(&a.created_unix));
        Ok(rows)
    }

    /// Fetch rows whose `created_unix` is strictly less than `cutoff`.
    /// Optionally filter by kind. Sorted ascending by `created_unix` so
    /// purge processes the oldest rows first.
    pub fn select_older_than(
        &self,
        cutoff: i64,
        filter_kind: Option<&str>,
    ) -> Result<Vec<TrackedEntry>, GcError> {
        let mut rows = self.collect_all(filter_kind)?;
        rows.retain(|r| r.created_unix < cutoff);
        rows.sort_by(|a, b| a.created_unix.cmp(&b.created_unix));
        Ok(rows)
    }

    /// Delete a single entry by id. The id is matched against the stored
    /// `TrackedRow::id` — a full table scan, but tracked_entries is
    /// bounded by the number of agent worktrees ever seen (low hundreds)
    /// so this is cheap in practice.
    pub fn delete(&self, id: i64) -> Result<(), GcError> {
        // First, locate the key with this id under a read txn.
        let target: Option<(String, String)> = {
            let rtxn = self.db.begin_read()?;
            let table = rtxn.open_table(TRACKED)?;
            let mut found = None;
            for entry in table.iter()? {
                let (k, v) = entry?;
                let row: TrackedRow = serde_json::from_slice(v.value())?;
                if row.id == id {
                    let (kind, path) = k.value();
                    found = Some((kind.to_string(), path.to_string()));
                    break;
                }
            }
            found
        };
        let Some((kind, path)) = target else {
            // No row with that id — treat as success (idempotent delete).
            return Ok(());
        };
        let wtxn = self.db.begin_write()?;
        {
            let mut table = wtxn.open_table(TRACKED)?;
            table.remove((kind.as_str(), path.as_str()))?;
        }
        wtxn.commit()?;
        Ok(())
    }

    /// Count rows. Mostly for tests.
    pub fn count(&self) -> Result<u64, GcError> {
        let rtxn = self.db.begin_read()?;
        let table = rtxn.open_table(TRACKED)?;
        Ok(table.len()?)
    }

    /// Internal helper: read every row, optionally filtered by kind, into
    /// `TrackedEntry`s. Iteration order is `redb`'s natural sort by key
    /// — callers re-sort as needed.
    fn collect_all(&self, filter_kind: Option<&str>) -> Result<Vec<TrackedEntry>, GcError> {
        let rtxn = self.db.begin_read()?;
        let table = rtxn.open_table(TRACKED)?;
        let mut out = Vec::new();
        for entry in table.iter()? {
            let (k, v) = entry?;
            let (kind, path) = k.value();
            if let Some(want) = filter_kind {
                if kind != want {
                    continue;
                }
            }
            let row: TrackedRow = serde_json::from_slice(v.value())?;
            out.push(TrackedEntry {
                id: row.id,
                kind: kind.to_string(),
                path: path.to_string(),
                repo_root: row.repo_root,
                branch: row.branch,
                agent_id: row.agent_id,
                created_unix: row.created_unix,
            });
        }
        Ok(out)
    }
}

/// Resolve the default DB path: `~/.clud/data.redb`. `CLUD_DATA_DB`
/// overrides.
pub fn default_data_db_path() -> Result<PathBuf, GcError> {
    let home = dirs::home_dir().ok_or(GcError::NoDefaultPath)?;
    Ok(home.join(".clud").join("data.redb"))
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

/// Walk `.claude/worktrees/` in the *current* repo and insert any
/// previously-untracked agent-* subdirectory we find. Returns the number
/// of new entries (i.e. rows that didn't previously exist).
pub fn run_reconcile(registry: &Registry) -> Result<usize, GcError> {
    let main_root = worktrees::locate_main_repo_root().map_err(GcError::Io)?;
    let watch_dir = main_root.join(".claude").join("worktrees");
    reconcile_dir(registry, &watch_dir, Some(&main_root)).map(|res| res.inserted)
}

/// Result of one scan pass.
///
/// `skipped` counts rows that were already present in the registry — the
/// scanner's intentional "insert-once" behavior. (Previously the scanner
/// updated `last_seen_unix` on every cycle and reported these as
/// `refreshed`; that field is gone, so the field is renamed to `skipped`
/// to reflect the new contract.)
#[derive(Debug, Default, Clone, Copy)]
pub struct ScanResult {
    pub inserted: usize,
    pub skipped: usize,
}

/// Walk `watch_dir` and insert each immediate subdir whose name starts
/// with `agent-` if it isn't already tracked. Returns counts of
/// inserted-vs-skipped rows.
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

        let existed_before = registry_has_entry(registry, "worktree", &path_str)?;

        if existed_before {
            // No-op insert; just bump the skipped counter.
            res.skipped += 1;
            continue;
        }

        let branch = best_effort_branch(&path);
        let input = InsertInput {
            kind: "worktree".to_string(),
            path: path_str,
            repo_root: repo_root.map(|p| p.to_string_lossy().to_string()),
            branch,
            agent_id: Some(name_str.to_string()),
            now_unix: now_unix(),
        };
        registry.insert_if_new(&input)?;
        res.inserted += 1;
    }
    Ok(res)
}

fn registry_has_entry(registry: &Registry, kind: &str, path: &str) -> Result<bool, GcError> {
    let rtxn = registry.db.begin_read()?;
    let table = rtxn.open_table(TRACKED)?;
    Ok(table.get((kind, path))?.is_some())
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
///
/// Issue #135 Phase 1: the scanner now sends `gc.insert` IPC ops to the
/// daemon instead of opening redb directly. If the daemon is unreachable
/// the scanner logs once at debug level and stops trying for the rest of
/// the session. Phase 2 moves this entire scanner into the daemon
/// process; for now the scanner thread still lives in the clud-bin
/// process.
pub struct WorktreeScanner {
    cancel: Arc<AtomicBool>,
    handle: Option<JoinHandle<()>>,
}

impl WorktreeScanner {
    /// Spawn a scanner watching the *current* repo's `.claude/worktrees/`.
    /// Returns `None` if the repo root can't be located — the caller logs
    /// and continues.
    pub fn maybe_spawn() -> Option<Self> {
        let main_root = match worktrees::locate_main_repo_root() {
            Ok(p) => p,
            Err(_) => {
                // Not inside a git repo (e.g. running `clud` from /tmp).
                // No worktrees to scan — skip spawning.
                return None;
            }
        };
        let watch_dir = main_root.join(".claude").join("worktrees");
        Some(Self::spawn(watch_dir, Some(main_root)))
    }

    /// Explicit spawn. Tests pass a custom watch dir. Inserts go through
    /// the GC daemon IPC; if the daemon is unreachable the scanner gives
    /// up silently.
    pub fn spawn(watch_dir: PathBuf, repo_root: Option<PathBuf>) -> Self {
        let cancel = Arc::new(AtomicBool::new(false));
        let cancel_t = cancel.clone();
        let handle = std::thread::Builder::new()
            .name("clud-gc-scanner".to_string())
            .spawn(move || run_scanner_loop(watch_dir, repo_root, cancel_t))
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

/// Walk `watch_dir` once, sending `gc.insert` IPC ops for each agent-*
/// subdir found. Returns `Err` on the first IPC failure so the caller
/// can stop retrying.
fn scan_once_via_ipc(watch_dir: &Path, repo_root: Option<&Path>) -> Result<(), String> {
    let entries = match std::fs::read_dir(watch_dir) {
        Ok(it) => it,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(e) => return Err(format!("read_dir({:?}): {e}", watch_dir)),
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
            Some(s) => s.to_string(),
            None => continue,
        };
        if !name_str.starts_with("agent-") {
            continue;
        }
        let path = entry.path();
        let path_str = path.to_string_lossy().to_string();
        let branch = best_effort_branch(&path);
        let input = InsertInput {
            kind: "worktree".to_string(),
            path: path_str,
            repo_root: repo_root.map(|p| p.to_string_lossy().to_string()),
            branch,
            agent_id: Some(name_str),
            now_unix: now_unix(),
        };
        let state_dir = crate::daemon::default_state_dir().map_err(|e| e.to_string())?;
        crate::daemon::gc_client_insert(&state_dir, &input).map_err(|e| e.to_string())?;
    }
    Ok(())
}

fn run_scanner_loop(watch_dir: PathBuf, repo_root: Option<PathBuf>, cancel: Arc<AtomicBool>) {
    let repo_root_ref = repo_root.as_deref();
    let mut ipc_failed = false;
    while !cancel.load(Ordering::SeqCst) {
        if !ipc_failed {
            if let Err(e) = scan_once_via_ipc(&watch_dir, repo_root_ref) {
                // Best-effort: log once, then stop trying.
                if std::env::var_os("CLUD_GC_SCANNER_VERBOSE").is_some() {
                    eprintln!("[clud] debug: gc scanner: daemon ipc failed: {e}");
                }
                ipc_failed = true;
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
//
// Issue #135: the CLI no longer opens the redb directly. Every subcommand
// is a thin IPC client against the always-on session daemon, which now
// owns the redb handle and serializes all reads/writes through a single
// registry worker thread (see `daemon/gc_service.rs`). `--no-daemon` (or
// `CLUD_NO_DAEMON=1`) on any `clud gc` op is an error — there is no
// read-only fallback.

/// Dispatch a `clud gc` invocation. Returns the process exit code.
pub fn run(args: &Args, sub: Option<GcSubcommand>) -> i32 {
    // Bare `clud gc` keeps printing help and does NOT contact the daemon.
    if sub.is_none() {
        return print_help_and_exit_zero();
    }
    if args.no_daemon || daemon_disabled_via_env() {
        eprintln!("error: gc operations require the clud daemon; remove --no-daemon");
        return 2;
    }
    let state_dir = match crate::daemon::default_state_dir() {
        Ok(p) => p,
        Err(e) => {
            eprintln!("error: cannot resolve clud state dir: {e}");
            return 1;
        }
    };
    match sub.unwrap() {
        GcSubcommand::List { json } => cmd_list(&state_dir, json),
        GcSubcommand::Purge {
            duration,
            dry_run,
            yes,
            kind,
        } => cmd_purge(
            &state_dir,
            duration.as_deref(),
            dry_run,
            yes,
            kind.as_deref(),
        ),
        GcSubcommand::Reconcile => cmd_reconcile(&state_dir),
    }
}

fn daemon_disabled_via_env() -> bool {
    std::env::var_os(crate::daemon::ENV_NO_DAEMON)
        .map(|v| v == "1" || v.eq_ignore_ascii_case("true"))
        .unwrap_or(false)
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

fn cmd_list(state_dir: &Path, json: bool) -> i32 {
    let rows = match crate::daemon::gc_client_list(state_dir, None) {
        Ok(v) => v,
        Err(e) => {
            eprintln!("error: list failed: {e}");
            return 1;
        }
    };
    if json {
        match serde_json::to_string(&rows) {
            Ok(s) => println!("{}", s),
            Err(e) => {
                eprintln!("error: serialize failed: {e}");
                return 1;
            }
        }
        return 0;
    }
    print_table_from_rows(&rows);
    0
}

fn cmd_reconcile(state_dir: &Path) -> i32 {
    let main_root = match worktrees::locate_main_repo_root() {
        Ok(p) => p,
        Err(e) => {
            eprintln!("error: reconcile requires a git repo: {e}");
            return 1;
        }
    };
    match crate::daemon::gc_client_reconcile(state_dir, &main_root) {
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

fn cmd_purge(
    state_dir: &Path,
    duration: Option<&str>,
    dry_run: bool,
    yes: bool,
    kind_filter: Option<&str>,
) -> i32 {
    // Pre-flight: validate the duration string before contacting the
    // daemon (gives a clean exit-2 with a specific message for malformed
    // input).
    if let Some(d) = duration {
        if let Err(e) = worktrees::parse_duration(d) {
            eprintln!("error: invalid duration: {e}");
            return 2;
        }
    }

    // Interactive safety prompt for purge-all (no duration). When `--yes`
    // is passed, skip. When `--dry-run` is passed, the daemon does not
    // actually delete anything anyway.
    if !dry_run && !yes && duration.is_none() && !confirm_purge_all() {
        println!("aborted.");
        return 0;
    }

    // Pre-purge reconcile so the daemon's view matches the current repo's
    // `.claude/worktrees/`. Best-effort.
    if let Ok(main_root) = worktrees::locate_main_repo_root() {
        let _ = crate::daemon::gc_client_reconcile(state_dir, &main_root);
    }

    match crate::daemon::gc_client_purge(state_dir, duration, kind_filter, dry_run) {
        Ok((removed, skipped)) => {
            if dry_run {
                println!("--dry-run: would remove {removed}, skip {skipped}.");
            } else {
                println!("summary: removed {removed}, skipped {skipped}.");
            }
            0
        }
        Err(e) => {
            eprintln!("error: purge failed: {e}");
            1
        }
    }
}

fn print_table_from_rows(rows: &[crate::daemon::ListRow]) {
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

fn confirm_purge_all() -> bool {
    use std::io::{self, Write};
    print!("purge ALL non-live-locked entries? [y/N] ");
    let _ = io::stdout().flush();
    let mut line = String::new();
    if io::stdin().read_line(&mut line).is_err() {
        return false;
    }
    matches!(line.trim().to_ascii_lowercase().as_str(), "y" | "yes")
}

#[cfg(test)]
#[path = "gc_tests.rs"]
mod tests;
