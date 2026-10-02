//! Issue #1705: a force-killed session still gets its terminal back.
//!
//! Drives `clud-term-guard-probe`, which enters raw mode through the real
//! `RawTerminalGuard` (starting the real out-of-process guard) and turns on
//! the input modes a child TUI enables. The probe is then killed with no
//! chance to clean up (`SIGKILL` / `TerminateProcess`), and the test checks
//! that the guard restored the terminal and exited. The clean-exit case
//! checks that the guard stands down and writes nothing.
//!
//! Uses raw `std::process::Command` so the kill lands on the exact probe pid
//! and the probe runs on a specific pseudo-terminal (Unix) or its own console
//! (Windows). `NativeProcess` would put the probe in a contained Job Object
//! and kill it as a tree, which is not the kill under test — exempted in
//! `ci/banned_imports.py`.

use std::io::{BufRead, BufReader};
use std::path::PathBuf;
use std::process::ChildStderr;
use std::sync::mpsc;
use std::time::{Duration, Instant};

use crate::exe;

fn probe() -> PathBuf {
    exe::bin_path(
        "clud-term-guard-probe",
        option_env!("CARGO_BIN_EXE_clud-term-guard-probe"),
    )
}

/// Lines from a probe stream, read on a thread so waits can time out.
fn lines_of(stream: impl std::io::Read + Send + 'static) -> mpsc::Receiver<String> {
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        for line in BufReader::new(stream).lines().map_while(Result::ok) {
            if tx.send(line).is_err() {
                break;
            }
        }
    });
    rx
}

/// The value after `prefix` on the next line that has it.
fn expect_line(rx: &mpsc::Receiver<String>, prefix: &str, timeout: Duration) -> String {
    let deadline = Instant::now() + timeout;
    loop {
        let left = deadline.saturating_duration_since(Instant::now());
        match rx.recv_timeout(left) {
            Ok(line) => {
                if let Some(rest) = line.strip_prefix(prefix) {
                    return rest.trim().to_string();
                }
            }
            Err(error) => panic!("no `{prefix}` line from the probe: {error}"),
        }
    }
}

/// The probe's `ORIGINAL`, `RAW` and armed guard pid.
fn armed(stderr: ChildStderr) -> (mpsc::Receiver<String>, String, String, u32) {
    let rx = lines_of(stderr);
    let original = expect_line(&rx, "ORIGINAL ", Duration::from_secs(30));
    let raw = expect_line(&rx, "RAW ", Duration::from_secs(5));
    let guard = expect_line(&rx, "ARMED ", Duration::from_secs(5));
    let guard = guard
        .parse()
        .unwrap_or_else(|_| panic!("the session started no restore guard (ARMED {guard})"));
    assert_ne!(original, raw, "raw mode must change the terminal settings");
    (rx, original, raw, guard)
}

#[cfg(unix)]
mod unix {
    use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
    use std::process::{Command, Stdio};
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::{Arc, Mutex};
    use std::time::{Duration, Instant};

    use super::{armed, expect_line, probe};

    /// A pseudo-terminal whose output a thread collects.
    struct Pty {
        /// Held open for the reader thread, which reads its raw descriptor.
        _master: OwnedFd,
        slave: OwnedFd,
        output: Arc<Mutex<Vec<u8>>>,
        stop: Arc<AtomicBool>,
        reader: Option<std::thread::JoinHandle<()>>,
    }

