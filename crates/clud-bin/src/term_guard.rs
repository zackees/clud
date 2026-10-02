//! Out-of-process terminal restore for a force-killed clud (#1705).
//!
//! [`crate::session::RawTerminalGuard`] restores the terminal in `Drop`, which
//! covers every exit clud controls: the child exiting, Ctrl+C, the Unix
//! termination signals, and panics. Nothing inside the process can run on
//! `kill -9`, the OOM killer, `taskkill /F` or `TerminateProcess`, and the
//! child dies with clud, so the terminal was left in raw mode with mouse
//! tracking, focus reporting and bracketed paste on.
//!
//! The terminal outlives clud, so a separate process can still repair it:
//!
//! 1. While entering raw mode, clud starts this guard (`clud __term-guard`,
//!    a multicall form, so there is still one binary) and listens on a
//!    loopback port. The guard connects and authenticates with a token.
//! 2. clud sends the terminal state from before the session and the raw
//!    state it applied, and keeps the kitty keyboard frame count current.
//! 3. On a clean exit, after its own restore, clud sends `D` and the guard
//!    exits silently. EOF without `D` means clud died: the guard writes the
//!    child-mode reset ([`crate::session::CHILD_TERMINAL_MODES_RESET`]) and
//!    the kitty pops, and restores the terminal settings, but only while they
//!    are still exactly clud's raw settings. A shell that already set its own
//!    modes is left alone.
//!
//! The guard is started through a short-lived launcher that exits at once,
//! so it is in neither clud's process group, its Job Object, nor its process
//! tree: a group kill, `taskkill /T` or a Job close does not take it along.
//! It reaches the terminal by path (Unix, `O_NOCTTY`) or by `AttachConsole`
//! (Windows). It never outlives the session: it exits as soon as its
//! connection ends, either way.

use std::ffi::OsString;
use std::io::{BufRead, BufReader, Read as _, Write};
use std::net::{Shutdown, TcpListener, TcpStream};
use std::process::Command; // running-process: command-builder
use std::sync::{Arc, Condvar, Mutex, OnceLock};
use std::time::{Duration, Instant};

/// `clud __term-guard <launch|run> ...`, dispatched by [`crate::multicall`].
pub const SUBCOMMAND: &str = "__term-guard";

/// Set to `0` to run sessions without a guard.
pub const DISABLE_ENV: &str = "CLUD_TERM_GUARD";

/// How long clud keeps its port open for the guard to connect.
const ACCEPT_WINDOW: Duration = Duration::from_secs(10);
/// How long the guard waits to reach clud's port.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(5);

/// The terminal's settings, encoded so the two processes can compare them:
/// termios flags and control characters on Unix, the console input mode on
/// Windows.
pub type TermModes = Vec<u8>;

/// The terminal settings from before clud first touched them, captured once.
/// `main` pins this before any console setup; a library caller gets the
/// settings as of its first raw-mode entry.
pub fn initial_modes() -> Option<TermModes> {
    static INITIAL: OnceLock<Option<TermModes>> = OnceLock::new();
    INITIAL.get_or_init(sys::stdin_modes).clone()
}

/// The terminal's current settings, as [`initial_modes`] encodes them.
pub fn current_modes() -> Option<TermModes> {
    sys::stdin_modes()
}

fn enabled() -> bool {
    std::env::var_os(DISABLE_ENV).is_none_or(|value| value != "0")
}

// ─── wire protocol ─────────────────────────────────────────────────────────

/// One line clud sends the guard.
#[derive(Debug, PartialEq, Eq)]
enum Message {
    /// Terminal settings before the session, and the raw settings applied.
    State { initial: TermModes, raw: TermModes },
    /// Kitty keyboard frames the session has on the terminal's stack.
    Kitty(usize),
    /// Clean exit: the session restored the terminal itself.
    Done,
}

