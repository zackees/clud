//! Windows native terminal-input adapter.
//!
//! `running-process` owns Win32 console-mode setup, `ReadConsoleInputW`,
//! virtual-key translation, repeat handling, and optional byte tracing.
//! Clud only applies its two product-specific policies before forwarding each
//! translated event to the PTY as one channel chunk:
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
//! `running-process` also owns reassembling UTF-16 surrogate pairs that arrive
//! split across separate `KEY_EVENT` records, which covers emoji and other
//! supplementary-plane characters (issue #1351). This adapter only ever sees
//! whole translated events, so it must not try to pair surrogates itself.

#![cfg(windows)]

use std::io;
use std::sync::mpsc;
use std::sync::Arc;
use std::thread;

use running_process::pty::terminal_input::{
    TerminalInputCore, TerminalInputError, TerminalInputEventRecord,
};

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

/// Start the authoritative `running-process` native terminal reader and bridge
/// each translated key event to clud's PTY input channel.
pub fn spawn_console_input_reader() -> io::Result<ConsoleInputHandle> {
    let core = Arc::new(TerminalInputCore::new());
    core.start_impl()?;
    spawn_terminal_input_adapter(core)
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
