use clud::{
    args, backend, backend_bootstrap, clud_settings, command, console_setup, console_title,
    crash_report, ctrl_c_track, daemon, gc, graphics, hook_health, large_file_guard, launch_log,
    launch_setup, loop_artifacts, loop_spec, optimize, orphan_reaper, runner, runtime_cache,
    startup, symbols, tool_info, tool_list, tool_run, tools, trampoline, trash, ui, verbose_log,
    wasm, worktrees,
};

use std::io::{self, IsTerminal, Read, Write};

fn main() {
    // Install the crash reporter first so a panic during the rest of startup
    // (arg parsing, runtime-cache hop, drop-target registration, ...) still
    // writes a JSON report under ~/.clud/state/crashes/. Idempotent; the
    // daemon and worker process entries re-call install_native() with their
    // own role to retag any future crash without reinstalling the hook.
    //
    // `install_native` covers SIGSEGV / SIGBUS / SIGILL / SIGFPE / SIGABRT on
    // Unix and structured exceptions on Windows in addition to Rust panics.
    // It explicitly does NOT attach a SIGINT / CTRL_C_EVENT handler — the
    // existing `ctrlc`-based path (`startup::install_ctrl_c_flag` below /
    // #372 forensic capture) remains the authoritative Ctrl-C handler.
    crash_report::install_native("foreground");

    // Issue #408 (Layer 3 of three-layer UV_CACHE_DIR enforcement): pin
    // every `uv` invocation spawned inside clud's process tree to
    // `~/.clud/cache/uv/`, so per-script venvs for bundled tools never
    // leak into the user's global `~/.cache/uv/`. The `clud tool run`
    // subcommand (Layer 1) re-affirms the same value; both layers read
    // from `tools::clud_uv_cache_dir()` so there is one source of truth.
    //
    // SAFETY: at this point we are still single-threaded (crash reporter
    // installs handlers but does not spawn threads). Setting env vars
    // before any other code runs is the standard cross-platform pattern
    // for this case.
    unsafe {
        std::env::set_var("UV_CACHE_DIR", tools::clud_uv_cache_dir());
    }

    verbose_log::init_launch_clock();

    if let Err(err) = runtime_cache::hop_to_runtime_cache_if_enabled() {
        eprintln!("[clud] warning: runtime cache hop failed: {err}");
    }

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

    // Issue #233: standalone graphics smoke test. It must emit only the
    // Sixel payload plus status line, without backend or setup side effects.
    if args.demo_gfx_sixel {
        let terminal_cols = terminal_size::terminal_size().map(|(width, _height)| width.0);
        match graphics::render_demo_sixel_bytes(terminal_cols) {
            Ok(bytes) => {
                let mut out = io::stdout().lock();
                if let Err(err) = out.write_all(&bytes).and_then(|_| out.flush()) {
                    eprintln!("error: failed to write Sixel demo: {err}");
                    std::process::exit(1);
                }
                std::process::exit(0);
            }
            Err(err) => {
                eprintln!("error: failed to render Sixel demo: {err}");
                std::process::exit(1);
            }
        }
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

    // Issue #408: `clud tool run <rel_path> [args]` invokes a bundled
    // Python tool with `UV_CACHE_DIR` pinned. Self-contained; never
    // launches an agent backend.
    if let Some(args::Command::Tool {
        subcommand:
            args::ToolSubcommand::Run {
                rel_path,
                args: tool_args,
            },
    }) = &args.command
    {
        match tool_run::run(rel_path, tool_args) {
            Ok(code) => std::process::exit(code),
            Err(err) => {
                eprintln!("[clud] tool run failed: {err}");
                std::process::exit(2);
            }
        }
    }

    // Slice 3 of #427: `clud tool list` — list invocations in this session.
    if let Some(args::Command::Tool {
        subcommand: args::ToolSubcommand::List { json, long },
    }) = &args.command
    {
        match tool_list::run(*json, *long) {
            Ok(code) => std::process::exit(code),
            Err(err) => {
                eprintln!("[clud] tool list failed: {err}");
                std::process::exit(2);
            }
        }
    }

    // Slice 3 of #427: `clud tool info [<ref>]` — show state + last N lines.
    if let Some(args::Command::Tool {
        subcommand:
            args::ToolSubcommand::Info {
                reference,
                pid,
                lines,
                json,
            },
    }) = &args.command
    {
        match tool_info::run(reference.as_deref(), *pid, *lines, *json) {
            Ok(code) => std::process::exit(code),
            Err(err) => {
                eprintln!("[clud] tool info failed: {err}");
                std::process::exit(2);
            }
        }
    }

    // #374 (PR 3): `clud symbols` inspects / verifies crash-report
    // symbolication against the running binary. Self-contained; never
    // launches a backend.
    if let Some(args::Command::Symbols { subcommand }) = &args.command {
        std::process::exit(symbols::run(&args, subcommand.clone()));
    }

    // `clud optimize` is machine/repo setup and never launches a backend.
    if let Some(args::Command::Optimize {
        target,
        global,
        repo,
        install_soldr,
        use_soldr_shims,
        soldr_version,
    }) = &args.command
    {
        std::process::exit(optimize::run(
            &args,
            *target,
            *global,
            *repo,
            *install_soldr,
            *use_soldr_shims,
            soldr_version,
        ));
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

    if args.no_fix_hooks {
        if args.dry_run {
            println!("[clud] dry-run: would disable automatic hook-health repairs globally");
        } else if let Err(error) = clud_settings::save_auto_fix_hooks_enabled(false) {
            eprintln!("[clud] error: failed to persist --no-fix-hooks: {error}");
            std::process::exit(1);
        } else {
            eprintln!(
                "[clud] disabled automatic hook-health repairs globally; run `clud --fix-hooks` to re-enable"
            );
        }
    }

    // Issue #112: explicit hook-parity remediation path. This flag resets the
    // sticky opt-out, applies deterministic repairs, and asks the selected
    // backend to migrate hook definitions when semantic translation is needed.
    if args.fix_hooks {
        let selected_backend = backend::resolve_backend(args.claude, args.codex);
        if !args.dry_run {
            if let Err(error) = clud_settings::save_auto_fix_hooks_enabled(true) {
                eprintln!("[clud] error: failed to persist --fix-hooks: {error}");
                std::process::exit(1);
            }
        }
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

    let interrupted = startup::install_ctrl_c_flag(args.verbose);
    if let Some(exit_code) = daemon::handle_special_command(&args, interrupted.as_ref()) {
        flush_ctrl_c_exit_event(ctrl_c_track::InvocationKind::Attach, exit_code);
        std::process::exit(exit_code);
    }

    if hook_health::should_check_launch(&args) {
        let auto_fix_hooks = if args.no_fix_hooks {
            false
        } else {
            match clud_settings::load_auto_fix_hooks_enabled() {
                Ok(enabled) => enabled,
                Err(error) => {
                    eprintln!(
                        "[clud] warning: failed to load hook-health settings: {error}; using default"
                    );
                    true
                }
            }
        };
        if auto_fix_hooks && !args.dry_run {
            if let Err(error) = hook_health::apply_default_repairs() {
                eprintln!("[clud] warning: failed to auto-repair hook health: {error}");
            }
        }
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

    // Issue #242: mutable harness setup is scoped per launch until the user
    // opts into a backend-level global preference. Dry-runs always remain
    // session-only; otherwise a stored `~/.clud/settings.json` scope wins.
    // Bare interactive TUI launches without a stored scope can opt into global
    // setup through a reusable selector. Global setup runs only the selected
    // backend's actions.
    let setup_interactive = io::stdin().is_terminal() && io::stderr().is_terminal();
    let configured_scope = if args.dry_run {
        None
    } else {
        match clud_settings::load_launch_setup_scope(backend) {
            Ok(scope) => scope,
            Err(error) => {
                if args.verbose {
                    eprintln!("[clud] note: could not read clud settings: {error}");
                }
                None
            }
        }
    };
    let mut persist_global_scope = false;
    let setup_scope = if let Some(scope) =
        launch_setup::scope_for_configured_launch(&args, setup_interactive, configured_scope)
    {
        scope
    } else {
        let mut err = io::stderr().lock();
        match launch_setup::prompt_scope(&mut err) {
            Ok(scope) => {
                persist_global_scope = matches!(scope, launch_setup::LaunchSetupScope::Global);
                scope
            }
            Err(error) => {
                eprintln!(
                    "[clud] note: could not read launch setup scope ({error}); using session-only"
                );
                launch_setup::LaunchSetupScope::SessionOnly
            }
        }
    };
    if persist_global_scope {
        if let Err(error) = clud_settings::save_launch_setup_scope(backend, setup_scope) {
            eprintln!("[clud] note: could not save global setup preference: {error}");
        }
    }
    if matches!(setup_scope, launch_setup::LaunchSetupScope::Global) {
        let mut err = io::stderr().lock();
        if let Err(error) = launch_setup::run_setup(setup_scope, backend, args.verbose, &mut err) {
            eprintln!("[clud] note: global setup failed: {error}");
        }
    }
    if args.verbose {
        verbose_log::log(format_args!("[clud] setup scope: {}", setup_scope.as_str()));
    }

    if matches!(backend, backend::Backend::Codex) {
        match clud_settings::load_or_init_codex_config_overrides(!args.dry_run) {
            Ok(overrides) => {
                args.codex_config_overrides = overrides;
            }
            Err(error) => {
                eprintln!(
                    "[clud] warning: failed to load Codex settings: {error}; using default Codex config overrides"
                );
                args.codex_config_overrides = clud_settings::default_codex_config_overrides();
            }
        }
    }

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
            "graphics": {
                "mode": plan.graphics.mode.to_string(),
                "image": plan.graphics.image_path.as_ref().map(|p| p.to_string_lossy().to_string()),
            },
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

    // Issue #110/#181/#178: spawn background GC scanners. They poll the
    // current repo's `.claude/worktrees/`, `.extern-repos/`, and sibling
    // temp-clone directories every ~2s and insert newly-detected tracked
    // entries.
    // Existing rows are left alone — the scanner is insert-only, no
    // write churn. `Drop` joins the worker thread; explicit `drop` below
    // sequences cancellation before the session-registry guard.
    if args.verbose {
        verbose_log::log("[clud] worktree scanner: starting");
    }
    let _scanner_guard = gc::WorktreeScanner::maybe_spawn();
    let _extern_repo_scanner_guard = gc::WorktreeScanner::maybe_spawn_extern_repos();
    let _sibling_clone_scanner_guard = gc::WorktreeScanner::maybe_spawn_sibling_clones();

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
    let launch_log = if let Ok(state_dir) = daemon::default_state_dir() {
        let source = if centralized { "centralized" } else { "direct" };
        match launch_log::start_launch(&state_dir, &plan, source) {
            Ok(handle) => Some(handle),
            Err(err) => {
                eprintln!("[clud] warning: failed to record launch start: {err}");
                None
            }
        }
    } else {
        None
    };
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
    if let Some(handle) = &launch_log {
        handle.finish(exit_code);
    }
    if let Some(session) = loop_session.as_mut() {
        let (summary, err) = runner::summarize_loop_outcome(exit_code);
        session.on_loop_end(summary, err);
    }
    // Issue #285 rec 3: signal cancellation on all three scanner guards
    // *before* dropping any of them so the three worker threads wake up
    // concurrently. The subsequent `drop` calls then join in parallel
    // rather than serializing 3 × scanner-poll-interval of dead time
    // into the Ctrl-C exit path.
    if let Some(g) = _sibling_clone_scanner_guard.as_ref() {
        g.signal_cancel();
    }
    if let Some(g) = _extern_repo_scanner_guard.as_ref() {
        g.signal_cancel();
    }
    if let Some(g) = _scanner_guard.as_ref() {
        g.signal_cancel();
    }
    drop(_sibling_clone_scanner_guard);
    drop(_extern_repo_scanner_guard);
    drop(_scanner_guard);
    drop(_session_guard);
    drop(_dnd_subprocess_guard);
    // Issue #340: detect env-tagged orphans we are about to leave behind and
    // (unless --keep-orphans) reap them. Skip for detached / detachable
    // sessions — those descendants are intentionally outliving us and are
    // owned by the daemon now.
    if !args.detach && !args.detachable {
        let opts = orphan_reaper::ReapOpts {
            keep: args.keep_orphans,
            quiet: args.quiet_orphans,
            explain: args.explain_orphans,
        };
        let outcome = orphan_reaper::scan_and_report(std::process::id(), &opts);
        if args.verbose && outcome.found > 0 {
            verbose_log::log(format_args!(
                "[clud] orphan reaper: found={} reaped={}",
                outcome.found, outcome.reaped
            ));
        }

        // Have the daemon do a broader sweep on our behalf: any CLUD-tagged
        // process whose originator is gone (e.g., a sibling clud was
        // SIGKILL'd and never ran its own exit hook) gets reaped on the
        // daemon's background thread. Fire-and-forget with a tight
        // timeout; failure is silently absorbed — the daemon's periodic
        // heartbeat sweep will catch anything we miss, and the next
        // `clud slay` does the synchronous version.
        if !args.keep_orphans {
            if let Ok(state_dir) = daemon::default_state_dir() {
                let _ = daemon::try_request_orphan_reap(&state_dir);
            }
        }
    }
    if args.verbose {
        verbose_log::log(format_args!("[clud] exit: code {exit_code}"));
    }
    let kind = if centralized {
        ctrl_c_track::InvocationKind::Centralized
    } else {
        ctrl_c_track::InvocationKind::Direct
    };
    flush_ctrl_c_exit_event(kind, exit_code);
    std::process::exit(exit_code);
}

/// Write the cross-path Ctrl+C exit-timing event (issue: `clud ui` ctrl-c
/// tracking) if a Ctrl+C was observed during this process's lifetime.
/// Best-effort: resolves the state dir lazily so an unreadable home dir
/// can't block the process exit. Errors are swallowed inside the tracker.
fn flush_ctrl_c_exit_event(kind: ctrl_c_track::InvocationKind, exit_code: i32) {
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
