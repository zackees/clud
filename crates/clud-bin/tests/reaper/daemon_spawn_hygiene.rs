//! #1186: `clud __daemon` is started through running-process's daemon spawn,
//! so it inherits nothing from its launcher except the environment it asked
//! for.
//!
//! Two properties are pinned here, and both need a **real** daemon started by
//! the **real** `clud` binary, because what they check is the detach itself:
//!
//! 1. **No leaked descriptors (Unix).** A launcher that holds a pipe without
//!    `FD_CLOEXEC` — a CI wrapper, an IDE host, a Python harness with
//!    inheritable fds — must not have that pipe's write end pinned open by the
//!    long-lived daemon. If it is, the reader never sees EOF. This is the Unix
//!    twin of the #37 Windows handle leak; the hand-rolled trampoline closed
//!    the Windows side and never swept Unix fds.
//! 2. **Declared, untagged.** The daemon carries `RUNNING_PROCESS_IS_DAEMON`
//!    (the cooperative spare signal DD-021/DD-023 rely on) and does **not**
//!    carry the launching session's `RUNNING_PROCESS_ORIGINATOR` tag, which a
//!    lazily started daemon would otherwise inherit from whichever `clud`
//!    subcommand happened to start it (#683).
//!
//! Raw `std::process::Command` is intentional and is why this file is in
//! `ci/banned_imports.py`'s exempt set: `NativeProcess` sweeps every fd above
//! 2 in the child, which would remove the very descriptor test 1 hands to
//! `clud`, making it pass whether or not the daemon leaks.

use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::time::{Duration, Instant};

use crate::exe;

fn clud() -> PathBuf {
    exe::bin_path("clud", option_env!("CARGO_BIN_EXE_clud"))
}

/// `clud daemon <verb>` against an isolated state dir in daemon test mode:
/// no host-wide scans, and a bounded lifetime so a daemon this test fails to
/// stop still exits on its own.
fn daemon_command(state_dir: &Path, verb: &str) -> Command {
    let mut command = Command::new(clud());
    command
        .args(["daemon", verb])
        .env("CLUD_DAEMON_STATE_DIR", state_dir)
        .env("CLUD_DAEMON_TEST_MODE", "1")
        .env("CLUD_DAEMON_TEST_IDLE_TIMEOUT_SECS", "20")
        .env("CLUD_DAEMON_TEST_MAX_LIFETIME_SECS", "120")
        .env_remove("CLUD_DAEMON_TEST_HOST_SCANS")
        .env_remove(running_process::ORIGINATOR_ENV_VAR)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    command
}

