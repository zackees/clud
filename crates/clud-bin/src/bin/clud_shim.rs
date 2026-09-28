//! `clud-shim` — tiny PATH-resolved relay for `python` / `python3` /
//! `pip` / etc. inside a clud session. Slice 1 of #406 / #409.
//!
//! Flow (happy path):
//!   1. Read the real interpreter selected by clud startup from
//!      `CLUD_PYTHON_SHIM_TARGET`.
//!   2. `exec` the resolved path with the same argv (Unix) /
//!      `CreateProcess` + propagate exit code (Windows).
//!
//! The older daemon-resolution protocol remains as a compatibility fallback
//! for callers that provide `CLUD_DAEMON_SOCKET` instead.
//!
//! Six degenerate-case error paths (this is the slice-1 public contract):
//!
//! | Condition                                  | stderr line                                                                | exit |
//! |--------------------------------------------|----------------------------------------------------------------------------|------|
//! | CLUD_DAEMON_SOCKET unset                   | `clud python shim invoked outside a clud session; run via clud`            | 127  |
//! | connect failed (daemon down / stale socket)| `clud python shim: daemon unreachable at <socket>`                         | 69   |
//! | daemon accepted + closed without writing   | `clud python shim: daemon disconnected while resolving interpreter`        | 71   |
//! | daemon wrote garbage / unparseable JSON    | `clud python shim: protocol error from daemon (cannot parse response); upgrade clud` | 71 |
//! | daemon returned `{"status": "not_available", "reason": …}` | `clud python shim: <reason>`                                      | 1    |
//! | exec returned with errno                   | `clud python shim: failed to exec <path>: <errno>`                         | 126  |
//!
//! The contract is the daemon-side wire format + exit codes + stderr
//! lines. Slice 2 (#410) wires the real `ResolveInterpreter` RPC; slice
//! 4 (#412) bundles + extracts the binary; slice 5 (#413) injects
//! `CLUD_DAEMON_SOCKET` into every session child.

use std::env;
use std::io::{self, BufRead, BufReader, Write};
use std::process::exit;

/// Exit code emitted when CLUD_DAEMON_SOCKET is unset — the shim was
/// invoked outside a clud session.
pub const EXIT_NO_SESSION: i32 = 127;
/// Exit code emitted when the daemon socket cannot be reached
/// (ECONNREFUSED on Unix, ERROR_PIPE_NOT_LISTENING / file-not-found on
/// Windows).
pub const EXIT_DAEMON_UNREACHABLE: i32 = 69;
/// Exit code emitted when the daemon disconnected mid-request or wrote
/// garbage that can't be parsed.
pub const EXIT_DAEMON_DISCONNECT: i32 = 71;
/// Exit code emitted when the daemon returned `NotAvailable`.
pub const EXIT_NOT_AVAILABLE: i32 = 1;
/// Exit code emitted when `exec` returned with errno (Unix) or
/// `CreateProcess` failed (Windows).
pub const EXIT_EXEC_FAILED: i32 = 126;

/// Stderr line emitted when CLUD_DAEMON_SOCKET is unset. Frozen as
/// part of the slice-1 public contract — downstream tooling greps for
/// this exact string.
pub const STDERR_NO_SESSION: &str = "clud python shim invoked outside a clud session; run via clud";

fn main() {
    let argv: Vec<_> = env::args_os().collect();
    if argv
        .first()
        .and_then(|a| std::path::Path::new(a).file_name())
        .is_some_and(|name| name == "gh" || name == "gh.exe")
    {
        exit(gh_shim::run(&argv[1..]));
    }
    // `safe-rm` (#1461): clud-controlled deletion for agents.
    if argv
        .first()
        .and_then(|a| std::path::Path::new(a).file_name())
        .is_some_and(|name| clud::rm_tool::is_program_name(&name.to_string_lossy()))
    {
        let args: Option<Vec<String>> = argv
            .into_iter()
            .skip(1)
            .map(|a| a.into_string().ok())
            .collect();
        let Some(args) = args else {
            eprintln!("safe-rm: non-UTF8 arguments are not supported");
            exit(2);
        };
        exit(clud::rm_tool::run(&args));
    }
    let is_rm = argv
        .first()
        .and_then(|a| std::path::Path::new(a).file_name())
        .is_some_and(|name| name == "rm" || name == "rm.exe");
    if is_rm {
        let args: Option<Vec<String>> = argv
            .into_iter()
            .skip(1)
            .map(|a| a.into_string().ok())
            .collect();
        let Some(args) = args else {
            println!(
                "{}",
                serde_json::json!({"decision":"deny", "reason":"non-UTF8 rm arguments"})
            );
            exit(2);
        };
        exit(run_rm(&args));
    }
    let env = RealEnv;
    let outcome = shim_run(&env, connect_real, exec_real, &mut io::stderr().lock());
    exit(outcome);
}

