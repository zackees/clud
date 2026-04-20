use std::io::{self, IsTerminal};
use std::sync::atomic::{AtomicBool, Ordering};

use crossterm::event::{
    KeyboardEnhancementFlags, PopKeyboardEnhancementFlags, PushKeyboardEnhancementFlags,
};
use crossterm::execute;
use running_process_core::pty::reexports::portable_pty::PtySize;
use running_process_core::pty::NativePtyProcess;

/// Resize the PTY. On Windows, `running_process_core::pty::NativePtyProcess::resize_impl`
/// is a deliberate no-op (see that crate's `pty/mod.rs:730-737`), so reaching
/// the underlying master's `resize()` directly is the only way to honor a
/// `SIGWINCH`/`Event::Resize`. On POSIX the library's implementation does the
/// right thing, so delegate. Issue #31, theory T2.
pub fn resize_pty(process: &NativePtyProcess, rows: u16, cols: u16) -> io::Result<()> {
    #[cfg(windows)]
    {
        let guard = process
            .handles
            .lock()
            .map_err(|e| io::Error::other(e.to_string()))?;
        if let Some(handles) = guard.as_ref() {
            handles
                .master
                .resize(PtySize {
                    rows,
                    cols,
                    pixel_width: 0,
                    pixel_height: 0,
                })
                .map_err(|e| io::Error::other(e.to_string()))?;
        }
        Ok(())
    }
    #[cfg(not(windows))]
    {
        // PtySize is used on Windows only; silence unused-import warnings.
        let _ = PtySize {
            rows,
            cols,
            pixel_width: 0,
            pixel_height: 0,
        };
        process
            .resize_impl(rows, cols)
            .map_err(|e| io::Error::other(e.to_string()))
    }
}

/// Byte-level observer that reports F3 presses seen in a stream, without
/// modifying the bytes. The raw pump forwards every byte to the child
/// verbatim, and asks this observer how many F3 press events flowed past
/// so it can call `InteractiveHooks::on_f3_press` that many times.
///
/// Only the SS3 form `\x1bOR` is matched — that's what crossterm was
/// previously decoding into `KeyCode::F(3)` press events on Windows ConPTY
/// and most POSIX terminals without kitty keyboard protocol. Kitty's
/// press/release CSI form is intentionally not parsed: voice mode is
/// press-to-toggle, so release events are redundant.
///
/// The matcher survives across `observe` calls, so `\x1bOR` split across
/// reads (even one byte at a time) still fires once.
pub struct F3Observer {
    /// How many bytes of `\x1bOR` have been matched so far. 0..=3.
    matched: usize,
}

impl F3Observer {
    const F3_SEQ: &'static [u8] = b"\x1bOR";

    pub fn new() -> Self {
        Self { matched: 0 }
    }

    /// Scan `chunk` and return the number of F3 presses it contains.
    /// Updates internal state so subsequent calls see continuing matches.
    pub fn observe(&mut self, chunk: &[u8]) -> u32 {
        let mut presses = 0u32;
        for &b in chunk {
            if b == Self::F3_SEQ[self.matched] {
                self.matched += 1;
                if self.matched == Self::F3_SEQ.len() {
                    presses += 1;
                    self.matched = 0;
                }
            } else {
                // Prefix broke. If this byte is itself `\x1b`, start a new
                // match from position 1; otherwise drop to 0.
                self.matched = if b == 0x1b { 1 } else { 0 };
            }
        }
        presses
    }
}

impl Default for F3Observer {
    fn default() -> Self {
        Self::new()
    }
}

pub trait InteractiveHooks {
    fn intercept_f3(&self) -> bool {
        false
    }

    fn on_f3_press(&mut self, _process: &NativePtyProcess) -> io::Result<()> {
        Ok(())
    }

    fn on_f3_release(&mut self, _process: &NativePtyProcess) -> io::Result<()> {
        Ok(())
    }

    fn on_tick(&mut self, _process: &NativePtyProcess) -> io::Result<()> {
        Ok(())
    }
}

/// Enable raw mode and keyboard-enhancement flags on the current
/// terminal, returning a guard that restores the original state on drop.
/// Only useful when stdin is an actual TTY; see `enter_raw_mode_if_tty`.
#[derive(Debug)]
pub struct RawTerminalGuard {
    enhancement_flags_pushed: bool,
}

/// Public factory for a `RawTerminalGuard` returning `None` when stdin
/// is piped (no point putting a pipe into raw mode). `run_plan_pty` in
/// main.rs owns the guard for the duration of the pump call so the
/// terminal is restored even if the pump panics.
pub fn enter_raw_mode_if_tty() -> Option<RawTerminalGuard> {
    if terminals_are_interactive() {
        RawTerminalGuard::enter().ok()
    } else {
        None
    }
}

