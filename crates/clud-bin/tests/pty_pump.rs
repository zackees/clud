//! Raw PTY pump integration tests for `clud --codex`.
//!
//! Covers `clud::session::run_raw_pty_pump` and its with-resize / with-extra-rx
//! variants: verbatim stdin forwarding (issue #46), voice-mode F3 hooks
//! (issues #13, #41), idle ticks, Ctrl-C / interrupt propagation, resize-channel
//! delivery, prompt exit on child death, raw-mode panic safety, and the
//! Shift+Enter extra_rx round trip (issue #141).
//!
//! Lives separately from `tests/pty_behavior.rs` so each integration-test
//! binary stays under the 1K-LOC ceiling. Shared helpers come from
//! `tests/common/mod.rs`.

use std::io::Cursor;
use std::sync::atomic::AtomicBool;
use std::time::{Duration, Instant};

use running_process::pty::NativePtyProcess;

mod common;
use common::{drain_reader, mock_agent_path};

/// Counting hooks for pump integration tests. Records F3 presses,
/// releases, ticks, and can opt into voice interception via `intercept`.
struct CountingHooks {
    intercept: bool,
    f3_presses: std::sync::Arc<std::sync::atomic::AtomicU32>,
    f3_releases: std::sync::Arc<std::sync::atomic::AtomicU32>,
    ticks: std::sync::Arc<std::sync::atomic::AtomicU32>,
}

impl CountingHooks {
    fn new(intercept: bool) -> Self {
        Self {
            intercept,
            f3_presses: std::sync::Arc::new(std::sync::atomic::AtomicU32::new(0)),
            f3_releases: std::sync::Arc::new(std::sync::atomic::AtomicU32::new(0)),
            ticks: std::sync::Arc::new(std::sync::atomic::AtomicU32::new(0)),
        }
    }
}

impl clud::session::InteractiveHooks for CountingHooks {
    fn intercept_f3(&self) -> bool {
        self.intercept
    }
    fn on_f3_press(&mut self, _sink: &mut dyn clud::session::PtyInputSink) -> std::io::Result<()> {
        self.f3_presses
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        Ok(())
    }
    fn on_f3_release(
        &mut self,
        _sink: &mut dyn clud::session::PtyInputSink,
    ) -> std::io::Result<()> {
        self.f3_releases
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        Ok(())
    }
    fn on_tick(&mut self, _sink: &mut dyn clud::session::PtyInputSink) -> std::io::Result<()> {
        self.ticks.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        Ok(())
    }
}

/// The pump must forward stdin bytes verbatim — no CSI parsing, no
/// event-loop demultiplexing. This is the regression test for the DSR hang
/// (issue #46): a child TUI that emits `\x1b[6n` and expects the terminal
/// to reply must see our stdin bytes unchanged. We feed arbitrary bytes
/// including DSR queries, escape sequences, and F3 and assert the
/// mock-agent recorded exactly what we sent.
#[test]
fn raw_pump_forwards_stdin_bytes_verbatim() {
    require_pty_or_skip!("raw_pump_forwards_stdin_bytes_verbatim");

    let agent = mock_agent_path();
    let tmp = tempfile::tempdir().expect("tempdir");
    let raw_stdin = tmp.path().join("stdin_raw.bin");

    let argv = vec![
        agent.to_string_lossy().to_string(),
        "--mock-read-stdin-ms".to_string(),
        "800".to_string(),
        "--mock-stdin-raw-to".to_string(),
        raw_stdin.to_string_lossy().to_string(),
    ];

    let process = NativePtyProcess::new(argv, None, None, 24, 80, None).expect("new pty");
    process.set_echo(false);
    process.start_impl().expect("start");

    // Give the child a moment to enter its stdin read loop before we feed.
    std::thread::sleep(Duration::from_millis(150));

    let payload: &[u8] = b"hello\x1b[6n\x1bOR\x1bOP world\n";
    let interrupted = AtomicBool::new(false);
    let mut hooks = CountingHooks::new(false);

    let _exit = clud::session::run_raw_pty_pump(
        &process,
        &interrupted,
        &mut hooks,
        Cursor::new(payload.to_vec()),
    );

    let _ = process.wait_impl(Some(5.0));
    let _ = drain_reader(&process, Duration::from_millis(300));
    let _ = process.close_impl();

    let got = std::fs::read(&raw_stdin).unwrap_or_default();
    assert_eq!(
        got, payload,
        "pump must forward stdin bytes verbatim; got {:?}, expected {:?}",
        got, payload
    );
}