fn encode_message(message: &Message) -> String {
    match message {
        Message::State { initial, raw } => format!("S {} {}\n", hex(initial), hex(raw)),
        Message::Kitty(frames) => format!("K {frames}\n"),
        Message::Done => "D\n".to_string(),
    }
}

fn decode_message(line: &str) -> Option<Message> {
    let mut fields = line.split_whitespace();
    match fields.next()? {
        "S" => Some(Message::State {
            initial: unhex(fields.next()?)?,
            raw: unhex(fields.next()?)?,
        }),
        "K" => Some(Message::Kitty(fields.next()?.parse().ok()?)),
        "D" => Some(Message::Done),
        _ => None,
    }
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

fn unhex(text: &str) -> Option<Vec<u8>> {
    if !text.len().is_multiple_of(2) {
        return None;
    }
    (0..text.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(text.get(i..i + 2)?, 16).ok())
        .collect()
}

/// What the guard learned before its connection ended.
#[derive(Debug, Default, PartialEq, Eq)]
struct Report {
    state: Option<(TermModes, TermModes)>,
    kitty: usize,
    done: bool,
}

/// Read clud's messages until the connection ends.
fn read_report(stream: impl std::io::Read) -> Report {
    let mut report = Report::default();
    for line in BufReader::new(stream).lines() {
        let Ok(line) = line else { break };
        match decode_message(&line) {
            Some(Message::State { initial, raw }) => report.state = Some((initial, raw)),
            Some(Message::Kitty(frames)) => report.kitty = frames,
            Some(Message::Done) => {
                report.done = true;
                break;
            }
            None => {}
        }
    }
    report
}

/// What the guard does once clud is gone: the bytes to write, and the
/// settings to restore if any. Nothing after a clean exit, or if clud never
/// sent its state. Settings are restored only while the terminal still has
/// clud's raw settings, so a shell that has since set its own is untouched.
fn plan_restore(
    report: &Report,
    current: Option<&TermModes>,
) -> Option<(Vec<u8>, Option<TermModes>)> {
    if report.done {
        return None;
    }
    let (initial, raw) = report.state.as_ref()?;
    let mut bytes = crate::session::keyboard_enhancement_pop_bytes(report.kitty);
    bytes.extend_from_slice(crate::session::CHILD_TERMINAL_MODES_RESET);
    let restore = (current == Some(raw) && raw != initial).then(|| initial.clone());
    Some((bytes, restore))
}

// ─── clud side ─────────────────────────────────────────────────────────────

#[derive(Debug, Default)]
struct LinkState {
    stream: Option<TcpStream>,
    kitty: usize,
    finished: bool,
    guard_pid: Option<u32>,
}

/// clud's end of the guard connection, owned by `RawTerminalGuard`.
#[derive(Debug, Clone)]
pub struct TermGuardLink {
    shared: Arc<(Mutex<LinkState>, Condvar)>,
}

impl TermGuardLink {
    /// Start a guard for this session in the background. `None` when the
    /// guard is disabled or the terminal is not one it can reach.
    pub fn start(initial: TermModes, raw: TermModes, kitty: usize) -> Option<Self> {
        if !enabled() {
            return None;
        }
        let exe = std::env::current_exe().ok()?;
        let terminal = sys::terminal_arg()?;
        let listener = TcpListener::bind(("127.0.0.1", 0)).ok()?;
        let port = listener.local_addr().ok()?.port();
        let mut token = [0u8; 16];
        getrandom::fill(&mut token).ok()?;
        let token = hex(&token);

        let link = Self {
            shared: Arc::new((
                Mutex::new(LinkState {
                    kitty,
                    ..LinkState::default()
                }),
                Condvar::new(),
            )),
        };
        let args = GuardArgs {
            port,
            token: token.clone(),
            clud_pid: std::process::id(),
            terminal,
        };
        let shared = Arc::clone(&link.shared);
        let state = Message::State { initial, raw };
        // The spawn and the accept stay off the session's startup path.
        let spawned = std::thread::Builder::new()
            .name("clud-term-guard".into())
            .spawn(move || {
                if spawn_guard(&exe, "launch", &args).is_ok() {
                    accept_guard(listener, &token, &state, &shared);
                }
            });
        spawned.ok()?;
        Some(link)
    }

    /// Keep the guard's kitty frame count current.
    pub fn set_kitty_frames(&self, frames: usize) {
        let (lock, _) = &*self.shared;
        let mut state = lock.lock().expect("term guard lock");
        if state.kitty == frames {
            return;
        }
        state.kitty = frames;
        if let Some(stream) = state.stream.as_mut() {
            let _ = stream.write_all(encode_message(&Message::Kitty(frames)).as_bytes());
        }
    }

    /// Clean exit: the session restored the terminal itself, so the guard
    /// stands down. Idempotent.
    pub fn finish(&self) {
        let (lock, condvar) = &*self.shared;
        let mut state = lock.lock().expect("term guard lock");
        state.finished = true;
        if let Some(mut stream) = state.stream.take() {
            let _ = stream.write_all(encode_message(&Message::Done).as_bytes());
            let _ = stream.flush();
            let _ = stream.shutdown(Shutdown::Both);
        }
        condvar.notify_all();
    }

    /// Wait until the guard is connected and armed; its pid. For the probe
    /// that tests this end to end.
    #[doc(hidden)]
    pub fn wait_armed(&self, timeout: Duration) -> Option<u32> {
        let (lock, condvar) = &*self.shared;
        let state = lock.lock().expect("term guard lock");
        let (state, _) = condvar
            .wait_timeout_while(state, timeout, |s| s.guard_pid.is_none() && !s.finished)
            .expect("term guard lock");
        state.guard_pid
    }
}

/// Accept the guard's connection, check its token, arm it.
fn accept_guard(
    listener: TcpListener,
    token: &str,
    state_message: &Message,
    shared: &Arc<(Mutex<LinkState>, Condvar)>,
) {
    let (lock, condvar) = &**shared;
    if listener.set_nonblocking(true).is_err() {
        return;
    }
    let deadline = Instant::now() + ACCEPT_WINDOW;
    while Instant::now() < deadline {
        if lock.lock().expect("term guard lock").finished {
            return;
        }
        let stream = match listener.accept() {
            Ok((stream, _)) => stream,
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                std::thread::sleep(Duration::from_millis(10));
                continue;
            }
            Err(_) => return,
        };
        let Some((mut stream, guard_pid)) = authenticate(stream, token) else {
            continue;
        };
        let mut state = lock.lock().expect("term guard lock");
        if state.finished {
            let _ = stream.write_all(encode_message(&Message::Done).as_bytes());
            return;
        }
        let hello = [
            encode_message(state_message),
            encode_message(&Message::Kitty(state.kitty)),
        ]
        .concat();
        if stream.write_all(hello.as_bytes()).is_err() {
            return;
        }
        state.stream = Some(stream);
        state.guard_pid = Some(guard_pid);
        condvar.notify_all();
        return;
    }
}

