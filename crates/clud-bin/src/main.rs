use clud::{
    args, backend, command, daemon, loop_spec, session, session_registry, subprocess, trampoline,
    voice, wasm,
};

use std::io::{self, Read};
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc,
};

fn main() {
    // Windows: rename ourselves so pip can always overwrite clud.exe.
    trampoline::unlock_exe();

    let mut args = args::Args::parse_with_passthrough();

    // Pipe mode: if stdin is not a terminal, read it as the prompt.
    if args.prompt.is_none()
        && args.message.is_none()
        && args.command.is_none()
        && !atty_is_terminal()
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

    let interrupted = install_ctrl_c_flag();
    if let Some(exit_code) = daemon::handle_special_command(&args, interrupted.as_ref()) {
        std::process::exit(exit_code);
    }

    let backend = backend::resolve_backend(args.claude, args.codex);
    let backend_path = match backend::find_backend(backend) {
        Some(path) => path.to_string_lossy().to_string(),
        None => {
            if args.dry_run {
                backend.executable_name().to_string()
            } else {
                eprintln!(
                    "error: {} not found on PATH. Install it or use --dry-run.",
                    backend.executable_name()
                );
                std::process::exit(1);
            }
        }
    };

    let plan = command::build_launch_plan(&args, backend, &backend_path);

    if args.dry_run {
        let json = serde_json::json!({
            "command": plan.command,
            "iterations": plan.iterations,
            "backend": backend.executable_name(),
            "launch_mode": plan.launch_mode.as_str(),
            "repeat_interval_secs": plan.repeat_schedule.as_ref().map(|s| s.interval_secs),
            "loop_markers": plan.loop_markers.as_ref().map(|m| serde_json::json!({
                "done_path": m.done_path,
                "blocked_path": m.blocked_path,
            })),
        });
        println!("{}", serde_json::to_string_pretty(&json).unwrap());
        std::process::exit(0);
    }

    // Issue #73: open the SQLite session registry, GC dead siblings,
    // refuse to launch if we're at the cap, otherwise insert our own row.
    // Held until end-of-`main` so `Drop` removes the row on graceful exit.
    let _registry_guard = enforce_session_cap();

    // Clear stale DONE/BLOCKED markers from a prior run so that loops don't
    // short-circuit on iteration 1. See loop_spec for semantics.
    if let Some(ref markers) = plan.loop_markers {
        loop_spec::clear_markers_at(&loop_spec::MarkerPaths {
            done: std::path::PathBuf::from(&markers.done_path),
            blocked: std::path::PathBuf::from(&markers.blocked_path),
        });
    }

    let exit_code = if daemon::experimental_enabled(&args) {
        daemon::run_centralized_session(&args, &plan, interrupted.as_ref())
    } else {
        match plan.launch_mode {
            backend::LaunchMode::Subprocess => {
                run_plan_subprocess(&plan, args.verbose, interrupted.as_ref())
            }
            backend::LaunchMode::Pty => run_plan_pty(&plan, args.verbose, interrupted.as_ref()),
        }
    };
    drop(_registry_guard);
    std::process::exit(exit_code);
}

