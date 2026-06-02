use std::io::{self, IsTerminal};
use std::sync::atomic::{AtomicBool, Ordering};

use crossterm::event::{
    KeyboardEnhancementFlags, PopKeyboardEnhancementFlags, PushKeyboardEnhancementFlags,
};
use crossterm::execute;
use running_process::pty::NativePtyProcess;
use running_process::pty::PtySize;

use crate::console_title::OscTitleStripper;
use crate::dnd::{looks_like_dropped_path, normalize_dropped_path};
use crate::graphics::GraphicsConfig;
use crate::verbose_log;

/// Resize the PTY. On Windows, `running_process::pty::NativePtyProcess::resize_impl`
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

/// Counts of F3 events observed in a stream chunk.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct F3Events {
    /// Number of F3 press events seen. Repeats (autorepeat) are intentionally
    /// not counted as new presses — they indicate the key is still held.
    pub presses: u32,
    /// Number of F3 release events seen. Only fires on terminals that
    /// implement the kitty keyboard protocol with REPORT_EVENT_TYPES.
    pub releases: u32,
}

/// Byte-level observer that reports F3 press / release events seen in a
/// stream, without modifying the bytes. The raw pump forwards every byte
/// to the child verbatim and asks this observer how many F3 events flowed
/// past so it can call `InteractiveHooks::on_f3_press` / `on_f3_release`
/// once per event.
///
/// Three encodings are matched, covering the cross-platform terminal
/// matrix:
///
/// * Legacy SS3 form `\x1bOR` — emitted by Windows ConPTY and most POSIX
///   terminals without kitty keyboard protocol. Press-only.
/// * CSI tilde form `\x1b[13~` — emitted by xterm and most Linux consoles.
///   Press-only by default; with kitty REPORT_EVENT_TYPES enabled the
///   terminal extends it to `\x1b[13;1:3~` for release, `\x1b[13;1:2~`
///   for repeat.
/// * Kitty CSI u form `\x1b[13u` or `\x1b[57346u` (functional encoding) —
///   press-only by default; the `;mod:event-type` suffix carries
///   release/repeat the same way.
///
/// Issue #13 hold-to-record relies on the release branch. Terminals that
/// don't emit release events (notably ConPTY) fall back to the
/// VAD-silence auto-stop inside the voice module — see `voice.rs`.
///
/// The state machine survives across `observe` calls, so any of these
/// sequences split across reads (even one byte at a time) still fires
/// exactly once.
pub struct F3Observer {
    state: F3State,
    /// Parameter bytes accumulated between `\x1b[` and a CSI terminator.
    /// Capped at MAX_CSI_LEN to keep a runaway terminal from growing this
    /// unboundedly.
    csi_buf: Vec<u8>,
}

#[derive(Debug, Clone, Copy)]
enum F3State {
    Idle,
    Esc,
    /// Saw `\x1bO`; one more byte and we know if this is SS3-R (F3 press).
    Ss3,
    /// Saw `\x1b[`; accumulating parameter bytes until a CSI terminator.
    Csi,
}

/// Max parameter-byte payload a CSI sequence can have before we abandon
/// the match. A real F3 event tops out at ~16 bytes (`\x1b[57346;1:3u`),
/// 64 is generous and bounds memory if the terminal is misbehaving.
const MAX_CSI_LEN: usize = 64;

impl F3Observer {
    pub fn new() -> Self {
        Self {
            state: F3State::Idle,
            csi_buf: Vec::new(),
        }
    }