/// Read the guard's `H <token> <pid>` line; the stream and the guard's pid.
fn authenticate(stream: TcpStream, token: &str) -> Option<(TcpStream, u32)> {
    stream.set_nonblocking(false).ok()?;
    stream.set_read_timeout(Some(Duration::from_secs(2))).ok()?;
    let mut line = String::new();
    BufReader::new(stream.try_clone().ok()?)
        .take(256)
        .read_line(&mut line)
        .ok()?;
    let mut fields = line.split_whitespace();
    if fields.next()? != "H" || fields.next()? != token {
        return None;
    }
    let pid = fields.next()?.parse().ok()?;
    stream.set_read_timeout(None).ok()?;
    Some((stream, pid))
}

// ─── guard side ────────────────────────────────────────────────────────────

#[derive(Debug, PartialEq, Eq)]
struct GuardArgs {
    port: u16,
    token: String,
    clud_pid: u32,
    /// The tty path on Unix; unused on Windows (the console is clud's).
    terminal: String,
}

impl GuardArgs {
    fn to_args(&self) -> Vec<String> {
        vec![
            "--port".into(),
            self.port.to_string(),
            "--token".into(),
            self.token.clone(),
            "--clud-pid".into(),
            self.clud_pid.to_string(),
            "--terminal".into(),
            self.terminal.clone(),
        ]
    }

