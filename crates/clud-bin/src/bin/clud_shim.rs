//! `clud-shim` — tiny PATH-resolved relay for `python` / `python3` /
//! `pip` / etc. inside a clud session. Slice 1 of #406 / #409.
//!
//! Flow (happy path):
//!   1. Read CLUD_DAEMON_SOCKET env var.
//!   2. Connect to the daemon over the local socket / named pipe.
//!   3. Send a `ResolveInterpreter` JSON line: `{"want": "python", "argv": [...]}`.
//!   4. Read one JSON line back: `{"path": "/usr/bin/python3"}` or
//!      `{"status": "not_available", "reason": "..."}`.
//!   5. `exec` the resolved path with the same argv (Unix) /
//!      `CreateProcess` + propagate exit code (Windows).
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
    let Some(socket) = env.var("CLUD_DAEMON_SOCKET") else {
        let _ = writeln!(err, "{STDERR_NO_SESSION}");
        return EXIT_NO_SESSION;
    };

    let argv = env.args();
    let want = current_exe_basename(&argv);
    let tail: Vec<String> = argv.iter().skip(1).cloned().collect();

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
        .and_then(|s| {
            std::path::Path::new(s)
                .file_stem()
                .map(|p| p.to_string_lossy().into_owned())
        })
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