    impl Pty {
        fn open() -> Self {
            let (mut master, mut slave) = (-1, -1);
            // SAFETY: openpty fills both descriptors or fails.
            let rc = unsafe {
                libc::openpty(
                    &mut master,
                    &mut slave,
                    std::ptr::null_mut(),
                    std::ptr::null(),
                    std::ptr::null(),
                )
            };
            assert_eq!(rc, 0, "openpty");
            // SAFETY: fresh descriptors this test now owns.
            let (master, slave) =
                unsafe { (OwnedFd::from_raw_fd(master), OwnedFd::from_raw_fd(slave)) };
            // SAFETY: fcntl on an owned descriptor.
            unsafe {
                let flags = libc::fcntl(master.as_raw_fd(), libc::F_GETFL);
                libc::fcntl(master.as_raw_fd(), libc::F_SETFL, flags | libc::O_NONBLOCK);
            }
            let output = Arc::new(Mutex::new(Vec::new()));
            let stop = Arc::new(AtomicBool::new(false));
            let fd = master.as_raw_fd();
            let (sink, halt) = (Arc::clone(&output), Arc::clone(&stop));
            let reader = std::thread::spawn(move || {
                let mut buf = [0u8; 4096];
                while !halt.load(Ordering::Acquire) {
                    // SAFETY: `buf` is valid for its length; `fd` stays open
                    // until this thread is joined.
                    let n = unsafe { libc::read(fd, buf.as_mut_ptr().cast(), buf.len()) };
                    if n > 0 {
                        sink.lock().unwrap().extend_from_slice(&buf[..n as usize]);
                    } else {
                        std::thread::sleep(Duration::from_millis(10));
                    }
                }
            });
            Self {
                _master: master,
                slave,
                output,
                stop,
                reader: Some(reader),
            }
        }

        fn termios(&self) -> libc::termios {
            // SAFETY: termios is plain data; tcgetattr fills it.
            let mut t: libc::termios = unsafe { std::mem::zeroed() };
            assert_eq!(
                unsafe { libc::tcgetattr(self.slave.as_raw_fd(), &mut t) },
                0
            );
            t
        }

        fn stdio(&self) -> Stdio {
            Stdio::from(self.slave.try_clone().expect("dup slave"))
        }

        fn count(&self, needle: &[u8]) -> usize {
            let out = self.output.lock().unwrap();
            out.windows(needle.len()).filter(|w| *w == needle).count()
        }
    }

    impl Drop for Pty {
        fn drop(&mut self) {
            self.stop.store(true, Ordering::Release);
            if let Some(reader) = self.reader.take() {
                let _ = reader.join();
            }
        }
    }

    fn same(a: &libc::termios, b: &libc::termios) -> bool {
        a.c_iflag == b.c_iflag
            && a.c_oflag == b.c_oflag
            && a.c_cflag == b.c_cflag
            && a.c_lflag == b.c_lflag
            && a.c_cc == b.c_cc
    }

