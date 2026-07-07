use std::path::Path;
use std::time::{Duration, SystemTime};

use clap::CommandFactory;

use super::registry::now_unix;
use super::uv_cache;
use crate::args::{Args, GcSubcommand};
use crate::gc::{EXTERN_REPO_KIND, SIBLING_CLONE_KIND, WORKTREE_KIND};
use crate::worktrees;

/// Literal value of `--kind` that routes to the filesystem-managed
/// uv-cache handlers instead of the redb-tracked daemon path.
const UV_CACHE_KIND: &str = "uv-cache";
const TRASH_KIND: &str = "trash";
const TRACKED_PRUNE_DURATION: &str = "48h";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum GcKindBackend {
    Tracked,
    UvCache,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct GcKindSpec {
    name: &'static str,
    summary: &'static str,
    backend: GcKindBackend,
    prune_duration: Option<&'static str>,
}

const MANAGED_KINDS: &[GcKindSpec] = &[
    GcKindSpec {
        name: WORKTREE_KIND,
        summary: "agent worktrees tracked in the daemon registry",
        backend: GcKindBackend::Tracked,
        prune_duration: Some(TRACKED_PRUNE_DURATION),
    },
    GcKindSpec {
        name: SIBLING_CLONE_KIND,
        summary: "repo sibling clones tracked in the daemon registry",
        backend: GcKindBackend::Tracked,
        prune_duration: Some(TRACKED_PRUNE_DURATION),
    },
    GcKindSpec {
        name: EXTERN_REPO_KIND,
        summary: "repo-local .extern-repos checkouts",
        backend: GcKindBackend::Tracked,
        prune_duration: None,
    },
    GcKindSpec {
        name: TRASH_KIND,
        summary: "quarantined paths under ~/.clud/trash/",
        backend: GcKindBackend::Tracked,
        prune_duration: None,
    },
    GcKindSpec {
        name: UV_CACHE_KIND,
        summary: "bundled Python tool uv environments under ~/.clud/cache/uv/",
        backend: GcKindBackend::UvCache,
        prune_duration: None,
    },
];

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
    let Some(sub) = sub else {
        return print_help_and_exit_zero();
    };

    if let Some(code) = validate_pre_daemon(&sub) {
        return code;
    }

    // Issue #422: `--kind uv-cache` is filesystem-managed (not redb-tracked),
    // so it short-circuits the daemon roundtrip entirely. Handle it before
    // the daemon-required check so users without the daemon running can
    // still manage their uv cache.
    match &sub {
        GcSubcommand::List {
            json,
            kind: Some(k),
        } if k == UV_CACHE_KIND => return cmd_list_uv_cache(*json),
        GcSubcommand::Purge {
            dry_run,
            yes,
            kind: Some(k),
        } if k == UV_CACHE_KIND => {
            return cmd_purge_uv_cache(*dry_run, *yes);
        }
        GcSubcommand::Prune {
            dry_run,
            kind: Some(k),
        } if k == UV_CACHE_KIND => return cmd_prune_uv_cache(*dry_run),
        _ => {}
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
    match sub {
        GcSubcommand::List { json, kind } => cmd_list(&state_dir, json, kind.as_deref()),
        GcSubcommand::Prune { dry_run, kind } => {
            let spec = find_kind(kind.as_deref().unwrap_or_default()).expect("validated kind");
            cmd_prune_tracked(&state_dir, spec, dry_run)
        }
        GcSubcommand::Purge { dry_run, yes, kind } => {
            let spec = find_kind(kind.as_deref().unwrap_or_default()).expect("validated kind");
            cmd_purge_tracked(&state_dir, spec, dry_run, yes)
        }
        GcSubcommand::All {
            purge,
            dry_run,
            yes,
        } => cmd_all(&state_dir, purge, dry_run, yes),
        GcSubcommand::Reconcile => cmd_reconcile(&state_dir),
    }
}

fn validate_pre_daemon(sub: &GcSubcommand) -> Option<i32> {
    match sub {
        GcSubcommand::Prune { kind: None, .. } | GcSubcommand::Purge { kind: None, .. } => {
            eprintln!(
                "error: prune/purge requires --kind <name>; use `clud gc all` to operate on every managed kind."
            );
            Some(2)
        }
        GcSubcommand::Prune { kind: Some(k), .. }
        | GcSubcommand::Purge { kind: Some(k), .. }
        | GcSubcommand::List { kind: Some(k), .. }
            if find_kind(k).is_none() =>
        {
            eprintln!(
                "error: unknown gc kind `{k}`; managed kinds: {}",
                managed_kind_names()
            );
            Some(2)
        }
        GcSubcommand::Purge {
            dry_run: false,
            yes: false,
            kind: Some(k),
        } => {
            eprintln!("error: --yes required for `clud gc purge --kind {k}` (destructive)");
            Some(2)
        }
        GcSubcommand::All {
            purge: true,
            yes: false,
            ..
        } => {
            eprintln!("error: `clud gc all --purge` requires --yes.");
            Some(2)
        }
        GcSubcommand::All {
            purge: false,
            yes: true,
            ..
        } => {
            eprintln!("error: --yes only applies with `clud gc all --purge`.");
            Some(2)
        }
        _ => None,
    }
}

/// Issue #422: `clud gc list --kind uv-cache` — print env count, total
/// bytes, oldest mtime. Filesystem-only; no daemon needed.
fn cmd_list_uv_cache(json: bool) -> i32 {
    let summary = match uv_cache::list() {
        Ok(s) => s,
        Err(e) => {
            eprintln!("error: uv-cache list failed: {e}");
            return 1;
        }
    };
    if json {
        let oldest_unix = summary
            .oldest_mtime
            .and_then(|t| t.duration_since(SystemTime::UNIX_EPOCH).ok())
            .map(|d| d.as_secs() as i64);
        println!(
            "{{\"kind\":\"uv-cache\",\"root\":{:?},\"exists\":{},\"env_count\":{},\"total_bytes\":{},\"oldest_mtime_unix\":{}}}",
            summary.root.to_string_lossy(),
            summary.exists,
            summary.env_count,
            summary.total_bytes,
            oldest_unix
                .map(|s| s.to_string())
                .unwrap_or_else(|| "null".into()),
        );
        return 0;
    }
    println!("uv-cache root: {}", summary.root.display());
    if !summary.exists {
        println!("(cache directory does not exist; no envs materialized yet)");
        return 0;
    }
    println!("envs:        {}", summary.env_count);
    println!(
        "total bytes: {} ({})",
        summary.total_bytes,
        format_bytes(summary.total_bytes)
    );
    if let Some(mtime) = summary.oldest_mtime {
        let age = SystemTime::now().duration_since(mtime).unwrap_or_default();
        println!("oldest env:  {} old", worktrees::fmt_age(age));
    }
    0
}

/// `clud gc prune --kind uv-cache` runs the same stale-env sweep as the
/// daemon's daily tick. Full cache deletion lives under `purge`.
fn cmd_prune_uv_cache(dry_run: bool) -> i32 {
    match uv_cache::sweep_stale(SystemTime::now(), dry_run) {
        Ok(report) => {
            print_uv_cache_sweep_report(&report);
            0
        }
        Err(e) => {
            eprintln!("error: uv-cache prune failed: {e}");
            1
        }
    }
}

fn cmd_purge_uv_cache(dry_run: bool, yes: bool) -> i32 {
    if !dry_run && !yes {
        eprintln!("error: --yes required for `clud gc purge --kind uv-cache` (destructive)");
        return 2;
    }
    if dry_run {
        println!("uv-cache: --dry-run would purge all of ~/.clud/cache/uv/.");
        return 0;
    }
    match uv_cache::purge_all() {
        Ok(()) => {
            println!("uv-cache: purged.");
            0
        }
        Err(e) => {
            eprintln!("error: uv-cache purge failed: {e}");
            1
        }
    }
}

fn print_uv_cache_sweep_report(report: &uv_cache::SweepReport) {
    let stale_word = if report.stale_envs_removed == 1 {
        ""
    } else {
        "s"
    };
    if report.dry_run {
        println!(
            "uv-cache: --dry-run would remove {} stale env{stale_word}, skip {} locked.",
            report.stale_envs_removed, report.locked_envs_skipped,
        );
    } else {
        println!(
            "uv-cache: pruned {} stale env{stale_word}, {} locked-skipped.",
            report.stale_envs_removed, report.locked_envs_skipped,
        );
    }
}

fn format_bytes(bytes: u64) -> String {
    const KB: u64 = 1024;
    const MB: u64 = KB * 1024;
    const GB: u64 = MB * 1024;
    if bytes >= GB {
        format!("{:.2} GB", bytes as f64 / GB as f64)
    } else if bytes >= MB {
        format!("{:.2} MB", bytes as f64 / MB as f64)
    } else if bytes >= KB {
        format!("{:.2} KB", bytes as f64 / KB as f64)
    } else {
        format!("{bytes} B")
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
            print!("{}", managed_kinds_help());
            0
        }
        None => {
            eprintln!("error: gc subcommand definition missing (internal bug)");
            2
        }
    }
}

