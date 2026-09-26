//! Raw keyboard input for an interactive daemon attach (#1355).
//!
//! `run_remote_interactive` used to read keys through `crossterm::event`,
//! which only yields key, paste, resize and mouse events. The reply a
//! terminal writes to the child's `ESC[6n` cursor query (`ESC[<row>;<col>R`)
//! parses into none of them, so it was dropped, and a ConPTY child waiting
//! for that reply hung at startup. Input now reaches the worker byte for
//! byte, the way the local PTY pump forwards stdin
//! (`session::forward_user_input`), and Ctrl+C, F3 and bracketed paste are
//! detected in the byte stream with the local pump's own helpers. Ctrl+V
//! with an image on the clipboard pastes the image's saved path (#1373),
//! expanded by the same code as the local pump: `console_input` on Windows,
//! `paste_image::expand_ctrl_v_bytes` for a byte-stream source.
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
    /// Expand a raw Ctrl+V (0x16) to the clipboard image's saved path
    /// (#1373). Set from [`RawInput::expands_ctrl_v`]: only a byte-stream
    /// source needs it, since the Windows `console_input` reader already
    /// expands Ctrl+V itself and must not read the clipboard twice.
    expand_ctrl_v: bool,
}

impl RemoteInputFilter {
    pub(super) fn new(expand_ctrl_v: bool) -> Self {
        Self {
            paste: BracketedPasteNormalizer::new(),
            f3: F3Observer::new(),
            expand_ctrl_v,
        }
    }

    /// Filter one raw input chunk. Everything that is not Ctrl+C is
    /// forwarded, including terminal replies no key parser recognizes.
    pub(super) fn process(&mut self, chunk: &[u8]) -> FilteredInput {
        self.process_with_clipboard(chunk, || {
            crate::paste_image::handle_clipboard().ok().flatten()
        })
    }

    /// [`Self::process`] with the clipboard read injected, so tests never
    /// touch the real clipboard.
    fn process_with_clipboard<F>(&mut self, chunk: &[u8], clipboard: F) -> FilteredInput
    where
        F: FnMut() -> Option<Vec<u8>>,
    {
        // Ctrl+C is checked on the raw chunk, before any clipboard read,
        // exactly as the local pump's stdin branch orders it.
        if stdin_chunk_requests_interrupt(chunk) {
            return FilteredInput {
                interrupt: true,
                ..FilteredInput::default()
            };
        }
        // #1373: the local pump's stdin branch substitution. Only the legacy
        // 0x16 byte counts, as there: clud never pushes kitty's
        // `DISAMBIGUATE_ESCAPE_CODES`, so a CSI u Ctrl+V only arrives when
        // the child pushed that flag itself, and then the child owns it.
        let mut chunk = if self.expand_ctrl_v {
            crate::paste_image::expand_ctrl_v_bytes(chunk, clipboard).into_owned()
        } else {
            chunk.to_vec()
        };
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
    console: Option<crate::console_input::ConsoleInputHandle>,
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

    #[cfg(windows)]
    fn from_console(mut handle: crate::console_input::ConsoleInputHandle) -> Option<Self> {
        let rx = handle.take_receiver()?;
        Some(Self {
            rx,
            console: Some(handle),
        })
    }

    /// True when this source delivers Ctrl+V as a raw 0x16 byte that the
    /// attach must expand itself (#1373), as the local pump does for its
    /// stdin byte stream. The Windows `console_input` reader already
    /// expands Ctrl+V (`console_input::adapt_event`), like the local
    /// pump's side channel.
    pub(super) fn expands_ctrl_v(&self) -> bool {
        #[cfg(windows)]
        {
            self.console.is_none()
        }
        #[cfg(unix)]
        {
            true
        }
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
        let mut filter = RemoteInputFilter::new(false);
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
            let mut filter = RemoteInputFilter::new(false);
            assert_eq!(filter.process(reply).bytes, reply.to_vec(), "{reply:?}");
        }
    }

    #[test]
    fn keys_and_enter_are_forwarded_and_lone_enter_submits() {
        let mut filter = RemoteInputFilter::new(false);
        let typed = filter.process(b"hi\x1b[A");
        assert_eq!(typed.bytes, b"hi\x1b[A".to_vec());
        assert!(!typed.submit());
        assert!(filter.process(b"\r").submit());
    }

    #[test]
    fn ctrl_c_requests_interrupt_without_forwarding() {
        for chunk in [b"\x03".as_slice(), b"abc\x03", b"\x1b[99;5u"] {
            let mut filter = RemoteInputFilter::new(false);
            let out = filter.process(chunk);
            assert!(out.interrupt, "{chunk:?}");
            assert!(out.bytes.is_empty(), "{chunk:?}");
        }
    }

    #[test]
    fn f3_is_counted_and_forwarded_like_the_local_pump() {
        let mut filter = RemoteInputFilter::new(false);
        let out = filter.process(b"\x1bOR");
        assert_eq!(out.f3.presses, 1);
        assert_eq!(out.bytes, b"\x1bOR".to_vec());
    }

    #[test]
    fn bracketed_paste_keeps_its_markers() {
        let mut filter = RemoteInputFilter::new(false);
        let out = filter.process(b"\x1b[200~hello world\x1b[201~");
        assert_eq!(out.bytes, b"\x1b[200~hello world\x1b[201~".to_vec());
    }

    #[test]
    fn lone_esc_is_released_by_flush() {
        let mut filter = RemoteInputFilter::new(false);
        assert!(filter.process(b"\x1b").bytes.is_empty());
        assert!(filter.has_pending());
        assert_eq!(filter.flush_pending(), b"\x1b".to_vec());
        assert!(!filter.has_pending());
    }

