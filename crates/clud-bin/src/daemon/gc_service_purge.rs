use super::*;

/// `spare_reasons` accumulates each extern-repo verdict so `clud gc list`
/// can explain a retained row without re-running the git probe (issue #896,
/// and see `list_state`). It is owned by the registry worker loop, which is
/// also the thread that runs the periodic tick, so no lock is involved.
pub(super) fn partition_purgeable(
    candidates: Vec<TrackedEntry>,
    live_cwds: Vec<PathBuf>,
    spare_reasons: &mut SpareReasons,
) -> (Vec<TrackedEntry>, usize) {
    let live_locks = collect_live_lock_paths();
    let live_cwds = canonicalize_live_cwds(live_cwds);
    let now = now_unix();
    let mut purgeable = Vec::new();
    let mut skipped = 0usize;
    for candidate in candidates {
        if entry_is_live(&candidate, &live_locks, &live_cwds) {
            // Record it. `list_state` can re-derive only the *worktree*
            // lock flavour of liveness (`live_locked`); the other flavour,
            // an agent cwd inside the entry, is exactly what spares
            // extern-repo rows and is invisible to `gc list`. Skipping the
            // insert here would let the end-of-tick eviction drop such a
            // row's prior verdict, and the checkout GC is actively
            // refusing to touch would then report as plain `reclaimable`.
            //
            // Note this asks `entry_is_cacheable`, NOT `entry_purge_verdict`:
            // the latter runs the mtime walk and the git probe, and a live
            // entry has already been spared — paying three subprocesses just
            // to decide whether to write a marker would be the opposite of
            // what this cache exists for.
            if entry_is_cacheable(&candidate) {
                spare_reasons.record(candidate.path.clone(), live_session_decision(), now);
            }
            skipped += 1;
            continue;
        }
        if let Some(decision) = entry_purge_verdict(&candidate) {
            spare_reasons.record(candidate.path.clone(), decision, now);
            if !decision.purge {
                skipped += 1;
                continue;
            }
        }
        purgeable.push(candidate);
    }
    (purgeable, skipped)
}

pub(super) fn dry_run_purge_entries(
    candidates: Vec<TrackedEntry>,
    live_cwds: Vec<PathBuf>,
    spare_reasons: &mut SpareReasons,
) -> GcReply {
    let (purgeable, skipped) = partition_purgeable(candidates, live_cwds, spare_reasons);
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
    spare_reasons: &mut SpareReasons,
) -> GcReply {
    let (purgeable, skipped) = partition_purgeable(candidates, live_cwds, spare_reasons);
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

/// The verdict recorded for an entry spared because a live session holds
/// it. Not produced by `extern_repo_purge_decision`, which never sees the
/// liveness check — that gate runs earlier, in `partition_purgeable`.
pub(super) fn live_session_decision() -> PurgeDecision {
    PurgeDecision {
        purge: false,
        reason: "spared: live session",
        class: PurgeClass::Pinned,
    }
}

/// The kind-specific purge verdict, or `None` for kinds that have no
/// extra gate beyond the shared liveness check (they are always allowed).
/// Returning the decision rather than a bare bool keeps the reason
/// available to `gc list` (issue #896) instead of discarding it here.
pub(super) fn entry_purge_verdict(entry: &TrackedEntry) -> Option<PurgeDecision> {
    if entry.kind == EXTERN_REPO_KIND {
        return Some(extern_repo_purge_verdict(entry, extern_repo_stale_after()));
    }
    None
}

/// Whether this kind produces a verdict worth caching for `gc list`.
/// Cheap by design — the same question as [`entry_purge_verdict`] but
/// without computing the answer, for callers that only need to know
/// whether a cache slot applies.
pub(super) fn entry_is_cacheable(entry: &TrackedEntry) -> bool {
    entry.kind == EXTERN_REPO_KIND
}

pub(super) fn now_unix() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}