impl RawTerminalGuard {
    pub fn enter() -> io::Result<Self> {
        crossterm::terminal::enable_raw_mode()?;

        let mut stdout = io::stdout();
        let flags = KeyboardEnhancementFlags::REPORT_EVENT_TYPES
            | KeyboardEnhancementFlags::DISAMBIGUATE_ESCAPE_CODES;
        let enhancement_flags_pushed =
            execute!(stdout, PushKeyboardEnhancementFlags(flags)).is_ok();

        Ok(Self {
            enhancement_flags_pushed,
        })
    }
}

impl Drop for RawTerminalGuard {
    fn drop(&mut self) {
        let _ = if self.enhancement_flags_pushed {
            execute!(io::stdout(), PopKeyboardEnhancementFlags)
        } else {
            Ok(())
        };
        let _ = crossterm::terminal::disable_raw_mode();
    }
}

pub fn terminals_are_interactive() -> bool {
    io::stdin().is_terminal() && io::stdout().is_terminal()
}

/// Raw-byte pump replacing the crossterm event loop on the PTY path.
///
/// Bytes from `stdin_source` flow verbatim into the child's PTY via
/// `write_impl`. An `F3Observer` watches the byte stream and — when
/// `hooks.intercept_f3()` is true — counts `\x1bOR` sequences, firing
/// `on_f3_press` once per observed press. The bytes are NOT consumed:
/// the child still receives `\x1bOR` and can handle F3 itself if it
/// wants to. `on_tick` runs every loop iteration regardless of stdin
/// activity so hooks that poll background state (e.g. voice transcripts)
/// still make progress during idle.
///
/// This replaces the old `run_interactive_pty_session` /
/// `run_pty_output_loop` split. The event-loop approach parsed stdin
/// through crossterm's `event::read` — a lossy demultiplexer that dropped
/// every escape sequence it didn't recognize (DSR replies, DA, XTWINOPS,
/// OSC color queries, etc.), which hung child TUIs like codex Ink that
/// write those queries on startup and wait for a reply. See issue #46.
///
/// Current scope: stdin forwarding + F3 observation + hook ticks +
/// Ctrl+C + child-exit detection. Resize handling (SIGWINCH on Unix,
/// `ReadConsoleInputW` on Windows) is added in Steps 9/10.
pub fn run_raw_pty_pump<H, R>(
    process: &NativePtyProcess,
    interrupted: &AtomicBool,
    hooks: &mut H,
    stdin_source: R,
) -> i32
where
    H: InteractiveHooks,
    R: std::io::Read + Send + 'static,
{
    let (resize_tx, resize_rx) = std::sync::mpsc::channel::<(u16, u16)>();
    spawn_os_resize_watcher(resize_tx);
    run_raw_pty_pump_with_resize_rx(process, interrupted, hooks, stdin_source, resize_rx)
}

/// Spawn the platform-native resize-watcher thread that feeds
/// `resize_tx` with `(rows, cols)` whenever the user resizes their
/// terminal window.
///
/// Unix: a SIGWINCH signal handler via `signal-hook`. Zero-latency;
/// the kernel delivers the signal the moment the terminal resizes.
///
/// Windows: 150 ms polling of `crossterm::terminal::size()`. The
/// zero-latency option would be `ReadConsoleInputW` filtering for
/// `WINDOW_BUFFER_SIZE_EVENT`, but that consumes events from the
/// shared console input buffer and races with our stdin reader for
/// keystrokes. Polling avoids the race at the cost of up to 150 ms
/// redraw lag — imperceptible in practice. See the plan file for the
/// deferred zero-latency variant.
fn spawn_os_resize_watcher(resize_tx: std::sync::mpsc::Sender<(u16, u16)>) {
    #[cfg(unix)]
    {
        use signal_hook::consts::signal::SIGWINCH;
        use signal_hook::iterator::Signals;
        std::thread::spawn(move || {
            let Ok(mut signals) = Signals::new([SIGWINCH]) else {
                return;
            };
            for _ in signals.forever() {
                if let Ok((cols, rows)) = crossterm::terminal::size() {
                    if resize_tx.send((rows, cols)).is_err() {
                        break; // pump exited
                    }
                }
            }
        });
    }
    #[cfg(windows)]
    {
        std::thread::spawn(move || {
            let mut last: Option<(u16, u16)> = None;
            loop {
                let Ok((cols, rows)) = crossterm::terminal::size() else {
                    break;
                };
                let now = (rows, cols);
                if Some(now) != last {
                    if resize_tx.send(now).is_err() {
                        break; // pump exited
                    }
                    last = Some(now);
                }
                std::thread::sleep(std::time::Duration::from_millis(150));
            }
        });
    }
}