    const IMAGE_PATH: &[u8] = b"/tmp/clud-clipboard/paste-1.png\n";

    /// #1373: a byte-stream source (POSIX stdin) delivers Ctrl+V as a raw
    /// 0x16, and the attach expands it to the clipboard image's saved path
    /// exactly like the local pump's stdin branch.
    #[test]
    fn ctrl_v_byte_expands_to_the_clipboard_image_path() {
        let mut filter = RemoteInputFilter::new(true);
        let out = filter.process_with_clipboard(b"a\x16b", || Some(IMAGE_PATH.to_vec()));
        assert_eq!(out.bytes, [b"a".as_slice(), IMAGE_PATH, b"b"].concat());
        assert!(!out.interrupt);
    }

    #[test]
    fn ctrl_v_without_a_clipboard_image_forwards_the_byte() {
        let mut filter = RemoteInputFilter::new(true);
        let out = filter.process_with_clipboard(b"\x16", || None);
        assert_eq!(out.bytes, vec![0x16]);
    }

    /// The Windows `console_input` reader already expanded Ctrl+V, so a
    /// 0x16 it forwards means "no image": the filter must not read the
    /// clipboard a second time.
    #[test]
    fn a_source_that_expands_ctrl_v_itself_is_not_expanded_again() {
        let mut filter = RemoteInputFilter::new(false);
        let mut reads = 0;
        let out = filter.process_with_clipboard(b"\x16", || {
            reads += 1;
            Some(IMAGE_PATH.to_vec())
        });
        assert_eq!(out.bytes, vec![0x16]);
        assert_eq!(reads, 0);
    }

    /// Ctrl+C wins over a Ctrl+V in the same chunk, before any clipboard
    /// read, as in the local pump.
    #[test]
    fn interrupt_is_checked_before_the_clipboard_is_read() {
        let mut filter = RemoteInputFilter::new(true);
        let mut reads = 0;
        let out = filter.process_with_clipboard(b"\x16\x03", || {
            reads += 1;
            Some(IMAGE_PATH.to_vec())
        });
        assert!(out.interrupt);
        assert!(out.bytes.is_empty());
        assert_eq!(reads, 0);
    }

    /// With the kitty keyboard frame the attach pushes (#1363) a terminal
    /// may also report key releases. Only the legacy 0x16 byte pastes, as
    /// in the local pump: CSI u spellings of Ctrl+V (press, repeat,
    /// release) reach the child verbatim and never read the clipboard, so a
    /// release cannot paste the image a second time.
    #[test]
    fn kitty_ctrl_v_sequences_are_forwarded_without_reading_the_clipboard() {
        for chunk in [
            b"\x1b[118;5u".as_slice(),
            b"\x1b[118;5:1u",
            b"\x1b[118;5:2u",
            b"\x1b[118;5:3u",
        ] {
            let mut filter = RemoteInputFilter::new(true);
            let mut reads = 0;
            let out = filter.process_with_clipboard(chunk, || {
                reads += 1;
                Some(IMAGE_PATH.to_vec())
            });
            assert_eq!(out.bytes, chunk.to_vec(), "{chunk:?}");
            assert_eq!(reads, 0, "{chunk:?}");
        }
    }

    /// A legacy Ctrl+V press followed by its kitty release event pastes the
    /// image exactly once and forwards the release unchanged.
    #[test]
    fn ctrl_v_press_then_release_pastes_once() {
        let mut filter = RemoteInputFilter::new(true);
        let mut reads = 0;
        let mut clipboard = || {
            reads += 1;
            Some(IMAGE_PATH.to_vec())
        };
        let press = filter.process_with_clipboard(b"\x16", &mut clipboard);
        let release = filter.process_with_clipboard(b"\x1b[118;5:3u", &mut clipboard);
        assert_eq!(press.bytes, IMAGE_PATH.to_vec());
        assert_eq!(release.bytes, b"\x1b[118;5:3u".to_vec());
        assert_eq!(reads, 1);
    }

    /// POSIX stdin is a byte stream, so the attach expands Ctrl+V itself.
    #[cfg(unix)]
    #[test]
    fn stdin_source_needs_ctrl_v_expansion() {
        assert!(RawInput::from_fd(0).expands_ctrl_v());
    }

    /// #1373 on Windows: the attach reads the `console_input` reader, which
    /// expands Ctrl+V itself, so the attach's filter must not. A Ctrl+V
    /// event injected into the reader arrives as the reader's own output
    /// (the image path, or 0x16 when the clipboard holds no image) and the
    /// filter forwards it without a second clipboard read.
    #[cfg(windows)]
    #[test]
    fn console_source_expands_ctrl_v_upstream_and_the_filter_passes_it_through() {
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
        let mut input = RawInput::from_console(handle).expect("console receiver");
        assert!(!input.expands_ctrl_v());

        let InputPoll::Chunk(chunk) = input.poll(Duration::from_secs(5)) else {
            panic!("the injected Ctrl+V event never reached the attach");
        };
        let mut filter = RemoteInputFilter::new(input.expands_ctrl_v());
        let mut reads = 0;
        let out = filter.process_with_clipboard(&chunk, || {
            reads += 1;
            Some(IMAGE_PATH.to_vec())
        });
        assert_eq!(out.bytes, chunk);
        assert_eq!(reads, 0);
        assert!(chunk == [0x16] || chunk.ends_with(b".png\n"), "{chunk:?}");
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