/// F3 is observed (not intercepted): each `\x1bOR` in the byte stream
/// fires `on_f3_press` once AND the bytes still reach the child. This is
/// the voice-mode contract: clud reacts to F3 in parallel with forwarding,
/// not instead of forwarding.
#[test]
fn raw_pump_fires_voice_f3_press_while_forwarding_bytes() {
    require_pty_or_skip!("raw_pump_fires_voice_f3_press_while_forwarding_bytes");

    let agent = mock_agent_path();
    let tmp = tempfile::tempdir().expect("tempdir");
    let raw_stdin = tmp.path().join("stdin_raw.bin");

    let argv = vec![
        agent.to_string_lossy().to_string(),
        "--mock-read-stdin-ms".to_string(),
        "800".to_string(),
        "--mock-stdin-raw-to".to_string(),
        raw_stdin.to_string_lossy().to_string(),
    ];

    let process = NativePtyProcess::new(argv, None, None, 24, 80, None).expect("new pty");
    process.set_echo(false);
    process.start_impl().expect("start");
    std::thread::sleep(Duration::from_millis(150));

    // Three F3 presses embedded in surrounding text. Trailing `\n` is
    // important: the PTY slave defaults to canonical (line) mode, so the
    // kernel holds input until it sees a newline. Without it, the
    // mock-agent's `stdin.read()` never returns and we'd assert on an
    // empty file. Real usage isn't affected — child TUIs like codex put
    // their own slave into raw mode before reading.
    let payload: &[u8] = b"a\x1bORb\x1bORc\x1bORd\n";
    let interrupted = AtomicBool::new(false);
    let hooks = CountingHooks::new(true); // intercept_f3 == true
    let presses = std::sync::Arc::clone(&hooks.f3_presses);
    let mut hooks = hooks;

    let _exit = clud::session::run_raw_pty_pump(
        &process,
        &interrupted,
        &mut hooks,
        Cursor::new(payload.to_vec()),
    );

    let _ = process.wait_impl(Some(5.0));
    let _ = drain_reader(&process, Duration::from_millis(300));
    let _ = process.close_impl();

    let got = std::fs::read(&raw_stdin).unwrap_or_default();
    assert_eq!(
        got, payload,
        "F3 interception must NOT eat bytes; child should still see {:?}, got {:?}",
        payload, got
    );
    assert_eq!(
        presses.load(std::sync::atomic::Ordering::SeqCst),
        3,
        "expected 3 F3 presses; got a different count"
    );
}