    fn parse(args: &[OsString]) -> Option<Self> {
        let mut port = None;
        let mut token = None;
        let mut clud_pid = None;
        let mut terminal = None;
        let mut iter = args.iter().map(|arg| arg.to_string_lossy().into_owned());
        while let Some(flag) = iter.next() {
            let value = iter.next()?;
            match flag.as_str() {
                "--port" => port = value.parse().ok(),
                "--token" => token = Some(value),
                "--clud-pid" => clud_pid = value.parse().ok(),
                "--terminal" => terminal = Some(value),
                _ => return None,
            }
        }
        Some(Self {
            port: port?,
            token: token?,
            clud_pid: clud_pid?,
            terminal: terminal?,
        })
    }
}

/// Start `exe __term-guard <mode>` fully detached: no console, no stdio, its
/// own process group or session, outside the spawner's Job Object.
fn spawn_guard(exe: &std::path::Path, mode: &str, args: &GuardArgs) -> std::io::Result<()> {
    let mut command = Command::new(exe); // running-process: command-builder
    command.arg(SUBCOMMAND).arg(mode).args(args.to_args());
    command.env_remove(running_process::ORIGINATOR_ENV_VAR);
    let _detached = running_process::spawn_daemon_breaking_away_with_env_policy(
        &mut command,
        running_process::EnvironmentPolicy::Inherit,
    )?;
    Ok(())
}

/// `clud __term-guard <launch|run> --port P --token T --clud-pid N --terminal X`.
pub fn run_cli(args: &[OsString]) -> i32 {
    let Some(mode) = args.first().and_then(|mode| mode.to_str()) else {
        return 2;
    };
    let Some(guard_args) = GuardArgs::parse(&args[1..]) else {
        return 2;
    };
    match mode {
        // Re-spawn and exit, so the guard's parent is gone and no tree kill
        // that starts at clud can reach it.
        "launch" => {
            let Ok(exe) = std::env::current_exe() else {
                return 1;
            };
            i32::from(spawn_guard(&exe, "run", &guard_args).is_err())
        }
        "run" => run_guard(&guard_args),
        _ => 2,
    }
}

fn run_guard(args: &GuardArgs) -> i32 {
    let Some(terminal) = sys::Terminal::open(args) else {
        return 1;
    };
    let Ok(stream) = TcpStream::connect_timeout(
        &std::net::SocketAddr::from(([127, 0, 0, 1], args.port)),
        CONNECT_TIMEOUT,
    ) else {
        return 1;
    };
    let hello = format!("H {} {}\n", args.token, std::process::id());
    if (&stream).write_all(hello.as_bytes()).is_err() {
        return 1;
    }
    let report = read_report(&stream);
    if let Some((bytes, restore)) = plan_restore(&report, terminal.modes().as_ref()) {
        terminal.write(&bytes);
        if let Some(initial) = restore {
            terminal.apply(&initial);
        }
    }
    0
}

// ─── platform ──────────────────────────────────────────────────────────────

#[cfg(unix)]
mod sys {
    use std::fs::{File, OpenOptions};
    use std::io::Write;
    use std::os::fd::AsRawFd;
    use std::os::unix::fs::OpenOptionsExt;

    use super::{GuardArgs, TermModes};

    /// termios flags (`u64` each, little endian) then `c_cc`.
    fn encode(t: &libc::termios) -> TermModes {
        let mut out = Vec::new();
        for flag in [t.c_iflag, t.c_oflag, t.c_cflag, t.c_lflag] {
            out.extend_from_slice(&u64::from(flag).to_le_bytes());
        }
        out.extend_from_slice(&t.c_cc);
        out
    }