    fn gone(pid: u32, timeout: Duration) -> bool {
        let deadline = Instant::now() + timeout;
        while Instant::now() < deadline {
            // SAFETY: signal 0 only checks that the pid exists.
            if unsafe { libc::kill(pid as libc::pid_t, 0) } != 0 {
                return true;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        false
    }

    fn eventually(timeout: Duration, mut check: impl FnMut() -> bool) -> bool {
        let deadline = Instant::now() + timeout;
        while Instant::now() < deadline {
            if check() {
                return true;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        check()
    }

    fn spawn(pty: &Pty, mode: &str) -> std::process::Child {
        Command::new(probe())
            .arg(mode)
            .stdin(pty.stdio())
            .stdout(pty.stdio())
            .stderr(Stdio::piped())
            .spawn()
            .expect("spawn clud-term-guard-probe")
    }

    const FOCUS_OFF: &[u8] = b"\x1b[?1004l";

    #[test]
    fn a_sigkilled_session_has_its_terminal_restored_by_the_guard() {
        let pty = Pty::open();
        let original = pty.termios();
        let mut child = spawn(&pty, "hold");
        let (_lines, _, _, guard) = armed(child.stderr.take().unwrap());
        assert!(
            !same(&pty.termios(), &original),
            "the session is in raw mode"
        );
        let child_modes_on = || pty.count(b"\x1b[?1004h") == 1;
        assert!(eventually(Duration::from_secs(5), child_modes_on));

        // SAFETY: a plain kill of the probe's own pid.
        assert_eq!(
            unsafe { libc::kill(child.id() as libc::pid_t, libc::SIGKILL) },
            0
        );
        let _ = child.wait();

        assert!(
            eventually(Duration::from_secs(10), || same(&pty.termios(), &original)),
            "the guard must put the terminal settings back"
        );
        // The guard writes before it restores the settings, but the reader
        // thread may not have collected those bytes yet.
        assert!(
            eventually(Duration::from_secs(5), || pty.count(b"\x1b[?25h") == 1),
            "the guard must write the reset"
        );
        for reset in [
            &b"\x1b[?1003l"[..],
            b"\x1b[?1006l",
            FOCUS_OFF,
            b"\x1b[?2004l",
            b"\x1b[?25h",
        ] {
            assert_eq!(pty.count(reset), 1, "guard reset {reset:?}");
        }
        assert!(
            gone(guard, Duration::from_secs(10)),
            "the guard exits once it is done"
        );
    }

    #[test]
    fn a_clean_exit_stands_the_guard_down() {
        let pty = Pty::open();
        let original = pty.termios();
        let mut child = spawn(&pty, "clean");
        let (lines, _, _, guard) = armed(child.stderr.take().unwrap());
        let _ = expect_line(&lines, "CLEAN", Duration::from_secs(10));
        assert!(child.wait().expect("probe exit").success());

        assert!(
            gone(guard, Duration::from_secs(10)),
            "the guard exits on `D`"
        );
        assert!(
            same(&pty.termios(), &original),
            "the session restored itself"
        );
        // Give a wrongly armed guard time to write; only the session's own
        // reset may appear.
        std::thread::sleep(Duration::from_millis(300));
        assert_eq!(pty.count(FOCUS_OFF), 1, "no second reset from the guard");
    }
}

#[cfg(windows)]
mod windows_console {
    use std::os::windows::process::CommandExt;
    use std::process::{Command, Stdio};
    use std::time::Duration;

    use windows::Win32::Foundation::{CloseHandle, WAIT_OBJECT_0};
    use windows::Win32::System::Threading::{
        OpenProcess, WaitForSingleObject, PROCESS_SYNCHRONIZE,
    };

    use super::{armed, expect_line, lines_of, probe};

    const CREATE_NEW_CONSOLE: u32 = 0x0000_0010;
    const DETACHED_PROCESS: u32 = 0x0000_0008;

    fn gone(pid: u32, timeout: Duration) -> bool {
        // SAFETY: plain Win32 calls; the handle is closed below.
        unsafe {
            let Ok(handle) = OpenProcess(PROCESS_SYNCHRONIZE, false, pid) else {
                return true;
            };
            let waited = WaitForSingleObject(handle, timeout.as_millis() as u32);
            let _ = CloseHandle(handle);
            waited == WAIT_OBJECT_0
        }
    }

    fn kill_and_reap(mut child: std::process::Child) {
        let _ = child.kill();
        let _ = child.wait();
    }

    fn spawn(mode: &str) -> std::process::Child {
        Command::new(probe())
            .arg(mode)
            .creation_flags(CREATE_NEW_CONSOLE)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .spawn()
            .expect("spawn clud-term-guard-probe")
    }

    #[test]
    fn a_terminated_session_has_its_console_mode_restored_by_the_guard() {
        let mut child = spawn("hold");
        let (_lines, original, raw, guard) = armed(child.stderr.take().unwrap());

        // A second process on the probe's console reports its input mode, and
        // keeps the console alive after the probe is gone.
        let mut keeper = Command::new(probe())
            .args(["keep", &child.id().to_string()])
            .creation_flags(DETACHED_PROCESS)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .expect("spawn keeper");
        let modes = lines_of(keeper.stdout.take().expect("keeper stdout"));
        let mode = expect_line(&modes, "MODE ", Duration::from_secs(10));
        assert_eq!(mode, raw, "the keeper sees the session's raw mode");

        // TerminateProcess: nothing in the probe runs.
        kill_and_reap(child);

        let deadline = std::time::Instant::now() + Duration::from_secs(10);
        loop {
            let mode = expect_line(&modes, "MODE ", Duration::from_secs(10));
            if mode == original {
                break;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "console input mode stayed {mode}, expected {original}"
            );
        }
        assert!(
            gone(guard, Duration::from_secs(10)),
            "the guard exits once it is done"
        );
        kill_and_reap(keeper);
    }

    #[test]
    fn a_clean_exit_stands_the_guard_down() {
        let mut child = spawn("clean");
        let (lines, _, _, guard) = armed(child.stderr.take().unwrap());
        let _ = expect_line(&lines, "CLEAN", Duration::from_secs(10));
        assert!(child.wait().expect("probe exit").success());
        assert!(
            gone(guard, Duration::from_secs(10)),
            "the guard exits on `D`"
        );
    }
}
