//! Windows native terminal-input adapter.
//!
//! `running-process` owns Win32 console-mode selection, virtual-key
//! translation, repeat handling, and optional byte tracing. Clud runs the
//! `ReadConsoleInputW` loop itself (`start_native_reader`) so it can pair
//! UTF-16 surrogates before that per-record translator sees them, then applies
//! its two product-specific policies before forwarding each translated event
//! to the PTY as one channel chunk:
//!
//! - Shift+Enter becomes ESC CR (the Alt+Enter newline Claude Code and Codex
//!   accept). ConPTY rewrites a bare LF into CR, so a literal LF would reach
//!   the child as plain Enter (#1369). This adapter only feeds the PTY (see
//!   `runner_execution.rs`); the inherited-console path does not use it and
//!   keeps LF.
//! - Ctrl+V may expand a clipboard image to its saved path.
//!
//! Keeping the generic translator in `running-process` prevents navigation
//! keys from drifting between two implementations (issue #575).
//!
//! Emoji and other supplementary-plane characters arrive as two `KEY_EVENT`
//! records, one per UTF-16 surrogate. `running-process` 4.9 translates each
//! record alone and drops both halves (issue #1351), so the reader loop runs
//! every batch through [`crate::console_surrogates::SurrogatePairer`], which
//! owns the pairing rules, and hands only non-surrogate records to
//! `translate_console_key_event`. The upstream fix is
//! zackees/running-process#1215; once clud depends on a release with it, this
//! loop can go back to `TerminalInputCore::start_impl`.

#![cfg(windows)]

use std::io;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::sync::{Arc, Condvar, Mutex};
use std::thread;

use running_process::pty::terminal_input::{
    append_native_terminal_input_trace_line, native_terminal_input_mode,
    trace_translated_console_key_event, translate_console_key_event, unix_now_seconds,
    ActiveTerminalInputCapture, TerminalInputCore, TerminalInputError, TerminalInputEventRecord,
    TerminalInputState,
};
use winapi::um::wincontypes::KEY_EVENT_RECORD;

use crate::console_surrogates::{KeyUnit, SurrogatePairer};

/// Windows virtual-key code for Enter / Return.
const VK_RETURN: u16 = 0x0D;
/// Windows virtual-key code for V.
const VK_V: u16 = 0x56;

/// Handle for clud's small policy/forwarding bridge around
/// [`TerminalInputCore`].
///
/// Dropping the handle stops the upstream native reader, restores the original
/// console mode, closes the event queue, and joins the bridge thread.
pub struct ConsoleInputHandle {
    rx: Option<mpsc::Receiver<Vec<u8>>>,
    core: Arc<TerminalInputCore>,
    bridge: Option<thread::JoinHandle<()>>,
}

impl ConsoleInputHandle {
    /// Take the receiver that feeds the PTY pump's `extra_rx` channel.
    pub fn take_receiver(&mut self) -> Option<mpsc::Receiver<Vec<u8>>> {
        self.rx.take()
    }
}

impl Drop for ConsoleInputHandle {
    fn drop(&mut self) {
        let _ = self.core.stop_impl();
        if let Some(bridge) = self.bridge.take() {
            let _ = bridge.join();
        }
    }
}

/// Start the native console reader and bridge each translated key event to
/// clud's PTY input channel.
pub fn spawn_console_input_reader() -> io::Result<ConsoleInputHandle> {
    let core = Arc::new(TerminalInputCore::new());
    start_native_reader(&core)?;
    spawn_terminal_input_adapter(core)
}