    fn modes_of(fd: i32) -> Option<TermModes> {
        // SAFETY: termios is plain data; tcgetattr fills it or fails.
        let mut t: libc::termios = unsafe { std::mem::zeroed() };
        (unsafe { libc::tcgetattr(fd, &mut t) } == 0).then(|| encode(&t))
    }

    pub(super) fn stdin_modes() -> Option<TermModes> {
        // SAFETY: isatty only inspects the descriptor.
        if unsafe { libc::isatty(libc::STDIN_FILENO) } != 1 {
            return None;
        }
        modes_of(libc::STDIN_FILENO)
    }

    /// The tty stdin is, which raw mode applies to.
    pub(super) fn terminal_arg() -> Option<String> {
        // SAFETY: ttyname returns a pointer into static storage or NULL;
        // the string is copied out at once.
        let name = unsafe { libc::ttyname(libc::STDIN_FILENO) };
        if name.is_null() {
            return None;
        }
        let name = unsafe { std::ffi::CStr::from_ptr(name) };
        Some(name.to_string_lossy().into_owned())
    }

    pub(super) struct Terminal {
        file: File,
    }

    impl Terminal {
        /// Open the session's tty by path. `O_NOCTTY`: the guard leads its
        /// own session and must not adopt the tty as its controlling one.
        pub(super) fn open(args: &GuardArgs) -> Option<Self> {
            let file = OpenOptions::new()
                .read(true)
                .write(true)
                .custom_flags(libc::O_NOCTTY)
                .open(&args.terminal)
                .ok()?;
            Some(Self { file })
        }

        pub(super) fn modes(&self) -> Option<TermModes> {
            modes_of(self.file.as_raw_fd())
        }

        pub(super) fn write(&self, bytes: &[u8]) {
            let _ = (&self.file).write_all(bytes);
            let _ = (&self.file).flush();
        }

        /// Restore the encoded flags and control characters; line speed and
        /// anything not encoded stay as they are.
        pub(super) fn apply(&self, modes: &TermModes) {
            let fd = self.file.as_raw_fd();
            // SAFETY: as in `modes_of`.
            let mut t: libc::termios = unsafe { std::mem::zeroed() };
            if unsafe { libc::tcgetattr(fd, &mut t) } != 0 {
                return;
            }
            if modes.len() != 32 + t.c_cc.len() {
                return;
            }
            let flag = |i: usize| {
                let mut word = [0u8; 8];
                word.copy_from_slice(&modes[i * 8..i * 8 + 8]);
                libc::tcflag_t::try_from(u64::from_le_bytes(word)).ok()
            };
            let (Some(iflag), Some(oflag), Some(cflag), Some(lflag)) =
                (flag(0), flag(1), flag(2), flag(3))
            else {
                return;
            };
            t.c_iflag = iflag;
            t.c_oflag = oflag;
            t.c_cflag = cflag;
            t.c_lflag = lflag;
            t.c_cc.copy_from_slice(&modes[32..]);
            // SAFETY: `t` is a valid termios for this fd.
            unsafe { libc::tcsetattr(fd, libc::TCSANOW, &t) };
        }
    }
}

#[cfg(windows)]
mod sys {
    use std::fs::{File, OpenOptions};
    use std::io::Write;
    use std::os::windows::io::AsRawHandle;

    use winapi::shared::minwindef::{BOOL, DWORD, FALSE, TRUE};
    use winapi::um::consoleapi::{GetConsoleMode, SetConsoleCtrlHandler, SetConsoleMode};
    use winapi::um::processenv::GetStdHandle;
    use winapi::um::winbase::STD_INPUT_HANDLE;
    use winapi::um::wincon::{
        AttachConsole, FreeConsole, CTRL_BREAK_EVENT, CTRL_C_EVENT,
        ENABLE_VIRTUAL_TERMINAL_PROCESSING,
    };
    use winapi::um::winnt::HANDLE;