/// Abstracted env-var lookup. Production uses [`RealEnv`]; tests inject
/// a `FakeEnv` to control `CLUD_DAEMON_SOCKET` without racing other
/// parallel tests.
pub trait ShimEnv {
    fn var(&self, key: &str) -> Option<String>;
    fn args(&self) -> Vec<String>;
}

pub struct RealEnv;

impl ShimEnv for RealEnv {
    fn var(&self, key: &str) -> Option<String> {
        env::var(key).ok()
    }

    fn args(&self) -> Vec<String> {
        env::args().collect()
    }
}

/// Connect callback: takes the socket path and returns an open
/// read/write stream or an io error. Tests inject fakes that simulate
/// connect failure, mid-write disconnect, garbage payloads, etc.
type ConnectFn = fn(&str) -> io::Result<Box<dyn ShimStream + Send>>;

/// Exec callback: takes the resolved interpreter path + argv (without
/// argv[0]). On Unix the real impl `execvp`s and never returns on
/// success; on Windows it spawns + waits + propagates the exit code.
/// Returns `Ok(exit_code)` if exec succeeded and returned a status,
/// `Err(io::Error)` if exec failed before the new process started.
type ExecFn = fn(&str, &[String]) -> io::Result<i32>;

/// A duplex byte stream — implemented by `interprocess::local_socket::Stream`
/// in production and by hand-rolled fakes in tests.
pub trait ShimStream: Write + io::Read {}

impl<T: Write + io::Read> ShimStream for T {}

/// Run the shim's logic with injectable hooks. Returns the desired
/// process exit code. Public so integration tests can drive it without
/// spawning a subprocess.
pub fn shim_run(env: &impl ShimEnv, connect: ConnectFn, exec: ExecFn, err: &mut dyn Write) -> i32 {
    let argv = env.args();
    let tail: Vec<String> = argv.iter().skip(1).cloned().collect();
    if let Some(path) = env.var(clud::shim_install::SHIM_TARGET_ENV_VAR) {
        return match exec(&path, &tail) {
            Ok(code) => code,
            Err(e) => {
                let _ = writeln!(err, "clud python shim: failed to exec {path}: {e}");
                EXIT_EXEC_FAILED
            }
        };
    }
    let Some(socket) = env.var("CLUD_DAEMON_SOCKET") else {
        let _ = writeln!(err, "{STDERR_NO_SESSION}");
        return EXIT_NO_SESSION;
    };

    let want = current_exe_basename(&argv);

    let mut stream = match connect(&socket) {
        Ok(s) => s,
        Err(_) => {
            let _ = writeln!(err, "clud python shim: daemon unreachable at {socket}");
            return EXIT_DAEMON_UNREACHABLE;
        }
    };

    let request = format!(
        "{{\"want\":\"{}\",\"argv\":{}}}\n",
        want,
        serde_json::to_string(&tail).unwrap_or_else(|_| "[]".to_string())
    );
    if stream.write_all(request.as_bytes()).is_err() {
        let _ = writeln!(
            err,
            "clud python shim: daemon disconnected while resolving interpreter"
        );
        return EXIT_DAEMON_DISCONNECT;
    }
    let _ = stream.flush();

    let mut reader = BufReader::new(&mut *stream);
    let mut line = String::new();
    let bytes = reader.read_line(&mut line).unwrap_or(0);
    if bytes == 0 {
        let _ = writeln!(
            err,
            "clud python shim: daemon disconnected while resolving interpreter"
        );
        return EXIT_DAEMON_DISCONNECT;
    }

    let parsed: serde_json::Value = match serde_json::from_str(line.trim()) {
        Ok(v) => v,
        Err(_) => {
            let _ = writeln!(
                err,
                "clud python shim: protocol error from daemon (cannot parse response); upgrade clud"
            );
            return EXIT_DAEMON_DISCONNECT;
        }
    };

    if parsed.get("status").and_then(|v| v.as_str()) == Some("not_available") {
        let reason = parsed
            .get("reason")
            .and_then(|v| v.as_str())
            .unwrap_or("no usable Python");
        let _ = writeln!(err, "clud python shim: {reason}");
        return EXIT_NOT_AVAILABLE;
    }

    let Some(path) = parsed.get("path").and_then(|v| v.as_str()) else {
        let _ = writeln!(
            err,
            "clud python shim: protocol error from daemon (no path field); upgrade clud"
        );
        return EXIT_DAEMON_DISCONNECT;
    };

    match exec(path, &tail) {
        Ok(code) => code,
        Err(e) => {
            let _ = writeln!(err, "clud python shim: failed to exec {path}: {e}");
            EXIT_EXEC_FAILED
        }
    }
}