    /// Scan `chunk` and return the F3 events it contains. Updates internal
    /// state so subsequent calls see continuing matches.
    pub fn observe(&mut self, chunk: &[u8]) -> F3Events {
        let mut events = F3Events::default();
        for &b in chunk {
            match self.state {
                F3State::Idle => {
                    if b == 0x1b {
                        self.state = F3State::Esc;
                    }
                }
                F3State::Esc => match b {
                    b'O' => self.state = F3State::Ss3,
                    b'[' => {
                        self.state = F3State::Csi;
                        self.csi_buf.clear();
                    }
                    0x1b => {} // stay in Esc, a new sequence is starting
                    _ => self.state = F3State::Idle,
                },
                F3State::Ss3 => match b {
                    b'R' => {
                        // \x1bOR — F3 press in SS3 encoding.
                        events.presses += 1;
                        self.state = F3State::Idle;
                    }
                    0x1b => self.state = F3State::Esc,
                    _ => self.state = F3State::Idle,
                },
                F3State::Csi => {
                    if is_csi_terminator(b) {
                        if let Some(kind) = parse_f3_csi(&self.csi_buf, b) {
                            match kind {
                                F3Kind::Press => events.presses += 1,
                                F3Kind::Release => events.releases += 1,
                                // Repeat = key still held; deliberately silent.
                                F3Kind::Repeat => {}
                            }
                        }
                        self.state = F3State::Idle;
                        self.csi_buf.clear();
                    } else if b == 0x1b {
                        // Nested escape — abandon this CSI, start a new sequence.
                        self.state = F3State::Esc;
                        self.csi_buf.clear();
                    } else if self.csi_buf.len() < MAX_CSI_LEN {
                        self.csi_buf.push(b);
                    } else {
                        // Overrun — give up on this sequence.
                        self.state = F3State::Idle;
                        self.csi_buf.clear();
                    }
                }
            }
        }
        events
    }
}

impl Default for F3Observer {
    fn default() -> Self {
        Self::new()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum F3Kind {
    Press,
    Repeat,
    Release,
}

/// CSI terminator bytes per ECMA-48 (`0x40..=0x7E`, "Final Byte"). We
/// only care about a couple in practice (`u`, `~`) but accepting the
/// full range keeps misbehaving terminals from getting us stuck inside
/// `F3State::Csi`.
fn is_csi_terminator(b: u8) -> bool {
    matches!(b, 0x40..=0x7E)
}

/// Decide whether a parameter-bytes payload (e.g. `13;1:3`) plus a
/// terminator (e.g. `~` or `u`) is an F3 event, and which kind.
/// Returns `None` for anything that isn't F3 — different keycodes,
/// non-keyboard CSI sequences, malformed payloads.
fn parse_f3_csi(params: &[u8], terminator: u8) -> Option<F3Kind> {
    if terminator != b'u' && terminator != b'~' {
        return None;
    }
    let payload = std::str::from_utf8(params).ok()?;
    let mut parts = payload.split(';');
    let keycode_str = parts.next()?;

    // First param is the keycode. F3 keycodes:
    //   - `\x1b[13~` (CSI tilde, legacy F3 — see Linux/xterm function-key map)
    //   - `\x1b[13u` (CSI u, F3 with disambiguation but no functional encoding)
    //   - `\x1b[57346u` (CSI u, F3 with kitty functional encoding)
    let is_f3 = match terminator {
        b'~' => keycode_str == "13",
        b'u' => keycode_str == "13" || keycode_str == "57346",
        _ => false,
    };
    if !is_f3 {
        return None;
    }

    // Second param is `modifier[:event-type[:text]]`. Event-type defaults
    // to 1 (press) when omitted.
    let event_type = parts
        .next()
        .and_then(|modifier_field| modifier_field.split(':').nth(1))
        .and_then(|et| et.parse::<u32>().ok())
        .unwrap_or(1);

    match event_type {
        2 => Some(F3Kind::Repeat),
        3 => Some(F3Kind::Release),
        _ => Some(F3Kind::Press),
    }
}

/// Where to send synthetic input bytes generated by an `InteractiveHooks`
/// implementation (voice transcript, drag-drop paths, etc.).
///
/// Two impls ship in-tree:
/// * `NativePtyProcessSink` — wraps a `&NativePtyProcess` and forwards
///   to `write_impl`. Used by the direct local-PTY pump in `runner.rs`.
/// * A TCP-backed sink in `daemon::attach` — sends `WorkerClientMessage::Input`
///   frames to the daemon worker. Used by centralized-mode foreground
///   attach so voice + DnD reach the daemon-owned PTY just like keystrokes.
///
/// The `submit` flag mirrors `NativePtyProcess::write_impl`'s second
/// argument: `true` ends the input with a synthetic Enter so the agent
/// processes the buffer, `false` leaves the cursor mid-line so a human
/// can edit before submitting.
pub trait PtyInputSink {
    fn write_input(&mut self, bytes: &[u8], submit: bool) -> io::Result<()>;
}

/// Adapter: a `PtyInputSink` over a local `NativePtyProcess`.
pub struct NativePtyProcessSink<'a> {
    process: &'a NativePtyProcess,
}

impl<'a> NativePtyProcessSink<'a> {
    pub fn new(process: &'a NativePtyProcess) -> Self {
        Self { process }
    }
}

impl<'a> PtyInputSink for NativePtyProcessSink<'a> {
    fn write_input(&mut self, bytes: &[u8], submit: bool) -> io::Result<()> {
        self.process
            .write_impl(bytes, submit)
            .map_err(|err| io::Error::other(err.to_string()))
    }
}

pub trait InteractiveHooks {
    fn intercept_f3(&self) -> bool {
        false
    }