fn find_kind(name: &str) -> Option<GcKindSpec> {
    MANAGED_KINDS.iter().copied().find(|kind| kind.name == name)
}

fn managed_kind_names() -> String {
    MANAGED_KINDS
        .iter()
        .map(|kind| kind.name)
        .collect::<Vec<_>>()
        .join(", ")
}

fn managed_kinds_help() -> String {
    let mut out = String::from("\nKINDS:\n");
    for kind in MANAGED_KINDS {
        out.push_str(&format!("  {:<14} {}\n", kind.name, kind.summary));
    }
    out
}

fn cmd_list(state_dir: &Path, json: bool, kind_filter: Option<&str>) -> i32 {
    let rows = match crate::daemon::gc_client_list(state_dir, kind_filter) {
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

fn cmd_prune_tracked(state_dir: &Path, spec: GcKindSpec, dry_run: bool) -> i32 {
    debug_assert_eq!(spec.backend, GcKindBackend::Tracked);
    maybe_reconcile_current_repo(state_dir);
    run_tracked_gc(state_dir, spec, "prune", spec.prune_duration, dry_run)
}

fn cmd_purge_tracked(state_dir: &Path, spec: GcKindSpec, dry_run: bool, yes: bool) -> i32 {
    debug_assert_eq!(spec.backend, GcKindBackend::Tracked);
    if !dry_run && !yes {
        eprintln!(
            "error: --yes required for `clud gc purge --kind {}` (destructive)",
            spec.name
        );
        return 2;
    }
    maybe_reconcile_current_repo(state_dir);
    run_tracked_gc(state_dir, spec, "purge", None, dry_run)
}

fn cmd_all(state_dir: &Path, purge: bool, dry_run: bool, yes: bool) -> i32 {
    if purge && !yes {
        eprintln!("error: `clud gc all --purge` requires --yes.");
        return 2;
    }
    if !purge && yes {
        eprintln!("error: --yes only applies with `clud gc all --purge`.");
        return 2;
    }

    maybe_reconcile_current_repo(state_dir);
    let mut status = 0;
    for spec in MANAGED_KINDS {
        let code = match (spec.backend, purge) {
            (GcKindBackend::UvCache, false) => cmd_prune_uv_cache(dry_run),
            (GcKindBackend::UvCache, true) => cmd_purge_uv_cache(dry_run, yes),
            (GcKindBackend::Tracked, false) => {
                run_tracked_gc(state_dir, *spec, "prune", spec.prune_duration, dry_run)
            }
            (GcKindBackend::Tracked, true) => {
                run_tracked_gc(state_dir, *spec, "purge", None, dry_run)
            }
        };
        if code != 0 {
            status = 1;
        }
    }
    status
}

fn run_tracked_gc(
    state_dir: &Path,
    spec: GcKindSpec,
    action: &str,
    duration: Option<&str>,
    dry_run: bool,
) -> i32 {
    if let Some(d) = duration {
        if let Err(e) = worktrees::parse_duration(d) {
            eprintln!("error: invalid prune duration for {}: {e}", spec.name);
            return 2;
        }
    }

    match crate::daemon::gc_client_purge(state_dir, duration, Some(spec.name), dry_run) {
        Ok(outcome) => {
            print_tracked_gc_outcome(spec.name, action, dry_run, outcome);
            0
        }
        Err(e) => {
            eprintln!("error: {action} failed for {}: {e}", spec.name);
            1
        }
    }
}

fn print_tracked_gc_outcome(
    kind: &str,
    action: &str,
    dry_run: bool,
    outcome: crate::daemon::GcPurgeOutcome,
) {
    match outcome {
        crate::daemon::GcPurgeOutcome::Completed { removed, skipped } => {
            if dry_run {
                println!("{kind}: --dry-run would {action} {removed}, skip {skipped}.");
            } else {
                println!("{kind}: {action} removed {removed}, skipped {skipped}.");
            }
        }
        crate::daemon::GcPurgeOutcome::Started {
            dispatched,
            skipped,
        } => {
            println!(
                "{kind}: {action} dispatched {dispatched} delete{} in background, skipped {skipped}.",
                if dispatched == 1 { "" } else { "s" }
            );
            println!(
                "{kind}: deletes happen asynchronously; re-run `clud gc list --kind {kind}` to watch the registry shrink."
            );
        }
    }
}

fn maybe_reconcile_current_repo(state_dir: &Path) {
    if let Ok(main_root) = worktrees::locate_main_repo_root() {
        let _ = crate::daemon::gc_client_reconcile(state_dir, &main_root);
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn managed_kind_help_lists_every_registered_kind() {
        let help = managed_kinds_help();
        for kind in MANAGED_KINDS {
            assert!(
                help.contains(kind.name),
                "help should include managed kind {}",
                kind.name
            );
        }
    }

    #[test]
    fn managed_kind_names_are_unique() {
        let mut names = MANAGED_KINDS
            .iter()
            .map(|kind| kind.name)
            .collect::<Vec<_>>();
        names.sort();
        names.dedup();
        assert_eq!(names.len(), MANAGED_KINDS.len());
    }

    #[test]
    fn prune_and_purge_without_kind_fail_before_daemon() {
        assert_eq!(
            validate_pre_daemon(&GcSubcommand::Prune {
                dry_run: false,
                kind: None,
            }),
            Some(2)
        );
        assert_eq!(
            validate_pre_daemon(&GcSubcommand::Purge {
                dry_run: false,
                yes: false,
                kind: None,
            }),
            Some(2)
        );
    }

    #[test]
    fn unknown_kind_fails_before_daemon() {
        assert_eq!(
            validate_pre_daemon(&GcSubcommand::Prune {
                dry_run: false,
                kind: Some("missing-kind".to_string()),
            }),
            Some(2)
        );
    }

    #[test]
    fn purge_kind_requires_yes_before_daemon() {
        assert_eq!(
            validate_pre_daemon(&GcSubcommand::Purge {
                dry_run: false,
                yes: false,
                kind: Some("trash".to_string()),
            }),
            Some(2)
        );
        assert_eq!(
            validate_pre_daemon(&GcSubcommand::Purge {
                dry_run: true,
                yes: false,
                kind: Some("trash".to_string()),
            }),
            None
        );
    }

    #[test]
    fn all_purge_requires_yes_before_daemon() {
        assert_eq!(
            validate_pre_daemon(&GcSubcommand::All {
                purge: true,
                dry_run: false,
                yes: false,
            }),
            Some(2)
        );
    }
}