/// `TerminalInputCore::start_impl`, but with a reader loop that pairs UTF-16
/// surrogates (#1351).
///
/// It fills the same public core fields `start_impl` does, so the upstream
/// `TerminalInputCore::stop_impl` (run by [`ConsoleInputHandle`]'s drop and
/// the core's own drop) still stops this worker and restores the console mode.
fn start_native_reader(core: &TerminalInputCore) -> io::Result<()> {
    use winapi::um::consoleapi::{GetConsoleMode, SetConsoleMode};
    use winapi::um::handleapi::INVALID_HANDLE_VALUE;
    use winapi::um::processenv::GetStdHandle;
    use winapi::um::winbase::STD_INPUT_HANDLE;

    let mut worker = core
        .worker
        .lock()
        .expect("terminal input worker mutex poisoned");
    if worker.is_some() {
        return Ok(());
    }

    // SAFETY: plain Win32 calls on this process's standard input handle.
    let input_handle = unsafe { GetStdHandle(STD_INPUT_HANDLE) };
    if input_handle.is_null() || input_handle == INVALID_HANDLE_VALUE {
        return Err(io::Error::last_os_error());
    }
    let mut original_mode = 0u32;
    // SAFETY: `input_handle` was checked above; `original_mode` outlives the call.
    if unsafe { GetConsoleMode(input_handle, &mut original_mode) } == 0 {
        return Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "native console input requires an attached Windows console stdin",
        ));
    }
    let active_mode = native_terminal_input_mode(original_mode);
    // SAFETY: `input_handle` is a console input handle (GetConsoleMode succeeded).
    if unsafe { SetConsoleMode(input_handle, active_mode) } == 0 {
        return Err(io::Error::last_os_error());
    }
    append_native_terminal_input_trace_line(&format!(
        "[{:.6}] native_terminal_input start handle={} original_mode={original_mode:#010x} active_mode={active_mode:#010x} reader=clud",
        unix_now_seconds(),
        input_handle as usize,
    ));

    core.stop.store(false, Ordering::Release);
    core.capturing.store(true, Ordering::Release);
    {
        let mut state = core.state.lock().expect("terminal input mutex poisoned");
        state.events.clear();
        state.closed = false;
    }
    *core
        .console
        .lock()
        .expect("terminal input console mutex poisoned") = Some(ActiveTerminalInputCapture {
        input_handle: input_handle as usize,
        original_mode,
        active_mode,
    });

    let state = Arc::clone(&core.state);
    let condvar = Arc::clone(&core.condvar);
    let stop = Arc::clone(&core.stop);
    let capturing = Arc::clone(&core.capturing);
    let input_handle = input_handle as usize;
    let spawned = thread::Builder::new()
        .name("clud-console-input-reader".into())
        .spawn(move || read_console_input(input_handle, state, condvar, stop, capturing));
    match spawned {
        Ok(handle) => {
            *worker = Some(handle);
            Ok(())
        }
        Err(error) => {
            drop(worker);
            let _ = core.stop_impl();
            Err(error)
        }
    }
}

/// `running-process`'s `native_terminal_input_worker` loop with one change:
/// every batch goes through [`translate_key_records`] and one
/// [`SurrogatePairer`] that outlives the batch, so a pair split across two
/// `ReadConsoleInputW` calls still joins.
fn read_console_input(
    input_handle: usize,
    state: Arc<Mutex<TerminalInputState>>,
    condvar: Arc<Condvar>,
    stop: Arc<AtomicBool>,
    capturing: Arc<AtomicBool>,
) {
    use winapi::shared::minwindef::DWORD;
    use winapi::shared::winerror::WAIT_TIMEOUT;
    use winapi::um::consoleapi::ReadConsoleInputW;
    use winapi::um::synchapi::WaitForSingleObject;
    use winapi::um::winbase::WAIT_OBJECT_0;
    use winapi::um::wincontypes::{INPUT_RECORD, KEY_EVENT};
    use winapi::um::winnt::HANDLE;

    let handle = input_handle as HANDLE;
    // SAFETY: INPUT_RECORD is plain data; all-zero is a valid value.
    let mut records: [INPUT_RECORD; 512] = unsafe { std::mem::zeroed() };
    let mut pairer = SurrogatePairer::new();
    append_native_terminal_input_trace_line(&format!(
        "[{:.6}] native_terminal_input worker_start handle={input_handle} reader=clud",
        unix_now_seconds(),
    ));

    while !stop.load(Ordering::Acquire) {
        // SAFETY: `handle` is the console input handle captured at start.
        let wait_result = unsafe { WaitForSingleObject(handle, 50) };
        match wait_result {
            WAIT_OBJECT_0 => {
                let mut read_count: DWORD = 0;
                // SAFETY: `records` holds `records.len()` INPUT_RECORDs and
                // `read_count` receives how many were filled.
                let ok = unsafe {
                    ReadConsoleInputW(
                        handle,
                        records.as_mut_ptr(),
                        records.len() as DWORD,
                        &mut read_count,
                    )
                };
                if ok == 0 {
                    append_native_terminal_input_trace_line(&format!(
                        "[{:.6}] native_terminal_input read_console_input_failed handle={input_handle}",
                        unix_now_seconds(),
                    ));
                    break;
                }
                let keys: Vec<KEY_EVENT_RECORD> = records
                    .iter()
                    .take(read_count as usize)
                    .filter(|record| record.EventType == KEY_EVENT)
                    // SAFETY: EventType == KEY_EVENT selects the KeyEvent arm.
                    .map(|record| *unsafe { record.Event.KeyEvent() })
                    .collect();
                let batch = translate_key_records(&mut pairer, &keys);
                if !batch.is_empty() {
                    let mut guard = state.lock().expect("terminal input mutex poisoned");
                    guard.events.extend(batch);
                    drop(guard);
                    condvar.notify_all();
                }
            }
            WAIT_TIMEOUT => continue,
            _ => {
                append_native_terminal_input_trace_line(&format!(
                    "[{:.6}] native_terminal_input wait_result={wait_result} handle={input_handle}",
                    unix_now_seconds(),
                ));
                break;
            }
        }
    }

    capturing.store(false, Ordering::Release);
    let mut guard = state.lock().expect("terminal input mutex poisoned");
    guard.closed = true;
    condvar.notify_all();
    drop(guard);
    append_native_terminal_input_trace_line(&format!(
        "[{:.6}] native_terminal_input worker_stop handle={input_handle}",
        unix_now_seconds(),
    ));
}