    fn on_f3_press(&mut self, _sink: &mut dyn PtyInputSink) -> io::Result<()> {
        Ok(())
    }

    fn on_f3_release(&mut self, _sink: &mut dyn PtyInputSink) -> io::Result<()> {
        Ok(())
    }

    fn on_tick(&mut self, _sink: &mut dyn PtyInputSink) -> io::Result<()> {
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
    run_raw_pty_pump_with_extra_rx_verbose(
        process,
        interrupted,
        hooks,
        stdin_source,
        extra_rx,
        false,
    )
}

/// Production pump entry with optional clud-level diagnostics.
pub fn run_raw_pty_pump_with_extra_rx_verbose<H, R>(
    process: &NativePtyProcess,
    interrupted: &AtomicBool,
    hooks: &mut H,
    stdin_source: R,
    extra_rx: Option<std::sync::mpsc::Receiver<Vec<u8>>>,
    verbose: bool,
) -> i32
where
    H: InteractiveHooks,
    R: std::io::Read + Send + 'static,
{
    run_raw_pty_pump_with_extra_rx_verbose_and_graphics(
        process,
        interrupted,
        hooks,
        stdin_source,
        extra_rx,
        verbose,
        None,
    )
}

/// Production pump entry with optional clud-level diagnostics and a PTY
/// header renderer that can redraw/reserve rows after terminal resizes.
pub fn run_raw_pty_pump_with_extra_rx_verbose_and_graphics<H, R>(
    process: &NativePtyProcess,
    interrupted: &AtomicBool,
    hooks: &mut H,
    stdin_source: R,
    extra_rx: Option<std::sync::mpsc::Receiver<Vec<u8>>>,
    verbose: bool,
    graphics: Option<GraphicsConfig>,
) -> i32
where
    H: InteractiveHooks,
    R: std::io::Read + Send + 'static,
{
    let (resize_tx, resize_rx) = std::sync::mpsc::channel::<(u16, u16)>();
    spawn_os_resize_watcher(resize_tx);
    run_raw_pty_pump_full_verbose(
        process,
        interrupted,
        hooks,
        stdin_source,
        resize_rx,
        extra_rx,
        PumpOptions { verbose, graphics },
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
    run_raw_pty_pump_full_verbose(
        process,
        interrupted,
        hooks,
        stdin_source,
        resize_rx,
        extra_rx,
        PumpOptions::default(),
    )
}

#[derive(Default)]
struct PumpOptions {
    verbose: bool,
    graphics: Option<GraphicsConfig>,
}

fn run_raw_pty_pump_full_verbose<H, R>(
    process: &NativePtyProcess,
    interrupted: &AtomicBool,
    hooks: &mut H,
    stdin_source: R,
    resize_rx: std::sync::mpsc::Receiver<(u16, u16)>,
    extra_rx: Option<std::sync::mpsc::Receiver<Vec<u8>>>,
    options: PumpOptions,
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
    // Issue #188: when an `extra_rx` is wired and we're on Windows with
    // an interactive real-stdin console, the `console_input`
    // `ReadConsoleInputW` reader is feeding that channel and is the
    // authoritative source of console bytes (including Shift+Enter →
    // `\n`). Spawning the byte-stream stdin reader below would race
    // with it on the same STDIN console queue — `ReadFile` strips
    // modifier state, so a stolen Shift+Enter surfaces as `\r`. Skip
    // it in that exact case so `console_input` is the sole consumer.
    let spawn_byte_stream_stdin_reader =
        should_spawn_byte_stream_stdin_reader(interactive_real_stdin, extra_rx.is_some());
    if options.verbose {
        verbose_log::log(format_args!(
            "[clud] pty pump: start interactive_stdin={} normalize_console_stdin={} \
             spawn_byte_stream_stdin_reader={}",
            interactive_real_stdin, normalize_console_stdin, spawn_byte_stream_stdin_reader
        ));
    }

    // Detached reader: pumps `stdin_source` → channel until EOF or error.
    // Detached (not joined) so a blocked `read()` on real stdin doesn't
    // wedge shutdown when the child exits — the process is terminating
    // anyway. See Step 12.
    if spawn_byte_stream_stdin_reader {
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
    } else {
        // `console_input` is the sole consumer via `extra_rx`. Drop the
        // unused stdin source and channel sender so the corresponding
        // `stdin_rx.try_recv()` in the main loop returns `Empty`
        // immediately (no thread will ever send on it).
        drop(stdin_source);
        drop(stdin_tx);
    }

    let mut observer = F3Observer::new();
    // Issue #63 / #79: bracketed-paste passes through the PTY pump as
    // raw bytes. When the user drags a file onto the terminal, the
    // terminal emits `\x1b[200~ <path-shaped string> \x1b[201~`. We
    // normalize that path BEFORE forwarding so all backends see a
    // canonical form, regardless of which terminal produced the drop.
    let mut paste = BracketedPasteNormalizer::new();
    // Strip OSC 0/2 (window-title) sequences from the child's output
    // before they reach the terminal. Otherwise the backend's TUI
    // (and any tool subprocess it spawns) overwrites the title that
    // `console_title::set_for_current_cwd` stamped at startup.
    // `main.rs` set `process.set_echo(false)` so the library's
    // built-in stdout writer is silent — we own forwarding now.
    let mut osc_strip = OscTitleStripper::new();

    loop {
        // Drain a chunk of child output, run it through the OSC title
        // stripper, and write the result to our stdout. The library
        // also keeps a copy in its `chunks` queue (used by callers like
        // capture/replay) — that copy still contains OSC 0/2, but only
        // the bytes we write here actually reach the user's terminal.
        match process.read_chunk_impl(Some(0.01)) {
            Ok(Some(chunk)) => {
                let filtered = osc_strip.process(&chunk);
                if !filtered.is_empty() {
                    use std::io::Write;
                    let mut out = io::stdout().lock();
                    let _ = out.write_all(&filtered);
                    let _ = out.flush();
                }
            }
            Ok(None) => {}
            Err(_) => return reap_pty_exit(process),
        }

        // Drain resize events — always before stdin so a late-arriving
        // resize doesn't wait on a chunk of typing to unblock the loop.
        while let Ok((rows, cols)) = resize_rx.try_recv() {
            let pty_rows = options
                .graphics
                .as_ref()
                .map(|config| {
                    redraw_graphics_header_for_resize(config, rows, cols, options.verbose)
                })
                .unwrap_or(rows);
            if let Err(err) = resize_pty(process, pty_rows, cols) {
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
        //
        // The 0x03 byte check is required on Windows: when the
        // `console_input` reader (issue #141 / PR #144) is active, it
        // turns off `ENABLE_PROCESSED_INPUT` so the OS no longer fires
        // a `CTRL_C_EVENT` for Ctrl-C. The press arrives instead as a
        // KEY_EVENT whose translated 0x03 byte is delivered via this
        // channel — without the check, clud forwards it to the child
        // but never observes the interrupt itself.
        if let Some(ref rx) = extra_rx {
            if let Ok(chunk) = rx.try_recv() {
                // Unlike stdin_rx, extra_rx is by construction always
                // user-driven (keyboard via console_input_rx on Windows,
                // or OLE drag-drop callback) — never a piped test
                // fixture — so we don't need the `interrupt_on_ctrl_c_byte`
                // gate that skips 0x03 detection on piped stdin.
                let requested_interrupt = stdin_chunk_requests_interrupt(&chunk);
                if let Err(err) = process.write_impl(&chunk, false) {
                    eprintln!(
                        "[clud] warning: failed to forward dropped paths to pty: {}",
                        err
                    );
                }
                if requested_interrupt {
                    if options.verbose {
                        verbose_log::log("[clud] pty pump: interrupt via extra_rx Ctrl+C byte");
                    }
                    return interrupt_pty_process(process, options.verbose);
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
                let events = observer.observe(&chunk);
                let mut sink = NativePtyProcessSink::new(process);
                for _ in 0..events.presses {
                    if let Err(err) = hooks.on_f3_press(&mut sink) {
                        eprintln!("[clud] warning: voice F3 press hook failed: {}", err);
                    }
                }
                for _ in 0..events.releases {
                    if let Err(err) = hooks.on_f3_release(&mut sink) {
                        eprintln!("[clud] warning: voice F3 release hook failed: {}", err);
                    }
                }
            }

            if requested_interrupt || interrupted.load(Ordering::SeqCst) {
                if options.verbose {
                    let source = if requested_interrupt {
                        "stdin Ctrl+C byte"
                    } else {
                        "interrupt flag"
                    };
                    verbose_log::log(format_args!("[clud] pty pump: interrupt via {source}"));
                }
                return interrupt_pty_process(process, options.verbose);
            }
        }

        {
            let mut sink = NativePtyProcessSink::new(process);
            if let Err(err) = hooks.on_tick(&mut sink) {
                eprintln!("[clud] warning: interactive hook tick failed: {}", err);
            }
        }

        if let Ok(Some(code)) =
            running_process::pty::poll_pty_process(&process.handles, &process.returncode)
        {
            if options.verbose {
                verbose_log::log(format_args!("[clud] pty pump: child exited code {code}"));
            }
            return code;
        }

        if interrupted.load(Ordering::SeqCst) {
            if options.verbose {
                verbose_log::log("[clud] pty pump: interrupt flag observed");
            }
            return interrupt_pty_process(process, options.verbose);
        }
    }
}

fn redraw_graphics_header_for_resize(
    config: &GraphicsConfig,
    terminal_rows: u16,
    terminal_cols: u16,
    verbose: bool,
) -> u16 {
    match crate::graphics::render_header(config, terminal_rows, terminal_cols) {
        Ok(Some(header)) => {
            use std::io::Write;
            let mut out = io::stdout().lock();
            let _ = out.write_all(&header.bytes);
            let _ = out.flush();
            header.text_rows
        }
        Ok(None) => {
            use std::io::Write;
            let restore = crate::graphics::reset_layout_bytes(terminal_rows, true);
            let mut out = io::stdout().lock();
            let _ = out.write_all(&restore);
            let _ = out.flush();
            terminal_rows
        }
        Err(err) => {
            if verbose {
                verbose_log::log(format_args!("[clud] graphics: resize redraw failed: {err}"));
            }
            use std::io::Write;
            let restore = crate::graphics::reset_layout_bytes(terminal_rows, true);
            let mut out = io::stdout().lock();
            let _ = out.write_all(&restore);
            let _ = out.flush();
            terminal_rows
        }
    }
}

fn stdin_source_is_real_stdin<R: 'static>() -> bool {
    std::any::TypeId::of::<R>() == std::any::TypeId::of::<std::io::Stdin>()
}

fn should_normalize_interactive_console_stdin(interactive_real_stdin: bool) -> bool {
    cfg!(windows) && interactive_real_stdin
}

/// Decide whether the PTY pump should spawn its byte-stream stdin
/// reader thread (the one that calls `io::stdin().read(...)`).
///
/// Issue #188: On Windows with an interactive real-stdin console and an
/// `extra_rx` already wired, the `console_input::ReadConsoleInputW`
/// worker is the authoritative source of console bytes — including the
/// modifier-aware Shift+Enter → `\n` translation. Spawning the
/// byte-stream reader in that case would race with `ReadConsoleInputW`
/// on the same STDIN console queue. `ReadFile` strips modifier state
/// before producing bytes, so a stolen Shift+Enter surfaces as `\r`
/// instead of `\n`. Returning `false` keeps `console_input` as the
/// sole consumer.
///
/// Every other configuration — POSIX, piped stdin, no `extra_rx` —
/// keeps the byte-stream reader so existing behavior (including
/// `echo "prompt" | clud` and POSIX interactive use) is unchanged.
fn should_spawn_byte_stream_stdin_reader(interactive_real_stdin: bool, has_extra_rx: bool) -> bool {
    !(cfg!(windows) && interactive_real_stdin && has_extra_rx)
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
fn interrupt_pty_process(process: &NativePtyProcess, verbose: bool) -> i32 {
    #[cfg(windows)]
    {
        // Closing the PTY triggers ConPTY's CTRL_CLOSE_EVENT path and
        // tears the child down without writing a second 0x03 byte.
        let _ = process.close_impl();
        if verbose {
            verbose_log::log("[clud] interrupted via Ctrl+C (pty)");
        }
        130
    }
    #[cfg(not(windows))]
    {
        // Belt-and-braces: portable-pty's `send_interrupt` queries
        // `tcgetpgrp(master_fd)` to find the FG pgroup and signals it.
        // If that query returns None (no controlling-terminal coupling
        // ever established, or the slave already lost FG), the library
        // falls back to writing a raw 0x03 byte to the master — which
        // only fires SIGINT if the slave still has ISIG set and the
        // child is actively reading. Tests that drive a sleep-only
        // child (issue #159) fail under that fallback because nothing
        // converts 0x03 to a signal. Send through both paths and also
        // signal the child's PID tree directly so all three vectors are
        // covered. Then `close_impl` ensures the master is torn down so
        // the pump loop doesn't keep polling a half-dead PTY.
        let _ = process.send_interrupt_impl();
        let tree_signal_result = process.terminate_tree_impl();
        let _ = process.wait_impl(Some(2.0));
        let _ = process.close_impl();
        if verbose {
            verbose_log::log("[clud] interrupted via Ctrl+C (pty)");
            if let Err(err) = tree_signal_result {
                verbose_log::log(format_args!(
                    "[clud] pty interrupt: tree-signal fallback failed: {err}"
                ));
            }
        }
        // Match the Windows branch above: always report SIGINT's
        // shell-convention 130 when clud itself handled the Ctrl-C.
        // The child's actual exit code is observable via the wait_impl
        // call above (stored in returncode for diagnostics); we just
        // don't propagate it, because the contract is "user pressed
        // Ctrl-C → exit 130" regardless of how the child shut down.
        130
    }
}

#[cfg(test)]
#[path = "session_tests.rs"]
mod tests;
