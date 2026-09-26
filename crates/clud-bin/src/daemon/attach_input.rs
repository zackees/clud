//! Raw keyboard input for an interactive daemon attach (#1355).
//!
//! `run_remote_interactive` used to read keys through `crossterm::event`,
//! which only yields key, paste, resize and mouse events. The reply a
//! terminal writes to the child's `ESC[6n` cursor query (`ESC[<row>;<col>R`)
//! parses into none of them, so it was dropped, and a ConPTY child waiting
//! for that reply hung at startup. Input now reaches the worker byte for
//! byte, the way the local PTY pump forwards stdin
//! (`session::forward_user_input`), and Ctrl+C, F3 and bracketed paste are
//! detected in the byte stream with the local pump's own helpers.
//!
//! The byte source mirrors `runner_execution.rs`: on Windows the
//! `console_input` reader (so Shift+Enter and Backspace match the local
//! pump), elsewhere stdin itself, polled so the source can stop cleanly
//! before the continue-in-background prompt reads the terminal.

use std::io;
use std::time::Duration;

use crate::session::session_stdin::{
    normalize_interactive_console_stdin_chunk, stdin_chunk_requests_interrupt,
};
use crate::session::{BracketedPasteNormalizer, F3Events, F3Observer};

/// One chunk of user input after [`RemoteInputFilter::process`].
#[derive(Debug, Default, PartialEq, Eq)]
pub(super) struct FilteredInput {
    /// Bytes to send to the worker as `WorkerClientMessage::Input`.
    pub(super) bytes: Vec<u8>,
    /// The chunk asked to interrupt the session. Its bytes are not
    /// forwarded: the attach's interrupt flow owns Ctrl+C, as before.
    pub(super) interrupt: bool,
    /// F3 presses and releases seen, for the voice hooks.
    pub(super) f3: F3Events,
}

impl FilteredInput {
    /// A lone Enter submits the prompt, matching the `submit` flag the
    /// key-event loop set for `KeyCode::Enter`.
    pub(super) fn submit(&self) -> bool {
        self.bytes == b"\r"
    }
}

/// Stateful byte filter between the terminal and the worker.
pub(super) struct RemoteInputFilter {
    paste: BracketedPasteNormalizer,
    f3: F3Observer,
}

impl RemoteInputFilter {
    pub(super) fn new() -> Self {
        Self {
            paste: BracketedPasteNormalizer::new(),
            f3: F3Observer::new(),
        }
    }

    /// Filter one raw input chunk. Everything that is not Ctrl+C is
    /// forwarded, including terminal replies no key parser recognizes.
    pub(super) fn process(&mut self, chunk: &[u8]) -> FilteredInput {
        if stdin_chunk_requests_interrupt(chunk) {
            return FilteredInput {
                interrupt: true,
                ..FilteredInput::default()
            };
        }
        let mut chunk = chunk.to_vec();
        // Windows-only Backspace fix-up the local pump applies (#1350).
        normalize_interactive_console_stdin_chunk(&mut chunk);
        FilteredInput {
            f3: self.f3.observe(&chunk),
            bytes: self.paste.process(&chunk),
            interrupt: false,
        }
    }

    /// True while the paste normalizer holds a partial `ESC[200~` prefix,
    /// such as a lone Esc keypress.
    pub(super) fn has_pending(&self) -> bool {
        self.paste.has_pending()
    }

    /// Release held prefix bytes once input has gone idle, so a lone Esc
    /// reaches the child instead of waiting for the next keystroke.
    pub(super) fn flush_pending(&mut self) -> Vec<u8> {
        self.paste.flush_pending()
    }
}

/// Result of one [`RawInput::poll`].
#[derive(Debug, PartialEq, Eq)]
pub(super) enum InputPoll {
    Chunk(Vec<u8>),
    Idle,
    /// The source hit EOF or failed; the attach cannot read input again.
    Closed,
}

/// The raw terminal input source for one interactive attach.
pub(super) struct RawInput {
    #[cfg(windows)]
    rx: std::sync::mpsc::Receiver<Vec<u8>>,
    /// Dropping the handle stops the native reader and restores the
    /// console mode it changed.
    #[cfg(windows)]
    _console: Option<crate::console_input::ConsoleInputHandle>,
    #[cfg(unix)]
    fd: std::os::fd::RawFd,
}