fn assert_success(output: &Output, what: &str) {
    assert!(
        output.status.success(),
        "{what} failed: {:?}\nstdout: {}\nstderr: {}",
        output.status,
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

fn read_daemon_pid(state_dir: &Path) -> u32 {
    let path = state_dir.join("daemon.json");
    let deadline = Instant::now() + Duration::from_secs(15);
    loop {
        if let Ok(text) = std::fs::read_to_string(&path) {
            if let Ok(value) = serde_json::from_str::<serde_json::Value>(&text) {
                if let Some(pid) = value.get("pid").and_then(serde_json::Value::as_u64) {
                    return pid as u32;
                }
            }
        }
        assert!(
            Instant::now() < deadline,
            "timed out waiting for {}",
            path.display()
        );
        std::thread::sleep(Duration::from_millis(25));
    }
}

/// Stops the test's daemon on every exit path, including a failed assertion.
struct DaemonGuard {
    state_dir: PathBuf,
}

impl Drop for DaemonGuard {
    fn drop(&mut self) {
        let _ = daemon_command(&self.state_dir, "stop").output();
    }
}

#[cfg(unix)]
#[test]
fn daemon_does_not_inherit_a_launchers_non_cloexec_fds() {
    use std::os::unix::process::CommandExt;

    let _serial = crate::REAPER_TEST_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let scratch = tempfile::tempdir().expect("scratch dir");
    let state_dir = scratch.path().join("state");

    let mut fds = [0 as libc::c_int; 2];
    assert_eq!(unsafe { libc::pipe(fds.as_mut_ptr()) }, 0, "pipe()");
    let (read_end, write_end) = (fds[0], fds[1]);
    // CLOEXEC in *this* process, so no other test's child can pick the pipe
    // up and hold it open — that would fail this test for someone else's leak.
    for fd in fds {
        unsafe {
            let flags = libc::fcntl(fd, libc::F_GETFD);
            assert_ne!(libc::fcntl(fd, libc::F_SETFD, flags | libc::FD_CLOEXEC), -1);
        }
    }

    let mut command = daemon_command(&state_dir, "restart");
    // Hand only the write end to `clud`, exactly as a launcher that leaked a
    // non-CLOEXEC pipe would. Runs in the forked child just before exec.
    unsafe {
        command.pre_exec(move || {
            let flags = libc::fcntl(write_end, libc::F_GETFD);
            if flags == -1 || libc::fcntl(write_end, libc::F_SETFD, flags & !libc::FD_CLOEXEC) == -1
            {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        });
    }
    let _guard = DaemonGuard {
        state_dir: state_dir.clone(),
    };
    let output = command.output().expect("run clud daemon restart");
    assert_success(&output, "clud daemon restart");
    let daemon_pid = read_daemon_pid(&state_dir);

    // `clud` has exited. Once this process drops its own copy, the only
    // possible remaining writer is something `clud` started.
    unsafe { libc::close(write_end) };
    let saw_eof = wait_for_eof(read_end, Duration::from_secs(5));
    unsafe { libc::close(read_end) };

    assert!(
        saw_eof,
        "the launcher's pipe never reached EOF: daemon pid {daemon_pid} inherited \
         a non-CLOEXEC write end from `clud` and is holding it open"
    );
}

/// Whether `fd` reports end-of-file within `timeout`.
#[cfg(unix)]
fn wait_for_eof(fd: libc::c_int, timeout: Duration) -> bool {
    let deadline = Instant::now() + timeout;
    let mut buf = [0u8; 64];
    while Instant::now() < deadline {
        let mut poll = libc::pollfd {
            fd,
            events: libc::POLLIN,
            revents: 0,
        };
        let ready = unsafe { libc::poll(&mut poll, 1, 100) };
        if ready > 0 {
            let read = unsafe { libc::read(fd, buf.as_mut_ptr().cast(), buf.len()) };
            if read == 0 {
                return true;
            }
        }
    }
    false
}

#[test]
fn daemon_declares_itself_and_drops_the_launchers_originator_tag() {
    let _serial = crate::REAPER_TEST_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let scratch = tempfile::tempdir().expect("scratch dir");
    let state_dir = scratch.path().join("state");

    // Stand in for a `clud` subcommand run from inside an agent session: this
    // test process is the live originator.
    let session_tag = format!("CLUD:{}", std::process::id());
    let mut command = daemon_command(&state_dir, "restart");
    command.env(running_process::ORIGINATOR_ENV_VAR, &session_tag);
    let _guard = DaemonGuard {
        state_dir: state_dir.clone(),
    };
    let output = command.output().expect("run clud daemon restart");
    assert_success(&output, "clud daemon restart");
    let daemon_pid = read_daemon_pid(&state_dir);

    let scan = clud::process_scan::scan_env("CLUD");
    let tagged = scan
        .tagged
        .iter()
        .find(|process| process.pid == daemon_pid)
        .map(|process| process.originator.clone());

    assert!(
        scan.declared_daemons.contains(&daemon_pid),
        "daemon pid {daemon_pid} does not carry {}",
        running_process::DAEMON_MARKER_ENV_VAR
    );
    assert_eq!(
        tagged, None,
        "daemon pid {daemon_pid} inherited the launching session's originator tag"
    );
}
