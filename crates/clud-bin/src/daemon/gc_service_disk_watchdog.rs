use super::*;

pub(super) fn collect_tracked_entry_roots(entries: &[TrackedEntry]) -> Vec<PathBuf> {
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

pub(super) fn purge_old_reclaimable_entries_for_roots(
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

pub(super) fn duration_secs_i64(duration: Duration) -> i64 {
    duration.as_secs().min(i64::MAX as u64) as i64
}

/// Log the watchdog's purge reply, and report how many purge jobs it
/// dispatched to the pool.
///
/// The count is what makes an asynchronous purge observable to its caller
/// (issue #560). Only `PurgeStarted` dispatches: `PurgeOk` already finished
/// synchronously and will send no completions, and the error arms send none
/// either — so zero is the honest answer for all of them.
pub(super) fn log_disk_watchdog_purge_reply(
    low_root_count: usize,
    config: &GcDiskWatchdogConfig,
    reply: GcReply,
) -> usize {
    match reply {
        GcReply::PurgeStarted {
            dispatched,
            skipped,
        } => {
            eprintln!(
                "[clud] gc tick disk: auto-purge checked {low_root_count} low root(s), min age {}h, dispatched {dispatched}, skipped {skipped}",
                config.min_age.as_secs() / (60 * 60)
            );
            dispatched
        }
        GcReply::PurgeOk { removed, skipped } => {
            eprintln!(
                "[clud] gc tick disk: auto-purge checked {low_root_count} low root(s), min age {}h, removed {removed}, skipped {skipped}",
                config.min_age.as_secs() / (60 * 60)
            );
            0
        }
        GcReply::Error { message } => {
            eprintln!("[clud] gc tick disk: auto-purge error: {message}");
            0
        }
        other => {
            eprintln!("[clud] gc tick disk: unexpected auto-purge reply: {other:?}");
            0
        }
    }
}

pub(super) fn format_gb(bytes: u64) -> String {
    format!("{:.2}", bytes as f64 / BYTES_PER_GB as f64)
}

pub(super) fn free_space_bytes_for_path(path: &Path) -> Result<u64, String> {
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

/// Log a periodic purge reply and report how many jobs it dispatched. See
/// [`log_disk_watchdog_purge_reply`] for why the count is returned.
pub(super) fn log_periodic_purge_reply(kind: &str, reply: GcReply) -> usize {
    match reply {
        GcReply::PurgeStarted {
            dispatched,
            skipped,
        } => {
            eprintln!("[clud] gc tick {kind}: dispatched {dispatched}, skipped {skipped}");
            dispatched
        }
        GcReply::PurgeOk { removed, skipped } => {
            eprintln!("[clud] gc tick {kind}: removed {removed}, skipped {skipped}");
            0
        }
        GcReply::Error { message } => {
            eprintln!("[clud] gc tick {kind}: error: {message}");
            0
        }
        other => {
            eprintln!("[clud] gc tick {kind}: unexpected reply: {other:?}");
            0
        }
    }
}