    use super::{GuardArgs, TermModes};

    fn mode_of(handle: HANDLE) -> Option<u32> {
        let mut mode: DWORD = 0;
        // SAFETY: GetConsoleMode only writes `mode`.
        (unsafe { GetConsoleMode(handle, &mut mode) } != 0).then_some(mode)
    }

    pub(super) fn stdin_modes() -> Option<TermModes> {
        // SAFETY: plain Win32 call on this process's standard input.
        let handle = unsafe { GetStdHandle(STD_INPUT_HANDLE) };
        mode_of(handle).map(|mode| mode.to_le_bytes().to_vec())
    }

    /// Windows reaches the console through clud's pid, not a path.
    pub(super) fn terminal_arg() -> Option<String> {
        stdin_modes().map(|_| "console".to_string())
    }

    /// The guard shares the user's console: ignore Ctrl+C and Ctrl+Break so
    /// a keystroke meant for the session cannot kill it. Close, logoff and
    /// shutdown still end it.
    unsafe extern "system" fn ignore_interrupts(event: DWORD) -> BOOL {
        if event == CTRL_C_EVENT || event == CTRL_BREAK_EVENT {
            TRUE
        } else {
            FALSE
        }
    }

    pub(super) struct Terminal {
        input: File,
        output: File,
    }

    impl Terminal {
        /// Attach to clud's console (clud is alive: it is waiting for this
        /// guard to connect) and open its input and output buffers.
        pub(super) fn open(args: &GuardArgs) -> Option<Self> {
            // SAFETY: plain Win32 calls; the guard was started detached, so
            // there is no console to lose.
            unsafe {
                FreeConsole();
                if AttachConsole(args.clud_pid) == 0 {
                    return None;
                }
                SetConsoleCtrlHandler(Some(ignore_interrupts), TRUE);
            }
            let open = |name: &str| OpenOptions::new().read(true).write(true).open(name).ok();
            Some(Self {
                input: open("CONIN$")?,
                output: open("CONOUT$")?,
            })
        }

        pub(super) fn modes(&self) -> Option<TermModes> {
            mode_of(self.input.as_raw_handle() as HANDLE).map(|mode| mode.to_le_bytes().to_vec())
        }

        pub(super) fn write(&self, bytes: &[u8]) {
            let handle = self.output.as_raw_handle() as HANDLE;
            if let Some(mode) = mode_of(handle) {
                // SAFETY: `handle` is a console screen buffer.
                unsafe { SetConsoleMode(handle, mode | ENABLE_VIRTUAL_TERMINAL_PROCESSING) };
            }
            let _ = (&self.output).write_all(bytes);
            let _ = (&self.output).flush();
        }