/// argv[0]'s file stem; defaults to `"python"` when argv is empty or
/// the stem can't be computed.
fn current_exe_basename(argv: &[String]) -> String {
    argv.first()
        .and_then(|s| clud::path_norm::file_stem_any_separator(s))
        .unwrap_or_else(|| "python".to_string())
}

/// Production connect: opens a local-socket / named-pipe stream to
/// the daemon. Slice 1 stubs this with a not-yet-implemented error so
/// the binary builds; slice 2 wires the real daemon RPC.
fn connect_real(_socket: &str) -> io::Result<Box<dyn ShimStream + Send>> {
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "clud-shim slice 1: daemon RPC stubbed; slice 2 (#410) wires the real ResolveInterpreter call",
    ))
}

/// Production exec: `execvp` on Unix (never returns on success);
/// `CreateProcess` + wait on Windows.
fn exec_real(path: &str, args: &[String]) -> io::Result<i32> {
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        let err = std::process::Command::new(path).args(args).exec();
        // exec only returns on failure on Unix.
        Err(err)
    }
    #[cfg(windows)]
    {
        let status = std::process::Command::new(path).args(args).status()?;
        Ok(status.code().unwrap_or(1))
    }
}

fn run_rm(args: &[String]) -> i32 {
    match clud::rm_guard::prepare(args) {
        Ok(plan) => {
            let code = execute_handoff(&plan);
            clud::rm_guard::audit(args, Some(&plan), code, None);
            code
        }
        Err(reason) => {
            let code = clud::rm_guard::deny(&reason);
            clud::rm_guard::audit(args, None, code, Some(&reason));
            code
        }
    }
}

fn execute_handoff(plan: &clud::rm_guard::Plan) -> i32 {
    let mut command = vec![plan.program.to_string_lossy().into_owned()];
    command.extend(plan.argv.iter().cloned());
    match clud::subprocess::ManagedSubprocess::start_inheriting_env(command, None, false, None) {
        Ok(child) => child.wait(None).unwrap_or(2),
        Err(error) => clud::rm_guard::deny(&format!("system handoff failed: {error}")),
    }
}

#[cfg(any())]
fn finish_in_roots(plan: clud::rm_guard::InRoots, dry_run: bool) -> i32 {
    #[cfg(not(test))]
    if !dry_run {
        return in_roots_execute(plan);
    }
    let _ = dry_run;
    clud::rm_guard::report_in_roots_dry_run(&plan)
}