/// Issue #73: enforce the live-session cap. On `Refuse` this calls
/// `std::process::exit(1)` directly — we never return to the launch path.
/// On `Warn` we print to stderr and continue. Failures to open / GC the
/// DB are *non-fatal*: we log to stderr and skip the cap check, because
/// breaking `clud` startup over a registry hiccup would be much worse
/// than the rare case where the guardrail is temporarily missing.
fn enforce_session_cap() -> Option<session_registry::SessionRegistry> {
    let registry = match session_registry::SessionRegistry::open_default() {
        Ok(r) => r,
        Err(e) => {
            eprintln!("[clud] warning: could not open session registry: {e}");
            return None;
        }
    };
    if let Err(e) = registry.gc_dead_sessions() {
        eprintln!("[clud] warning: session-registry GC failed: {e}");
    }
    let cfg = session_registry::SessionRegistry::cap_config_from_env();
    match registry.check_cap(&cfg) {
        Ok(session_registry::CapDecision::Allow) => {}
        Ok(session_registry::CapDecision::Warn(count)) => {
            eprintln!(
                "[clud] warning: {count} live clud sessions detected (warn threshold {warn}, cap {cap}). \
                 Set {env_max}=0 to disable, or wind down old sessions.",
                warn = cfg.warn,
                cap = cfg.max,
                env_max = session_registry::ENV_MAX_INSTANCES,
            );
        }
        Ok(session_registry::CapDecision::Refuse(count)) => {
            eprintln!(
                "[clud] error: {count} live clud sessions exceed the cap of {cap}. \
                 Refusing to launch (fork-bomb guardrail, issue #73). \
                 Wind down old sessions, or override via {env_max}=<larger> / \
                 {env_max}=0 to disable.",
                cap = cfg.max,
                env_max = session_registry::ENV_MAX_INSTANCES,
            );
            std::process::exit(1);
        }
        Err(e) => {
            eprintln!("[clud] warning: session-registry cap check failed: {e}");
        }
    }
    let info = session_registry::SessionInfo::for_self(None, None);
    if let Err(e) = registry.register_self(info) {
        eprintln!("[clud] warning: could not register session: {e}");
    }
    Some(registry)
}

/// Build the child environment: inherit parent env + inject tracking vars.
/// Deduplicates keys so we never pass the same var twice.
fn child_env() -> Vec<(String, String)> {
    let originator_key = running_process_core::ORIGINATOR_ENV_VAR;

    let mut env: Vec<(String, String)> = std::env::vars()
        .filter(|(k, _)| k != "IN_CLUD" && k != originator_key)
        .collect();

    env.push(("IN_CLUD".to_string(), "1".to_string()));

    let originator_value = format!("CLUD:{}", std::process::id());
    env.push((originator_key.to_string(), originator_value));

    env
}

fn install_ctrl_c_flag() -> Arc<AtomicBool> {
    let interrupted = Arc::new(AtomicBool::new(false));
    let handler_flag = Arc::clone(&interrupted);
    if let Err(e) = ctrlc::set_handler(move || {
        handler_flag.store(true, Ordering::SeqCst);
    }) {
        eprintln!("[clud] warning: failed to install Ctrl+C handler: {}", e);
    }
    interrupted
}

fn get_terminal_size() -> (u16, u16) {
    let probe = terminal_size::terminal_size().map(|(w, h)| (w.0, h.0));
    resolve_terminal_size(probe)
}

/// Translate a `(cols, rows)` probe result into a `(rows, cols)` size to hand
/// to the PTY. `None` means no real terminal — return a safe fallback.
/// 200 cols is wide enough that typical codex/claude output doesn't wrap
/// awkwardly, but stays within the range real terminal emulators actually
/// exercise — passing 32767 to ConPTY pushes layout math into corners that
/// trigger cursor drift in ratatui/Ink-based TUIs (issue #31, T3).
fn resolve_terminal_size(probe: Option<(u16, u16)>) -> (u16, u16) {
    match probe {
        Some((cols, rows)) => (rows, cols),
        None => (24, 200),
    }
}

fn normalize_exit_code(code: i32) -> i32 {
    match code {
        -2 => 130,
        -9 => 137,
        -15 => 143,
        _ => code,
    }
}

