//! Windows half of `crate::daemon::attach_input`'s byte source: the
//! `console_input` reader (so Shift+Enter and Backspace match the local
//! pump), with a detached byte-stream stdin reader as the fallback.

use std::io;
use std::time::Duration;

/// The raw terminal input source for one interactive attach.
pub struct RawInputSource {
    rx: std::sync::mpsc::Receiver<Vec<u8>>,
    /// Dropping the handle stops the native reader and restores the
    /// console mode it changed.
    console: Option<crate::console_input::ConsoleInputHandle>,
}

impl RawInputSource {
    /// Start reading the real terminal. Call before entering raw mode:
    /// the native reader snapshots the original console mode.
    pub fn start() -> Self {
        match crate::console_input::spawn_console_input_reader() {
            Ok(handle) => {
                if let Some(input) = Self::from_console(handle) {
                    return input;
                }
            }
            Err(err) => {
                eprintln!("[clud] note: console-input reader unavailable: {err}");
            }
        }
        // Same fallback as the local pump: a detached byte-stream
        // reader. It cannot be stopped while blocked in `read`, so it
        // is only the fallback.
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            use std::io::Read;
            let mut stdin = io::stdin();
            let mut buf = [0u8; 4096];
            while let Ok(n) = stdin.read(&mut buf) {
                if n == 0 || tx.send(buf[..n].to_vec()).is_err() {
                    break;
                }
            }
        });
        Self { rx, console: None }
    }

    fn from_console(mut handle: crate::console_input::ConsoleInputHandle) -> Option<Self> {
        let rx = handle.take_receiver()?;
        Some(Self {
            rx,
            console: Some(handle),
        })
    }

    /// The `console_input` reader already expands Ctrl+V
    /// (`console_input::adapt_event`); only the byte-stream fallback needs
    /// the attach to expand it.
    pub fn expands_ctrl_v(&self) -> bool {
        self.console.is_none()
    }

    /// Wait up to `timeout` for the next chunk: `Some(Some(chunk))` for
    /// bytes, `Some(None)` when idle, `None` once the source closed.
    pub fn poll(&mut self, timeout: Duration) -> Option<Option<Vec<u8>>> {
        use std::sync::mpsc::RecvTimeoutError;
        match self.rx.recv_timeout(timeout) {
            Ok(chunk) => Some(Some(chunk)),
            Err(RecvTimeoutError::Timeout) => Some(None),
            Err(RecvTimeoutError::Disconnected) => None,
        }
    }
}

/// Test seam: POSIX's stdin-descriptor source does not exist here.
#[cfg(test)]
pub fn stdin_fd_source_for_test() -> Option<RawInputSource> {
    None
}

/// Test seam: POSIX's pipe-backed source does not exist here.
#[cfg(test)]
pub fn pipe_source_for_test() -> Option<(RawInputSource, std::fs::File, std::fs::File)> {
    None
}

/// Test seam (#1373): a console source whose reader has one injected Ctrl+V
/// key event queued.
#[cfg(test)]
pub fn injected_ctrl_v_console_source_for_test() -> Option<RawInputSource> {
    use running_process::pty::terminal_input::{TerminalInputCore, TerminalInputEventRecord};
    use std::sync::Arc;

    let core = Arc::new(TerminalInputCore::new());
    {
        let mut state = core.state.lock().expect("terminal input state");
        state.events.push_back(TerminalInputEventRecord {
            data: vec![0x16],
            submit: false,
            shift: false,
            ctrl: true,
            alt: false,
            virtual_key_code: 0x56,
            repeat_count: 1,
        });
        state.closed = false;
    }
    core.condvar.notify_all();
    let handle = crate::console_input::spawn_terminal_input_adapter(Arc::clone(&core))
        .expect("spawn terminal input adapter");
    Some(RawInputSource::from_console(handle).expect("console receiver"))
}