impl RawInput {
    /// Start reading the real terminal. Call before entering raw mode:
    /// on Windows the native reader snapshots the original console mode.
    pub(super) fn start() -> Self {
        #[cfg(windows)]
        {
            match crate::console_input::spawn_console_input_reader() {
                Ok(mut handle) => {
                    if let Some(rx) = handle.take_receiver() {
                        return Self {
                            rx,
                            _console: Some(handle),
                        };
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
            Self { rx, _console: None }
        }
        #[cfg(unix)]
        {
            use std::os::fd::AsRawFd;
            Self::from_fd(io::stdin().as_raw_fd())
        }
    }

    #[cfg(unix)]
    fn from_fd(fd: std::os::fd::RawFd) -> Self {
        Self { fd }
    }

    /// Wait up to `timeout` for the next chunk.
    pub(super) fn poll(&mut self, timeout: Duration) -> InputPoll {
        #[cfg(windows)]
        {
            use std::sync::mpsc::RecvTimeoutError;
            match self.rx.recv_timeout(timeout) {
                Ok(chunk) => InputPoll::Chunk(chunk),
                Err(RecvTimeoutError::Timeout) => InputPoll::Idle,
                Err(RecvTimeoutError::Disconnected) => InputPoll::Closed,
            }
        }
        #[cfg(unix)]
        {
            poll_fd(self.fd, timeout)
        }
    }
}

/// Wait for `fd` to become readable, then read what is there. Polling
/// instead of a blocked reader thread means nothing is left reading the
/// terminal once the attach loop returns.
#[cfg(unix)]
fn poll_fd(fd: std::os::fd::RawFd, timeout: Duration) -> InputPoll {
    let mut pollfd = libc::pollfd {
        fd,
        events: libc::POLLIN,
        revents: 0,
    };
    let timeout_ms = libc::c_int::try_from(timeout.as_millis()).unwrap_or(libc::c_int::MAX);
    // SAFETY: `pollfd` is a valid, exclusively borrowed array of length 1.
    let ready = unsafe { libc::poll(&mut pollfd, 1, timeout_ms) };
    if ready < 0 {
        return if io::Error::last_os_error().kind() == io::ErrorKind::Interrupted {
            InputPoll::Idle
        } else {
            InputPoll::Closed
        };
    }
    if ready == 0 {
        return InputPoll::Idle;
    }
    let mut buf = [0u8; 4096];
    // SAFETY: `buf` is valid for writes of `buf.len()` bytes.
    let read = unsafe { libc::read(fd, buf.as_mut_ptr().cast(), buf.len()) };
    match read {
        0 => InputPoll::Closed,
        n if n < 0 => match io::Error::last_os_error().kind() {
            io::ErrorKind::Interrupted | io::ErrorKind::WouldBlock => InputPoll::Idle,
            _ => InputPoll::Closed,
        },
        n => InputPoll::Chunk(buf[..n as usize].to_vec()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// #1355: the terminal's reply to the child's `ESC[6n` must reach the
    /// worker unchanged, or a ConPTY child waiting for it never starts.
    #[test]
    fn cursor_position_reply_is_forwarded_verbatim() {
        let mut filter = RemoteInputFilter::new();
        let out = filter.process(b"\x1b[12;40R");
        assert_eq!(out.bytes, b"\x1b[12;40R".to_vec());
        assert!(!out.interrupt);
        assert!(!out.submit());
    }

    /// #1347 lists the other probes a child TUI may block on; their
    /// replies are just as unrecognizable to a key-event parser.
    #[test]
    fn device_attribute_and_colour_replies_are_forwarded_verbatim() {
        for reply in [
            b"\x1b[?1;2c".as_slice(),
            b"\x1b[>0;0;0c",
            b"\x1b[?0u",
            b"\x1b]11;rgb:0000/0000/0000\x1b\\",
        ] {
            let mut filter = RemoteInputFilter::new();
            assert_eq!(filter.process(reply).bytes, reply.to_vec(), "{reply:?}");
        }
    }

    #[test]
    fn keys_and_enter_are_forwarded_and_lone_enter_submits() {
        let mut filter = RemoteInputFilter::new();
        let typed = filter.process(b"hi\x1b[A");
        assert_eq!(typed.bytes, b"hi\x1b[A".to_vec());
        assert!(!typed.submit());
        assert!(filter.process(b"\r").submit());
    }

    #[test]
    fn ctrl_c_requests_interrupt_without_forwarding() {
        for chunk in [b"\x03".as_slice(), b"abc\x03", b"\x1b[99;5u"] {
            let mut filter = RemoteInputFilter::new();
            let out = filter.process(chunk);
            assert!(out.interrupt, "{chunk:?}");
            assert!(out.bytes.is_empty(), "{chunk:?}");
        }
    }

    #[test]
    fn f3_is_counted_and_forwarded_like_the_local_pump() {
        let mut filter = RemoteInputFilter::new();
        let out = filter.process(b"\x1bOR");
        assert_eq!(out.f3.presses, 1);
        assert_eq!(out.bytes, b"\x1bOR".to_vec());
    }

    #[test]
    fn bracketed_paste_keeps_its_markers() {
        let mut filter = RemoteInputFilter::new();
        let out = filter.process(b"\x1b[200~hello world\x1b[201~");
        assert_eq!(out.bytes, b"\x1b[200~hello world\x1b[201~".to_vec());
    }

    #[test]
    fn lone_esc_is_released_by_flush() {
        let mut filter = RemoteInputFilter::new();
        assert!(filter.process(b"\x1b").bytes.is_empty());
        assert!(filter.has_pending());
        assert_eq!(filter.flush_pending(), b"\x1b".to_vec());
        assert!(!filter.has_pending());
    }

    #[cfg(unix)]
    #[test]
    fn fd_source_reads_raw_bytes_then_reports_close() {
        use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};

        let mut fds = [0 as libc::c_int; 2];
        // SAFETY: `fds` is a valid array of two descriptors for pipe(2).
        assert_eq!(unsafe { libc::pipe(fds.as_mut_ptr()) }, 0);
        // SAFETY: pipe(2) returned two fresh descriptors this test owns.
        let (read_end, write_end) =
            unsafe { (OwnedFd::from_raw_fd(fds[0]), OwnedFd::from_raw_fd(fds[1])) };
        let mut input = RawInput::from_fd(read_end.as_raw_fd());

        assert_eq!(input.poll(Duration::from_millis(10)), InputPoll::Idle);

        let reply = b"\x1b[3;1R";
        let mut writer = std::fs::File::from(write_end);
        std::io::Write::write_all(&mut writer, reply).unwrap();
        assert_eq!(
            input.poll(Duration::from_secs(5)),
            InputPoll::Chunk(reply.to_vec())
        );

        drop(writer);
        assert_eq!(input.poll(Duration::from_secs(5)), InputPoll::Closed);
    }
}
