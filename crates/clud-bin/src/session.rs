use std::io::{self, IsTerminal};
use std::sync::atomic::{AtomicBool, Ordering};

use crossterm::event::{
    KeyboardEnhancementFlags, PopKeyboardEnhancementFlags, PushKeyboardEnhancementFlags,
};
use crossterm::execute;
use running_process_core::pty::reexports::portable_pty::PtySize;
use running_process_core::pty::NativePtyProcess;

use crate::dnd::{looks_like_dropped_path, normalize_dropped_path};

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
/// Bytes from `stdin_source` flow into the child's PTY via
/// `write_impl`. An `F3Observer` watches the byte stream and — when
/// `hooks.intercept_f3()` is true — counts `\x1bOR` sequences, firing
/// `on_f3_press` once per observed press. The bytes are NOT consumed:
/// the child still receives `\x1bOR` and can handle F3 itself if it
/// wants to. `on_tick` runs every loop iteration regardless of stdin
/// activity so hooks that poll background state (e.g. voice transcripts)
/// still make progress during idle.
///
/// For interactive Windows console input only, the reader normalizes BS
/// (`0x08`) to DEL (`0x7f`) before forwarding. That keeps Backspace aligned
/// with xterm-style TUI expectations even when the console does not emit VT
/// input bytes despite PTY mode requesting them. Non-interactive sources used
/// by tests and pipes are still forwarded unchanged.
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
/// polled `terminal_size::terminal_size()` on Windows).
///
/// Ctrl-C flow: raw mode turns the keyboard chord into byte `0x03` instead
/// of a terminal signal. For interactive stdin, the pump forwards that byte
/// once, then escalates via `interrupt_pty_process`. On POSIX this sends
/// SIGINT to the child's pgroup and waits up to 2s for exit. On Windows the
/// escalation closes the PTY directly (no extra 0x03 write), because the
/// underlying `send_interrupt_impl` duplicates the byte that stdin already
/// forwarded and makes Ink-based TUIs see a single press as
/// "Ctrl-C twice = exit". The external `interrupted` flag is still honored
/// for non-keyboard interrupts such as OS signals or tests.
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
    run_raw_pty_pump_with_extra_rx(process, interrupted, hooks, stdin_source, None)
}

/// Production pump entry that wires a side channel for IDropTarget
/// callbacks (issue #79). Constructs the platform-native resize watcher
/// internally and passes through to `run_raw_pty_pump_full`.
///
/// `extra_rx` chunks are interleaved with stdin chunks and forwarded to
/// the PTY exactly like real stdin, EXCEPT they bypass the
/// bracketed-paste normalizer (the OLE/IDropTarget callback already
/// hands us a normalized, newline-joined path).
pub fn run_raw_pty_pump_with_extra_rx<H, R>(
    process: &NativePtyProcess,
    interrupted: &AtomicBool,
    hooks: &mut H,
    stdin_source: R,
    extra_rx: Option<std::sync::mpsc::Receiver<Vec<u8>>>,
) -> i32
where
    H: InteractiveHooks,
    R: std::io::Read + Send + 'static,
{
    let (resize_tx, resize_rx) = std::sync::mpsc::channel::<(u16, u16)>();
    spawn_os_resize_watcher(resize_tx);
    run_raw_pty_pump_full(
        process,
        interrupted,
        hooks,
        stdin_source,
        resize_rx,
        extra_rx,
    )
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
    run_raw_pty_pump_full(process, interrupted, hooks, stdin_source, resize_rx, None)
}