/// Translate one batch of key records: surrogate halves are paired into UTF-8
/// text, and every other record goes to `running-process`'s translator.
fn translate_key_records(
    pairer: &mut SurrogatePairer,
    keys: &[KEY_EVENT_RECORD],
) -> Vec<TerminalInputEventRecord> {
    pairer.translate_batch(
        keys,
        |key| KeyUnit {
            key_down: key.bKeyDown != 0,
            // SAFETY: ReadConsoleInputW fills the UnicodeChar arm.
            unit: unsafe { *key.uChar.UnicodeChar() },
            repeat_count: key.wRepeatCount,
        },
        translate_console_key_event,
        |key, text| {
            // Literal text: no modifiers, so neither the Shift+Enter nor the
            // Ctrl+V policy in `adapt_event_with_clipboard` can match it.
            let event = TerminalInputEventRecord {
                data: text,
                submit: false,
                shift: false,
                ctrl: false,
                alt: false,
                virtual_key_code: key.wVirtualKeyCode,
                repeat_count: key.wRepeatCount.max(1),
            };
            trace_translated_console_key_event(key, event)
        },
    )
}

/// Bridge an existing terminal-input core into clud's policy channel.
///
/// Production callers normally use [`spawn_console_input_reader`]. Keeping
/// this composition point public also lets consumer integration tests inject
/// already-translated upstream events without requiring an attached console.
#[doc(hidden)]
pub fn spawn_terminal_input_adapter(
    core: Arc<TerminalInputCore>,
) -> io::Result<ConsoleInputHandle> {
    let (tx, rx) = mpsc::channel::<Vec<u8>>();
    let bridge_core = Arc::clone(&core);
    let bridge = thread::Builder::new()
        .name("clud-console-input-adapter".into())
        .spawn(move || loop {
            match bridge_core.wait_for_event(Some(0.1)) {
                Ok(event) => {
                    let bytes = adapt_event(event);
                    if !bytes.is_empty() && tx.send(bytes).is_err() {
                        break;
                    }
                }
                Err(TerminalInputError::Timeout) => continue,
                Err(TerminalInputError::Closed) => break,
                Err(error) => {
                    eprintln!("[clud] warning: native terminal input failed: {error}");
                    break;
                }
            }
        })?;

    Ok(ConsoleInputHandle {
        rx: Some(rx),
        core,
        bridge: Some(bridge),
    })
}

fn adapt_event(event: TerminalInputEventRecord) -> Vec<u8> {
    adapt_event_with_clipboard(event, || {
        crate::paste_image::handle_clipboard().ok().flatten()
    })
}