fn run_plan_subprocess(plan: &command::LaunchPlan, verbose: bool, interrupted: &AtomicBool) -> i32 {
    use std::path::PathBuf;

    use running_process_core::{Containment, NativeProcess, ProcessConfig, StderrMode, StdinMode};

    let env = child_env();
    let mut last_exit = 0i32;

    for iteration in 0..plan.iterations {
        if plan.iterations > 1 {
            eprintln!("[clud] iteration {}/{}", iteration + 1, plan.iterations);
        }

        if verbose {
            eprintln!("[clud] exec (subprocess): {}", plan.command.join(" "));
        }

        let config = ProcessConfig {
            command: subprocess::command_spec_for_subprocess(plan.command.clone()),
            cwd: plan.cwd.as_ref().map(PathBuf::from),
            env: Some(env.clone()),
            capture: false,
            stderr_mode: StderrMode::Stdout,
            creationflags: None,
            create_process_group: false,
            stdin_mode: StdinMode::Inherit,
            nice: None,
            // Issue #9: Claude/Codex spawn tool subprocesses (cargo test,
            // npm test, long builds) that leak as zombies when a clud
            // session dies abnormally (crash, terminal close, Task Manager
            // kill). `Containment::Contained` binds the child tree's
            // lifetime to ours: PR_SET_PDEATHSIG(SIGKILL) on Linux, a
            // kill-on-close Job Object on Windows. The daemon path already
            // sets this (daemon.rs); direct subprocess runs now do too.
            containment: Some(Containment::Contained),
        };

        let process = NativeProcess::new(config);
        if let Err(e) = process.start() {
            eprintln!("[clud] failed to execute {}: {}", plan.command[0], e);
            return 1;
        }

        loop {
            match process.poll() {
                Ok(Some(code)) => {
                    if interrupted.load(Ordering::SeqCst) {
                        eprintln!("[clud] interrupted via Ctrl+C");
                        return 130;
                    }
                    last_exit = code;
                    if last_exit != 0 && plan.iterations > 1 {
                        eprintln!(
                            "[clud] iteration {} failed with exit code {}",
                            iteration + 1,
                            last_exit
                        );
                        return last_exit;
                    }
                    break;
                }
                Ok(None) => {
                    if interrupted.load(Ordering::SeqCst) {
                        let _ = process.kill();
                        let _ = process.wait(Some(std::time::Duration::from_secs(2)));
                        eprintln!("[clud] interrupted via Ctrl+C");
                        return 130;
                    }
                    std::thread::sleep(std::time::Duration::from_millis(50));
                }
                Err(e) => {
                    eprintln!("[clud] error waiting for process: {}", e);
                    return 1;
                }
            }
        }

        if let Some(code) = check_loop_markers(plan, iteration + 1) {
            return code;
        }
    }

    if let Some(code) = loop_unconverged_exit(plan) {
        return code;
    }

    last_exit
}

fn run_plan_pty(plan: &command::LaunchPlan, verbose: bool, interrupted: &AtomicBool) -> i32 {
    use running_process_core::pty::NativePtyProcess;

    // Enable VT input on the Windows console for the whole PTY session.
    // The raw byte pump reads from clud's stdin, so PTY mode needs the
    // console to emit terminal-style bytes. In subprocess mode the child
    // inherits the console directly and must be allowed to configure input
    // modes itself.
    let _console_guard = enable_console_vt_input();

    let env = child_env();
    let mut last_exit = 0i32;
    let (rows, cols) = get_terminal_size();

    for iteration in 0..plan.iterations {
        if plan.iterations > 1 {
            eprintln!("[clud] iteration {}/{}", iteration + 1, plan.iterations);
        }

        if verbose {
            eprintln!("[clud] exec (pty): {}", plan.command.join(" "));
        }

        let process = match NativePtyProcess::new(
            plan.command.clone(),
            plan.cwd.clone(),
            Some(env.clone()),
            rows,
            cols,
            None,
        ) {
            Ok(p) => p,
            Err(e) => {
                eprintln!("[clud] failed to create pty: {}", e);
                return 1;
            }
        };

        process.set_echo(true);

        if let Err(e) = process.start_impl() {
            eprintln!("[clud] failed to execute {}: {}", plan.command[0], e);
            return 1;
        }

        let mut hooks = voice::VoiceMode::from_env();
        let _raw_guard = session::enter_raw_mode_if_tty();
        let exit_code = session::run_raw_pty_pump(&process, interrupted, &mut hooks, io::stdin());
        drop(_raw_guard);
        last_exit = normalize_exit_code(exit_code);

        if last_exit != 0 && plan.iterations > 1 {
            eprintln!(
                "[clud] iteration {} failed with exit code {}",
                iteration + 1,
                last_exit
            );
            return last_exit;
        }

        if let Some(code) = check_loop_markers(plan, iteration + 1) {
            return code;
        }
    }

    if let Some(code) = loop_unconverged_exit(plan) {
        return code;
    }

    last_exit
}

