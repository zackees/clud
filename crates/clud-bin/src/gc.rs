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
use crate::session_registry::{LivenessProbe, OsLivenessProbe};
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
/// row delete. Each entry's removal + delete is its own transaction so a
/// stuck row doesn't block the others.
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

    fn fresh_db_path(tag: &str) -> PathBuf {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join(format!("data-{tag}.redb"));
        std::mem::forget(dir);
        path
    }

    fn fresh_registry(tag: &str) -> Registry {
        let path = fresh_db_path(tag);
        Registry::open_at(&path).expect("open registry")
    }

    fn insert(reg: &Registry, kind: &str, path: &str, now: i64) {
        reg.insert_if_new(&InsertInput {
            kind: kind.to_string(),
            path: path.to_string(),
            repo_root: None,
            branch: None,
            agent_id: None,
            now_unix: now,
        })
        .expect("insert");
    }

    #[test]
    fn schema_bootstraps_on_first_open() {
        let path = fresh_db_path("bootstrap");
        let _r1 = Registry::open_at(&path).expect("first open");
        drop(_r1);
        let _r2 = Registry::open_at(&path).expect("reopen");
        // Reopening on a populated db must not error out.
    }

    #[test]
    fn insert_then_list_round_trips() {
        let reg = fresh_registry("rt");
        insert(&reg, "worktree", "/tmp/a", 100);
        insert(&reg, "worktree", "/tmp/b", 200);
        let rows = reg.list(None).expect("list");
        assert_eq!(rows.len(), 2);
        // ORDER BY created_unix DESC → /tmp/b first.
        assert_eq!(rows[0].path, "/tmp/b");
        assert_eq!(rows[1].path, "/tmp/a");
    }

    #[test]
    fn insert_if_new_is_noop_on_existing() {
        // The scanner-behavior contract: a second call on the same
        // (kind, path) leaves the original row untouched. The original
        // `created_unix` must survive, and no field is updated.
        let reg = fresh_registry("noop-existing");
        insert(&reg, "worktree", "/tmp/a", 100);
        let before = reg.list(None).unwrap()[0].clone();
        // Second insert with a later timestamp must be a no-op.
        reg.insert_if_new(&InsertInput {
            kind: "worktree".to_string(),
            path: "/tmp/a".to_string(),
            repo_root: Some("/repo".to_string()),
            branch: Some("main".to_string()),
            agent_id: Some("agent-x".to_string()),
            now_unix: 500,
        })
        .unwrap();
        let after = reg.list(None).unwrap();
        assert_eq!(after.len(), 1);
        assert_eq!(after[0].created_unix, 100, "created_unix must not change");
        assert_eq!(after[0].repo_root, before.repo_root);
        assert_eq!(after[0].branch, before.branch);
        assert_eq!(after[0].agent_id, before.agent_id);
        assert_eq!(
            after[0].id, before.id,
            "id must be stable across re-inserts"
        );
    }

    #[test]
    fn purge_respects_kind_filter() {
        let reg = fresh_registry("kind-filter");
        insert(&reg, "worktree", "/tmp/wt-1", 100);
        insert(&reg, "worktree", "/tmp/wt-2", 100);
        insert(&reg, "cache", "/tmp/cache-1", 100);
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
        insert(&reg, "worktree", "/tmp/a", 100);
        insert(&reg, "worktree", "/tmp/b", 100);
        let rows = reg.list(None).unwrap();
        assert_eq!(rows.len(), 2);
        reg.delete(rows[0].id).unwrap();
        assert_eq!(reg.count().unwrap(), 1);
    }

    #[test]
    fn delete_on_missing_id_is_noop() {
        // The redb-backed delete scans for a matching id and silently
        // succeeds if nothing matches. This is intentional — `gc purge`
        // can fire the delete after the row has already been removed by
        // a concurrent operation, and we don't want to error out.
        let reg = fresh_registry("delete-missing");
        insert(&reg, "worktree", "/tmp/a", 100);
        // Try to delete a never-issued id.
        reg.delete(9999).unwrap();
        assert_eq!(reg.count().unwrap(), 1);
    }

    #[test]
    fn ids_are_monotonic_across_inserts() {
        // The id counter is stored in the META table. Insert several rows
        // and confirm the ids strictly increase. (`gc purge` references
        // rows by id, so this needs to be stable.)
        let reg = fresh_registry("ids-mono");
        insert(&reg, "worktree", "/tmp/a", 100);
        insert(&reg, "worktree", "/tmp/b", 100);
        insert(&reg, "worktree", "/tmp/c", 100);
        let rows = reg.list(None).unwrap();
        let mut ids: Vec<i64> = rows.iter().map(|r| r.id).collect();
        ids.sort();
        // ids must be strictly increasing and distinct.
        for w in ids.windows(2) {
            assert!(w[0] < w[1], "ids must be strictly increasing: {:?}", ids);
        }
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
        assert_eq!(res.skipped, 0);
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
    fn reconcile_dir_is_idempotent_and_does_not_churn() {
        // Two passes over the same directory: the second pass must report
        // 0 inserted / 1 skipped, and the row's `created_unix` must
        // *not* change between passes — the new scanner contract is
        // "insert once, leave alone".
        let reg = fresh_registry("reconcile-idem");
        let dir = tempfile::tempdir().unwrap();
        let watch = dir.path().to_path_buf();
        std::fs::create_dir_all(watch.join("agent-abc")).unwrap();
        let first = reconcile_dir(&reg, &watch, None).unwrap();
        assert_eq!(first.inserted, 1);
        assert_eq!(first.skipped, 0);
        let before = reg.list(None).unwrap()[0].clone();
        // Sleep so that now_unix() would advance — if anything *did*
        // write the row, we'd notice via a changed created_unix.
        std::thread::sleep(Duration::from_millis(1100));
        let second = reconcile_dir(&reg, &watch, None).unwrap();
        assert_eq!(second.inserted, 0);
        assert_eq!(second.skipped, 1);
        let after = reg.list(None).unwrap()[0].clone();
        assert_eq!(
            after.created_unix, before.created_unix,
            "second pass must not modify the existing row"
        );
        assert_eq!(after.id, before.id, "id must remain stable");
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