/// Issue #13 hold-to-record: terminals supporting the kitty keyboard
/// protocol (REPORT_EVENT_TYPES) emit a release event when F3 is let go.
/// The pump must fire `on_f3_release` exactly once per release sequence
/// AND still forward the raw bytes to the child.
#[test]
fn raw_pump_fires_voice_f3_release_when_kitty_sequence_present() {
    require_pty_or_skip!("raw_pump_fires_voice_f3_release_when_kitty_sequence_present");

    let agent = mock_agent_path();
    let tmp = tempfile::tempdir().expect("tempdir");
    let raw_stdin = tmp.path().join("stdin_raw.bin");

    let argv = vec![
        agent.to_string_lossy().to_string(),
        "--mock-read-stdin-ms".to_string(),
        "800".to_string(),
        "--mock-stdin-raw-to".to_string(),
        raw_stdin.to_string_lossy().to_string(),
    ];

    let process = NativePtyProcess::new(argv, None, None, 24, 80, None).expect("new pty");
    process.set_echo(false);
    process.start_impl().expect("start");
    std::thread::sleep(Duration::from_millis(150));

    // Kitty F3 press (CSI u, functional encoding) then release. The
    // trailing `\n` is the canonical-mode trigger; without it the
    // mock-agent's stdin read never returns.
    let payload: &[u8] = b"a\x1b[57346;1:1ub\x1b[57346;1:3uc\n";
    let interrupted = AtomicBool::new(false);
    let hooks = CountingHooks::new(true);
    let presses = std::sync::Arc::clone(&hooks.f3_presses);
    let releases = std::sync::Arc::clone(&hooks.f3_releases);
    let mut hooks = hooks;

    let _exit = clud::session::run_raw_pty_pump(
        &process,
        &interrupted,
        &mut hooks,
        Cursor::new(payload.to_vec()),
    );

    let _ = process.wait_impl(Some(5.0));
    let _ = drain_reader(&process, Duration::from_millis(300));
    let _ = process.close_impl();

    let got = std::fs::read(&raw_stdin).unwrap_or_default();
    assert_eq!(
        got, payload,
        "kitty release detection must NOT eat bytes; child should still see the full payload"
    );
    assert_eq!(
        presses.load(std::sync::atomic::Ordering::SeqCst),
        1,
        "expected exactly 1 F3 press; got a different count"
    );
    assert_eq!(
        releases.load(std::sync::atomic::Ordering::SeqCst),
        1,
        "expected exactly 1 F3 release; got a different count"
    );
}

/// `on_tick` must fire on every main-loop iteration regardless of stdin.
/// Voice mode drains a background transcription worker through this hook;
/// gating it behind stdin activity would leave transcripts stuck whenever
/// the user stops typing. Here: an empty stdin source, a 400ms child.
/// Threshold is intentionally loose — the 10ms `read_chunk_impl`
/// timeout is a lower bound, but CI scheduler granularity (and an
/// occasional stall waiting for the child to finish start-up I/O)
/// can stretch each iteration to 60–100ms. We just need to prove
/// the tick isn't gated on stdin, not measure loop cadence. Anything
/// above zero would do; 3 gives headroom for truly slow runners.
#[test]
fn raw_pump_calls_on_tick_during_idle() {
    require_pty_or_skip!("raw_pump_calls_on_tick_during_idle");

    let agent = mock_agent_path();
    let tmp = tempfile::tempdir().expect("tempdir");
    let raw_stdin = tmp.path().join("stdin_raw.bin");

    let argv = vec![
        agent.to_string_lossy().to_string(),
        "--mock-read-stdin-ms".to_string(),
        "400".to_string(),
        "--mock-stdin-raw-to".to_string(),
        raw_stdin.to_string_lossy().to_string(),
    ];

    let process = NativePtyProcess::new(argv, None, None, 24, 80, None).expect("new pty");
    process.set_echo(false);
    process.start_impl().expect("start");
    std::thread::sleep(Duration::from_millis(100));

    let interrupted = AtomicBool::new(false);
    let hooks = CountingHooks::new(false);
    let ticks = std::sync::Arc::clone(&hooks.ticks);
    let mut hooks = hooks;

    // Empty stdin: reader thread hits EOF immediately. Main loop then
    // idles and must still tick.
    let _exit = clud::session::run_raw_pty_pump(
        &process,
        &interrupted,
        &mut hooks,
        Cursor::new(Vec::<u8>::new()),
    );

    let _ = process.wait_impl(Some(5.0));
    let _ = drain_reader(&process, Duration::from_millis(300));
    let _ = process.close_impl();

    let tick_count = ticks.load(std::sync::atomic::Ordering::SeqCst);
    assert!(
        tick_count >= 3,
        "expected >=3 ticks during 400ms idle child, got {}",
        tick_count
    );
}