/// Inner pump entry that takes an explicit resize receiver. Tests use this
/// to inject synthetic resize events without involving platform signal
/// machinery; production wrappers (`run_raw_pty_pump`) construct the
/// channel and spawn a platform-native resize producer thread.
pub fn run_raw_pty_pump_with_resize_rx<H, R>(
    process: &NativePtyProcess,
    interrupted: &AtomicBool,
    hooks: &mut H,
    stdin_source: R,
    resize_rx: std::sync::mpsc::Receiver<(u16, u16)>,
) -> i32
where
    H: InteractiveHooks,
    R: std::io::Read + Send + 'static,
{
    use std::sync::mpsc;

    let (stdin_tx, stdin_rx) = mpsc::channel::<Vec<u8>>();

    // Detached reader: pumps `stdin_source` → channel until EOF or error.
    // Detached (not joined) so a blocked `read()` on real stdin doesn't
    // wedge shutdown when the child exits — the process is terminating
    // anyway. See Step 12.
    std::thread::spawn(move || {
        let mut reader = stdin_source;
        let mut buf = [0u8; 4096];
        loop {
            match reader.read(&mut buf) {
                Ok(0) => break, // EOF
                Ok(n) => {
                    if stdin_tx.send(buf[..n].to_vec()).is_err() {
                        break; // Main thread dropped the receiver → exit.
                    }
                }
                Err(_) => break,
            }
        }
    });

    let mut observer = F3Observer::new();

    loop {
        // Child output → our stdout is handled by the library's PTY plumbing;
        // reading here just drains the master and keeps the child unblocked.
        match process.read_chunk_impl(Some(0.01)) {
            Ok(Some(_chunk)) => {}
            Ok(None) => {}
            Err(_) => return reap_pty_exit(process),
        }

        // Drain resize events — always before stdin so a late-arriving
        // resize doesn't wait on a chunk of typing to unblock the loop.
        while let Ok((rows, cols)) = resize_rx.try_recv() {
            if let Err(err) = resize_pty(process, rows, cols) {
                eprintln!("[clud] warning: failed to resize pty: {}", err);
            }
        }

        // Drain one chunk of stdin per iteration — draining unbounded
        // can wedge shutdown: `write_impl` shares a lock with
        // `poll_pty_process` and blocks on a full PTY input buffer, so
        // a large pending stdin backlog stops us from noticing that the
        // child has exited. One chunk per loop keeps the cadence even.
        if let Ok(chunk) = stdin_rx.try_recv() {
            if let Err(err) = process.write_impl(&chunk, false) {
                eprintln!("[clud] warning: failed to forward stdin to pty: {}", err);
            } else if hooks.intercept_f3() {
                let presses = observer.observe(&chunk);
                for _ in 0..presses {
                    if let Err(err) = hooks.on_f3_press(process) {
                        eprintln!("[clud] warning: voice F3 press hook failed: {}", err);
                    }
                }
            }

            if interrupted.load(Ordering::SeqCst) {
                return interrupt_pty_process(process);
            }
        }

        if let Err(err) = hooks.on_tick(process) {
            eprintln!("[clud] warning: interactive hook tick failed: {}", err);
        }

        if let Ok(Some(code)) =
            running_process_core::pty::poll_pty_process(&process.handles, &process.returncode)
        {
            return code;
        }

        if interrupted.load(Ordering::SeqCst) {
            return interrupt_pty_process(process);
        }
    }
}

fn reap_pty_exit(process: &NativePtyProcess) -> i32 {
    process.wait_impl(Some(1.0)).unwrap_or(1)
}

