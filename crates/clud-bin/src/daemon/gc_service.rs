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
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use crate::gc::{
    reconcile_repo_root, InsertInput, Registry, TrackedEntry, EXTERN_REPO_KIND, SIBLING_CLONE_KIND,
    WORKTREE_KIND,
};
use crate::worktrees;

use super::types::{GcOp, GcReply, ListRow};

mod extern_repo;
mod filesystem;

use extern_repo::{extern_repo_is_purgeable, extern_repo_stale_after};
use filesystem::{
    collect_live_lock_paths, reap_trash_entries, remove_entry_and_delete_row,
    remove_entry_filesystem,
};

/// How long a connection thread waits for the registry worker before
/// giving up. Since #268 the worker no longer runs `remove_dir_all`
/// inline — bulk purges fan out to the purge pool and reply
/// `PurgeStarted` within milliseconds — so this timeout protects only
/// against a genuinely wedged worker thread.
pub(super) const WORKER_REPLY_TIMEOUT: Duration = Duration::from_secs(30);

const ENV_GC_TICK_SECS: &str = "CLUD_GC_TICK_SECS";
const ENV_GC_EXTERN_REPO_MAX_AGE_SECS: &str = "CLUD_GC_EXTERN_REPO_MAX_AGE_SECS";
const ENV_GC_WARN_FREE_GB: &str = "CLUD_GC_WARN_FREE_GB";
const ENV_GC_AUTO_PURGE_FREE_GB: &str = "CLUD_GC_AUTO_PURGE_FREE_GB";
const ENV_GC_MIN_AGE_HOURS: &str = "CLUD_GC_MIN_AGE_HOURS";
const ENV_GC_AUTO_PURGE_ENABLED: &str = "CLUD_GC_AUTO_PURGE_ENABLED";
/// Issue #268: per-daemon cap on parallel `remove_dir_all` workers
/// servicing the purge pool. Defaults to `min(num_cpus, 8)`. Setting
/// `0` falls back to the default (not synchronous-mode); to truly
/// disable parallelism the user should set it to `1`.
const ENV_GC_PURGE_CONCURRENCY: &str = "CLUD_GC_PURGE_CONCURRENCY";
const DEFAULT_GC_TICK_SECS: u64 = 3600;
const DEFAULT_EXTERN_REPO_STALE_AFTER_SECS: u64 = 24 * 60 * 60;
const DEFAULT_GC_WARN_FREE_GB: u64 = 10;
const DEFAULT_GC_AUTO_PURGE_FREE_GB: u64 = 5;
const DEFAULT_GC_MIN_AGE_HOURS: u64 = 24;
const DEFAULT_GC_AUTO_PURGE_ENABLED: bool = true;
const DEFAULT_GC_PURGE_CONCURRENCY_CAP: usize = 8;
const PERIODIC_GC_WORKTREE_STALE_AFTER: &str = "48h";
const BYTES_PER_GB: u64 = 1024 * 1024 * 1024;

/// One client-initiated GC op handed from a connection thread (or HTTP
/// handler) to the registry worker. The worker processes it inline and
/// fires `reply_tx` exactly once with the resulting `GcReply`.
pub(super) struct GcRequestMsg {
    pub(super) op: GcOp,
    pub(super) reply_tx: mpsc::SyncSender<GcReply>,
}

/// Issue #268: the worker's mpsc carries two kinds of message. Client
/// ops (`Op`) are fast — they touch redb only — and reply on
/// `GcRequestMsg::reply_tx`. Purge completions (`PurgeCompletion`) are
/// fire-and-forget callbacks from the purge-pool threads: they tell the
/// worker that the filesystem half of one entry's deletion has finished
/// (success or failure) so the worker can run `registry.delete(id)`
/// — also fast — and log the outcome.
pub(super) enum RegistryMsg {
    Op(GcRequestMsg),
    PurgeCompletion(PurgeCompletion),
}

/// Issue #268: result of one entry's parallel filesystem deletion,
/// delivered back to the registry worker so it can drop the
/// corresponding redb row on success or log+keep on failure.
pub(super) struct PurgeCompletion {
    pub(super) id: i64,
    pub(super) path: String,
    pub(super) kind: String,
    pub(super) result: Result<(), String>,
}

/// One unit of work for the purge pool — the entry to remove plus the
/// channel a completion message should be sent back on. `completion_tx`
/// is a clone of the registry worker's own `RegistryMsg` sender; the
/// pool thread sends `RegistryMsg::PurgeCompletion(..)` into it, so
/// completions land in the same queue the worker is already draining.
struct PurgeJob {
    entry: TrackedEntry,
    completion_tx: mpsc::Sender<RegistryMsg>,
}

