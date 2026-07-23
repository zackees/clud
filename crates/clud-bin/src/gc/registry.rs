use std::path::{Path, PathBuf};
#[cfg(test)]
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use redb::{Database, ReadableTable, ReadableTableMetadata, TableDefinition};
use serde::{Deserialize, Serialize};

/// Env-var override for the DB path. Mirrors `CLUD_SESSION_DB` in
/// `session_registry.rs`. Tests set this to a tempdir.
pub const ENV_DATA_DB: &str = "CLUD_DATA_DB";

/// redb table: `(kind, path) -> serde_json::to_vec(&TrackedRow)`.
const TRACKED: TableDefinition<(&str, &str), &[u8]> = TableDefinition::new("tracked_entries");

/// redb table: `meta_key -> u64`. Holds `schema_version` and the
/// monotonic `next_id` counter used to mint `TrackedEntry.id` values.
const META: TableDefinition<&str, u64> = TableDefinition::new("meta");

/// redb table (issue #183): `repo_root -> serde_json::to_vec(&RepoVisitRow)`.
/// One row per unique git repo `clud` has been launched in. Upserted on
/// every startup; powers the `Repos` tab of `clud ui` and the
/// `repos[]` array in `/state.json`.
const REPO_VISITS: TableDefinition<&str, &[u8]> = TableDefinition::new("repo_visits");

const META_SCHEMA_VERSION: &str = "schema_version";
const META_NEXT_ID: &str = "next_id";

pub const WORKTREE_KIND: &str = "worktree";
pub const EXTERN_REPO_KIND: &str = "extern-repo";
pub const SIBLING_CLONE_KIND: &str = "sibling-clone";

/// Errors surfaced by `gc`. Narrow on purpose — the CLI handlers
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
    /// list and delete paths keep referring to the same row.
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

/// On-disk row for `REPO_VISITS`. Stored as serde_json bytes keyed by
/// `repo_root`. `run_count` increments on every upsert; `last_cwd` is the
/// CWD of the most recent invocation (may differ from `repo_root` when
/// `clud` was run from a subdirectory of the repo).
#[derive(Debug, Clone, Serialize, Deserialize)]
struct RepoVisitRow {
    last_visited_unix: i64,
    run_count: u64,
    last_cwd: String,
}

/// One row from the `repo_visits` table. Public so the daemon HTTP
/// handler and `clud ui` JSON output can serialize it directly.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct RepoVisit {
    pub repo_root: String,
    pub last_visited_unix: i64,
    pub run_count: u64,
    pub last_cwd: String,
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
    #[cfg(test)]
    insert_write_transactions: AtomicUsize,
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
        Ok(Self {
            db,
            #[cfg(test)]
            insert_write_transactions: AtomicUsize::new(0),
        })
    }

    fn bootstrap_schema(db: &Database) -> Result<(), GcError> {
        let txn = db.begin_write()?;
        {
            let _ = txn.open_table(TRACKED)?;
            // Issue #183: opens (or creates) the `repo_visits` table on every
            // daemon start. Existing data.redb files predate this table and
            // will auto-migrate on first open — redb's `open_table` is
            // create-if-missing.
            let _ = txn.open_table(REPO_VISITS)?;
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

    /// Issue #183: upsert one `repo_visits` row. `repo_root` is the key.
    /// On first call inserts `{last_visited_unix, run_count: 1, last_cwd}`;
    /// subsequent calls bump `run_count` and overwrite `last_visited_unix`
    /// and `last_cwd`.
    pub fn record_repo_visit(
        &self,
        repo_root: &str,
        cwd: &str,
        now_unix: i64,
    ) -> Result<(), GcError> {
        let wtxn = self.db.begin_write()?;
        {
            let mut table = wtxn.open_table(REPO_VISITS)?;
            let prior = table.get(repo_root)?;
            let prior_count: u64 = match prior.as_ref() {
                Some(g) => serde_json::from_slice::<RepoVisitRow>(g.value())?.run_count,
                None => 0,
            };
            // Drop the read guard before re-borrowing `table` mutably.
            drop(prior);
            let row = RepoVisitRow {
                last_visited_unix: now_unix,
                run_count: prior_count + 1,
                last_cwd: cwd.to_string(),
            };
            let bytes = serde_json::to_vec(&row)?;
            table.insert(repo_root, bytes.as_slice())?;
        }
        wtxn.commit()?;
        Ok(())
    }

    /// Issue #183: return every `repo_visits` row, newest first.
    pub fn list_repo_visits(&self) -> Result<Vec<RepoVisit>, GcError> {
        let rtxn = self.db.begin_read()?;
        let table = rtxn.open_table(REPO_VISITS)?;
        let mut out = Vec::new();
        for entry in table.iter()? {
            let (k, v) = entry?;
            let row: RepoVisitRow = serde_json::from_slice(v.value())?;
            out.push(RepoVisit {
                repo_root: k.value().to_string(),
                last_visited_unix: row.last_visited_unix,
                run_count: row.run_count,
                last_cwd: row.last_cwd,
            });
        }
        out.sort_by(|a, b| b.last_visited_unix.cmp(&a.last_visited_unix));
        Ok(out)
    }

    /// Insert a new entry keyed by `(kind, path)`. **No-op if a row with
    /// the same `(kind, path)` already exists** — the existing row is
    /// left exactly as-is (no field updates, no write). This is the
    /// scanner-friendly contract: `WorktreeScanner` calls this every
    /// cycle, and we want to avoid ~0.5 writes/sec of pure churn.
    /// Returns `true` when a row was inserted and `false` when it already
    /// existed, so callers can avoid recording no-op mutations.
    pub fn insert_if_new(&self, input: &InsertInput) -> Result<bool, GcError> {
        if registry_has_entry(self, &input.kind, &input.path)? {
            // The daemon registry worker serializes inserts, so this
            // read-before-write probe has no check-then-write race.
            return Ok(false);
        }
        #[cfg(test)]
        self.insert_write_transactions
            .fetch_add(1, Ordering::Relaxed);
        let wtxn = self.db.begin_write()?;
        {
            let mut table = wtxn.open_table(TRACKED)?;
            let key = (input.kind.as_str(), input.path.as_str());
            if table.get(key)?.is_some() {
                // Row already present — explicit no-op. Drop the txn
                // without committing.
                drop(table);
                drop(wtxn);
                return Ok(false);
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
        Ok(true)
    }

    #[cfg(test)]
    pub(in crate::gc) fn insert_write_transactions(&self) -> usize {
        self.insert_write_transactions.load(Ordering::Relaxed)
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

pub(in crate::gc) fn now_unix() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

pub(in crate::gc) fn registry_has_entry(
    registry: &Registry,
    kind: &str,
    path: &str,
) -> Result<bool, GcError> {
    let rtxn = registry.db.begin_read()?;
    let table = rtxn.open_table(TRACKED)?;
    Ok(table.get((kind, path))?.is_some())
}