        pub(super) fn apply(&self, modes: &TermModes) {
            let Ok(bytes) = <[u8; 4]>::try_from(modes.as_slice()) else {
                return;
            };
            // SAFETY: `input` is the console input buffer.
            unsafe {
                SetConsoleMode(
                    self.input.as_raw_handle() as HANDLE,
                    u32::from_le_bytes(bytes),
                )
            };
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn armed(initial: &[u8], raw: &[u8], kitty: usize) -> Report {
        Report {
            state: Some((initial.to_vec(), raw.to_vec())),
            kitty,
            done: false,
        }
    }

    #[test]
    fn messages_round_trip_through_their_wire_form() {
        for message in [
            Message::State {
                initial: vec![0, 1, 0xab, 0xff],
                raw: vec![7],
            },
            Message::Kitty(3),
            Message::Done,
        ] {
            let line = encode_message(&message);
            assert!(line.ends_with('\n'));
            assert_eq!(decode_message(line.trim_end()), Some(message));
        }
        assert_eq!(decode_message("S zz 00"), None);
        assert_eq!(decode_message("K many"), None);
        assert_eq!(decode_message("X"), None);
    }

    #[test]
    fn a_report_keeps_the_latest_state_and_frame_count() {
        let wire = "S 0102 0304\nK 1\nK 2\n";
        assert_eq!(read_report(wire.as_bytes()), armed(&[1, 2], &[3, 4], 2));
        let clean = "S 01 02\nK 1\nD\nK 9\n";
        let report = read_report(clean.as_bytes());
        assert!(report.done);
        assert_eq!(report.kitty, 1, "nothing after D is read");
    }

    #[test]
    fn a_clean_exit_or_a_guard_never_armed_does_nothing() {
        let mut report = armed(&[1], &[2], 1);
        report.done = true;
        assert_eq!(plan_restore(&report, Some(&vec![2])), None);
        assert_eq!(plan_restore(&Report::default(), Some(&vec![2])), None);
    }

    #[test]
    fn a_killed_session_gets_the_reset_and_its_settings_back() {
        let (bytes, restore) = plan_restore(&armed(&[1], &[2], 2), Some(&vec![2])).unwrap();
        let mut expected = b"\x1b[<1u\x1b[<1u".to_vec();
        expected.extend_from_slice(crate::session::CHILD_TERMINAL_MODES_RESET);
        assert_eq!(bytes, expected);
        assert_eq!(restore, Some(vec![1]));
    }

    #[test]
    fn settings_a_shell_already_changed_are_left_alone() {
        let (_, restore) = plan_restore(&armed(&[1], &[2], 0), Some(&vec![3])).unwrap();
        assert_eq!(restore, None, "not clud's raw settings any more");
        let (_, restore) = plan_restore(&armed(&[1], &[2], 0), None).unwrap();
        assert_eq!(restore, None, "unreadable terminal");
        let (_, restore) = plan_restore(&armed(&[1], &[1], 0), Some(&vec![1])).unwrap();
        assert_eq!(restore, None, "raw mode changed nothing");
    }

    #[test]
    fn guard_args_round_trip_and_reject_garbage() {
        let args = GuardArgs {
            port: 4242,
            token: "abcd".into(),
            clud_pid: 77,
            terminal: "/dev/pts/9".into(),
        };
        let os: Vec<OsString> = args.to_args().into_iter().map(OsString::from).collect();
        assert_eq!(GuardArgs::parse(&os), Some(args));
        assert_eq!(GuardArgs::parse(&[OsString::from("--port")]), None);
        assert_eq!(
            GuardArgs::parse(&[OsString::from("--bogus"), OsString::from("1")]),
            None
        );
        assert_eq!(run_cli(&[OsString::from("launch")]), 2);
    }

    /// The clud side end to end, with this test standing in for the guard:
    /// a wrong token is refused, the right one is armed with the state and
    /// the live frame count, and a clean finish sends `D`.
    #[test]
    fn the_link_arms_an_authenticated_guard_and_stands_it_down() {
        let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let port = listener.local_addr().unwrap().port();
        let link = TermGuardLink {
            shared: Arc::new((
                Mutex::new(LinkState {
                    kitty: 1,
                    ..LinkState::default()
                }),
                Condvar::new(),
            )),
        };
        let shared = Arc::clone(&link.shared);
        let state = Message::State {
            initial: vec![1],
            raw: vec![2],
        };
        let acceptor = std::thread::spawn(move || accept_guard(listener, "tok", &state, &shared));

        let mut impostor = TcpStream::connect(("127.0.0.1", port)).unwrap();
        impostor.write_all(b"H wrong 1\n").unwrap();
        let mut guard = TcpStream::connect(("127.0.0.1", port)).unwrap();
        guard.write_all(b"H tok 4321\n").unwrap();
        assert_eq!(link.wait_armed(Duration::from_secs(5)), Some(4321));
        acceptor.join().unwrap();

        link.set_kitty_frames(2);
        link.finish();
        let report = read_report(&guard);
        assert_eq!(report.state, Some((vec![1], vec![2])));
        assert_eq!(report.kitty, 2);
        assert!(report.done);
    }
}