type LiveCwdsProvider = Arc<dyn Fn() -> Vec<PathBuf> + Send + Sync + 'static>;

fn purge_concurrency_from_env() -> usize {
    purge_concurrency_from_raw(std::env::var(ENV_GC_PURGE_CONCURRENCY).ok().as_deref())
}

fn purge_concurrency_from_raw(raw: Option<&str>) -> usize {
    raw.and_then(|v| v.trim().parse::<usize>().ok())
        .filter(|n| *n > 0)
        .unwrap_or_else(default_purge_concurrency)
}

fn default_purge_concurrency() -> usize {
    let cpus = std::thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(1);
    cpus.clamp(1, DEFAULT_GC_PURGE_CONCURRENCY_CAP)
}

/// Spawn N long-lived pool threads, each pulling `PurgeJob` items off
/// a shared mpsc and running `remove_entry_filesystem` in parallel.
/// Returns the sender every dispatch site uses to enqueue jobs.
/// Dropping the sender signals the pool threads to exit.
fn spawn_purge_pool(concurrency: usize) -> mpsc::Sender<PurgeJob> {
    let (tx, rx) = mpsc::channel::<PurgeJob>();
    let shared_rx = Arc::new(Mutex::new(rx));
    for i in 0..concurrency {
        let rx = Arc::clone(&shared_rx);
        let _ = thread::Builder::new()
            .name(format!("clud-gc-purge-{i}"))
            .spawn(move || purge_pool_worker(rx));
    }
    tx
}

fn purge_pool_worker(rx: Arc<Mutex<mpsc::Receiver<PurgeJob>>>) {
    loop {
        let job = {
            let guard = match rx.lock() {
                Ok(g) => g,
                Err(poisoned) => poisoned.into_inner(),
            };
            guard.recv()
        };
        let Ok(job) = job else { return };
        let result = remove_entry_filesystem(&job.entry);
        let completion = PurgeCompletion {
            id: job.entry.id,
            path: job.entry.path.clone(),
            kind: job.entry.kind.clone(),
            result,
        };
        // If the worker has hung up we just drop the completion;
        // there's nothing left to apply it against.
        let _ = job
            .completion_tx
            .send(RegistryMsg::PurgeCompletion(completion));
    }
}

/// Open the registry and spawn the single worker thread. Returns the
/// sender every connection thread uses to dispatch GC ops. Caller keeps
/// the sender alive for the daemon's lifetime; dropping it stops the
/// worker.
#[cfg(test)]
pub(super) fn spawn_registry_worker() -> std::io::Result<mpsc::Sender<RegistryMsg>> {
    let registry = Registry::open_default().map_err(std::io::Error::other)?;
    spawn_registry_worker_with(registry)
}

pub(super) fn spawn_registry_worker_for_state(
    state_dir: PathBuf,
) -> std::io::Result<mpsc::Sender<RegistryMsg>> {
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
) -> std::io::Result<mpsc::Sender<RegistryMsg>> {
    spawn_registry_worker_with_live_cwds(registry, Arc::new(Vec::<PathBuf>::new))
}