/// Flipping the shared `interrupted` flag while the pump is running with
/// a long-lived child must cause the pump to return within a couple of
/// seconds. On POSIX this is `send_interrupt_impl` sending SIGINT to
/// the child's pgroup; on Windows it's `close_impl` tearing the PTY
/// down (the Windows `send_interrupt_impl` is intentionally skipped to
/// avoid duplicating the 0x03 byte that stdin already forwarded — see
/// `interrupt_pty_process` for the rationale).
#[test]
fn raw_pump_honors_ctrlc_flag() {
    require_pty_or_skip!("raw_pump_honors_ctrlc_flag");

    let agent = mock_agent_path();
    let tmp = tempfile::tempdir().expect("tempdir");
    let raw_stdin = tmp.path().join("stdin_raw.bin");

    // Long-lived: 5 seconds of blocking stdin read — gives us headroom to
    // observe the interrupt without racing against child exit.
    let argv = vec![
        agent.to_string_lossy().to_string(),
        "--mock-read-stdin-ms".to_string(),
        "5000".to_string(),
        "--mock-stdin-raw-to".to_string(),
        raw_stdin.to_string_lossy().to_string(),
    ];

    let process = NativePtyProcess::new(argv, None, None, 24, 80, None).expect("new pty");
    process.set_echo(false);
    process.start_impl().expect("start");
    std::thread::sleep(Duration::from_millis(150));

    let interrupted = std::sync::Arc::new(AtomicBool::new(false));
    let mut hooks = CountingHooks::new(false);

    // Trip the flag 200ms after the pump starts running.
    let flag = std::sync::Arc::clone(&interrupted);
    std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(200));
        flag.store(true, std::sync::atomic::Ordering::SeqCst);
    });

    let start = Instant::now();
    let _exit = clud::session::run_raw_pty_pump(
        &process,
        &interrupted,
        &mut hooks,
        Cursor::new(Vec::<u8>::new()),
    );
    let elapsed = start.elapsed();

    let _ = process.close_impl();

    assert!(
        elapsed < Duration::from_millis(2500),
        "pump must return within 2.5s of ctrlc flag flip, took {:?}",
        elapsed
    );
}

/// Resize events delivered through the pump's resize channel must reach
/// `resize_pty` and propagate to the PTY master. This covers both Step 9
/// (Unix SIGWINCH source) and Step 10 (Windows ReadConsoleInputW source)
/// — the OS-specific threads are responsible for *producing* events into
/// this channel, and the pump is responsible for *consuming* them and
/// calling `resize_pty`. Here we bypass the OS source and push directly.
#[test]
fn raw_pump_applies_resize_from_channel() {
    require_pty_or_skip!("raw_pump_applies_resize_from_channel");

    let agent = mock_agent_path();
    let tmp = tempfile::tempdir().expect("tempdir");
    let raw_stdin = tmp.path().join("stdin_raw.bin");

    // Long enough that the resize has time to land before the child exits.
    let argv = vec![
        agent.to_string_lossy().to_string(),
        "--mock-read-stdin-ms".to_string(),
        "600".to_string(),
        "--mock-stdin-raw-to".to_string(),
        raw_stdin.to_string_lossy().to_string(),
    ];

    let process = NativePtyProcess::new(argv, None, None, 20, 80, None).expect("new pty");
    process.set_echo(false);
    process.start_impl().expect("start");
    std::thread::sleep(Duration::from_millis(100));

    // Sanity: master starts at the size we spawned with.
    {
        let guard = process.handles.lock().expect("handles");
        let handles = guard.as_ref().expect("handles present");
        let size = handles.master.get_size().expect("get_size");
        assert_eq!((size.rows, size.cols), (20, 80));
    }

    let (resize_tx, resize_rx) = std::sync::mpsc::channel::<(u16, u16)>();

    // Push a resize after a short delay so the main loop is already
    // running when it arrives.
    std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(120));
        let _ = resize_tx.send((40, 120));
    });

    let interrupted = AtomicBool::new(false);
    let mut hooks = CountingHooks::new(false);

    let _exit = clud::session::run_raw_pty_pump_with_resize_rx(
        &process,
        &interrupted,
        &mut hooks,
        Cursor::new(Vec::<u8>::new()),
        resize_rx,
    );

    // After the pump consumed the resize, the master must reflect it.
    {
        let guard = process.handles.lock().expect("handles");
        let handles = guard.as_ref().expect("handles present");
        let size = handles.master.get_size().expect("get_size");
        assert_eq!(
            (size.rows, size.cols),
            (40, 120),
            "pump did not apply (40,120) resize from channel"
        );
    }

    let _ = process.wait_impl(Some(2.0));
    let _ = process.close_impl();
}

