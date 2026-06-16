use std::path::Path;
use std::time::Duration;

use clap::CommandFactory;

use super::registry::now_unix;
use crate::args::{Args, GcSubcommand};
use crate::worktrees;

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
    if sub.is_none() {
        return print_help_and_exit_zero();
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
    match sub.unwrap() {
        GcSubcommand::List { json, kind } => cmd_list(&state_dir, json, kind.as_deref()),
        GcSubcommand::Purge {
            duration,
            dry_run,
            yes,
            kind,
        } => cmd_purge(
            &state_dir,
            duration.as_deref(),
            dry_run,
            yes,
            kind.as_deref(),
        ),
        GcSubcommand::Reconcile => cmd_reconcile(&state_dir),
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
            0
        }
        None => {
            eprintln!("error: gc subcommand definition missing (internal bug)");
            2
        }
    }
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

fn cmd_purge(
    state_dir: &Path,
    duration: Option<&str>,
    dry_run: bool,
    yes: bool,
    kind_filter: Option<&str>,
) -> i32 {
    // Pre-flight: validate the duration string before contacting the
    // daemon (gives a clean exit-2 with a specific message for malformed
    // input).
    if let Some(d) = duration {
        if let Err(e) = worktrees::parse_duration(d) {
            eprintln!("error: invalid duration: {e}");
            return 2;
        }
    }

    // Interactive safety prompt for purge-all (no duration). When `--yes`
    // is passed, skip. When `--dry-run` is passed, the daemon does not
    // actually delete anything anyway.
    if !dry_run && !yes && duration.is_none() && !confirm_purge_all() {
        println!("aborted.");
        return 0;
    }

    // Pre-purge reconcile so the daemon's view matches the current repo's
    // `.claude/worktrees/`. Best-effort.
    if let Ok(main_root) = worktrees::locate_main_repo_root() {
        let _ = crate::daemon::gc_client_reconcile(state_dir, &main_root);
    }

    match crate::daemon::gc_client_purge(state_dir, duration, kind_filter, dry_run) {
        Ok(crate::daemon::GcPurgeOutcome::Completed { removed, skipped }) => {
            if dry_run {
                println!("--dry-run: would remove {removed}, skip {skipped}.");
            } else {
                println!("summary: removed {removed}, skipped {skipped}.");
            }
            0
        }
        Ok(crate::daemon::GcPurgeOutcome::Started {
            dispatched,
            skipped,
        }) => {
            // Issue #268: bulk purges fan out across the daemon's
            // purge pool; the actual `remove_dir_all` calls and the
            // matching redb deletes happen in the background. The
            // daemon's stderr log records each completion; running
            // `clud gc list` again will show the registry shrinking.
            println!(
                "purge: dispatched {dispatched} delete{} in background, skipped {skipped}.",
                if dispatched == 1 { "" } else { "s" }
            );
            println!("(deletes happen asynchronously; re-run `clud gc list` to watch the registry shrink)");
            0
        }
        Err(e) => {
            eprintln!("error: purge failed: {e}");
            1
        }
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

fn confirm_purge_all() -> bool {
    use std::io::{self, Write};
    print!("purge ALL non-live-locked entries? [y/N] ");
    let _ = io::stdout().flush();
    let mut line = String::new();
    if io::stdin().read_line(&mut line).is_err() {
        return false;
    }
    matches!(line.trim().to_ascii_lowercase().as_str(), "y" | "yes")
}