fn spawn_registry_worker_with_live_cwds(
    registry: Registry,
    live_cwds_provider: LiveCwdsProvider,
) -> std::io::Result<mpsc::Sender<RegistryMsg>> {
    let (tx, rx) = mpsc::channel::<RegistryMsg>();
    let pool_tx = spawn_purge_pool(purge_concurrency_from_env());
    let completion_tx = tx.clone();
    let tick_cadence = gc_tick_cadence_from_env();
    thread::Builder::new()
        .name("clud-gc-registry-worker".to_string())
        .spawn(move || {
            run_worker_loop(
                registry,
                rx,
                completion_tx,
                pool_tx,
                tick_cadence,
                live_cwds_provider,
            )
        })?;
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
    rx: mpsc::Receiver<RegistryMsg>,
    completion_tx: mpsc::Sender<RegistryMsg>,
    pool_tx: mpsc::Sender<PurgeJob>,
    tick_cadence: Option<Duration>,
    live_cwds_provider: LiveCwdsProvider,
) {
    let Some(tick_cadence) = tick_cadence else {
        while let Ok(msg) = rx.recv() {
            handle_registry_msg(
                &registry,
                &pool_tx,
                &completion_tx,
                msg,
                &live_cwds_provider,
            );
        }
        return;
    };

    let mut next_tick = Instant::now() + tick_cadence;
    loop {
        let timeout = next_tick.saturating_duration_since(Instant::now());
        match rx.recv_timeout(timeout) {
            Ok(msg) => {
                handle_registry_msg(
                    &registry,
                    &pool_tx,
                    &completion_tx,
                    msg,
                    &live_cwds_provider,
                );
                if Instant::now() >= next_tick {
                    run_periodic_purge_tick(
                        &registry,
                        &pool_tx,
                        &completion_tx,
                        &live_cwds_provider,
                    );
                    next_tick = Instant::now() + tick_cadence;
                }
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {
                run_periodic_purge_tick(&registry, &pool_tx, &completion_tx, &live_cwds_provider);
                next_tick = Instant::now() + tick_cadence;
            }
            Err(mpsc::RecvTimeoutError::Disconnected) => break,
        }
    }
}

fn handle_registry_msg(
    registry: &Registry,
    pool_tx: &mpsc::Sender<PurgeJob>,
    completion_tx: &mpsc::Sender<RegistryMsg>,
    msg: RegistryMsg,
    live_cwds_provider: &LiveCwdsProvider,
) {
    match msg {
        RegistryMsg::Op(req) => {
            let reply = process_op(
                registry,
                pool_tx,
                completion_tx,
                req.op,
                live_cwds_provider(),
            );
            // Hung-up callers are fine — the worker keeps serving the rest.
            let _ = req.reply_tx.send(reply);
        }
        RegistryMsg::PurgeCompletion(c) => apply_purge_completion(registry, c),
    }
}

/// Apply one parallel-purge completion: on success, drop the redb row
/// for the removed entry; on failure, log and keep the row so the next
/// purge attempt retries. Stale rows during/after a partial purge are
/// acceptable — eventual consistency by design (#268).
fn apply_purge_completion(registry: &Registry, c: PurgeCompletion) {
    match c.result {
        Ok(()) => match registry.delete(c.id) {
            Ok(()) => eprintln!("[gc] purge: removed {} ({})", c.path, c.kind),
            Err(err) => eprintln!(
                "[gc] purge: removed dir but failed to delete redb row id={} path={} ({}): {err}",
                c.id, c.path, c.kind
            ),
        },
        Err(message) => {
            eprintln!(
                "[gc] purge: failed to remove {} ({}): {message}",
                c.path, c.kind
            );
        }
    }
}

fn run_periodic_purge_tick(
    registry: &Registry,
    pool_tx: &mpsc::Sender<PurgeJob>,
    completion_tx: &mpsc::Sender<RegistryMsg>,
    live_cwds_provider: &LiveCwdsProvider,
) {
    let config = GcDiskWatchdogConfig::from_env();
    run_periodic_purge_tick_with_free_space(
        registry,
        pool_tx,
        completion_tx,
        live_cwds_provider,
        &config,
        &free_space_bytes_for_path,
    );
}

fn run_periodic_purge_tick_with_free_space<F>(
    registry: &Registry,
    pool_tx: &mpsc::Sender<PurgeJob>,
    completion_tx: &mpsc::Sender<RegistryMsg>,
    live_cwds_provider: &LiveCwdsProvider,
    disk_config: &GcDiskWatchdogConfig,
    free_space: &F,
) where
    F: Fn(&Path) -> Result<u64, String> + ?Sized,
{
    run_disk_watchdog_tick(
        registry,
        pool_tx,
        completion_tx,
        live_cwds_provider,
        disk_config,
        free_space,
    );

    let worktree_reply = process_op(
        registry,
        pool_tx,
        completion_tx,
        GcOp::Purge {
            duration: Some(PERIODIC_GC_WORKTREE_STALE_AFTER.to_string()),
            kind: Some(WORKTREE_KIND.to_string()),
            dry_run: false,
        },
        live_cwds_provider(),
    );
    log_periodic_purge_reply(WORKTREE_KIND, worktree_reply);

    let extern_reply = process_op(
        registry,
        pool_tx,
        completion_tx,
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

    // Issue #423: daily 7-day sweep of stale uv-cache envs. The
    // helper handles its own 24h sentinel under ~/.clud/state/ so
    // this call is cheap on every tick (one stat + age compare).
    crate::daemon::uv_cache_sweep::maybe_sweep_uv_cache();

    // Issues #509/#510: the session-temp (48h) and target/ (opt-in) sweeps
    // walk the filesystem and can take a while, so they run on a detached
    // background thread rather than blocking this tick loop. They prioritize
    // by disk pressure and system load — see `spawn_maintenance_sweeps`.
    spawn_maintenance_sweeps(disk_config);
}

/// Guards against overlapping background maintenance sweeps. The tick fires
/// every cadence; a long `target/` walk can outlast one interval, so we never
/// spawn a second sweep thread while one is still in flight.
static MAINTENANCE_SWEEP_IN_FLIGHT: AtomicBool = AtomicBool::new(false);

/// Global-CPU ceiling (percent) above which the non-urgent sweeps defer.
const ENV_GC_SWEEP_MAX_CPU_PCT: &str = "CLUD_GC_SWEEP_MAX_CPU_PCT";
const DEFAULT_GC_SWEEP_MAX_CPU_PCT: f32 = 60.0;

/// What the background maintenance sweep should do this cycle.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum MaintenanceAction {
    /// Disk is low — reclaim immediately, bypassing the sentinel throttle.
    RunUrgent,
    /// Disk is fine and the box is idle — run the throttled sweeps.
    RunNormal,
    /// Disk is fine but the box is busy — skip; the next tick retries.
    Defer,
}

/// Pure decision: disk pressure wins over everything (reclaim now); otherwise
/// only run when the machine is not under load.
fn maintenance_action(low_disk: bool, cpu_busy: bool) -> MaintenanceAction {
    if low_disk {
        MaintenanceAction::RunUrgent
    } else if cpu_busy {
        MaintenanceAction::Defer
    } else {
        MaintenanceAction::RunNormal
    }
}

/// Issues #509/#510: fan the filesystem sweeps onto a detached thread so a
/// long walk never blocks the GC tick loop. At most one sweep thread runs at
/// a time (guarded by [`MAINTENANCE_SWEEP_IN_FLIGHT`]).
fn spawn_maintenance_sweeps(disk_config: &GcDiskWatchdogConfig) {
    if MAINTENANCE_SWEEP_IN_FLIGHT
        .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
        .is_err()
    {
        // A previous sweep is still running — don't stack another.
        return;
    }
    let warn_free_bytes = disk_config.warn_free_bytes;
    let spawned = std::thread::Builder::new()
        .name("clud-gc-sweep".to_string())
        .spawn(move || {
            run_maintenance_sweeps(warn_free_bytes);
            MAINTENANCE_SWEEP_IN_FLIGHT.store(false, Ordering::Release);
        });
    if spawned.is_err() {
        // Thread spawn failed — release the guard so the next tick can retry.
        MAINTENANCE_SWEEP_IN_FLIGHT.store(false, Ordering::Release);
    }
}

/// Body of the background sweep thread. Runs the CPU sample + disk probe here
/// (off the tick loop) and then dispatches per [`maintenance_action`].
fn run_maintenance_sweeps(warn_free_bytes: u64) {
    let low_disk = maintenance_disk_pressure(warn_free_bytes);
    // Only pay for the ~200ms CPU sample when it can change the decision.
    let cpu_busy = if low_disk { false } else { system_cpu_busy() };
    match maintenance_action(low_disk, cpu_busy) {
        MaintenanceAction::RunUrgent => {
            crate::daemon::session_tmp_sweep::sweep_now();
            crate::daemon::target_sweep::sweep_now();
        }
        MaintenanceAction::RunNormal => {
            crate::daemon::session_tmp_sweep::maybe_sweep_session_tmp();
            crate::daemon::target_sweep::maybe_sweep_targets();
        }
        MaintenanceAction::Defer => {}
    }
}

/// True when free space on the `~/.clud` volume (session temp + uv cache live
/// there) or any configured target root is below `warn_free_bytes`.
fn maintenance_disk_pressure(warn_free_bytes: u64) -> bool {
    let mut paths: Vec<PathBuf> = Vec::new();
    if let Some(dir) = crate::gc::session_tmp::session_tmp_dir() {
        paths.push(dir);
    }
    paths.extend(crate::daemon::target_sweep::configured_roots());
    paths
        .iter()
        .any(|path| matches!(free_space_bytes_for_path(path), Ok(free) if free < warn_free_bytes))
}

/// Sample global CPU usage over a short window; true when above the configured
/// ceiling (default 60%, override `CLUD_GC_SWEEP_MAX_CPU_PCT`). Called on the
/// background thread, so the mandatory ~200ms settle never touches the tick.
fn system_cpu_busy() -> bool {
    let ceiling = sweep_cpu_ceiling_pct(std::env::var(ENV_GC_SWEEP_MAX_CPU_PCT).ok().as_deref());
    let mut sys = sysinfo::System::new();
    sys.refresh_cpu_usage();
    std::thread::sleep(sysinfo::MINIMUM_CPU_UPDATE_INTERVAL);
    sys.refresh_cpu_usage();
    sys.global_cpu_usage() > ceiling
}

fn sweep_cpu_ceiling_pct(raw: Option<&str>) -> f32 {
    raw.and_then(|v| v.trim().parse::<f32>().ok())
        .filter(|p| p.is_finite() && *p > 0.0)
        .unwrap_or(DEFAULT_GC_SWEEP_MAX_CPU_PCT)
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
    pool_tx: &mpsc::Sender<PurgeJob>,
    completion_tx: &mpsc::Sender<RegistryMsg>,
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
        pool_tx,
        completion_tx,
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
    pool_tx: &mpsc::Sender<PurgeJob>,
    completion_tx: &mpsc::Sender<RegistryMsg>,
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
    dispatch_purge_entries(pool_tx, completion_tx, candidates, live_cwds_provider())
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
        GcReply::PurgeStarted {
            dispatched,
            skipped,
        } => {
            eprintln!(
                "[clud] gc tick disk: auto-purge checked {low_root_count} low root(s), min age {}h, dispatched {dispatched}, skipped {skipped}",
                config.min_age.as_secs() / (60 * 60)
            );
        }
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
        GcReply::PurgeStarted {
            dispatched,
            skipped,
        } => {
            eprintln!("[clud] gc tick {kind}: dispatched {dispatched}, skipped {skipped}");
        }
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

fn process_op(
    registry: &Registry,
    pool_tx: &mpsc::Sender<PurgeJob>,
    completion_tx: &mpsc::Sender<RegistryMsg>,
    op: GcOp,
    live_cwds: Vec<PathBuf>,
) -> GcReply {
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
            if dry_run {
                dry_run_purge_entries(candidates, live_cwds)
            } else {
                dispatch_purge_entries(pool_tx, completion_tx, candidates, live_cwds)
            }
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
                Ok(_) => GcReply::InsertOk,
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

/// Filter `candidates` down to entries that are safe to purge. Returns
/// `(purgeable, skipped)`. Centralizes the live-lock / live-cwd /
/// kind-allows-purge gates so the dry-run and dispatch paths agree on
/// what counts as eligible.
fn partition_purgeable(
    candidates: Vec<TrackedEntry>,
    live_cwds: Vec<PathBuf>,
) -> (Vec<TrackedEntry>, usize) {
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
    (purgeable, skipped)
}

fn dry_run_purge_entries(candidates: Vec<TrackedEntry>, live_cwds: Vec<PathBuf>) -> GcReply {
    let (purgeable, skipped) = partition_purgeable(candidates, live_cwds);
    GcReply::PurgeOk {
        removed: purgeable.len(),
        skipped,
    }
}

/// Issue #268: fan out each purgeable entry to the purge pool and
/// return immediately with `PurgeStarted`. The pool threads each run
/// `remove_entry_filesystem` in parallel and report completion via
/// `RegistryMsg::PurgeCompletion`, which the registry worker applies
/// to redb asynchronously. Bias: delete first, update index after —
/// the redb writer never blocks on filesystem work.
fn dispatch_purge_entries(
    pool_tx: &mpsc::Sender<PurgeJob>,
    completion_tx: &mpsc::Sender<RegistryMsg>,
    candidates: Vec<TrackedEntry>,
    live_cwds: Vec<PathBuf>,
) -> GcReply {
    let (purgeable, skipped) = partition_purgeable(candidates, live_cwds);
    let mut dispatched = 0usize;
    for entry in purgeable {
        let job = PurgeJob {
            entry,
            completion_tx: completion_tx.clone(),
        };
        if pool_tx.send(job).is_err() {
            // Pool hung up — likely daemon teardown. Report what we
            // managed to enqueue plus an explanatory error so the
            // caller doesn't silently think the rest of the purge is
            // still in flight.
            return GcReply::Error {
                message: format!(
                    "gc purge pool stopped after dispatching {dispatched} of {} entr{}",
                    dispatched + 1,
                    if dispatched == 0 { "y" } else { "ies" }
                ),
            };
        }
        dispatched += 1;
    }
    GcReply::PurgeStarted {
        dispatched,
        skipped,
    }
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

fn now_unix() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests;
