use super::*;

/// Issue #466: assemble the [`cpu_banner::CpuBannerCfg`] from CLI flags
/// and `~/.clud/settings.json`. Returns the disabled variant whenever the
/// banner would be wrong to print: `--no-cpu-banner`, `--dry-run`,
/// `--detach`, `--detachable`, `--repeat`, or the
/// `[foreground.cpu_banner] enabled = false` settings toggle. Otherwise
/// reads `heartbeat_secs` from settings (default 30) and resolves
/// `num_cpus` via `std::thread::available_parallelism` (no syscall on the
/// hot path; falls back to 1 if probing fails).
pub(super) fn build_cpu_banner_cfg(
    args: &args::Args,
    plan: &command::LaunchPlan,
) -> cpu_banner::CpuBannerCfg {
    if args.no_cpu_banner
        || args.dry_run
        || args.detach
        || args.detachable
        || plan.repeat_schedule.is_some()
    {
        return cpu_banner::CpuBannerCfg::disabled();
    }
    let settings = clud_settings::load_cpu_banner_settings().unwrap_or_default();
    if !settings.enabled {
        return cpu_banner::CpuBannerCfg::disabled();
    }
    let num_cpus = std::thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(1);
    let mut cfg = cpu_banner::CpuBannerCfg::new(std::process::id(), num_cpus);
    cfg.heartbeat_secs = settings.heartbeat_secs;
    cfg
}

/// Write the cross-path Ctrl+C exit-timing event (issue: `clud ui` ctrl-c
/// tracking) if a Ctrl+C was observed during this process's lifetime.
/// Best-effort: resolves the state dir lazily so an unreadable home dir
/// can't block the process exit. Errors are swallowed inside the tracker.
pub(super) fn flush_ctrl_c_exit_event(kind: ctrl_c_track::InvocationKind, exit_code: i32) {
    if !ctrl_c_track::was_observed() {
        return;
    }
    if let Ok(state_dir) = daemon::default_state_dir() {
        ctrl_c_track::flush_on_exit(&state_dir, kind, exit_code);
    }
}

/// Issue #183: best-effort upsert of `(repo_root, cwd)` into the daemon's
/// `repo_visits` table. Resolves the current git root; if there isn't one,
/// this is a no-op (we don't track scratch-dir launches). All errors are
/// swallowed — recording a repo visit must never block a launch.
pub(super) fn record_repo_visit_best_effort(state_dir: &std::path::Path, verbose: bool) {
    let cwd = match std::env::current_dir() {
        Ok(p) => p,
        Err(_) => return,
    };
    // `git_root_from` returns its input verbatim when no `.git` is found
    // anywhere up the tree. We treat that as "not in a repo" and skip,
    // so we don't accumulate one row per random scratch directory.
    let repo_root = loop_spec::git_root_from(&cwd);
    if !repo_root.join(".git").exists() {
        return;
    }
    if let Err(e) = daemon::gc_client_record_repo_visit(state_dir, &repo_root, &cwd) {
        if verbose {
            verbose_log::log(format_args!("[clud] repo_visit: {e}"));
        }
    }
}