/// Most-general pump entry. See `run_raw_pty_pump_with_extra_rx` for
/// the public-facing version that constructs the resize receiver.
pub fn run_raw_pty_pump_full<H, R>(
    process: &NativePtyProcess,
    interrupted: &AtomicBool,
    hooks: &mut H,
    stdin_source: R,
    resize_rx: std::sync::mpsc::Receiver<(u16, u16)>,
    extra_rx: Option<std::sync::mpsc::Receiver<Vec<u8>>>,
) -> i32
where
    H: InteractiveHooks,
    R: std::io::Read + Send + 'static,
{
    use std::sync::mpsc;

    let (stdin_tx, stdin_rx) = mpsc::channel::<Vec<u8>>();
    let interactive_real_stdin = stdin_source_is_real_stdin::<R>() && terminals_are_interactive();
    let normalize_console_stdin =
        should_normalize_interactive_console_stdin(interactive_real_stdin);
    let interrupt_on_ctrl_c_byte = interactive_real_stdin;

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
                    let mut chunk = buf[..n].to_vec();
                    if normalize_console_stdin {
                        normalize_interactive_console_stdin_chunk(&mut chunk);
                    }
                    if stdin_tx.send(chunk).is_err() {
                        break; // Main thread dropped the receiver → exit.
                    }
                }
                Err(_) => break,
            }
        }
    });

    let mut observer = F3Observer::new();
    // Issue #63 / #79: bracketed-paste passes through the PTY pump as
    // raw bytes. When the user drags a file onto the terminal, the
    // terminal emits `\x1b[200~ <path-shaped string> \x1b[201~`. We
    // normalize that path BEFORE forwarding so all backends see a
    // canonical form, regardless of which terminal produced the drop.
    let mut paste = BracketedPasteNormalizer::new();

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
        // Drain one chunk from the side channel (drag-drop OLE
        // callback) — these are pre-normalized path bytes that should
        // bypass the bracketed-paste detector and go straight to the
        // PTY.
        if let Some(ref rx) = extra_rx {
            if let Ok(chunk) = rx.try_recv() {
                if let Err(err) = process.write_impl(&chunk, false) {
                    eprintln!(
                        "[clud] warning: failed to forward dropped paths to pty: {}",
                        err
                    );
                }
            }
        }

        if let Ok(chunk) = stdin_rx.try_recv() {
            let requested_interrupt =
                interrupt_on_ctrl_c_byte && stdin_chunk_requests_interrupt(&chunk);
            // Run the bracketed-paste normalizer over the chunk BEFORE
            // forwarding to the PTY. Non-paste bytes pass through with
            // O(1) state cost (just a 6-byte prefix matcher); paste
            // bodies are buffered and rewritten in place.
            let outgoing = paste.process(&chunk);
            if let Err(err) = process.write_impl(&outgoing, false) {
                eprintln!("[clud] warning: failed to forward stdin to pty: {}", err);
            } else if hooks.intercept_f3() {
                // F3 detection still runs over the ORIGINAL chunk: a
                // press inside a paste body is unusual but we want
                // detection symmetry with raw byte forwarding.
                let presses = observer.observe(&chunk);
                for _ in 0..presses {
                    if let Err(err) = hooks.on_f3_press(process) {
                        eprintln!("[clud] warning: voice F3 press hook failed: {}", err);
                    }
                }
            }

            if requested_interrupt || interrupted.load(Ordering::SeqCst) {
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

fn stdin_source_is_real_stdin<R: 'static>() -> bool {
    std::any::TypeId::of::<R>() == std::any::TypeId::of::<std::io::Stdin>()
}

fn should_normalize_interactive_console_stdin(interactive_real_stdin: bool) -> bool {
    cfg!(windows) && interactive_real_stdin
}

fn normalize_interactive_console_stdin_chunk(chunk: &mut [u8]) {
    if !cfg!(windows) {
        return;
    }
    for byte in chunk {
        if *byte == 0x08 {
            *byte = 0x7f;
        }
    }
}

fn stdin_chunk_requests_interrupt(chunk: &[u8]) -> bool {
    chunk.contains(&0x03)
}

/// Bracketed-paste byte sequence emitted by xterm-class terminals when
/// the user pastes (or drags-and-drops) text.
const PASTE_START: &[u8] = b"\x1b[200~";
const PASTE_END: &[u8] = b"\x1b[201~";

/// Stream-resumable bracketed-paste detector that, on each completed
/// paste, runs the buffered inner content through
/// [`looks_like_dropped_path`] / [`normalize_dropped_path`] to canonicalize
/// terminal-specific drop encodings (issue #63 / #79).
///
/// Behavior:
/// - Bytes outside any paste pass through unchanged.
/// - Bytes inside a bracketed paste are buffered. On `\x1b[201~`:
///     - If `looks_like_dropped_path(inner)` returns true, emit
///       `\x1b[200~` + `normalize_dropped_path(inner)` + `\x1b[201~`.
///     - Otherwise, emit the original `\x1b[200~ inner \x1b[201~` verbatim.
/// - The detector survives across chunks: a paste split across reads is
///   reassembled correctly.
///
/// The PASS-IT-VERBATIM rule on non-path content is essential — a
/// multi-line code paste must not be mutated, even if its first line
/// happens to start with `/`.
pub struct BracketedPasteNormalizer {
    /// How many bytes of `PASTE_START` we've matched while outside a
    /// paste. 0..PASTE_START.len().
    start_match: usize,
    /// `Some(buf)` while we are inside a paste body. The buffer holds
    /// the *inner* paste content (no `\x1b[200~` prefix and no terminal
    /// `\x1b[201~`).
    inside: Option<Vec<u8>>,
    /// How many bytes of `PASTE_END` we've matched while inside a paste.
    end_match: usize,
}

impl BracketedPasteNormalizer {
    pub fn new() -> Self {
        Self {
            start_match: 0,
            inside: None,
            end_match: 0,
        }
    }

    /// Process a chunk, returning the byte stream that should be
    /// forwarded downstream (PTY master, in production).
    pub fn process(&mut self, chunk: &[u8]) -> Vec<u8> {
        let mut out = Vec::with_capacity(chunk.len());
        for &b in chunk {
            if let Some(buf) = self.inside.as_mut() {
                // We are inside a paste body. Look for PASTE_END.
                if b == PASTE_END[self.end_match] {
                    self.end_match += 1;
                    if self.end_match == PASTE_END.len() {
                        // Complete: emit normalized form and reset.
                        let inner = std::mem::take(buf);
                        self.inside = None;
                        self.end_match = 0;
                        emit_paste_block(&mut out, &inner);
                    }
                    continue;
                }

                // PASTE_END prefix broke. Flush any partial-end bytes
                // back into the inner buffer, then this byte too.
                if self.end_match > 0 {
                    buf.extend_from_slice(&PASTE_END[..self.end_match]);
                    // The byte that broke the prefix may itself start a
                    // new PASTE_END match.
                    self.end_match = if b == PASTE_END[0] { 1 } else { 0 };
                    if self.end_match == 0 {
                        buf.push(b);
                    }
                } else {
                    buf.push(b);
                }
            } else {
                // We are outside a paste. Look for PASTE_START.
                if b == PASTE_START[self.start_match] {
                    self.start_match += 1;
                    if self.start_match == PASTE_START.len() {
                        // Complete: enter paste body.
                        self.start_match = 0;
                        self.end_match = 0;
                        self.inside = Some(Vec::new());
                    }
                    continue;
                }

                // PASTE_START prefix broke. Flush partial bytes verbatim.
                if self.start_match > 0 {
                    out.extend_from_slice(&PASTE_START[..self.start_match]);
                    self.start_match = if b == PASTE_START[0] { 1 } else { 0 };
                    if self.start_match == 0 {
                        out.push(b);
                    }
                } else {
                    out.push(b);
                }
            }
        }
        out
    }
}

impl Default for BracketedPasteNormalizer {
    fn default() -> Self {
        Self::new()
    }
}

/// Helper for `BracketedPasteNormalizer::process` — given a captured
/// inner paste body, emit the wrapped (and possibly path-normalized)
/// bracketed-paste block to `out`.
fn emit_paste_block(out: &mut Vec<u8>, inner: &[u8]) {
    out.extend_from_slice(PASTE_START);
    // Decide path-rewrite on the WHOLE buffer, not per-line: a
    // multi-line code paste with a path-shaped first line must remain
    // verbatim. `looks_like_dropped_path` is conservative — it requires
    // the entire trimmed string to look like a single path token.
    let s = match std::str::from_utf8(inner) {
        Ok(s) => s,
        Err(_) => {
            out.extend_from_slice(inner);
            out.extend_from_slice(PASTE_END);
            return;
        }
    };
    if looks_like_dropped_path(s) {
        let normalized = normalize_dropped_path(s);
        out.extend_from_slice(normalized.as_bytes());
    } else {
        out.extend_from_slice(inner);
    }
    out.extend_from_slice(PASTE_END);
}

fn reap_pty_exit(process: &NativePtyProcess) -> i32 {
    process.wait_impl(Some(1.0)).unwrap_or(1)
}

/// Escalate the `interrupted` flag to a real child-kill. Called once the
/// pump has observed the flag. Platform-split because `send_interrupt_impl`
/// is a byte-write on Windows (duplicates the 0x03 already forwarded via
/// raw-mode stdin) and a pgroup-SIGINT on POSIX (cooperative, no duplicate).
fn interrupt_pty_process(process: &NativePtyProcess) -> i32 {
    #[cfg(windows)]
    {
        // Closing the PTY triggers ConPTY's CTRL_CLOSE_EVENT path and
        // tears the child down without writing a second 0x03 byte.
        let _ = process.close_impl();
        eprintln!("[clud] interrupted via Ctrl+C (pty)");
        130
    }
    #[cfg(not(windows))]
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

    #[test]
    fn console_stdin_normalization_is_windows_only() {
        let mut chunk = vec![b'a', 0x08, 0x7f, b'z'];
        normalize_interactive_console_stdin_chunk(&mut chunk);
        if cfg!(windows) {
            assert_eq!(chunk, vec![b'a', 0x7f, 0x7f, b'z']);
        } else {
            assert_eq!(chunk, vec![b'a', 0x08, 0x7f, b'z']);
        }
    }

    #[test]
    fn ctrl_c_byte_requests_interrupt() {
        assert!(!stdin_chunk_requests_interrupt(b"abc"));
        assert!(stdin_chunk_requests_interrupt(b"a\x03c"));
    }

    #[test]
    fn only_real_stdin_gets_interactive_console_policy() {
        assert!(stdin_source_is_real_stdin::<std::io::Stdin>());
        assert!(!stdin_source_is_real_stdin::<std::io::Cursor<Vec<u8>>>());
    }

    // ─── BracketedPasteNormalizer (issue #63 / #79) ────────────────────

    #[test]
    fn paste_normalizer_passthrough_bytes_outside_paste() {
        let mut p = BracketedPasteNormalizer::new();
        // Plain typing — no PASTE_START seen — passes through verbatim.
        let out = p.process(b"hello world\n");
        assert_eq!(out, b"hello world\n");
    }

    /// session_paste_normalizes_path_on_drop — when a bracketed paste
    /// arrives whose body looks like a dragged path, the body must be
    /// rewritten through `normalize_dropped_path` before forwarding.
    #[test]
    fn session_paste_normalizes_path_on_drop() {
        let mut p = BracketedPasteNormalizer::new();
        // GNOME-Terminal-style file URI drop. Should canonicalize to
        // the platform-appropriate path form.
        let chunk = b"\x1b[200~file:///home/me/my%20file.txt\x1b[201~";
        let out = p.process(chunk);
        // Both POSIX and Windows must wrap output in bracketed-paste
        // markers and percent-decode the URI.
        assert!(out.starts_with(PASTE_START), "must keep PASTE_START");
        assert!(out.ends_with(PASTE_END), "must keep PASTE_END");
        let inner_start = PASTE_START.len();
        let inner_end = out.len() - PASTE_END.len();
        let inner = std::str::from_utf8(&out[inner_start..inner_end]).expect("utf8");
        assert!(
            inner.contains("my file.txt"),
            "inner must be percent-decoded; got {inner:?}"
        );
        // The original URI scheme is gone (normalized form is a path,
        // not a URI).
        assert!(!inner.contains("file://"), "URI must be stripped");
    }

    /// session_paste_passthrough_for_non_path_text — a paste of plain
    /// text (e.g. a code snippet) must be forwarded VERBATIM with the
    /// PASTE_START / PASTE_END markers preserved.
    #[test]
    fn session_paste_passthrough_for_non_path_text() {
        let mut p = BracketedPasteNormalizer::new();
        let chunk = b"\x1b[200~hello world\x1b[201~";
        let out = p.process(chunk);
        // No path → exact passthrough.
        assert_eq!(out, b"\x1b[200~hello world\x1b[201~");
    }

    #[test]
    fn paste_normalizer_multiline_paste_with_path_first_line_is_passthrough() {
        // A multi-line paste whose first line happens to look like a
        // path must not have the path-rewrite applied. The whole-buffer
        // `looks_like_dropped_path` check handles this — multi-line
        // strings don't match the heuristic.
        let mut p = BracketedPasteNormalizer::new();
        let chunk = b"\x1b[200~/Users/me/x.txt\nlet x = 1;\x1b[201~";
        let out = p.process(chunk);
        assert_eq!(out, chunk);
    }

    #[test]
    fn paste_normalizer_handles_split_chunks() {
        // PASTE_START split across two chunks, body in a third, end in a
        // fourth. The detector must reassemble correctly.
        let mut p = BracketedPasteNormalizer::new();
        let mut all = Vec::new();
        all.extend_from_slice(&p.process(b"abc\x1b[2"));
        all.extend_from_slice(&p.process(b"00~"));
        all.extend_from_slice(&p.process(b"hello"));
        all.extend_from_slice(&p.process(b"\x1b[201~tail"));
        assert_eq!(all, b"abc\x1b[200~hello\x1b[201~tail");
    }

    #[test]
    fn paste_normalizer_broken_start_prefix_is_flushed() {
        // \x1b[2 then a non-matching byte — the partial prefix should
        // be forwarded verbatim, not swallowed.
        let mut p = BracketedPasteNormalizer::new();
        let out = p.process(b"\x1b[2X");
        assert_eq!(out, b"\x1b[2X");
    }

    #[test]
    fn paste_normalizer_two_pastes_back_to_back() {
        // Two pastes, neither path-shaped, should pass through cleanly.
        let mut p = BracketedPasteNormalizer::new();
        let out = p.process(b"\x1b[200~foo\x1b[201~bar\x1b[200~baz\x1b[201~");
        assert_eq!(out, b"\x1b[200~foo\x1b[201~bar\x1b[200~baz\x1b[201~");
    }
}
