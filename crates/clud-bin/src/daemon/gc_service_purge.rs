use super::*;

pub(super) fn partition_purgeable(
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

pub(super) fn dry_run_purge_entries(
    candidates: Vec<TrackedEntry>,
    live_cwds: Vec<PathBuf>,
) -> GcReply {
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
pub(super) fn dispatch_purge_entries(
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

pub(super) fn canonicalize_live_cwds(live_cwds: Vec<PathBuf>) -> Vec<PathBuf> {
    let mut out: Vec<PathBuf> = live_cwds
        .into_iter()
        .filter_map(|path| std::fs::canonicalize(path).ok())
        .collect();
    out.sort();
    out.dedup();
    out
}

pub(super) fn entry_is_live(
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

pub(super) fn entry_path_contains_live_cwd(entry: &TrackedEntry, live_cwds: &[PathBuf]) -> bool {
    let Ok(entry_path) = std::fs::canonicalize(&entry.path) else {
        return false;
    };
    live_cwds
        .iter()
        .any(|cwd| cwd == &entry_path || cwd.starts_with(&entry_path))
}

pub(super) fn entry_kind_allows_purge(entry: &TrackedEntry) -> bool {
    if entry.kind == EXTERN_REPO_KIND {
        return extern_repo_is_purgeable(entry, extern_repo_stale_after());
    }
    true
}

pub(super) fn now_unix() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}
