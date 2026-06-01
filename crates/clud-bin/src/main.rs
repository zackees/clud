use clud::{
    args, backend, backend_bootstrap, command, console_setup, console_title, daemon, gc,
    hook_health, large_file_guard, loop_artifacts, loop_spec, runner, skill_install, skills,
    startup, trampoline, trash, ui, verbose_log, wasm, worktrees,
};

use std::io::{self, IsTerminal, Read, Write};

fn main() {
    verbose_log::init_launch_clock();

    // Windows: rename ourselves so pip can always overwrite clud.exe.
    trampoline::unlock_exe();

    // Stamp the console title with `clud <cwd-name>` so the active
    // window is identifiable at a glance. Windows-only effective; a
    // no-op on POSIX (out of scope per the originating request).
    //
    // The one-shot stamp gets overwritten as soon as the backend (and
    // its tool subprocesses) emit OSC 0/2 sequences, so we also kick
    // off a background keeper that re-applies the title whenever it
    // drifts. PTY mode additionally strips the OSC sequences upstream
    // (see session.rs) so the keeper rarely fires and the title doesn't
    // visibly flicker. In subprocess mode (the default Claude path on
    // Windows) the child inherits stdio directly, so the keeper is the
    // only way to defend the title.
    console_title::set_for_current_cwd();
    console_title::keep_setting_in_background();

    // Expand bundled slash-command skills (clud-issue, clud-pr) into every
    // backend's global skills directory that already exists
    // (~/.claude/skills/ for Claude Code, ~/.codex/skills/ for Codex).
    // Existing files are left alone so user edits survive. Failures are
    // non-fatal — we log and continue, since a skills hiccup must never
    // block a launch.
    if let Err(e) = skills::ensure_installed() {
        eprintln!("[clud] note: could not install bundled skills: {e}");
    }

    // Additionally run the narrower `skill_install` flow from PR #88 (drift
    // detection for the single bundled `/clud-pr` skill). Redundant with the
    // broader `skills::ensure_installed()` above, but harmless — both flows
    // are idempotent and never overwrite existing user-edited skill files.
    skill_install::ensure_installed();

    let mut args = args::Args::parse_with_passthrough();
    if args.verbose {
        match verbose_log::enable_file_logging() {
            Ok(path) => {
                verbose_log::log(format_args!(
                    "[clud] verbose log: {}",
                    verbose_log::display_path(&path)
                ));
            }
            Err(err) => {
                verbose_log::log(format_args!("[clud] verbose log unavailable: {err}"));
            }
        }
        verbose_log::log(format_args!("[clud] pid {}", std::process::id()));
    }

    // Issue #110: `clud gc <subcommand>` is a self-contained
    // maintenance path that never launches a backend. Dispatch before
    // backend resolution and before any session registry / dnd work
    // so a registry-less host can still run `clud gc reconcile`.
    if let Some(args::Command::Gc { subcommand }) = &args.command {
        std::process::exit(gc::run(&args, subcommand.clone()));
    }

    // Issue #183: `clud ui` opens the local dashboard. Self-contained;
    // never launches a backend.
    if let Some(args::Command::Ui { json, no_open }) = &args.command {
        std::process::exit(ui::run(*json, *no_open));
    }

    // Issue #182: `clud trash` is self-contained maintenance. Dispatch
    // before backend resolution so quarantining a locked artifact never
    // launches an agent process.
    if let Some(args::Command::Trash {
        cross_volume,
        paths,
    }) = &args.command
    {
        std::process::exit(trash::run(&args, paths, *cross_volume));
    }

    // Issue #83: `--clean-worktrees` is a self-contained maintenance path.
    // It never launches a backend, so handle it before backend resolution.
    if args.clean_worktrees {
        let stale_after = match worktrees::parse_duration(&args.stale_after) {
            Ok(d) => d,
            Err(e) => {
                eprintln!("error: invalid --stale-after value: {e}");
                std::process::exit(2);
            }
        };
        let opts = worktrees::CleanOptions {
            stale_after,
            dry_run: args.dry_run,
            yes: args.yes,
            force: args.force,
        };
        std::process::exit(worktrees::run(&opts));
    }

    // Issue #112: explicit hook-parity remediation path. Normal launches only
    // warn; this flag is the opt-in path that may edit deterministic Codex
    // trust config and ask the selected backend to migrate hook definitions.
    if args.fix_hooks {
        let selected_backend = backend::resolve_backend(args.claude, args.codex);
        std::process::exit(hook_health::run_fix_hooks(&args, selected_backend));
    }

    // Pipe mode: if stdin is not a terminal, read it as the prompt.
    if args.prompt.is_none()
        && args.message.is_none()
        && args.command.is_none()
        && !console_setup::atty_is_terminal()
    {
        let mut input = String::new();
        if io::stdin().read_to_string(&mut input).is_ok() && !input.trim().is_empty() {
            args.prompt = Some(input.trim().to_string());
        }
    }

    if let Some(args::Command::Wasm { module, invoke }) = &args.command {
        if args.dry_run {
            let json = serde_json::json!({
                "mode": "wasm",
                "module": module,
                "invoke": invoke,
            });
            println!("{}", serde_json::to_string_pretty(&json).unwrap());
            std::process::exit(0);
        }

        match wasm::run_file(module, invoke) {
            Ok(code) => std::process::exit(code),
            Err(error) => {
                eprintln!("error: {error}");
                std::process::exit(1);
            }
        }
    }

    if let Some(args::Command::Loop {
        repeat,
        done,
        no_done,
        ..
    }) = &args.command
    {
        if let Some(msg) =
            command::repeat_implies_no_done_warning(repeat.as_deref(), *no_done, done.as_deref())
        {
            eprintln!("{}", msg);
        }
    }

    let interrupted = startup::install_ctrl_c_flag();
    if let Some(exit_code) = daemon::handle_special_command(&args, interrupted.as_ref()) {
        std::process::exit(exit_code);
    }

    if hook_health::should_check_launch(&args) {
        if args.verbose {
            verbose_log::log("[clud] hooks: checking launch parity");
        }
        hook_health::emit_launch_warnings();
    }

    if !args.clean_worktrees && !args.fix_hooks {
        let cwd = std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from("."));
        let root = loop_spec::git_root_from(&cwd);
        if args.verbose {
            verbose_log::log("[clud] large-file guard: scanning project");
        }
        large_file_guard::run(&root);
    }

    // Issue #135: always-on clud daemon. One background process per user
    // hosts the GC registry (redb owner + worker thread) and is the
    // execution target for `--detach` / `--detachable` / repeat jobs /
    // `--experimental-daemon-centralized`. Foreground interactive
    // launches still use the direct runner by default (PR #152 reverted
    // the attach-pump default). Skip on `--no-daemon` /
    // `CLUD_NO_DAEMON=1` and on `--dry-run` so unit tests that copy
    // the binary into a tempdir don't leave the daemon's `.old` exe
    // locked when tempdir cleanup runs. Never blocks a launch on
    // spawn failure.
    if !args.no_daemon && !args.dry_run {
        if args.verbose {
            verbose_log::log("[clud] daemon: ensure running");
        }
        match daemon::default_state_dir() {
            Ok(state_dir) => {
                if let Err(e) = daemon::ensure_daemon(&state_dir) {
                    eprintln!("[clud] note: daemon unavailable: {}", e);
                    if args.verbose {
                        verbose_log::log(format_args!("[clud] daemon: unavailable: {e}"));
                    }
                } else {
                    // Issue #183: record one row in the `repo_visits` table
                    // per (repo_root, current launch). Errors are non-fatal:
                    // failing to record a visit must never block a launch.
                    record_repo_visit_best_effort(&state_dir, args.verbose);
                }
            }
            Err(e) => {
                eprintln!("[clud] note: cannot resolve state dir: {}", e);
            }
        }
    } else if args.verbose {
        verbose_log::log("[clud] daemon: skipped");
    }

    let backend = backend::resolve_backend(args.claude, args.codex);
    let backend_path = {
        let mut bootstrap_host = backend_bootstrap::ProductionBackendBootstrapHost;
        let interactive = io::stdin().is_terminal() && io::stderr().is_terminal();
        let stdin = io::stdin();
        let stderr = io::stderr();
        let mut input = stdin.lock();
        let mut err = stderr.lock();
        match backend_bootstrap::resolve_backend_path(
            backend,
            args.dry_run,
            interactive,
            &mut input,
            &mut err,
            &mut bootstrap_host,
        ) {
            Ok(path) => path,
            Err(error) => {
                let _ = writeln!(err, "{error}");
                std::process::exit(error.exit_code());
            }
        }
    };

    let plan = command::build_launch_plan(&args, backend, &backend_path);
    if args.verbose {
        verbose_log::log(format_args!(
            "[clud] plan: backend={} mode={} iterations={} stream_json={}",
            backend.executable_name(),
            plan.launch_mode.as_str(),
            plan.iterations,
            plan.stream_json_progress
        ));
    }

    if args.dry_run {
        let json = serde_json::json!({
            "command": plan.command,
            "iterations": plan.iterations,
            "backend": backend.executable_name(),
            "launch_mode": plan.launch_mode.as_str(),
            "repeat_interval_secs": plan.repeat_schedule.as_ref().map(|s| s.interval_secs),
            "transcript": args.transcript.as_ref().map(|p| p.to_string_lossy().to_string()),
            "loop_markers": plan.loop_markers.as_ref().map(|m| serde_json::json!({
                "done_path": m.done_path,
                "blocked_path": m.blocked_path,
            })),
        });
        println!("{}", serde_json::to_string_pretty(&json).unwrap());
        std::process::exit(0);
    }

    // Issue #79 / #65 / #66: register `clud` as the IDropTarget for
    // the console window so dropped files reach the backend. Held for
    // the lifetime of the launch; dropped on graceful exit so the
    // refresh worker thread joins and `RevokeDragDrop` runs. POSIX
    // skips this — terminals there already deliver drops as stdin
    // bytes that the #63 normalizer handles. `--no-dnd` opts out.
    //
    // PTY mode wires the registration *inside* `run_plan_pty` so the
    // injector can write into the live PTY via a channel. Subprocess
    // mode registers up-front because the `subprocess_console_injector`
    // operates on the shared console input buffer, no per-iteration
    // state required.
    let _dnd_subprocess_guard = if startup::should_register_drop_target(&args)
        && plan.launch_mode == backend::LaunchMode::Subprocess
    {
        if args.verbose {
            verbose_log::log("[clud] dnd: registering subprocess drop target");
        }
        startup::try_register_console_drop_target_subprocess()
    } else {
        if args.verbose {
            verbose_log::log("[clud] dnd: subprocess drop target skipped");
        }
        None
    };

    // Issue #73 / #138: enforce the live-session cap. Opens the redb file
    // inside a cross-process advisory lock, performs gc / cap-check /
    // register-self, and **closes redb before returning**. The returned
    // guard holds nothing but a "we registered" flag; on Drop it briefly
    // re-acquires the lock to remove our row. Holding redb for the
    // lifetime of `main` would race with concurrent `clud` launches and
    // print spurious `Database already open` warnings (issue #138).
    if args.verbose {
        verbose_log::log("[clud] session registry: enforcing cap");
    }
    let _session_guard = startup::enforce_session_cap();

    // Issue #110: spawn the background worktree scanner. Polls the
    // current repo's `.claude/worktrees/` every ~2s and inserts any
    // newly-detected agent-<id> dir into the tracked-entries table.
    // Existing rows are left alone — the scanner is insert-only, no
    // write churn. `Drop` joins the worker thread; explicit `drop` below
    // sequences cancellation before the session-registry guard.
    if args.verbose {
        verbose_log::log("[clud] worktree scanner: starting");
    }
    let _scanner_guard = gc::WorktreeScanner::maybe_spawn();

    // Clear stale DONE/BLOCKED markers from a prior run so that loops don't
    // short-circuit on iteration 1. See loop_spec for semantics.
    if let Some(ref markers) = plan.loop_markers {
        if args.verbose {
            verbose_log::log("[clud] loop markers: clearing stale DONE/BLOCKED files");
        }
        loop_spec::clear_markers_at(&loop_spec::MarkerPaths {
            done: std::path::PathBuf::from(&markers.done_path),
            blocked: std::path::PathBuf::from(&markers.blocked_path),
        });
    }

    // Issue #96: durable `.clud/loop/` artifacts (info.json, log.txt,
    // motivation.md, working copy of LOOP.md / task file, .gitignore
    // auto-injection). Only active when the user actually ran
    // `clud loop`; other commands skip the bookkeeping entirely.
    let mut loop_session: Option<loop_artifacts::LoopSession> =
        if let Some(args::Command::Loop { task, .. }) = &args.command {
            if args.verbose {
                verbose_log::log("[clud] loop artifacts: starting session");
            }
            let cwd = std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from("."));
            let git_root = loop_spec::git_root_from(&cwd);
            let _ = loop_spec::ensure_loop_dir(&git_root);
            loop_artifacts::ensure_loop_in_gitignore(&git_root);
            if let Some(t) = task {
                let spec = loop_spec::classify(t);
                let _ = loop_artifacts::materialize_working_copy(&git_root, &spec);
            }
            Some(loop_artifacts::LoopSession::start(
                &git_root,
                plan.iterations,
            ))
        } else {
            None
        };

    let centralized = daemon::experimental_enabled(&args);
    if args.verbose {
        verbose_log::log(if centralized {
            "[clud] launch: centralized daemon"
        } else {
            "[clud] launch: direct runner"
        });
    }
    let exit_code = if centralized {
        daemon::run_centralized_session(&args, &plan, interrupted.as_ref())
    } else {
        match plan.launch_mode {
            backend::LaunchMode::Subprocess => runner::run_plan_subprocess(
                &plan,
                args.verbose,
                interrupted.as_ref(),
                loop_session.as_mut(),
            ),
            backend::LaunchMode::Pty => runner::run_plan_pty(
                &plan,
                args.verbose,
                interrupted.as_ref(),
                startup::should_register_drop_target(&args),
                loop_session.as_mut(),
            ),
        }
    };
    if let Some(session) = loop_session.as_mut() {
        let (summary, err) = runner::summarize_loop_outcome(exit_code);
        session.on_loop_end(summary, err);
    }
    drop(_scanner_guard);
    drop(_session_guard);
    drop(_dnd_subprocess_guard);
    if args.verbose {
        verbose_log::log(format_args!("[clud] exit: code {exit_code}"));
    }
    std::process::exit(exit_code);
}

/// Issue #183: best-effort upsert of `(repo_root, cwd)` into the daemon's
/// `repo_visits` table. Resolves the current git root; if there isn't one,
/// this is a no-op (we don't track scratch-dir launches). All errors are
/// swallowed — recording a repo visit must never block a launch.
fn record_repo_visit_best_effort(state_dir: &std::path::Path, verbose: bool) {
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