/// Delete in process, as `rm` would: `-r` for directories, `-f` to ignore
/// missing operands, `-v` to report. Every operand already passed the
/// in-roots gate. One audit record per call, with role `child`.
#[cfg(any())]
fn in_roots_execute(plan: clud::rm_guard::InRoots) -> i32 {
    let mut failed = false;
    let mut paths = Vec::new();
    for missing in &plan.missing {
        if !plan.force {
            failed = true;
            eprintln!(
                "rm: cannot remove '{}': No such file or directory",
                missing.display()
            );
        }
        paths.push(serde_json::json!({"path": missing, "action": "missing"}));
    }
    for target in &plan.targets {
        let result = if target.is_dir {
            if plan.recursive {
                clud::gc::delete_audit::record("rm-shim.child", &target.path, "rm -r in roots");
                std::fs::remove_dir_all(&target.path)
            } else {
                Err(std::io::Error::other("Is a directory"))
            }
        } else {
            // Audit before acting (#893), like every other deletion path.
            clud::gc::delete_audit::record("rm-shim.child", &target.path, "rm in roots");
            clud::rm_tool::remove_link_or_file(&target.path)
        };
        match result {
            Ok(()) => {
                if plan.verbose {
                    println!("removed '{}'", target.path.display());
                }
                paths.push(serde_json::json!({"path": target.path, "action": "purged"}));
            }
            Err(error) => {
                failed = true;
                eprintln!("rm: cannot remove '{}': {error}", target.path.display());
                paths.push(serde_json::json!({
                    "path": target.path,
                    "action": "failed",
                    "reason": error.to_string(),
                }));
            }
        }
    }
    let code = i32::from(failed);
    if let Ok(state) = clud::daemon::default_state_dir() {
        let record = serde_json::json!({
            "ts_unix": std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_secs())
                .unwrap_or(0),
            "command": "rm",
            "role": "child",
            "session_id": std::env::var("CLUD_SESSION_ID")
                .or_else(|_| std::env::var(clud::grind_facts::SESSION_ENV))
                .ok(),
            "cwd": std::env::current_dir().ok(),
            "paths": paths,
            "exit": code,
        });
        clud::rm_tool::append_audit(
            &state.join("logs").join("rm"),
            std::time::SystemTime::now(),
            &record,
        );
    }
    code
}

#[cfg(any())]
fn finish_rm(approved: clud::rm_guard::Approved, action: clud::rm_guard::Action) -> i32 {
    #[cfg(not(test))]
    if action == clud::rm_guard::Action::Execute {
        return real_execute(approved);
    }
    clud::rm_guard::report_dry_run(approved, action)
}

#[cfg(any())]
#[test]
fn rm_binary_unit_build_cannot_execute_even_with_both_gate_facts() {
    let temp = tempfile::tempdir().unwrap();
    let file = temp.path().join("must-survive");
    std::fs::write(&file, b"unit tests never remove").unwrap();
    let approved = clud::rm_guard::Approved {
        operands: vec![file.clone()],
        recursive: true,
        force: true,
        verbose: false,
    };
    assert_eq!(
        finish_rm(approved, clud::rm_guard::gate(false, true, true)),
        0
    );
    assert!(file.exists());
}

