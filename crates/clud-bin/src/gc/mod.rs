//! `clud gc` — tracked-entry garbage collection (issue #110).
//!
//! Background: Claude Code creates per-agent git worktrees under
//! `.claude/worktrees/agent-<id>/` whenever a subagent runs with
//! `isolation: "worktree"`. Over a long debugging session these accumulate
//! across repos and across `clud` invocations, and the existing
//! `--clean-worktrees` flag only knows about the current repo. This module
//! adds a per-user `redb` registry of every tracked entry, plus CLI
//! handlers for `list`, `prune`, explicit destructive `purge`, `all`,
//! and `reconcile`.
//!
//! Storage lives in a `tracked_entries` redb table keyed by `(kind, path)`
//! whose value is a JSON-serialized row. The `kind` field is generic so
//! future kinds (caches, daemon state) drop in without a migration.
//!
//! Foreground launches register conventional discovery roots with the session
//! daemon. The daemon owns one deduplicated OS watcher per root plus a bounded
//! fallback reconciliation schedule; it inserts only previously unseen rows.

mod cli;
mod reconcile;
mod registry;
mod scanner;
pub mod session_tmp;
pub mod target_sweep;
pub mod uv_cache;

pub use cli::run;
pub use reconcile::{
    extract_pid_from_lock_reason, reconcile_dir, reconcile_extern_repos_dir, reconcile_repo_root,
    reconcile_sibling_clones_dir, run_reconcile, ScanResult,
};
pub(crate) use reconcile::{reconcile_registered_dir, watch_event_may_affect_registration};
pub use registry::{
    default_data_db_path, GcError, InsertInput, Registry, RepoVisit, TrackedEntry, ENV_DATA_DB,
    EXTERN_REPO_KIND, SIBLING_CLONE_KIND, WORKTREE_KIND,
};
pub use scanner::watch_roots_for_current_repo;

#[cfg(test)]
use reconcile::is_sibling_clone_dir_name;
#[cfg(test)]
use std::path::PathBuf;
#[cfg(test)]
use std::time::Duration;

#[cfg(test)]
#[path = "../gc_tests.rs"]
mod tests;