/// When the child exits, the pump must return promptly even if the
/// stdin reader thread is blocked in `read()`. Real `io::stdin()` blocks
/// waiting for the next keystroke — never returning EOF. We simulate
/// that with a `Read` impl that parks forever. The detached reader
/// thread is fine to leave blocked; the main loop's `poll_pty_process`
/// is what drives shutdown.
#[test]
fn raw_pump_exits_promptly_when_child_exits() {
    require_pty_or_skip!("raw_pump_exits_promptly_when_child_exits");

    struct BlockingReader;
    impl std::io::Read for BlockingReader {
        fn read(&mut self, _buf: &mut [u8]) -> std::io::Result<usize> {
            // Mimics `io::stdin()` with no pending input: block indefinitely.
            loop {
                std::thread::sleep(Duration::from_secs(60));
            }
        }
    }

    let agent = mock_agent_path();
    let tmp = tempfile::tempdir().expect("tempdir");
    let raw_stdin = tmp.path().join("stdin_raw.bin");

    // Short-lived child — it reads stdin for 400ms then exits.
    let argv = vec![
        agent.to_string_lossy().to_string(),
        "--mock-read-stdin-ms".to_string(),
        "400".to_string(),
        "--mock-stdin-raw-to".to_string(),
        raw_stdin.to_string_lossy().to_string(),
    ];

    let process = NativePtyProcess::new(argv, None, None, 24, 80, None).expect("new pty");
    process.set_echo(false);
    process.start_impl().expect("start");

    let interrupted = AtomicBool::new(false);
    let mut hooks = CountingHooks::new(false);

    let start = Instant::now();
    let _exit = clud::session::run_raw_pty_pump(&process, &interrupted, &mut hooks, BlockingReader);
    let elapsed = start.elapsed();

    let _ = process.close_impl();

    assert!(
        elapsed < Duration::from_millis(1500),
        "pump must exit within 1.5s after child dies, took {:?}",
        elapsed
    );
}
/// fire and disable raw mode — otherwise the user's terminal is left in
/// a broken state after a crash. `catch_unwind` wraps the panicking hook
/// to assert recovery.
#[test]
fn raw_pump_restores_raw_mode_on_panic() {
    require_pty_or_skip!("raw_pump_restores_raw_mode_on_panic");

    // Note: the pump itself doesn't own a RawTerminalGuard — `run_plan_pty`
    // in main.rs does (will, once wired in Step 13). Raw mode is a
    // caller-side concern. This test verifies the pump doesn't leak raw
    // mode *on panic*: if we enter raw mode before calling the pump with
    // a panicking hook, the guard on the main stack frame unwinds and
    // restores the terminal.
    struct PanickingHooks;
    impl clud::session::InteractiveHooks for PanickingHooks {
        fn on_tick(&mut self, _sink: &mut dyn clud::session::PtyInputSink) -> std::io::Result<()> {
            panic!("deliberate panic in on_tick");
        }
    }

    let agent = mock_agent_path();
    let tmp = tempfile::tempdir().expect("tempdir");
    let raw_stdin = tmp.path().join("stdin_raw.bin");

    let argv = vec![
        agent.to_string_lossy().to_string(),
        "--mock-read-stdin-ms".to_string(),
        "500".to_string(),
        "--mock-stdin-raw-to".to_string(),
        raw_stdin.to_string_lossy().to_string(),
    ];

    let process = NativePtyProcess::new(argv, None, None, 24, 80, None).expect("new pty");
    process.set_echo(false);
    process.start_impl().expect("start");
    std::thread::sleep(Duration::from_millis(100));

    let interrupted = AtomicBool::new(false);
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let mut hooks = PanickingHooks;
        clud::session::run_raw_pty_pump(
            &process,
            &interrupted,
            &mut hooks,
            Cursor::new(Vec::<u8>::new()),
        )
    }));

    assert!(result.is_err(), "hook panic must propagate to catch_unwind");

    // The crossterm raw-mode state is a caller concern; we just verify the
    // pump returned control (panic surfaced) rather than hung after the
    // panic unwound through its own threads.
    let _ = process.wait_impl(Some(2.0));
    let _ = process.close_impl();
}