/// The only removal implementation. Absent from unit-test builds, with no
/// callback seam that could smuggle a removal into a unit test.
#[cfg(any())]
fn real_execute(approved: clud::rm_guard::Approved) -> i32 {
    #[cfg(target_os = "linux")]
    {
        let mut argv = vec![
            "/bin/rm".to_string(),
            "--preserve-root=all".into(),
            "--one-file-system".into(),
        ];
        if approved.recursive {
            argv.push("-r".into());
        }
        if approved.force {
            argv.push("-f".into());
        }
        if approved.verbose {
            argv.push("-v".into());
        }
        argv.push("--".into());
        argv.extend(
            approved
                .operands
                .iter()
                .map(|p| p.to_string_lossy().into_owned()),
        );
        match clud::subprocess::ManagedSubprocess::start_inheriting_env(argv, None, false, None) {
            Ok(child) => child.wait(None).unwrap_or(2),
            Err(e) => clud::rm_guard::deny(&format!("system rm failed: {e}")),
        }
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = approved;
        clud::rm_guard::deny("real rm is unsupported on this platform")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;
    use std::io::{Cursor, ErrorKind};
    use std::sync::Mutex;

    struct FakeEnv {
        vars: HashMap<String, String>,
        argv: Vec<String>,
    }

    impl ShimEnv for FakeEnv {
        fn var(&self, k: &str) -> Option<String> {
            self.vars.get(k).cloned()
        }
        fn args(&self) -> Vec<String> {
            self.argv.clone()
        }
    }

    /// A scripted stream: writes go to a sink, reads come from a
    /// pre-loaded response buffer. Lets tests simulate every degenerate
    /// case without spinning up a real socket.
    struct ScriptedStream {
        read_buf: Cursor<Vec<u8>>,
        written: Mutex<Vec<u8>>,
        write_should_fail: bool,
    }

    impl io::Read for ScriptedStream {
        fn read(&mut self, b: &mut [u8]) -> io::Result<usize> {
            self.read_buf.read(b)
        }
    }

    impl io::Write for ScriptedStream {
        fn write(&mut self, b: &[u8]) -> io::Result<usize> {
            if self.write_should_fail {
                return Err(io::Error::new(ErrorKind::BrokenPipe, "scripted disconnect"));
            }
            self.written.lock().unwrap().extend_from_slice(b);
            Ok(b.len())
        }
        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    fn make_env_with_socket(args: &[&str]) -> FakeEnv {
        let mut vars = HashMap::new();
        vars.insert(
            "CLUD_DAEMON_SOCKET".to_string(),
            "/tmp/clud.sock".to_string(),
        );
        FakeEnv {
            vars,
            argv: args.iter().map(|s| s.to_string()).collect(),
        }
    }

    fn make_env_no_socket() -> FakeEnv {
        FakeEnv {
            vars: HashMap::new(),
            argv: vec!["python".to_string()],
        }
    }

    // Production fn pointers can't capture state, so tests park
    // scripted bytes + exec result in a per-thread cell.
    mod thread_local_helper {
        use std::cell::RefCell;
        thread_local! {
            pub static SCRIPTED_BYTES: RefCell<Vec<u8>> = const { RefCell::new(Vec::new()) };
            pub static EXEC_RESULT: RefCell<Result<i32, std::io::ErrorKind>> =
                const { RefCell::new(Ok(0)) };
            pub static EXEC_PATH: RefCell<String> = const { RefCell::new(String::new()) };
        }

        pub fn set_scripted(bytes: Vec<u8>) {
            SCRIPTED_BYTES.with(|b| *b.borrow_mut() = bytes);
        }

        pub fn take_scripted() -> Vec<u8> {
            SCRIPTED_BYTES.with(|b| std::mem::take(&mut *b.borrow_mut()))
        }

        pub fn set_exec_result(r: Result<i32, std::io::ErrorKind>) {
            EXEC_RESULT.with(|e| *e.borrow_mut() = r);
        }

        pub fn take_exec_result() -> Result<i32, std::io::ErrorKind> {
            EXEC_RESULT.with(|e| *e.borrow())
        }

        pub fn set_exec_path(p: String) {
            EXEC_PATH.with(|e| *e.borrow_mut() = p);
        }

        pub fn taken_exec_path() -> String {
            EXEC_PATH.with(|e| e.borrow().clone())
        }
    }

    // Set up the scripted-bytes thread-local before the test, then this
    // fn pointer reads from it.
    fn connect_with_scripted_bytes(_: &str) -> io::Result<Box<dyn ShimStream + Send>> {
        let bytes = thread_local_helper::take_scripted();
        Ok(Box::new(ScriptedStream {
            read_buf: Cursor::new(bytes),
            written: Mutex::new(Vec::new()),
            write_should_fail: false,
        }))
    }

    fn connect_failing(_: &str) -> io::Result<Box<dyn ShimStream + Send>> {
        Err(io::Error::from(ErrorKind::ConnectionRefused))
    }

    fn connect_write_breaks(_: &str) -> io::Result<Box<dyn ShimStream + Send>> {
        Ok(Box::new(ScriptedStream {
            read_buf: Cursor::new(Vec::new()),
            written: Mutex::new(Vec::new()),
            write_should_fail: true,
        }))
    }

    fn fake_exec_success(path: &str, _args: &[String]) -> io::Result<i32> {
        thread_local_helper::set_exec_path(path.to_string());
        thread_local_helper::take_exec_result().map_err(io::Error::from)
    }

    fn fake_exec_error(_path: &str, _args: &[String]) -> io::Result<i32> {
        Err(io::Error::new(ErrorKind::NotFound, "exec not found"))
    }

    fn captured_stderr<F: FnOnce(&mut Vec<u8>) -> i32>(f: F) -> (i32, String) {
        let mut buf = Vec::new();
        let code = f(&mut buf);
        (code, String::from_utf8_lossy(&buf).to_string())
    }

    #[test]
    fn missing_session_var_exits_127() {
        let env = make_env_no_socket();
        let (code, err) =
            captured_stderr(|buf| shim_run(&env, connect_failing, fake_exec_success, buf));
        assert_eq!(code, EXIT_NO_SESSION);
        assert!(err.contains(STDERR_NO_SESSION), "stderr was: {err}");
    }

    #[test]
    fn prepared_target_executes_without_daemon_socket() {
        let mut env = make_env_no_socket();
        env.vars.insert(
            clud::shim_install::SHIM_TARGET_ENV_VAR.to_string(),
            "/usr/bin/python3".to_string(),
        );
        env.argv = vec!["python".to_string(), "hook.py".to_string()];
        thread_local_helper::set_exec_result(Ok(0));

        let (code, err) =
            captured_stderr(|err| shim_run(&env, connect_failing, fake_exec_success, err));

        assert_eq!(code, 0);
        assert!(err.is_empty());
        assert_eq!(thread_local_helper::taken_exec_path(), "/usr/bin/python3");
    }

    #[test]
    fn connect_failure_exits_69() {
        let env = make_env_with_socket(&["python"]);
        let (code, err) =
            captured_stderr(|buf| shim_run(&env, connect_failing, fake_exec_success, buf));
        assert_eq!(code, EXIT_DAEMON_UNREACHABLE);
        assert!(err.contains("daemon unreachable at /tmp/clud.sock"));
    }

    #[test]
    fn daemon_eof_mid_wait_exits_71() {
        thread_local_helper::set_scripted(Vec::new()); // empty response
        let env = make_env_with_socket(&["python"]);
        let (code, err) = captured_stderr(|buf| {
            shim_run(&env, connect_with_scripted_bytes, fake_exec_success, buf)
        });
        assert_eq!(code, EXIT_DAEMON_DISCONNECT);
        assert!(err.contains("daemon disconnected while resolving interpreter"));
    }

    #[test]
    fn write_breaks_mid_request_exits_71() {
        let env = make_env_with_socket(&["python"]);
        let (code, err) =
            captured_stderr(|buf| shim_run(&env, connect_write_breaks, fake_exec_success, buf));
        assert_eq!(code, EXIT_DAEMON_DISCONNECT);
        assert!(err.contains("daemon disconnected while resolving interpreter"));
    }

    #[test]
    fn garbage_response_exits_71() {
        thread_local_helper::set_scripted(b"this is not json\n".to_vec());
        let env = make_env_with_socket(&["python"]);
        let (code, err) = captured_stderr(|buf| {
            shim_run(&env, connect_with_scripted_bytes, fake_exec_success, buf)
        });
        assert_eq!(code, EXIT_DAEMON_DISCONNECT);
        assert!(err.contains("protocol error from daemon"));
    }

    #[test]
    fn not_available_exits_1() {
        thread_local_helper::set_scripted(
            b"{\"status\":\"not_available\",\"reason\":\"no Python 3 found\"}\n".to_vec(),
        );
        let env = make_env_with_socket(&["python"]);
        let (code, err) = captured_stderr(|buf| {
            shim_run(&env, connect_with_scripted_bytes, fake_exec_success, buf)
        });
        assert_eq!(code, EXIT_NOT_AVAILABLE);
        assert!(err.contains("no Python 3 found"));
    }

    #[test]
    fn exec_failure_exits_126() {
        thread_local_helper::set_scripted(b"{\"path\":\"/usr/bin/python3\"}\n".to_vec());
        let env = make_env_with_socket(&["python"]);
        let (code, err) = captured_stderr(|buf| {
            shim_run(&env, connect_with_scripted_bytes, fake_exec_error, buf)
        });
        assert_eq!(code, EXIT_EXEC_FAILED);
        assert!(err.contains("failed to exec /usr/bin/python3"));
    }

    #[test]
    fn successful_resolution_invokes_exec_with_path_and_args() {
        thread_local_helper::set_scripted(b"{\"path\":\"/usr/bin/python3\"}\n".to_vec());
        thread_local_helper::set_exec_result(Ok(0));
        let env = make_env_with_socket(&["python", "script.py", "--flag"]);
        let mut buf = Vec::new();
        let code = shim_run(
            &env,
            connect_with_scripted_bytes,
            fake_exec_success,
            &mut buf,
        );
        assert_eq!(code, 0);
        assert_eq!(thread_local_helper::taken_exec_path(), "/usr/bin/python3");
    }

    #[test]
    fn current_exe_basename_handles_paths_and_extensions() {
        assert_eq!(current_exe_basename(&["python".to_string()]), "python");
        assert_eq!(
            current_exe_basename(&["/usr/bin/python3".to_string()]),
            "python3"
        );
        assert_eq!(
            current_exe_basename(&["C:\\Tools\\python.exe".to_string()]),
            "python"
        );
        assert_eq!(current_exe_basename(&[]), "python");
    }
}

mod gh_shim {
    //! Session-local `gh` relay. Only `pr checks --watch` changes behavior.

    use std::ffi::OsString;
    use std::io::Read;
    use std::path::{Path, PathBuf};
    use std::process::{Command, Stdio};
    use std::time::{Duration, Instant};

    const TARGET: &str = "CLUD_GH_SHIM_TARGET";

    pub fn run(args: &[OsString]) -> i32 {
        let target = match std::env::var_os(TARGET) {
            Some(value) if !value.is_empty() => PathBuf::from(value),
            _ => {
                eprintln!("clud gh shim: {TARGET} is unset; run gh inside a clud session");
                return 127;
            }
        };
        if !target.is_absolute() || !target.is_file() || recursive_target(&target) {
            eprintln!("clud gh shim: invalid real gh target: {}", target.display());
            return 126;
        }
        if std::env::var("CLUD_GH_SHIM_FAIL_FAST").as_deref() != Ok("0") {
            match watch_words(args) {
                Ok(Some(words)) => return watch(&target, &words),
                Ok(None) => {}
                Err(code) => return code,
            }
        }
        exec(&target, args)
    }

    fn watch_words(args: &[OsString]) -> Result<Option<Vec<&str>>, i32> {
        let words: Option<Vec<&str>> = args.iter().map(|arg| arg.to_str()).collect();
        if let Some(words) = words {
            return Ok(is_watch(&words).then_some(words));
        }
        let lossy: Vec<_> = args.iter().map(|arg| arg.to_string_lossy()).collect();
        let words: Vec<_> = lossy.iter().map(|word| word.as_ref()).collect();
        if is_watch(&words) {
            eprintln!("clud gh shim: non-UTF8 PR-watch arguments are ambiguous");
            Err(2)
        } else {
            Ok(None)
        }
    }

    fn recursive_target(target: &Path) -> bool {
        let Ok(current) = std::env::current_exe() else {
            return false;
        };
        if target == current
            || std::fs::canonicalize(target)
                .ok()
                .is_some_and(|resolved| std::fs::canonicalize(current).ok() == Some(resolved))
        {
            return true;
        }
        std::env::var_os("CLUD_RM_SHIM_DIR")
            .is_some_and(|dir| target.starts_with(PathBuf::from(dir)))
    }

    fn is_watch(words: &[&str]) -> bool {
        if !words
            .iter()
            .any(|w| *w == "--watch" || w.starts_with("--watch="))
        {
            return false;
        }
        let mut positionals = Vec::new();
        let mut skip_value = false;
        for word in words {
            if skip_value {
                skip_value = false;
            } else if matches!(*word, "--repo" | "-R" | "--interval" | "-i" | "--hostname") {
                skip_value = true;
            } else if !word.starts_with('-') {
                positionals.push(*word);
            }
        }
        positionals.starts_with(&["pr", "checks"])
    }

    fn watch(target: &Path, words: &[&str]) -> i32 {
        let mut selector: Option<&str> = None;
        let mut repo: Option<&str> = None;
        let mut interval: Option<&str> = None;
        let mut watch_seen = false;
        let mut command = Vec::new();
        let mut i = 0;
        while i < words.len() {
            let word = words[i];
            match word {
                "pr" if command.is_empty() => command.push(word),
                "checks" if command == ["pr"] => command.push(word),
                "--watch" => watch_seen = true,
                "--fail-fast" => {}
                "--repo" | "-R" | "--interval" | "-i" => {
                    i += 1;
                    let Some(value) = words.get(i).copied() else {
                        return invalid(word);
                    };
                    if value.is_empty() || value.starts_with('-') {
                        return invalid(word);
                    }
                    if word == "--repo" || word == "-R" {
                        repo = Some(value)
                    } else {
                        interval = Some(value)
                    }
                }
                _ if word.starts_with("--repo=") && word.len() > 7 => repo = Some(&word[7..]),
                _ if word.starts_with("-R") && word.len() > 2 => repo = Some(&word[2..]),
                _ if word.starts_with("--interval=") && word.len() > 11 => {
                    interval = Some(&word[11..])
                }
                _ if word.starts_with("-i") && word.len() > 2 => interval = Some(&word[2..]),
                _ if word.starts_with('-') => return invalid(word),
                _ if selector.is_none() => selector = Some(word),
                _ => return invalid(word),
            }
            i += 1;
        }
        if command != ["pr", "checks"] || !watch_seen {
            return invalid("ambiguous gh command");
        }
        let number =
            if selector.is_some_and(|s| !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit())) {
                selector.unwrap().to_string()
            } else {
                let mut lookup = Command::new(target);
                lookup.arg("pr").arg("view");
                if let Some(value) = selector {
                    lookup.arg(value);
                }
                lookup.args(["--json", "number", "--jq", ".number"]);
                if let Some(value) = repo {
                    lookup.args(["--repo", value]);
                }
                let mut child = match lookup
                    .stderr(Stdio::inherit())
                    .stdout(Stdio::piped())
                    .spawn()
                {
                    Ok(child) => child,
                    Err(error) => {
                        eprintln!("clud gh shim: cannot resolve PR: {error}");
                        return 126;
                    }
                };
                let Some(stdout) = child.stdout.take() else {
                    eprintln!("clud gh shim: cannot capture PR number");
                    let _ = child.kill();
                    let _ = child.wait();
                    return 126;
                };
                let (sender, receiver) = std::sync::mpsc::channel();
                std::thread::spawn(move || {
                    let mut bytes = Vec::new();
                    let _ = sender.send(stdout.take(129).read_to_end(&mut bytes).map(|_| bytes));
                });
                let deadline = Instant::now() + Duration::from_secs(15);
                let status = loop {
                    match child.try_wait() {
                        Ok(Some(status)) => break status,
                        Ok(None) if Instant::now() < deadline => {
                            std::thread::sleep(Duration::from_millis(25));
                        }
                        Ok(None) => {
                            let _ = child.kill();
                            let _ = child.wait();
                            eprintln!("clud gh shim: PR lookup timed out");
                            return 124;
                        }
                        Err(error) => {
                            let _ = child.kill();
                            let _ = child.wait();
                            eprintln!("clud gh shim: PR lookup failed: {error}");
                            return 126;
                        }
                    }
                };
                if !status.success() {
                    return status.code().unwrap_or(1);
                }
                let bytes = match receiver.recv_timeout(Duration::from_secs(2)) {
                    Ok(Ok(bytes)) if bytes.len() <= 128 => bytes,
                    _ => {
                        eprintln!("clud gh shim: PR lookup output was too large or did not close");
                        return 2;
                    }
                };
                let value = String::from_utf8_lossy(&bytes).trim().to_string();
                if value.is_empty() || !value.bytes().all(|b| b.is_ascii_digit()) {
                    eprintln!("clud gh shim: gh pr view did not return a PR number");
                    return 2;
                }
                value
            };
        let Some(clud) = std::env::var_os("CLUD_EXE") else {
            eprintln!("clud gh shim: CLUD_EXE is unset");
            return 127;
        };
        let clud = PathBuf::from(clud);
        if !clud.is_absolute() || !clud.is_file() {
            eprintln!("clud gh shim: invalid CLUD_EXE: {}", clud.display());
            return 126;
        }
        let mut args: Vec<OsString> = ["tool", "run", "github/pr_merge_watch.py"]
            .map(OsString::from)
            .to_vec();
        args.push(number.into());
        if let Some(value) = repo {
            args.extend([OsString::from("--repo"), value.into()]);
        }
        if let Some(value) = interval {
            args.extend([OsString::from("--interval"), value.into()]);
        }
        exec(&clud, &args)
    }

    fn invalid(value: &str) -> i32 {
        eprintln!("clud gh shim: unsupported or ambiguous pr checks --watch argument: {value}");
        2
    }

    fn exec(path: &Path, args: &[OsString]) -> i32 {
        let mut command = Command::new(path);
        command.args(args);
        #[cfg(unix)]
        {
            use std::os::unix::process::CommandExt;
            let error = command.exec();
            eprintln!("clud gh shim: failed to exec {}: {error}", path.display());
            126
        }
        #[cfg(not(unix))]
        {
            match command.status() {
                Ok(status) => status.code().unwrap_or(1),
                Err(error) => {
                    eprintln!("clud gh shim: failed to exec {}: {error}", path.display());
                    126
                }
            }
        }
    }

    #[cfg(all(test, unix))]
    #[test]
    fn non_utf8_watch_selector_fails_closed() {
        use std::os::unix::ffi::OsStringExt;
        let args = [
            OsString::from("pr"),
            OsString::from("checks"),
            OsString::from_vec(b"branch-\xff".to_vec()),
            OsString::from("--watch"),
        ];
        assert_eq!(watch_words(&args), Err(2));
    }
}
