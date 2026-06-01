//! GC service running inside the always-on session daemon.
//!
//! Owns the redb registry exclusively (issue #135). All `clud gc *`
//! IPC ops on the session daemon's TCP listener get routed to a single
//! registry worker thread; the worker is the sole reader/writer of
//! `~/.clud/data.redb`. This module replaces the standalone `gc_daemon`
//! process that shipped in Phase 1 of #135 — there is now exactly one
//! background daemon per user (see [docs/architecture/gc-and-registry.md]).

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::{mpsc, Arc};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use running_process::{NativeProcess, ProcessConfig, ReadStatus, StderrMode, StdinMode};

use crate::gc::{
    extract_pid_from_lock_reason, reconcile_repo_root, InsertInput, Registry, TrackedEntry,
    EXTERN_REPO_KIND, SIBLING_CLONE_KIND, WORKTREE_KIND,
};
use crate::session_registry::{LivenessProbe, OsLivenessProbe};
use crate::subprocess;
use crate::win_creation_flags::invisible_helper_creationflags;
use crate::worktrees;

use super::types::{GcOp, GcReply, ListRow};

/// How long a connection thread waits for the registry worker before
/// giving up. Generous because purge can rm-rf large trees synchronously.
pub(super) const WORKER_REPLY_TIMEOUT: Duration = Duration::from_secs(30);

const ENV_GC_TICK_SECS: &str = "CLUD_GC_TICK_SECS";
const ENV_GC_EXTERN_REPO_MAX_AGE_SECS: &str = "CLUD_GC_EXTERN_REPO_MAX_AGE_SECS";
const ENV_GC_WARN_FREE_GB: &str = "CLUD_GC_WARN_FREE_GB";
const ENV_GC_AUTO_PURGE_FREE_GB: &str = "CLUD_GC_AUTO_PURGE_FREE_GB";
const ENV_GC_MIN_AGE_HOURS: &str = "CLUD_GC_MIN_AGE_HOURS";
const ENV_GC_AUTO_PURGE_ENABLED: &str = "CLUD_GC_AUTO_PURGE_ENABLED";
const DEFAULT_GC_TICK_SECS: u64 = 3600;
const DEFAULT_EXTERN_REPO_STALE_AFTER_SECS: u64 = 24 * 60 * 60;
const DEFAULT_GC_WARN_FREE_GB: u64 = 10;
const DEFAULT_GC_AUTO_PURGE_FREE_GB: u64 = 5;
const DEFAULT_GC_MIN_AGE_HOURS: u64 = 24;
const DEFAULT_GC_AUTO_PURGE_ENABLED: bool = true;
const PERIODIC_GC_WORKTREE_STALE_AFTER: &str = "48h";
const BYTES_PER_GB: u64 = 1024 * 1024 * 1024;

#[cfg(test)]
const ENV_TEST_GH_BIN: &str = "CLUD_TEST_GH_BIN";

/// One request handed from a connection thread to the registry worker.
pub(super) struct GcRequestMsg {
    pub(super) op: GcOp,
    pub(super) reply_tx: mpsc::SyncSender<GcReply>,
}

type LiveCwdsProvider = Arc<dyn Fn() -> Vec<PathBuf> + Send + Sync + 'static>;

/// Open the registry and spawn the single worker thread. Returns the
/// sender every connection thread uses to dispatch GC ops. Caller keeps
/// the sender alive for the daemon's lifetime; dropping it stops the
/// worker.
#[cfg(test)]
pub(super) fn spawn_registry_worker() -> std::io::Result<mpsc::Sender<GcRequestMsg>> {
    let registry = Registry::open_default().map_err(std::io::Error::other)?;
    spawn_registry_worker_with(registry)
}

pub(super) fn spawn_registry_worker_for_state(
    state_dir: PathBuf,
) -> std::io::Result<mpsc::Sender<GcRequestMsg>> {
    let registry = Registry::open_default().map_err(std::io::Error::other)?;
    spawn_registry_worker_with_live_cwds(
        registry,
        Arc::new(move || super::sessions::list_live_session_cwds(&state_dir)),
    )
}

/// Same as [`spawn_registry_worker`] but accepts a pre-constructed
/// `Registry`. Tests use this to bind a worker to an isolated `redb`
/// file without depending on the process-global `CLUD_DATA_DB` env var.
#[cfg(test)]
pub(super) fn spawn_registry_worker_with(
    registry: Registry,
) -> std::io::Result<mpsc::Sender<GcRequestMsg>> {
    spawn_registry_worker_with_live_cwds(registry, Arc::new(Vec::<PathBuf>::new))
}

fn spawn_registry_worker_with_live_cwds(
    registry: Registry,
    live_cwds_provider: LiveCwdsProvider,
) -> std::io::Result<mpsc::Sender<GcRequestMsg>> {
    let (tx, rx) = mpsc::channel::<GcRequestMsg>();
    let tick_cadence = gc_tick_cadence_from_env();
    thread::Builder::new()
        .name("clud-gc-registry-worker".to_string())
        .spawn(move || run_worker_loop(registry, rx, tick_cadence, live_cwds_provider))?;
    Ok(tx)
}

fn gc_tick_cadence_from_env() -> Option<Duration> {
    let raw = std::env::var(ENV_GC_TICK_SECS).ok();
    gc_tick_cadence_from_raw(raw.as_deref())
}