fn interrupt_pty_process(process: &NativePtyProcess) -> i32 {
    match process.send_interrupt_impl() {
        Ok(()) => match process.wait_impl(Some(2.0)) {
            Ok(code) => {
                eprintln!("[clud] interrupted via Ctrl+C (pty)");
                code
            }
            Err(_) => {
                let _ = process.close_impl();
                eprintln!("[clud] interrupted via Ctrl+C (pty)");
                130
            }
        },
        Err(_) => {
            let _ = process.close_impl();
            eprintln!("[clud] interrupted via Ctrl+C (pty)");
            130
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Spawn a short-lived sleep-like command in a PTY so we have a live
    /// master handle to test resize_pty against. Returns `None` if the
    /// host environment can't allocate a PTY (nested Windows shells where
    /// ConPTY spawn silently no-ops; see issue #31 notes).
    fn spawn_idle_pty(rows: u16, cols: u16) -> Option<NativePtyProcess> {
        let argv: Vec<String> = if cfg!(windows) {
            // `ping -n 3 127.0.0.1` keeps the child alive ~2s without needing
            // a console for stdout, which is enough for a resize roundtrip.
            vec![
                "cmd.exe".into(),
                "/c".into(),
                "ping -n 3 127.0.0.1 > NUL".into(),
            ]
        } else {
            vec!["/bin/sh".into(), "-c".into(), "sleep 2".into()]
        };
        let process = NativePtyProcess::new(argv, None, None, rows, cols, None).ok()?;
        process.set_echo(false);
        process.start_impl().ok()?;
        Some(process)
    }

    /// resize_pty should change the master's reported size on every platform,
    /// including Windows (where the library's own `resize_impl` is a no-op).
    /// Issue #31, theory T2.
    #[test]
    fn resize_pty_updates_master_size_on_all_platforms() {
        let Some(process) = spawn_idle_pty(20, 80) else {
            eprintln!(
                "resize_pty_updates_master_size_on_all_platforms: SKIP (PTY spawn unavailable)"
            );
            return;
        };

        // Sanity: the master reports the initial size we requested.
        {
            let guard = process.handles.lock().expect("handles");
            let handles = guard.as_ref().expect("handles present");
            let before = handles.master.get_size().expect("get_size");
            assert_eq!(
                (before.rows, before.cols),
                (20, 80),
                "initial master size wrong: {:?}",
                before
            );
        }

        // Resize via the helper and verify the master advances.
        resize_pty(&process, 40, 120).expect("resize_pty");

        {
            let guard = process.handles.lock().expect("handles");
            let handles = guard.as_ref().expect("handles present");
            let after = handles.master.get_size().expect("get_size");
            assert_eq!(
                (after.rows, after.cols),
                (40, 120),
                "resize_pty did not propagate to master: {:?}",
                after
            );
        }

        let _ = process.close_impl();
    }

    // F3Observer — byte-level observer for voice-mode F3 press detection.
    // Observer, not interceptor: bytes are still forwarded verbatim to the
    // child. These tests drive Steps 1–4 of the raw-pump refactor.

    #[test]
    fn observer_passes_arbitrary_bytes_through_without_detecting_f3() {
        // Random bytes, DSR, paste chunks, newlines — none of these should
        // register as F3 presses. The observer doesn't modify bytes; tests
        // here only assert the count of presses it reports.
        let mut obs = F3Observer::new();
        assert_eq!(obs.observe(b"\x1b[6n"), 0, "DSR query is not F3");
        assert_eq!(obs.observe(b"hello\n"), 0);
        assert_eq!(obs.observe(b"\x03"), 0, "raw Ctrl+C byte is not F3");
        let smoke: Vec<u8> = (0..=255u8).collect();
        // The smoke vector happens to contain \x1b,O,R bytes somewhere, but
        // they are not adjacent in that order, so no press should fire.
        assert_eq!(obs.observe(&smoke), 0);
    }

    #[test]
    fn observer_detects_single_and_multiple_f3_presses() {
        let mut obs = F3Observer::new();
        assert_eq!(obs.observe(b"\x1bOR"), 1);
        let mut obs = F3Observer::new();
        assert_eq!(obs.observe(b"hello\x1bORworld"), 1);
        let mut obs = F3Observer::new();
        assert_eq!(obs.observe(b"\x1bOR\x1bOR\x1bOR"), 3);
    }

    #[test]
    fn observer_detects_f3_across_fragmented_reads() {
        // 2-way split: \x1b | OR
        let mut obs = F3Observer::new();
        let mut total = 0;
        total += obs.observe(b"\x1b");
        total += obs.observe(b"OR");
        assert_eq!(total, 1, "2-way split should still detect one press");

        // 3-way split: \x1b | O | R
        let mut obs = F3Observer::new();
        let mut total = 0;
        for chunk in [&b"\x1b"[..], &b"O"[..], &b"R"[..]] {
            total += obs.observe(chunk);
        }
        assert_eq!(total, 1, "3-way split should still detect one press");

        // Broken prefix then a clean press later: only the clean one counts.
        let mut obs = F3Observer::new();
        let mut total = 0;
        total += obs.observe(b"\x1b");
        total += obs.observe(b"XYZ"); // breaks the prefix, X is not O
        total += obs.observe(b"\x1bOR");
        assert_eq!(total, 1);
    }

    #[test]
    fn observer_ignores_non_f3_escapes() {
        let mut obs = F3Observer::new();
        assert_eq!(obs.observe(b"\x1b[6n"), 0, "DSR");
        assert_eq!(obs.observe(b"\x1bOA"), 0, "SS3 up arrow");
        assert_eq!(obs.observe(b"\x1bOP"), 0, "F1 (SS3 P)");
        assert_eq!(
            obs.observe(b"\x1bOX\x1bOR tail"),
            1,
            "valid F3 after a bogus SS3 prefix should still count"
        );
    }
}