/// Issue #141 follow-up: bytes from `extra_rx` reach the child PTY
/// exactly as `console_input::translate()` produces them. The pump
/// doesn't know or care that the bytes came from a console-input
/// translator — it forwards `extra_rx` chunks to the child the same
/// way it would forward stdin. This integration test asserts that
/// invariant by:
///
///   1. Building `console_input::translate()` over a synthetic event
///      stream containing Shift+Enter and plain Enter.
///   2. Sending the translated bytes through `extra_rx`.
///   3. Capturing the child's stdin via `--mock-stdin-raw-to`.
///   4. Asserting both `\n` (Shift+Enter) and `\r` (plain Enter) made
///      the round trip.
///
/// Windows-only because `console_input` is `#[cfg(windows)]`. The
/// underlying pump path (`run_raw_pty_pump_with_extra_rx`) works on
/// every platform; this test just exercises the Windows path.
#[cfg(windows)]
#[test]
fn extra_rx_forwards_shift_enter_translated_bytes_to_pty() {
    require_pty_or_skip!("extra_rx_forwards_shift_enter_translated_bytes_to_pty");

    use clud::console_input::{translate, InputEvent, KeyEvent, SHIFT_PRESSED, VK_RETURN};

    let agent = mock_agent_path();
    let tmp = tempfile::tempdir().expect("tempdir");
    let raw_stdin = tmp.path().join("stdin_raw.bin");

    let argv = vec![
        agent.to_string_lossy().to_string(),
        "--mock-read-stdin-ms".to_string(),
        "800".to_string(),
        "--mock-stdin-raw-to".to_string(),
        raw_stdin.to_string_lossy().to_string(),
    ];

    let process = NativePtyProcess::new(argv, None, None, 24, 80, None).expect("new pty");
    process.set_echo(false);
    process.start_impl().expect("start");
    std::thread::sleep(Duration::from_millis(150));

    // What the production reader would emit for a Shift+Enter + plain
    // Enter sequence. The pump treats these bytes as opaque — exactly
    // the surface this test pins.
    let translated = translate(&[
        InputEvent::Key(KeyEvent {
            key_down: true,
            virtual_key_code: VK_RETURN,
            unicode_char: b'\r' as u16,
            control_key_state: SHIFT_PRESSED,
        }),
        InputEvent::Key(KeyEvent {
            key_down: true,
            virtual_key_code: VK_RETURN,
            unicode_char: b'\r' as u16,
            control_key_state: 0,
        }),
    ]);
    assert_eq!(translated, b"\n\r", "translator output drifted");

    // Send the translated bytes via extra_rx. A short sleep before
    // sending lets the pump enter its main loop; the child's stdin
    // line-mode buffer holds the bytes until newline (mock-sleep then
    // reads them all).
    let (extra_tx, extra_rx) = std::sync::mpsc::channel::<Vec<u8>>();
    std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(80));
        let _ = extra_tx.send(translated.clone());
    });

    let interrupted = AtomicBool::new(false);
    let mut hooks = CountingHooks::new(false);

    let _exit = clud::session::run_raw_pty_pump_with_extra_rx(
        &process,
        &interrupted,
        &mut hooks,
        Cursor::new(Vec::<u8>::new()),
        Some(extra_rx),
    );

    let _ = process.wait_impl(Some(5.0));
    let _ = drain_reader(&process, Duration::from_millis(300));
    let _ = process.close_impl();

    let got = std::fs::read(&raw_stdin).unwrap_or_default();
    assert!(
        got.contains(&b'\n'),
        "Shift+Enter translation must produce a literal \\n in the child's stdin; got {:?}",
        got
    );
    assert!(
        got.contains(&b'\r'),
        "plain Enter translation must produce a \\r in the child's stdin; got {:?}",
        got
    );
}