/// Check for DONE/BLOCKED markers after an iteration finishes. Returns a
/// terminal exit code to return from the runner, or `None` to continue.
fn check_loop_markers(plan: &command::LaunchPlan, iteration: u32) -> Option<i32> {
    let markers = plan.loop_markers.as_ref()?;
    match loop_spec::read_markers_at(&loop_spec::MarkerPaths {
        done: std::path::PathBuf::from(&markers.done_path),
        blocked: std::path::PathBuf::from(&markers.blocked_path),
    }) {
        loop_spec::MarkerState::Done(summary) => {
            if summary.is_empty() {
                eprintln!(
                    "[clud loop] DONE marker detected at iteration {iteration}; task resolved."
                );
            } else {
                eprintln!("[clud loop] DONE at iteration {iteration}: {summary}");
            }
            Some(0)
        }
        loop_spec::MarkerState::Blocked(reason) => {
            if reason.is_empty() {
                eprintln!("[clud loop] BLOCKED marker detected at iteration {iteration}; halting.");
            } else {
                eprintln!("[clud loop] BLOCKED at iteration {iteration}: {reason}");
            }
            Some(3)
        }
        loop_spec::MarkerState::None => None,
    }
}

/// Called after the iteration count is exhausted without a DONE/BLOCKED
/// marker. Only returns an override exit code when loop markers are active.
fn loop_unconverged_exit(plan: &command::LaunchPlan) -> Option<i32> {
    plan.loop_markers.as_ref().map(|_| {
        eprintln!(
            "[clud loop] iteration count ({}) exhausted without a DONE marker; task did not converge.",
            plan.iterations
        );
        2
    })
}

/// RAII guard that restores the original console input mode on drop.
struct ConsoleVtGuard {
    #[cfg(windows)]
    original_mode: Option<u32>,
}

impl Drop for ConsoleVtGuard {
    fn drop(&mut self) {
        #[cfg(windows)]
        if let Some(mode) = self.original_mode {
            restore_console_mode(mode);
        }
    }
}

/// Enable `ENABLE_VIRTUAL_TERMINAL_INPUT` on the Windows console so ANSI
/// sequences (bracketed paste, etc.) pass through to the child process.
/// Returns a guard that restores the original mode on drop.
/// On non-Windows platforms this is a no-op.
fn enable_console_vt_input() -> ConsoleVtGuard {
    #[cfg(windows)]
    {
        use std::io::IsTerminal;
        if !io::stdin().is_terminal() {
            return ConsoleVtGuard {
                original_mode: None,
            };
        }
        match set_console_vt_input(true) {
            Some(original) => ConsoleVtGuard {
                original_mode: Some(original),
            },
            None => ConsoleVtGuard {
                original_mode: None,
            },
        }
    }
    #[cfg(not(windows))]
    {
        ConsoleVtGuard {}
    }
}

#[cfg(windows)]
fn set_console_vt_input(enable: bool) -> Option<u32> {
    use std::os::windows::io::AsRawHandle;

    // Windows console mode flag for virtual terminal input processing.
    const ENABLE_VIRTUAL_TERMINAL_INPUT: u32 = 0x0200;

    extern "system" {
        fn GetConsoleMode(handle: isize, mode: *mut u32) -> i32;
        fn SetConsoleMode(handle: isize, mode: u32) -> i32;
    }

    let handle = io::stdin().as_raw_handle() as isize;
    unsafe {
        let mut mode: u32 = 0;
        if GetConsoleMode(handle, &mut mode) == 0 {
            return None;
        }
        let original = mode;
        if enable {
            mode |= ENABLE_VIRTUAL_TERMINAL_INPUT;
        } else {
            mode &= !ENABLE_VIRTUAL_TERMINAL_INPUT;
        }
        if SetConsoleMode(handle, mode) == 0 {
            return None;
        }
        Some(original)
    }
}

#[cfg(windows)]
fn restore_console_mode(mode: u32) {
    use std::os::windows::io::AsRawHandle;

    extern "system" {
        fn SetConsoleMode(handle: isize, mode: u32) -> i32;
    }

    let handle = io::stdin().as_raw_handle() as isize;
    unsafe {
        SetConsoleMode(handle, mode);
    }
}

/// Check if stdin is a terminal (not piped).
fn atty_is_terminal() -> bool {
    use std::io::IsTerminal;
    io::stdin().is_terminal()
}