fn gc_tick_cadence_from_raw(raw: Option<&str>) -> Option<Duration> {
    let secs = raw
        .and_then(|value| value.trim().parse::<u64>().ok())
        .unwrap_or(DEFAULT_GC_TICK_SECS);
    if secs == 0 {
        None
    } else {
        Some(Duration::from_secs(secs))
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct GcDiskWatchdogConfig {
    warn_free_bytes: u64,
    auto_purge_free_bytes: u64,
    min_age: Duration,
    auto_purge_enabled: bool,
}

impl GcDiskWatchdogConfig {
    fn from_env() -> Self {
        let warn_free_gb = std::env::var(ENV_GC_WARN_FREE_GB).ok();
        let auto_purge_free_gb = std::env::var(ENV_GC_AUTO_PURGE_FREE_GB).ok();
        let min_age_hours = std::env::var(ENV_GC_MIN_AGE_HOURS).ok();
        let auto_purge_enabled = std::env::var(ENV_GC_AUTO_PURGE_ENABLED).ok();
        gc_disk_watchdog_config_from_raw(
            warn_free_gb.as_deref(),
            auto_purge_free_gb.as_deref(),
            min_age_hours.as_deref(),
            auto_purge_enabled.as_deref(),
        )
    }
}

fn gc_disk_watchdog_config_from_raw(
    warn_free_gb: Option<&str>,
    auto_purge_free_gb: Option<&str>,
    min_age_hours: Option<&str>,
    auto_purge_enabled: Option<&str>,
) -> GcDiskWatchdogConfig {
    GcDiskWatchdogConfig {
        warn_free_bytes: parse_gb_to_bytes(warn_free_gb, DEFAULT_GC_WARN_FREE_GB),
        auto_purge_free_bytes: parse_gb_to_bytes(auto_purge_free_gb, DEFAULT_GC_AUTO_PURGE_FREE_GB),
        min_age: parse_hours_to_duration(min_age_hours, DEFAULT_GC_MIN_AGE_HOURS),
        auto_purge_enabled: parse_bool_setting(auto_purge_enabled, DEFAULT_GC_AUTO_PURGE_ENABLED),
    }
}

fn parse_gb_to_bytes(raw: Option<&str>, default_gb: u64) -> u64 {
    raw.and_then(|value| {
        let gb = value.trim().parse::<f64>().ok()?;
        if !gb.is_finite() || gb < 0.0 {
            return None;
        }
        let bytes = gb * BYTES_PER_GB as f64;
        if bytes >= u64::MAX as f64 {
            Some(u64::MAX)
        } else {
            Some(bytes.round() as u64)
        }
    })
    .unwrap_or_else(|| default_gb.saturating_mul(BYTES_PER_GB))
}

fn parse_hours_to_duration(raw: Option<&str>, default_hours: u64) -> Duration {
    let hours = raw
        .and_then(|value| value.trim().parse::<u64>().ok())
        .unwrap_or(default_hours);
    Duration::from_secs(hours.saturating_mul(60 * 60))
}

fn parse_bool_setting(raw: Option<&str>, default: bool) -> bool {
    match raw.map(str::trim).map(str::to_ascii_lowercase).as_deref() {
        Some("1" | "true" | "yes" | "on") => true,
        Some("0" | "false" | "no" | "off") => false,
        _ => default,
    }
}

fn run_worker_loop(
    registry: Registry,
    rx: mpsc::Receiver<GcRequestMsg>,
    tick_cadence: Option<Duration>,
    live_cwds_provider: LiveCwdsProvider,
) {
    let Some(tick_cadence) = tick_cadence else {
        while let Ok(msg) = rx.recv() {
            handle_worker_msg(&registry, msg, &live_cwds_provider);
        }
        return;
    };

    let mut next_tick = Instant::now() + tick_cadence;
    loop {
        let timeout = next_tick.saturating_duration_since(Instant::now());
        match rx.recv_timeout(timeout) {
            Ok(msg) => {
                handle_worker_msg(&registry, msg, &live_cwds_provider);
                if Instant::now() >= next_tick {
                    run_periodic_purge_tick(&registry, &live_cwds_provider);
                    next_tick = Instant::now() + tick_cadence;
                }
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {
                run_periodic_purge_tick(&registry, &live_cwds_provider);
                next_tick = Instant::now() + tick_cadence;
            }
            Err(mpsc::RecvTimeoutError::Disconnected) => break,
        }
    }
}

fn handle_worker_msg(
    registry: &Registry,
    msg: GcRequestMsg,
    live_cwds_provider: &LiveCwdsProvider,
) {
    let reply = process_op_with_live_cwds(registry, msg.op, live_cwds_provider());
    // Hung-up callers are fine — the worker keeps serving the rest.
    let _ = msg.reply_tx.send(reply);
}

fn run_periodic_purge_tick(registry: &Registry, live_cwds_provider: &LiveCwdsProvider) {
    let config = GcDiskWatchdogConfig::from_env();
    run_periodic_purge_tick_with_free_space(
        registry,
        live_cwds_provider,
        &config,
        &free_space_bytes_for_path,
    );
}

fn run_periodic_purge_tick_with_free_space<F>(
    registry: &Registry,
    live_cwds_provider: &LiveCwdsProvider,
    disk_config: &GcDiskWatchdogConfig,
    free_space: &F,
) where
    F: Fn(&Path) -> Result<u64, String> + ?Sized,
{
    run_disk_watchdog_tick(registry, live_cwds_provider, disk_config, free_space);

    let worktree_reply = process_op_with_live_cwds(
        registry,
        GcOp::Purge {
            duration: Some(PERIODIC_GC_WORKTREE_STALE_AFTER.to_string()),
            kind: Some(WORKTREE_KIND.to_string()),
            dry_run: false,
        },
        live_cwds_provider(),
    );
    log_periodic_purge_reply(WORKTREE_KIND, worktree_reply);

    let extern_reply = process_op_with_live_cwds(
        registry,
        GcOp::Purge {
            duration: None,
            kind: Some(EXTERN_REPO_KIND.to_string()),
            dry_run: false,
        },
        live_cwds_provider(),
    );
    log_periodic_purge_reply(EXTERN_REPO_KIND, extern_reply);

    match reap_trash_entries(registry) {
        Ok((removed, failed)) => {
            if removed > 0 || failed > 0 {
                eprintln!("[clud] gc tick: trash removed {removed}, failed {failed}");
            }
        }
        Err(message) => {
            eprintln!("[clud] gc tick: trash error: {message}");
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct DiskWatchdogDecision {
    warn: bool,
    auto_purge: bool,
}

fn disk_watchdog_decision(config: &GcDiskWatchdogConfig, free_bytes: u64) -> DiskWatchdogDecision {
    DiskWatchdogDecision {
        warn: free_bytes < config.warn_free_bytes,
        auto_purge: config.auto_purge_enabled && free_bytes < config.auto_purge_free_bytes,
    }
}

fn run_disk_watchdog_tick<F>(
    registry: &Registry,
    live_cwds_provider: &LiveCwdsProvider,
    config: &GcDiskWatchdogConfig,
    free_space: &F,
) where
    F: Fn(&Path) -> Result<u64, String> + ?Sized,
{
    let entries = match registry.list(None) {
        Ok(entries) => entries,
        Err(err) => {
            eprintln!("[clud] gc tick disk: failed to list tracked roots: {err}");
            return;
        }
    };
    let roots = collect_tracked_entry_roots(&entries);
    let mut purge_roots = Vec::new();
    for root in roots {
        match free_space(&root) {
            Ok(free_bytes) => {
                let decision = disk_watchdog_decision(config, free_bytes);
                if decision.warn {
                    eprintln!(
                        "[clud] gc tick disk: {} has {} GB free, below warn threshold {} GB",
                        root.display(),
                        format_gb(free_bytes),
                        format_gb(config.warn_free_bytes)
                    );
                }
                if decision.auto_purge {
                    purge_roots.push(root);
                }
            }
            Err(message) => {
                eprintln!(
                    "[clud] gc tick disk: failed to sample free space for {}: {message}",
                    root.display()
                );
            }
        }
    }

    if purge_roots.is_empty() {
        return;
    }
    let purge_reply = purge_old_reclaimable_entries_for_roots(
        registry,
        live_cwds_provider,
        &purge_roots,
        config.min_age,
    );
    log_disk_watchdog_purge_reply(purge_roots.len(), config, purge_reply);
}

fn collect_tracked_entry_roots(entries: &[TrackedEntry]) -> Vec<PathBuf> {
    let mut seen = HashSet::new();
    let mut roots = Vec::new();
    for entry in entries {
        let root = tracked_entry_root(entry);
        let key = root.to_string_lossy().to_string();
        if seen.insert(key) {
            roots.push(root);
        }
    }
    roots
}

fn tracked_entry_root(entry: &TrackedEntry) -> PathBuf {
    if let Some(repo_root) = entry
        .repo_root
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
    {
        PathBuf::from(repo_root)
    } else {
        PathBuf::from(&entry.path)
    }
}

fn purge_old_reclaimable_entries_for_roots(
    registry: &Registry,
    live_cwds_provider: &LiveCwdsProvider,
    roots: &[PathBuf],
    min_age: Duration,
) -> GcReply {
    let cutoff = now_unix().saturating_sub(duration_secs_i64(min_age));
    let candidates = match registry.select_older_than(cutoff, None) {
        Ok(candidates) => candidates,
        Err(err) => {
            return GcReply::Error {
                message: err.to_string(),
            };
        }
    };
    let candidates = candidates
        .into_iter()
        .filter(|entry| entry.kind == WORKTREE_KIND || entry.kind == SIBLING_CLONE_KIND)
        .filter(|entry| tracked_entry_matches_any_root(entry, roots))
        .collect();
    purge_entries(registry, candidates, live_cwds_provider(), false)
}

fn tracked_entry_matches_any_root(entry: &TrackedEntry, roots: &[PathBuf]) -> bool {
    let entry_root = tracked_entry_root(entry);
    let entry_path = Path::new(&entry.path);
    roots.iter().any(|root| {
        path_matches_or_is_under(&entry_root, root) || path_matches_or_is_under(entry_path, root)
    })
}

fn path_matches_or_is_under(path: &Path, root: &Path) -> bool {
    path == root || path.starts_with(root)
}

fn duration_secs_i64(duration: Duration) -> i64 {
    duration.as_secs().min(i64::MAX as u64) as i64
}

fn log_disk_watchdog_purge_reply(
    low_root_count: usize,
    config: &GcDiskWatchdogConfig,
    reply: GcReply,
) {
    match reply {
        GcReply::PurgeOk { removed, skipped } => {
            eprintln!(
                "[clud] gc tick disk: auto-purge checked {low_root_count} low root(s), min age {}h, removed {removed}, skipped {skipped}",
                config.min_age.as_secs() / (60 * 60)
            );
        }
        GcReply::Error { message } => {
            eprintln!("[clud] gc tick disk: auto-purge error: {message}");
        }
        other => {
            eprintln!("[clud] gc tick disk: unexpected auto-purge reply: {other:?}");
        }
    }
}

fn format_gb(bytes: u64) -> String {
    format!("{:.2}", bytes as f64 / BYTES_PER_GB as f64)
}

fn free_space_bytes_for_path(path: &Path) -> Result<u64, String> {
    let probe_path = disk_probe_path(path)?;
    let disks = sysinfo::Disks::new_with_refreshed_list();
    disks
        .list()
        .iter()
        .filter(|disk| probe_path.starts_with(disk.mount_point()))
        .max_by_key(|disk| disk.mount_point().components().count())
        .map(|disk| disk.available_space())
        .ok_or_else(|| format!("no mounted disk found for {}", probe_path.display()))
}

fn disk_probe_path(path: &Path) -> Result<PathBuf, String> {
    let absolute = if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir()
            .map_err(|err| format!("current_dir: {err}"))?
            .join(path)
    };
    let mut probe = absolute.clone();
    loop {
        if probe.exists() {
            return Ok(probe);
        }
        if !probe.pop() {
            return Ok(absolute);
        }
    }
}

fn log_periodic_purge_reply(kind: &str, reply: GcReply) {
    match reply {
        GcReply::PurgeOk { removed, skipped } => {
            eprintln!("[clud] gc tick {kind}: removed {removed}, skipped {skipped}");
        }
        GcReply::Error { message } => {
            eprintln!("[clud] gc tick {kind}: error: {message}");
        }
        other => {
            eprintln!("[clud] gc tick {kind}: unexpected reply: {other:?}");
        }
    }
}

fn process_op_with_live_cwds(registry: &Registry, op: GcOp, live_cwds: Vec<PathBuf>) -> GcReply {
    match op {
        GcOp::List { kind } => match registry.list(kind.as_deref()) {
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
                GcReply::ListOk { rows: out }
            }
            Err(e) => GcReply::Error {
                message: e.to_string(),
            },
        },

        GcOp::Purge {
            duration,
            kind,
            dry_run,
        } => {
            let candidates_res = match &duration {
                Some(d) => match worktrees::parse_duration(d) {
                    Ok(dur) => {
                        let cutoff = now_unix().saturating_sub(duration_secs_i64(dur));
                        registry.select_older_than(cutoff, kind.as_deref())
                    }
                    Err(e) => {
                        return GcReply::Error {
                            message: format!("invalid duration: {e}"),
                        };
                    }
                },
                None => registry.list(kind.as_deref()),
            };
            let candidates: Vec<TrackedEntry> = match candidates_res {
                Ok(v) => v,
                Err(e) => {
                    return GcReply::Error {
                        message: e.to_string(),
                    };
                }
            };
            purge_entries(registry, candidates, live_cwds, dry_run)
        }

        GcOp::Reconcile { repo_root } => {
            let root = PathBuf::from(&repo_root);
            match reconcile_repo_root(registry, &root) {
                Ok(inserted) => GcReply::ReconcileOk { inserted },
                Err(e) => GcReply::Error {
                    message: e.to_string(),
                },
            }
        }

        GcOp::Insert {
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
                Ok(()) => GcReply::InsertOk,
                Err(e) => GcReply::Error {
                    message: e.to_string(),
                },
            }
        }

        GcOp::RecordRepoVisit {
            repo_root,
            cwd,
            now_unix: provided,
        } => {
            let stamp = provided.unwrap_or_else(now_unix);
            match registry.record_repo_visit(&repo_root, &cwd, stamp) {
                Ok(()) => GcReply::RepoVisitOk,
                Err(e) => GcReply::Error {
                    message: e.to_string(),
                },
            }
        }

        GcOp::ListRepoVisits => match registry.list_repo_visits() {
            Ok(rows) => GcReply::RepoVisitsOk { rows },
            Err(e) => GcReply::Error {
                message: e.to_string(),
            },
        },

        GcOp::DeleteById { id } => {
            let entries = match registry.list(None) {
                Ok(v) => v,
                Err(e) => {
                    return GcReply::Error {
                        message: e.to_string(),
                    };
                }
            };
            let Some(target) = entries.into_iter().find(|e| e.id == id) else {
                // Idempotent: an id that no longer exists is `removed=0,
                // skipped=0`. The dashboard refreshes after every delete
                // so a stale id click is silently a no-op.
                return GcReply::PurgeOk {
                    removed: 0,
                    skipped: 0,
                };
            };
            let live_locks = collect_live_lock_paths();
            let live_cwds = canonicalize_live_cwds(live_cwds);
            if entry_is_live(&target, &live_locks, &live_cwds) {
                return GcReply::PurgeOk {
                    removed: 0,
                    skipped: 1,
                };
            }
            match remove_entry_and_delete_row(registry, &target) {
                Ok(()) => GcReply::PurgeOk {
                    removed: 1,
                    skipped: 0,
                },
                Err(message) => GcReply::Error { message },
            }
        }
    }
}

fn purge_entries(
    registry: &Registry,
    candidates: Vec<TrackedEntry>,
    live_cwds: Vec<PathBuf>,
    dry_run: bool,
) -> GcReply {
    let live_locks = collect_live_lock_paths();
    let live_cwds = canonicalize_live_cwds(live_cwds);
    let mut purgeable = Vec::new();
    let mut skipped = 0usize;
    for candidate in candidates {
        if entry_is_live(&candidate, &live_locks, &live_cwds) {
            skipped += 1;
            continue;
        }
        if !entry_kind_allows_purge(&candidate) {
            skipped += 1;
            continue;
        }
        purgeable.push(candidate);
    }
    if dry_run {
        return GcReply::PurgeOk {
            removed: purgeable.len(),
            skipped,
        };
    }
    let mut removed = 0usize;
    for entry in &purgeable {
        if remove_entry_and_delete_row(registry, entry).is_ok() {
            removed += 1;
        }
    }
    GcReply::PurgeOk { removed, skipped }
}

fn canonicalize_live_cwds(live_cwds: Vec<PathBuf>) -> Vec<PathBuf> {
    let mut out: Vec<PathBuf> = live_cwds
        .into_iter()
        .filter_map(|path| std::fs::canonicalize(path).ok())
        .collect();
    out.sort();
    out.dedup();
    out
}

fn entry_is_live(
    entry: &TrackedEntry,
    live_locks: &HashSet<String>,
    live_cwds: &[PathBuf],
) -> bool {
    if entry.kind == "trash" {
        return false;
    }
    if entry.kind == "worktree" && live_locks.contains(&entry.path) {
        return true;
    }
    entry_path_contains_live_cwd(entry, live_cwds)
}

fn entry_path_contains_live_cwd(entry: &TrackedEntry, live_cwds: &[PathBuf]) -> bool {
    let Ok(entry_path) = std::fs::canonicalize(&entry.path) else {
        return false;
    };
    live_cwds
        .iter()
        .any(|cwd| cwd == &entry_path || cwd.starts_with(&entry_path))
}

fn entry_kind_allows_purge(entry: &TrackedEntry) -> bool {
    if entry.kind == EXTERN_REPO_KIND {
        return extern_repo_is_purgeable(entry, extern_repo_stale_after());
    }
    true
}

fn extern_repo_stale_after() -> Duration {
    let secs = std::env::var(ENV_GC_EXTERN_REPO_MAX_AGE_SECS)
        .ok()
        .and_then(|raw| raw.trim().parse::<u64>().ok())
        .unwrap_or(DEFAULT_EXTERN_REPO_STALE_AFTER_SECS);
    Duration::from_secs(secs)
}

fn extern_repo_is_purgeable(entry: &TrackedEntry, stale_after: Duration) -> bool {
    let path = Path::new(&entry.path);
    if !path.is_dir() {
        return false;
    }
    let Some(mtime) = most_recent_mtime(path) else {
        return false;
    };
    let Ok(age) = SystemTime::now().duration_since(mtime) else {
        return false;
    };
    if age < stale_after {
        return false;
    }
    let Some(branch) = entry
        .branch
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .or_else(|| crate::gc::best_effort_branch(path))
    else {
        return false;
    };
    let Some(slug) = repo_slug_for_extern_repo(path) else {
        return false;
    };
    gh_pr_list_reports_merged(&branch, &slug)
}

fn most_recent_mtime(path: &Path) -> Option<SystemTime> {
    let metadata = std::fs::symlink_metadata(path).ok()?;
    let mut latest = metadata.modified().ok()?;
    if metadata.is_dir() {
        let entries = std::fs::read_dir(path).ok()?;
        for entry in entries.flatten() {
            if let Some(child_mtime) = most_recent_mtime(&entry.path()) {
                if child_mtime > latest {
                    latest = child_mtime;
                }
            }
        }
    }
    Some(latest)
}

fn repo_slug_for_extern_repo(path: &Path) -> Option<String> {
    let remote = worktrees::run_git(path, &["remote", "get-url", "origin"]).ok()?;
    parse_github_slug_from_remote_url(&remote)
}

fn parse_github_slug_from_remote_url(remote: &str) -> Option<String> {
    let s = remote.trim().trim_end_matches('/');
    if let Some(rest) = s.strip_prefix("git@github.com:") {
        return slug_from_github_path(rest);
    }
    for prefix in [
        "https://github.com/",
        "http://github.com/",
        "ssh://git@github.com/",
        "git://github.com/",
    ] {
        if let Some(rest) = s.strip_prefix(prefix) {
            return slug_from_github_path(rest);
        }
    }
    None
}

fn slug_from_github_path(path: &str) -> Option<String> {
    let clean = path.trim().trim_matches('/').trim_end_matches(".git");
    let mut parts = clean.split('/');
    let owner = parts.next()?.trim();
    let repo = parts.next()?.trim().trim_end_matches(".git");
    if owner.is_empty() || repo.is_empty() {
        return None;
    }
    Some(format!("{owner}/{repo}"))
}

fn gh_pr_list_reports_merged(branch: &str, slug: &str) -> bool {
    let args = vec![
        "pr".to_string(),
        "list".to_string(),
        "--head".to_string(),
        branch.to_string(),
        "--state".to_string(),
        "all".to_string(),
        "--json".to_string(),
        "mergedAt,url".to_string(),
        "--repo".to_string(),
        slug.to_string(),
    ];
    let Ok((exit_code, stdout)) = run_gh_capture(&args) else {
        return false;
    };
    exit_code == 0 && gh_pr_list_json_has_merged(&stdout)
}

fn gh_pr_list_json_has_merged(stdout: &str) -> bool {
    let Ok(value) = serde_json::from_str::<serde_json::Value>(stdout) else {
        return false;
    };
    value
        .as_array()
        .map(|items| {
            items.iter().any(|item| {
                item.get("mergedAt")
                    .and_then(|merged_at| merged_at.as_str())
                    .map(|s| !s.trim().is_empty())
                    .unwrap_or(false)
            })
        })
        .unwrap_or(false)
}

fn run_gh_capture(args: &[String]) -> Result<(i32, String), String> {
    let mut argv = vec![gh_program()];
    argv.extend(args.iter().cloned());
    let config = ProcessConfig {
        command: subprocess::command_spec_for_subprocess(argv),
        cwd: None,
        env: None,
        capture: true,
        stderr_mode: StderrMode::Stdout,
        creationflags: invisible_helper_creationflags(),
        create_process_group: false,
        stdin_mode: StdinMode::Null,
        nice: None,
    };
    let process = NativeProcess::new(config);
    process
        .start()
        .map_err(|e| format!("failed to start gh: {e}"))?;

    let mut buf = Vec::<u8>::new();
    loop {
        match process.read_combined(Some(Duration::from_millis(100))) {
            ReadStatus::Line(event) => {
                buf.extend_from_slice(&event.line);
                buf.push(b'\n');
            }
            ReadStatus::Timeout => {
                if process.returncode().is_some() {
                    break;
                }
            }
            ReadStatus::Eof => break,
        }
    }
    let exit_code = process
        .wait(Some(Duration::from_secs(30)))
        .map_err(|e| format!("waiting for gh: {e}"))?;
    Ok((exit_code, String::from_utf8_lossy(&buf).to_string()))
}

fn gh_program() -> String {
    #[cfg(test)]
    {
        if let Some(path) = std::env::var_os(ENV_TEST_GH_BIN) {
            if !path.is_empty() {
                return path.to_string_lossy().to_string();
            }
        }
    }
    "gh".to_string()
}

fn now_unix() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

/// Paths that `git worktree list --porcelain` reports as `locked` with a
/// reason of the form `agent <pid>` where the PID is still alive. Used to
/// shield in-flight `clud` worktrees from `clud gc purge`.
fn collect_live_lock_paths() -> HashSet<String> {
    let mut out = HashSet::new();
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
        let _ =
            worktrees::remove_worktree_path(Path::new(&main_root), Path::new(&entry.path), true)?;
    } else if entry.kind == "trash" {
        std::fs::remove_dir_all(&entry.path).map_err(|e| e.to_string())?;
    } else {
        let p = Path::new(&entry.path);
        if p.exists() {
            std::fs::remove_dir_all(p).map_err(|e| e.to_string())?;
        }
    }
    registry.delete(entry.id).map_err(|e| e.to_string())
}

fn reap_trash_entries(registry: &Registry) -> Result<(usize, usize), String> {
    let entries = registry
        .list(Some("trash"))
        .map_err(|err| err.to_string())?;
    let mut removed = 0usize;
    let mut failed = 0usize;
    for entry in entries {
        match std::fs::remove_dir_all(&entry.path) {
            Ok(()) => {
                registry.delete(entry.id).map_err(|err| err.to_string())?;
                eprintln!("[gc] trash: reaped {}", entry.path);
                removed += 1;
            }
            Err(_) => {
                failed += 1;
            }
        }
    }
    Ok((removed, failed))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::gc::ENV_DATA_DB;
    use std::ffi::OsString;
    use std::fs;
    use std::sync::Mutex;

    #[cfg(unix)]
    use std::os::unix::fs::PermissionsExt;

    // ENV_DATA_DB is process-global; serialize so two test threads
    // never race to open the same redb file concurrently.
    static TEST_LOCK: Mutex<()> = Mutex::new(());

    /// Spin up a registry worker against an isolated redb file and return
    /// its sender plus a guard that holds `TEST_LOCK` for the test's
    /// lifetime. The worker thread stops when the returned sender is
    /// dropped.
    fn spawn_test_worker(
        db_path: &Path,
    ) -> (
        mpsc::Sender<GcRequestMsg>,
        std::sync::MutexGuard<'static, ()>,
    ) {
        spawn_test_worker_with_tick(db_path, "0")
    }

    fn spawn_test_worker_with_tick(
        db_path: &Path,
        tick_secs: &str,
    ) -> (
        mpsc::Sender<GcRequestMsg>,
        std::sync::MutexGuard<'static, ()>,
    ) {
        let guard = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let prior_db = std::env::var_os(ENV_DATA_DB);
        let prior_tick = std::env::var_os(ENV_GC_TICK_SECS);
        std::env::set_var(ENV_DATA_DB, db_path);
        std::env::set_var(ENV_GC_TICK_SECS, tick_secs);
        let tx = spawn_registry_worker();
        restore_env_var(ENV_GC_TICK_SECS, prior_tick);
        restore_env_var(ENV_DATA_DB, prior_db);
        let tx = tx.unwrap();
        (tx, guard)
    }

    fn spawn_test_worker_with_live_cwds(
        db_path: &Path,
        live_cwds: Vec<PathBuf>,
    ) -> mpsc::Sender<GcRequestMsg> {
        let registry = Registry::open_at(db_path).expect("open registry");
        spawn_registry_worker_with_live_cwds(registry, Arc::new(move || live_cwds.clone()))
            .expect("spawn registry worker")
    }

    fn restore_env_var(key: &str, prior: Option<std::ffi::OsString>) {
        match prior {
            Some(value) => std::env::set_var(key, value),
            None => std::env::remove_var(key),
        }
    }

    struct ScopedEnv {
        key: &'static str,
        prior: Option<OsString>,
    }

    impl ScopedEnv {
        fn set(key: &'static str, value: impl AsRef<std::ffi::OsStr>) -> Self {
            let prior = std::env::var_os(key);
            std::env::set_var(key, value);
            Self { key, prior }
        }
    }

    impl Drop for ScopedEnv {
        fn drop(&mut self) {
            restore_env_var(self.key, self.prior.take());
        }
    }

    fn call(tx: &mpsc::Sender<GcRequestMsg>, op: GcOp) -> GcReply {
        let (reply_tx, reply_rx) = mpsc::sync_channel::<GcReply>(1);
        tx.send(GcRequestMsg { op, reply_tx }).unwrap();
        reply_rx.recv_timeout(Duration::from_secs(5)).unwrap()
    }

    fn write_mock_gh(dir: &Path, json: &str) -> PathBuf {
        #[cfg(windows)]
        {
            let path = dir.join("gh.cmd");
            fs::write(&path, format!("@echo off\r\necho {json}\r\nexit /b 0\r\n")).unwrap();
            path
        }
        #[cfg(not(windows))]
        {
            let path = dir.join("gh");
            fs::write(
                &path,
                format!("#!/bin/sh\ncat <<'JSON'\n{json}\nJSON\nexit 0\n"),
            )
            .unwrap();
            #[cfg(unix)]
            {
                let mut perms = fs::metadata(&path).unwrap().permissions();
                perms.set_mode(0o755);
                fs::set_permissions(&path, perms).unwrap();
            }
            path
        }
    }

    #[test]
    fn gc_tick_cadence_config_handles_default_disable_and_positive() {
        assert_eq!(
            gc_tick_cadence_from_raw(None),
            Some(Duration::from_secs(DEFAULT_GC_TICK_SECS))
        );
        assert_eq!(gc_tick_cadence_from_raw(Some("0")), None);
        assert_eq!(
            gc_tick_cadence_from_raw(Some("1")),
            Some(Duration::from_secs(1))
        );
    }

    #[test]
    fn gc_disk_watchdog_config_parses_defaults_and_overrides() {
        let defaults = gc_disk_watchdog_config_from_raw(None, None, None, None);
        assert_eq!(defaults.warn_free_bytes, 10 * BYTES_PER_GB);
        assert_eq!(defaults.auto_purge_free_bytes, 5 * BYTES_PER_GB);
        assert_eq!(defaults.min_age, Duration::from_secs(24 * 60 * 60));
        assert!(defaults.auto_purge_enabled);

        let overrides =
            gc_disk_watchdog_config_from_raw(Some("1.5"), Some("2"), Some("7"), Some("off"));
        assert_eq!(overrides.warn_free_bytes, BYTES_PER_GB + BYTES_PER_GB / 2);
        assert_eq!(overrides.auto_purge_free_bytes, 2 * BYTES_PER_GB);
        assert_eq!(overrides.min_age, Duration::from_secs(7 * 60 * 60));
        assert!(!overrides.auto_purge_enabled);
    }

    #[test]
    fn gc_disk_watchdog_config_falls_back_on_invalid_values() {
        let config =
            gc_disk_watchdog_config_from_raw(Some("-1"), Some("nan"), Some("bad"), Some("maybe"));
        assert_eq!(config.warn_free_bytes, 10 * BYTES_PER_GB);
        assert_eq!(config.auto_purge_free_bytes, 5 * BYTES_PER_GB);
        assert_eq!(config.min_age, Duration::from_secs(24 * 60 * 60));
        assert!(config.auto_purge_enabled);
    }

    #[test]
    fn disk_watchdog_decision_warns_and_purges_only_below_thresholds() {
        let config = GcDiskWatchdogConfig {
            warn_free_bytes: 10 * BYTES_PER_GB,
            auto_purge_free_bytes: 5 * BYTES_PER_GB,
            min_age: Duration::from_secs(24 * 60 * 60),
            auto_purge_enabled: true,
        };

        assert_eq!(
            disk_watchdog_decision(&config, 10 * BYTES_PER_GB),
            DiskWatchdogDecision {
                warn: false,
                auto_purge: false
            }
        );
        assert_eq!(
            disk_watchdog_decision(&config, 9 * BYTES_PER_GB),
            DiskWatchdogDecision {
                warn: true,
                auto_purge: false
            }
        );
        assert_eq!(
            disk_watchdog_decision(&config, 4 * BYTES_PER_GB),
            DiskWatchdogDecision {
                warn: true,
                auto_purge: true
            }
        );

        let disabled = GcDiskWatchdogConfig {
            auto_purge_enabled: false,
            ..config
        };
        assert_eq!(
            disk_watchdog_decision(&disabled, 4 * BYTES_PER_GB),
            DiskWatchdogDecision {
                warn: true,
                auto_purge: false
            }
        );
    }

    #[test]
    fn github_remote_slug_parser_accepts_common_origin_urls() {
        assert_eq!(
            parse_github_slug_from_remote_url("git@github.com:zackees/dep.git"),
            Some("zackees/dep".to_string())
        );
        assert_eq!(
            parse_github_slug_from_remote_url("https://github.com/zackees/dep.git\n"),
            Some("zackees/dep".to_string())
        );
        assert_eq!(
            parse_github_slug_from_remote_url("ssh://git@github.com/zackees/dep"),
            Some("zackees/dep".to_string())
        );
        assert_eq!(
            parse_github_slug_from_remote_url("https://example.com/x/y"),
            None
        );
    }

    #[test]
    fn gh_pr_list_json_requires_non_empty_merged_at() {
        assert!(gh_pr_list_json_has_merged(
            r#"[{"mergedAt":"2026-01-01T00:00:00Z","url":"https://github.com/a/b/pull/1"}]"#
        ));
        assert!(!gh_pr_list_json_has_merged(
            r#"[{"mergedAt":null,"url":"https://github.com/a/b/pull/1"}]"#
        ));
        assert!(!gh_pr_list_json_has_merged("not json"));
    }

    #[test]
    fn round_trip_insert_then_list() {
        let dir = tempfile::tempdir().unwrap();
        let db_path = dir.path().join("test.redb");
        let (tx, _g) = spawn_test_worker(&db_path);

        let resp = call(
            &tx,
            GcOp::Insert {
                kind: "worktree".to_string(),
                path: "/tmp/test-a".to_string(),
                repo_root: Some("/tmp/repo".to_string()),
                branch: Some("main".to_string()),
                agent_id: Some("agent-abc".to_string()),
                created_unix: Some(100),
            },
        );
        assert!(matches!(resp, GcReply::InsertOk));

        let resp = call(&tx, GcOp::List { kind: None });
        match resp {
            GcReply::ListOk { rows } => {
                assert_eq!(rows.len(), 1);
                assert_eq!(rows[0].path, "/tmp/test-a");
                assert_eq!(rows[0].agent_id.as_deref(), Some("agent-abc"));
            }
            other => panic!("unexpected reply: {other:?}"),
        }
    }

    #[test]
    fn purge_with_no_duration_removes_all_non_live() {
        let dir = tempfile::tempdir().unwrap();
        let db_path = dir.path().join("purge-all.redb");
        let (tx, _g) = spawn_test_worker(&db_path);

        for path in ["/tmp/c1", "/tmp/c2"] {
            call(
                &tx,
                GcOp::Insert {
                    kind: "cache".to_string(),
                    path: path.to_string(),
                    repo_root: None,
                    branch: None,
                    agent_id: None,
                    created_unix: Some(100),
                },
            );
        }

        let resp = call(
            &tx,
            GcOp::Purge {
                duration: None,
                kind: None,
                dry_run: false,
            },
        );
        match resp {
            GcReply::PurgeOk { removed, skipped } => {
                assert_eq!(removed, 2);
                assert_eq!(skipped, 0);
            }
            other => panic!("unexpected reply: {other:?}"),
        }

        let resp = call(&tx, GcOp::List { kind: None });
        match resp {
            GcReply::ListOk { rows } => assert!(rows.is_empty()),
            other => panic!("unexpected reply: {other:?}"),
        }
    }

    #[test]
    fn purge_dry_run_does_not_modify_db() {
        let dir = tempfile::tempdir().unwrap();
        let db_path = dir.path().join("purge-dry.redb");
        let (tx, _g) = spawn_test_worker(&db_path);

        call(
            &tx,
            GcOp::Insert {
                kind: "cache".to_string(),
                path: "/tmp/dry".to_string(),
                repo_root: None,
                branch: None,
                agent_id: None,
                created_unix: Some(100),
            },
        );
        let resp = call(
            &tx,
            GcOp::Purge {
                duration: None,
                kind: None,
                dry_run: true,
            },
        );
        match resp {
            GcReply::PurgeOk { removed, .. } => assert_eq!(removed, 1),
            other => panic!("unexpected reply: {other:?}"),
        }
        let resp = call(&tx, GcOp::List { kind: None });
        match resp {
            GcReply::ListOk { rows } => assert_eq!(rows.len(), 1),
            other => panic!("unexpected reply: {other:?}"),
        }
    }

    #[test]
    fn purge_skips_entry_equal_to_live_session_cwd() {
        let dir = tempfile::tempdir().unwrap();
        let db_path = dir.path().join("live-cwd-direct.redb");
        let path_a = dir.path().join("A");
        let path_b = dir.path().join("B");
        std::fs::create_dir_all(&path_a).unwrap();
        std::fs::create_dir_all(&path_b).unwrap();
        let tx = spawn_test_worker_with_live_cwds(&db_path, vec![path_a.clone()]);

        for path in [&path_a, &path_b] {
            call(
                &tx,
                GcOp::Insert {
                    kind: "cache".to_string(),
                    path: path.to_string_lossy().to_string(),
                    repo_root: None,
                    branch: None,
                    agent_id: None,
                    created_unix: Some(100),
                },
            );
        }

        let resp = call(
            &tx,
            GcOp::Purge {
                duration: None,
                kind: None,
                dry_run: false,
            },
        );
        match resp {
            GcReply::PurgeOk { removed, skipped } => {
                assert_eq!(removed, 1);
                assert_eq!(skipped, 1);
            }
            other => panic!("unexpected reply: {other:?}"),
        }

        assert!(path_a.exists(), "live cwd entry should remain on disk");
        assert!(!path_b.exists(), "non-live entry should be deleted");
        let rows = match call(&tx, GcOp::List { kind: None }) {
            GcReply::ListOk { rows } => rows,
            other => panic!("unexpected reply: {other:?}"),
        };
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].path, path_a.to_string_lossy().to_string());
    }

    #[test]
    fn purge_skips_entry_that_is_ancestor_of_live_session_cwd() {
        let dir = tempfile::tempdir().unwrap();
        let db_path = dir.path().join("live-cwd-ancestor.redb");
        let path_a = dir.path().join("A");
        let live_subdir = path_a.join("sub");
        std::fs::create_dir_all(&live_subdir).unwrap();
        let tx = spawn_test_worker_with_live_cwds(&db_path, vec![live_subdir]);

        call(
            &tx,
            GcOp::Insert {
                kind: "cache".to_string(),
                path: path_a.to_string_lossy().to_string(),
                repo_root: None,
                branch: None,
                agent_id: None,
                created_unix: Some(100),
            },
        );

        let resp = call(
            &tx,
            GcOp::Purge {
                duration: None,
                kind: None,
                dry_run: false,
            },
        );
        match resp {
            GcReply::PurgeOk { removed, skipped } => {
                assert_eq!(removed, 0);
                assert_eq!(skipped, 1);
            }
            other => panic!("unexpected reply: {other:?}"),
        }

        assert!(
            path_a.exists(),
            "ancestor of live cwd should remain on disk"
        );
        let rows = match call(&tx, GcOp::List { kind: None }) {
            GcReply::ListOk { rows } => rows,
            other => panic!("unexpected reply: {other:?}"),
        };
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].path, path_a.to_string_lossy().to_string());
    }

    #[test]
    fn periodic_tick_auto_purges_old_worktree_entry_when_free_space_low() {
        let dir = tempfile::tempdir().unwrap();
        let db_path = dir.path().join("periodic-purge.redb");
        let registry = Registry::open_at(&db_path).unwrap();
        let old_path = dir.path().join("old-worktree");
        let old_sibling = dir.path().join("clud-pr-old");
        std::fs::create_dir_all(&old_path).unwrap();
        std::fs::create_dir_all(&old_sibling).unwrap();

        registry
            .insert_if_new(&InsertInput {
                kind: "worktree".to_string(),
                path: old_path.to_string_lossy().to_string(),
                repo_root: Some(dir.path().to_string_lossy().to_string()),
                branch: Some("stale".to_string()),
                agent_id: Some("agent-old".to_string()),
                now_unix: now_unix().saturating_sub(25 * 60 * 60),
            })
            .unwrap();
        registry
            .insert_if_new(&InsertInput {
                kind: SIBLING_CLONE_KIND.to_string(),
                path: old_sibling.to_string_lossy().to_string(),
                repo_root: Some(dir.path().to_string_lossy().to_string()),
                branch: Some("old".to_string()),
                agent_id: None,
                now_unix: now_unix().saturating_sub(25 * 60 * 60),
            })
            .unwrap();

        let config = GcDiskWatchdogConfig {
            warn_free_bytes: 10 * BYTES_PER_GB,
            auto_purge_free_bytes: 5 * BYTES_PER_GB,
            min_age: Duration::from_secs(24 * 60 * 60),
            auto_purge_enabled: true,
        };
        let live_cwds_provider: LiveCwdsProvider = Arc::new(Vec::<PathBuf>::new);
        run_periodic_purge_tick_with_free_space(&registry, &live_cwds_provider, &config, &|_| {
            Ok(4 * BYTES_PER_GB)
        });

        assert!(registry.list(Some(WORKTREE_KIND)).unwrap().is_empty());
        assert!(registry.list(Some(SIBLING_CLONE_KIND)).unwrap().is_empty());
        assert!(!old_path.exists());
        assert!(!old_sibling.exists());
    }

    #[test]
    fn periodic_tick_keeps_old_worktree_entry_when_free_space_is_healthy() {
        let dir = tempfile::tempdir().unwrap();
        let db_path = dir.path().join("periodic-healthy.redb");
        let registry = Registry::open_at(&db_path).unwrap();
        let old_path = dir.path().join("old-worktree");
        std::fs::create_dir_all(&old_path).unwrap();

        registry
            .insert_if_new(&InsertInput {
                kind: "worktree".to_string(),
                path: old_path.to_string_lossy().to_string(),
                repo_root: Some(dir.path().to_string_lossy().to_string()),
                branch: Some("stale".to_string()),
                agent_id: Some("agent-old".to_string()),
                now_unix: now_unix().saturating_sub(25 * 60 * 60),
            })
            .unwrap();

        let config = GcDiskWatchdogConfig {
            warn_free_bytes: 10 * BYTES_PER_GB,
            auto_purge_free_bytes: 5 * BYTES_PER_GB,
            min_age: Duration::from_secs(24 * 60 * 60),
            auto_purge_enabled: true,
        };
        let live_cwds_provider: LiveCwdsProvider = Arc::new(Vec::<PathBuf>::new);
        run_periodic_purge_tick_with_free_space(&registry, &live_cwds_provider, &config, &|_| {
            Ok(20 * BYTES_PER_GB)
        });

        assert_eq!(registry.list(Some(WORKTREE_KIND)).unwrap().len(), 1);
        assert!(old_path.exists());
    }

    #[test]
    fn trash_reaper_deletes_successful_entry_and_row() {
        let dir = tempfile::tempdir().unwrap();
        let db_path = dir.path().join("trash-reap.redb");
        let registry = Registry::open_at(&db_path).unwrap();
        let trash_dir = dir.path().join("trash-item");
        std::fs::create_dir_all(&trash_dir).unwrap();
        registry
            .insert_if_new(&InsertInput {
                kind: "trash".to_string(),
                path: trash_dir.to_string_lossy().to_string(),
                repo_root: None,
                branch: None,
                agent_id: Some("C:/repo/target/debug/foo.dll".to_string()),
                now_unix: 100,
            })
            .unwrap();

        let (removed, failed) = reap_trash_entries(&registry).unwrap();

        assert_eq!((removed, failed), (1, 0));
        assert!(!trash_dir.exists());
        assert!(registry.list(Some("trash")).unwrap().is_empty());
    }

    #[test]
    fn trash_reaper_keeps_row_when_delete_fails() {
        let dir = tempfile::tempdir().unwrap();
        let db_path = dir.path().join("trash-reap-fail.redb");
        let registry = Registry::open_at(&db_path).unwrap();
        let not_a_dir = dir.path().join("still-locked.dll");
        std::fs::write(&not_a_dir, b"locked").unwrap();
        registry
            .insert_if_new(&InsertInput {
                kind: "trash".to_string(),
                path: not_a_dir.to_string_lossy().to_string(),
                repo_root: None,
                branch: None,
                agent_id: Some("C:/repo/target/debug/still-locked.dll".to_string()),
                now_unix: 100,
            })
            .unwrap();

        let (removed, failed) = reap_trash_entries(&registry).unwrap();

        assert_eq!((removed, failed), (0, 1));
        assert!(not_a_dir.exists());
        assert_eq!(registry.list(Some("trash")).unwrap().len(), 1);
    }

    #[test]
    fn periodic_tick_removes_merged_stale_extern_repo_entry() {
        let _guard = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let dir = tempfile::tempdir().unwrap();
        let db_path = dir.path().join("extern-purge.redb");
        let repo = dir.path().join("extern");
        fs::create_dir_all(&repo).unwrap();
        worktrees::run_git(&repo, &["init"]).expect("git init");
        worktrees::run_git(&repo, &["checkout", "-b", "feat/test"]).expect("git branch");
        worktrees::run_git(
            &repo,
            &[
                "remote",
                "add",
                "origin",
                "https://github.com/zackees/dependency.git",
            ],
        )
        .expect("git remote");

        let mock_gh = write_mock_gh(
            dir.path(),
            r#"[{"mergedAt":"2026-01-01T00:00:00Z","url":"https://github.com/zackees/dependency/pull/1"}]"#,
        );
        let _age = ScopedEnv::set(ENV_GC_EXTERN_REPO_MAX_AGE_SECS, "0");
        let _gh = ScopedEnv::set(ENV_TEST_GH_BIN, mock_gh.as_os_str());

        let registry = Registry::open_at(&db_path).expect("open registry");
        registry
            .insert_if_new(&InsertInput {
                kind: EXTERN_REPO_KIND.to_string(),
                path: repo.to_string_lossy().to_string(),
                repo_root: Some(dir.path().to_string_lossy().to_string()),
                branch: Some("feat/test".to_string()),
                agent_id: None,
                now_unix: now_unix(),
            })
            .expect("insert extern repo");

        let live_cwds_provider: LiveCwdsProvider = Arc::new(Vec::<PathBuf>::new);
        run_periodic_purge_tick(&registry, &live_cwds_provider);

        let rows = registry.list(Some(EXTERN_REPO_KIND)).expect("list");
        assert!(rows.is_empty(), "merged extern-repo row should be deleted");
        assert!(!repo.exists(), "merged extern-repo dir should be deleted");
    }

    #[test]
    fn list_filter_by_kind() {
        let dir = tempfile::tempdir().unwrap();
        let db_path = dir.path().join("filter.redb");
        let (tx, _g) = spawn_test_worker(&db_path);

        call(
            &tx,
            GcOp::Insert {
                kind: "worktree".to_string(),
                path: "/tmp/wt".to_string(),
                repo_root: None,
                branch: None,
                agent_id: None,
                created_unix: Some(100),
            },
        );
        call(
            &tx,
            GcOp::Insert {
                kind: "cache".to_string(),
                path: "/tmp/ca".to_string(),
                repo_root: None,
                branch: None,
                agent_id: None,
                created_unix: Some(100),
            },
        );
        let resp = call(
            &tx,
            GcOp::List {
                kind: Some("worktree".to_string()),
            },
        );
        match resp {
            GcReply::ListOk { rows } => {
                assert_eq!(rows.len(), 1);
                assert_eq!(rows[0].kind, "worktree");
            }
            other => panic!("unexpected reply: {other:?}"),
        }
    }

    /// Issue #183: per-row Delete must target exactly the requested id
    /// regardless of how many siblings share its kind. Earlier iterations
    /// of the dashboard worked around the missing IPC primitive by
    /// issuing `Purge { kind: Some(k) }` and refusing when k had >1 row,
    /// which broke the per-row button in the common multi-row case.
    #[test]
    fn delete_by_id_removes_only_the_targeted_row() {
        let dir = tempfile::tempdir().unwrap();
        let db_path = dir.path().join("delete-by-id.redb");
        let (tx, _g) = spawn_test_worker(&db_path);

        // Three rows of the same kind — the bug case the workaround
        // refused to handle.
        let paths = [
            dir.path().join("e1").to_string_lossy().to_string(),
            dir.path().join("e2").to_string_lossy().to_string(),
            dir.path().join("e3").to_string_lossy().to_string(),
        ];
        for p in &paths {
            std::fs::create_dir_all(p).unwrap();
            call(
                &tx,
                GcOp::Insert {
                    kind: "cache".to_string(),
                    path: p.clone(),
                    repo_root: None,
                    branch: None,
                    agent_id: None,
                    created_unix: Some(100),
                },
            );
        }

        // Snapshot the rows so we can pick the middle id by stable mapping.
        let list = match call(&tx, GcOp::List { kind: None }) {
            GcReply::ListOk { rows } => rows,
            other => panic!("unexpected reply: {other:?}"),
        };
        assert_eq!(list.len(), 3);
        let middle = list
            .iter()
            .find(|r| r.path == paths[1])
            .expect("middle row");

        let resp = call(&tx, GcOp::DeleteById { id: middle.id });
        match resp {
            GcReply::PurgeOk { removed, skipped } => {
                assert_eq!(removed, 1);
                assert_eq!(skipped, 0);
            }
            other => panic!("unexpected reply: {other:?}"),
        }

        // The two siblings must survive.
        let after = match call(&tx, GcOp::List { kind: None }) {
            GcReply::ListOk { rows } => rows,
            other => panic!("unexpected reply: {other:?}"),
        };
        let remaining: Vec<&str> = after.iter().map(|r| r.path.as_str()).collect();
        assert_eq!(after.len(), 2);
        assert!(remaining.contains(&paths[0].as_str()));
        assert!(remaining.contains(&paths[2].as_str()));
        assert!(!remaining.contains(&paths[1].as_str()));

        // The on-disk path for the targeted row should be gone too.
        assert!(!std::path::Path::new(&paths[1]).exists());
        // Siblings should still be on disk.
        assert!(std::path::Path::new(&paths[0]).exists());
        assert!(std::path::Path::new(&paths[2]).exists());
    }

    /// Deleting a non-existent id is idempotent (`removed=0, skipped=0`).
    /// Lets the dashboard refresh-then-click race resolve without a 500.
    #[test]
    fn delete_by_id_with_missing_id_is_idempotent() {
        let dir = tempfile::tempdir().unwrap();
        let db_path = dir.path().join("delete-missing.redb");
        let (tx, _g) = spawn_test_worker(&db_path);

        let resp = call(&tx, GcOp::DeleteById { id: 9_999_999 });
        match resp {
            GcReply::PurgeOk { removed, skipped } => {
                assert_eq!(removed, 0);
                assert_eq!(skipped, 0);
            }
            other => panic!("unexpected reply: {other:?}"),
        }
    }
}