fn adapt_event_with_clipboard<F>(
    event: TerminalInputEventRecord,
    mut handle_clipboard: F,
) -> Vec<u8>
where
    F: FnMut() -> Option<Vec<u8>>,
{
    // running-process represents Shift+Enter as CSI-u so generic terminal
    // consumers can distinguish it. ConPTY rewrites a bare LF into CR, so a
    // literal LF would arrive as plain Enter (#1369). This adapter only feeds
    // the PTY, so send ESC CR (the Alt+Enter newline Claude Code and Codex
    // accept), which ConPTY passes through unchanged.
    if event.virtual_key_code == VK_RETURN && event.shift && !event.ctrl && !event.alt {
        return b"\x1b\r".repeat(usize::from(event.repeat_count.max(1)));
    }

    if event.virtual_key_code == VK_V && event.ctrl {
        // Honor wRepeatCount like the Shift+Enter branch: one clipboard
        // expansion per repeat (issue #1361).
        let mut pasted: Option<Vec<u8>> = None;
        for _ in 0..usize::from(event.repeat_count.max(1)) {
            if let Some(bytes) = handle_clipboard() {
                pasted
                    .get_or_insert_with(Vec::new)
                    .extend_from_slice(&bytes);
            }
        }
        if let Some(bytes) = pasted {
            return bytes;
        }
    }

    event.data
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::VecDeque;
    use std::time::Duration;

    fn event(
        data: &[u8],
        virtual_key_code: u16,
        shift: bool,
        ctrl: bool,
        alt: bool,
    ) -> TerminalInputEventRecord {
        TerminalInputEventRecord {
            data: data.to_vec(),
            submit: virtual_key_code == VK_RETURN && !shift,
            shift,
            ctrl,
            alt,
            virtual_key_code,
            repeat_count: 1,
        }
    }

    #[test]
    fn shift_enter_emits_esc_cr_that_conpty_preserves() {
        let upstream = event(b"\x1b[13;2u", VK_RETURN, true, false, false);
        let bytes = adapt_event_with_clipboard(upstream, || None);
        assert_eq!(bytes, b"\x1b\r");
        assert!(
            !bytes.contains(&b'\n'),
            "ConPTY rewrites LF into CR, so Shift+Enter must not emit LF; got {bytes:?}"
        );
    }

    #[test]
    fn shift_enter_honors_repeat_count() {
        let mut upstream = event(b"\x1b[13;2u\x1b[13;2u", VK_RETURN, true, false, false);
        upstream.repeat_count = 2;
        assert_eq!(
            adapt_event_with_clipboard(upstream, || None),
            b"\x1b\r\x1b\r"
        );
    }

    #[test]
    fn plain_and_modified_enter_other_than_shift_keep_upstream_bytes() {
        let plain = event(b"\r", VK_RETURN, false, false, false);
        assert_eq!(adapt_event_with_clipboard(plain, || None), b"\r");

        let ctrl = event(b"\r", VK_RETURN, false, true, false);
        assert_eq!(adapt_event_with_clipboard(ctrl, || None), b"\r");
    }

    #[test]
    fn ctrl_v_uses_clipboard_image_bytes_when_available() {
        let upstream = event(&[0x16], VK_V, false, true, false);
        let bytes = adapt_event_with_clipboard(upstream, || Some(b"C:\\tmp\\paste.png\n".to_vec()));
        assert_eq!(bytes, b"C:\\tmp\\paste.png\n");
    }

    #[test]
    fn ctrl_v_falls_through_to_upstream_control_byte() {
        let upstream = event(&[0x16], VK_V, false, true, false);
        assert_eq!(adapt_event_with_clipboard(upstream, || None), vec![0x16]);
    }

    #[test]
    fn ctrl_v_honors_repeat_count() {
        let mut upstream = event(b"\x16", VK_V, false, true, false);
        upstream.repeat_count = 3;
        let mut calls = 0usize;
        let bytes = adapt_event_with_clipboard(upstream, || {
            calls += 1;
            Some(format!("[img{calls}]").into_bytes())
        });
        assert_eq!(calls, 3);
        assert_eq!(bytes, b"[img1][img2][img3]");
    }

    #[test]
    fn ctrl_v_repeat_zero_treated_as_one() {
        let mut upstream = event(b"\x16", VK_V, false, true, false);
        upstream.repeat_count = 0;
        let mut calls = 0usize;
        let bytes = adapt_event_with_clipboard(upstream, || {
            calls += 1;
            Some(format!("[img{calls}]").into_bytes())
        });
        assert_eq!(calls, 1);
        assert_eq!(bytes, b"[img1]");
    }

    #[test]
    fn ctrl_v_without_clipboard_image_falls_back_to_event_data() {
        let mut upstream = event(b"\x16", VK_V, false, true, false);
        upstream.repeat_count = 3;
        let expected = upstream.data.clone();
        assert_eq!(adapt_event_with_clipboard(upstream, || None), expected);
    }

    fn key_record(key_down: bool, unit: u16, repeat_count: u16) -> KEY_EVENT_RECORD {
        // SAFETY: all-zero is a valid KEY_EVENT_RECORD; the fields the
        // translator reads are set below.
        let mut record: KEY_EVENT_RECORD = unsafe { std::mem::zeroed() };
        record.bKeyDown = i32::from(key_down);
        record.wRepeatCount = repeat_count;
        record.wVirtualKeyCode = VK_PACKET;
        // SAFETY: UnicodeChar is the arm the translator reads.
        unsafe {
            *record.uChar.UnicodeChar_mut() = unit;
        }
        record
    }

    /// Down/up records for every UTF-16 unit of `text`, as the emoji picker,
    /// an IME, or Windows Terminal's keystroke paste deliver it.
    fn typed(text: &str) -> Vec<KEY_EVENT_RECORD> {
        text.encode_utf16()
            .flat_map(|unit| [key_record(true, unit, 1), key_record(false, unit, 1)])
            .collect()
    }

    fn bytes(events: Vec<TerminalInputEventRecord>) -> Vec<u8> {
        events.into_iter().flat_map(|event| event.data).collect()
    }

    /// Windows virtual-key code the console reports for synthesized text.
    const VK_PACKET: u16 = 0xE7;

    #[test]
    fn upstream_translator_alone_drops_both_surrogate_halves() {
        // Pins the running-process 4.9 defect this module works around
        // (#1351). When it starts to fail, running-process pairs surrogates
        // itself and `start_native_reader` can go back to `start_impl`.
        let dropped: Vec<_> = typed("😀")
            .iter()
            .filter_map(translate_console_key_event)
            .collect();
        assert!(dropped.is_empty(), "upstream now emits {dropped:?}");
    }

    #[test]
    fn emoji_key_records_become_utf8_through_the_real_translator() {
        let mut pairer = SurrogatePairer::new();
        let got = bytes(translate_key_records(&mut pairer, &typed("a😀b")));
        assert_eq!(got, "a😀b".as_bytes());
    }

    #[test]
    fn emoji_split_across_read_batches_joins() {
        let records = typed("😀");
        let mut pairer = SurrogatePairer::new();
        // Batch 1 ends after the high surrogate's down/up records.
        assert!(translate_key_records(&mut pairer, &records[..2]).is_empty());
        let got = bytes(translate_key_records(&mut pairer, &records[2..]));
        assert_eq!(got, "😀".as_bytes());
    }

    #[test]
    fn surrogate_text_event_is_literal_and_bypasses_adapter_policies() {
        let mut pairer = SurrogatePairer::new();
        let events = translate_key_records(&mut pairer, &typed("😀"));
        assert_eq!(events.len(), 1);
        let event = events.into_iter().next().expect("one text event");
        assert!(!event.shift && !event.ctrl && !event.alt && !event.submit);
        assert_eq!(
            adapt_event_with_clipboard(event, || panic!("no clipboard for text")),
            "😀".as_bytes()
        );
    }

    #[test]
    fn navigation_sequences_remain_complete_atomic_chunks() {
        let core = Arc::new(TerminalInputCore::new());
        {
            let mut state = core.state.lock().expect("terminal input state");
            state.events = VecDeque::from([
                event(b"\x1b[D", 0x25, false, false, false),
                event(b"\x1b[B", 0x28, false, false, false),
                event(b"\x1b[C", 0x27, false, false, false),
                event(b"\x1b[A", 0x26, false, false, false),
            ]);
            state.closed = false;
        }
        core.condvar.notify_all();

        let mut handle =
            spawn_terminal_input_adapter(Arc::clone(&core)).expect("spawn terminal input adapter");
        let rx = handle.take_receiver().expect("terminal input receiver");
        for expected in [b"\x1b[D", b"\x1b[B", b"\x1b[C", b"\x1b[A"] {
            assert_eq!(
                rx.recv_timeout(Duration::from_secs(1))
                    .expect("translated navigation event"),
                expected
            );
        }
    }
}