#[cfg(test)]
mod tests {
    use super::resolve_terminal_size;

    #[test]
    fn launch_mode_defaults_to_subprocess() {
        let launch_mode = crate::backend::LaunchMode::Subprocess;
        assert_eq!(launch_mode.as_str(), "subprocess");
    }

    #[test]
    fn resolve_terminal_size_uses_probe_when_present() {
        // Input is (cols, rows) from the terminal_size crate. Output is the
        // (rows, cols) pair we pass to NativePtyProcess::new.
        assert_eq!(resolve_terminal_size(Some((120, 40))), (40, 120));
    }

    #[test]
    fn resolve_terminal_size_caps_fallback_at_200_cols() {
        // Issue #31 T3: the previous `(24, 32767)` fallback blew up ratatui
        // layout math inside the child. The cap keeps us in normal terminal
        // territory.
        let (rows, cols) = resolve_terminal_size(None);
        assert_eq!(rows, 24);
        assert_eq!(cols, 200);
        assert!(cols <= 1024, "fallback cols must stay sane: {}", cols);
    }

    /// Windows: `enable_console_vt_input()` must actually set the
    /// `ENABLE_VIRTUAL_TERMINAL_INPUT` bit (0x0200) on the console input
    /// handle, and restore the original mode on drop. Without this bit,
    /// `ReadConsoleW` delivers Backspace as 0x08 instead of the xterm 0x7f
    /// that Ink-based TUIs (codex) expect, which manifests as "Backspace
    /// doesn't delete anything" inside `clud --codex`.
    ///
    /// Skipped when stdin is not a real console (piped `cargo test`,
    /// CI boxes without an attached TTY).
    #[cfg(windows)]
    #[test]
    fn enable_console_vt_input_sets_and_restores_bit() {
        use super::enable_console_vt_input;
        use std::io::IsTerminal;
        use std::os::windows::io::AsRawHandle;

        const ENABLE_VIRTUAL_TERMINAL_INPUT: u32 = 0x0200;

        extern "system" {
            fn GetConsoleMode(handle: isize, mode: *mut u32) -> i32;
            fn SetConsoleMode(handle: isize, mode: u32) -> i32;
        }

        if !std::io::stdin().is_terminal() {
            eprintln!(
                "enable_console_vt_input_sets_and_restores_bit: SKIP \
                 (stdin not a real console in this test runner)"
            );
            return;
        }

        let handle = std::io::stdin().as_raw_handle() as isize;
        let saved: u32 = unsafe {
            let mut mode: u32 = 0;
            assert_ne!(GetConsoleMode(handle, &mut mode), 0, "GetConsoleMode");
            mode
        };
        // Clear the VT-input bit so we're starting from a known state.
        unsafe {
            assert_ne!(
                SetConsoleMode(handle, saved & !ENABLE_VIRTUAL_TERMINAL_INPUT),
                0,
                "clear VT input bit"
            );
        }

        let before: u32 = unsafe {
            let mut mode: u32 = 0;
            assert_ne!(GetConsoleMode(handle, &mut mode), 0);
            mode
        };
        assert_eq!(
            before & ENABLE_VIRTUAL_TERMINAL_INPUT,
            0,
            "VT input bit should be cleared at start of test"
        );

        {
            let _guard = enable_console_vt_input();
            let during: u32 = unsafe {
                let mut mode: u32 = 0;
                assert_ne!(GetConsoleMode(handle, &mut mode), 0);
                mode
            };
            assert_ne!(
                during & ENABLE_VIRTUAL_TERMINAL_INPUT,
                0,
                "enable_console_vt_input must set ENABLE_VIRTUAL_TERMINAL_INPUT"
            );
        }

        let after: u32 = unsafe {
            let mut mode: u32 = 0;
            assert_ne!(GetConsoleMode(handle, &mut mode), 0);
            mode
        };
        assert_eq!(
            after & ENABLE_VIRTUAL_TERMINAL_INPUT,
            0,
            "guard must restore the original (cleared) VT input state on drop"
        );

        // Restore the truly-original mode we saved at the top.
        unsafe {
            SetConsoleMode(handle, saved);
        }
    }
}
