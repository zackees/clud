//! Set the console window title to `clud <cwd-name>` on launch and keep
//! it pinned for the lifetime of the process.
//!
//! On Windows, when `clud` runs in cmd.exe / Windows Terminal, the title
//! bar otherwise shows the host shell's title — usually a generic
//! `Command Prompt` or the path to cmd.exe. Stamping `clud <cwd-name>`
//! makes it obvious at a glance which window is the active session and
//! which directory it's working in.
//!
//! Just stamping once isn't enough: clud spawns a TUI backend
//! (claude.exe / codex.exe) that — along with any tool subprocess it
//! invokes (git, npm, build runners) — emits OSC 0/2 title-set escape
//! sequences continuously. Two complementary defenses keep our title
//! visible:
//!
//! 1. [`keep_setting_in_background`] starts a low-frequency poller that
//!    re-applies our title whenever the live console title drifts. This
//!    is the only option in subprocess mode (the default Claude path on
//!    Windows) because the child inherits clud's stdio handles directly
//!    — we can't intercept its OSC bytes.
//!
//! 2. [`OscTitleStripper`] is a stream-resumable byte filter used by the
//!    PTY pump (`session.rs`) to drop OSC 0/2 sequences from the child's
//!    output before they reach our terminal. PTY mode is opt-in
//!    (`--pty`) and used by `clud loop` on POSIX. With the stripper in
//!    place the title doesn't flicker — the keeper rarely fires.
//!
//! POSIX terminals are out of scope for the title-setting half; the
//! cross-platform stubs here are no-ops so the call sites in `main.rs`
//! don't need a `cfg`. The OSC stripper is platform-agnostic because
//! the PTY pump runs on every platform.

use std::sync::{Arc, Mutex, OnceLock};

/// The title we want the console to display, shared between the
/// foreground thread and the keeper thread. Empty string means
/// `set_for_current_cwd` was never called and the keeper should idle.
fn desired_title_cell() -> &'static Arc<Mutex<String>> {
    static CELL: OnceLock<Arc<Mutex<String>>> = OnceLock::new();
    CELL.get_or_init(|| Arc::new(Mutex::new(String::new())))
}

/// Set the console title to `clud <cwd-basename>` for the current
/// working directory and record it so `keep_setting_in_background` can
/// re-apply it after drift. Best-effort — failures are silent.
///
/// Called once near the top of `main`.
pub fn set_for_current_cwd() {
    let basename = current_cwd_basename().unwrap_or_else(|| "?".to_string());
    let title = title_for_cwd_name(&basename);
    *desired_title_cell()
        .lock()
        .expect("desired title mutex poisoned") = title.clone();
    set_title(&title);
}

/// Spawn a Windows-only daemon thread that re-applies the desired title
/// every ~750 ms whenever the live console title has drifted away. No-op
/// if `set_for_current_cwd` was never called (desired title is empty).
///
/// Idempotent: the `OnceLock` guarantees at most one keeper thread per
/// process, even if this is called more than once.
///
/// The thread is a daemon — it has no join handle and runs until
/// process exit. The 750 ms cadence is a tradeoff: short enough that
/// drift is corrected within a noticeable beat, long enough that
/// re-stamping doesn't visibly compete with a child that legitimately
/// changes the title (e.g. the OSC stripper covers PTY mode, the keeper
/// is the safety net for subprocess mode).
pub fn keep_setting_in_background() {
    static STARTED: OnceLock<()> = OnceLock::new();
    STARTED.get_or_init(spawn_keeper_thread);
}

#[cfg(windows)]
fn spawn_keeper_thread() {
    let _ = std::thread::Builder::new()
        .name("clud-title-keeper".into())
        .spawn(|| loop {
            let want = desired_title_cell()
                .lock()
                .expect("desired title mutex poisoned")
                .clone();
            if !want.is_empty() {
                let now = read_console_title();
                if now.as_deref() != Some(want.as_str()) {
                    set_title(&want);
                }
            }
            std::thread::sleep(std::time::Duration::from_millis(750));
        });
}

#[cfg(not(windows))]
fn spawn_keeper_thread() {
    // Title management is out of scope on POSIX (matches the per-call
    // `set_title` no-op). Don't spawn a thread that does nothing.
}

#[cfg(windows)]
fn read_console_title() -> Option<String> {
    extern "system" {
        fn GetConsoleTitleW(buf: *mut u16, size: u32) -> u32;
    }
    let mut buf: Vec<u16> = vec![0; 1024];
    // SAFETY: `buf` is a valid, mutable, aligned u16 buffer of length
    // 1024. GetConsoleTitleW writes at most `size` u16s to `buf`.
    let n = unsafe { GetConsoleTitleW(buf.as_mut_ptr(), buf.len() as u32) };
    if n == 0 {
        // 0 = empty title or error (e.g. no console attached). Treat as
        // unknown so the keeper doesn't try to re-stamp.
        return None;
    }
    Some(String::from_utf16_lossy(&buf[..n as usize]))
}

/// Format `<cwd-name>` into the canonical title string. Pure helper —
/// unit-tested on every platform.
pub fn title_for_cwd_name(cwd_name: &str) -> String {
    format!("clud {}", cwd_name)
}

/// Best-effort lookup of the current working directory's leaf name.
/// Returns `None` if `current_dir()` fails or the path has no final
/// component (e.g. the filesystem root on Windows like `C:\`).
fn current_cwd_basename() -> Option<String> {
    let cwd = std::env::current_dir().ok()?;
    cwd.file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .or_else(|| {
            // On `C:\` (drive root) there's no `file_name`; fall back to
            // the drive letter so the title is still informative.
            cwd.to_string_lossy()
                .split(':')
                .next()
                .map(|s| s.to_string())
        })
}

#[cfg(windows)]
fn set_title(title: &str) {
    // SetConsoleTitleW writes to the console window owning this
    // process — which is the cmd.exe / Windows Terminal we want. Wide
    // (UTF-16) form so non-ASCII cwd names render correctly.
    extern "system" {
        fn SetConsoleTitleW(lp_console_title: *const u16) -> i32;
    }
    let mut wide: Vec<u16> = title.encode_utf16().collect();
    wide.push(0);
    // SAFETY: `wide` is a properly null-terminated UTF-16 buffer with
    // a stable address until the unsafe block ends.
    unsafe {
        let _ = SetConsoleTitleW(wide.as_ptr());
    }
}

#[cfg(not(windows))]
fn set_title(_title: &str) {
    // Out of scope per the issue; intentional no-op.
}

// ─── OSC 0/2 stream filter ──────────────────────────────────────────────

/// Stream-resumable filter that drops OSC 0 and OSC 2 (window-title)
/// escape sequences from a byte stream and passes everything else
/// through verbatim.
///
/// OSC syntax: `ESC ] Ps ; Pt ST` where `ST` is `BEL` (0x07) or
/// `ESC \\` (0x1B 0x5C). OSC 0 sets icon name + window title; OSC 2
/// sets only the window title. Other OSC numbers (8 hyperlinks, 10/11
/// colors, 52 clipboard, 133 prompt marks, …) pass through.
///
/// The filter survives across `process()` calls: an OSC sequence split
/// across reads is handled correctly, including ST split between two
/// chunks.
pub struct OscTitleStripper {
    state: OscState,
    /// Buffered digits between `ESC ]` and `;`. Used to decide swallow
    /// vs passthrough once the `;` arrives.
    digits: Vec<u8>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum OscState {
    Normal,
    AfterEsc,
    InOscNumber,
    SwallowOscBody,
    SwallowAfterEsc,
    PassthroughOscBody,
    PassthroughAfterEsc,
}

impl OscTitleStripper {
    pub fn new() -> Self {
        Self {
            state: OscState::Normal,
            digits: Vec::new(),
        }
    }

    /// Process a chunk and return the bytes that should be forwarded
    /// downstream (terminal stdout, in production).
    pub fn process(&mut self, chunk: &[u8]) -> Vec<u8> {
        let mut out = Vec::with_capacity(chunk.len());
        for &b in chunk {
            self.process_byte(b, &mut out);
        }
        out
    }

    fn process_byte(&mut self, b: u8, out: &mut Vec<u8>) {
        match self.state {
            OscState::Normal => {
                if b == 0x1b {
                    self.state = OscState::AfterEsc;
                } else {
                    out.push(b);
                }
            }
            OscState::AfterEsc => match b {
                b']' => {
                    self.state = OscState::InOscNumber;
                    self.digits.clear();
                }
                0x1b => {
                    // ESC ESC: emit the first ESC, stay waiting on the second.
                    out.push(0x1b);
                }
                _ => {
                    out.push(0x1b);
                    out.push(b);
                    self.state = OscState::Normal;
                }
            },
            OscState::InOscNumber => {
                if b.is_ascii_digit() {
                    self.digits.push(b);
                } else if b == b';' {
                    if self.digits == b"0" || self.digits == b"2" {
                        self.state = OscState::SwallowOscBody;
                    } else {
                        out.push(0x1b);
                        out.push(b']');
                        out.extend_from_slice(&self.digits);
                        out.push(b';');
                        self.state = OscState::PassthroughOscBody;
                    }
                    self.digits.clear();
                } else if b == 0x07 {
                    // BEL with empty/numeric body and no `;` — terminator
                    // for a malformed OSC. Drop quietly; nothing visible
                    // was set.
                    self.digits.clear();
                    self.state = OscState::Normal;
                } else if b == 0x1b {
                    // ESC inside the number — could be the start of an
                    // ST (`ESC \\`). Emit prefix as passthrough so we
                    // don't lose the sequence on a real terminal.
                    out.push(0x1b);
                    out.push(b']');
                    out.extend_from_slice(&self.digits);
                    self.digits.clear();
                    self.state = OscState::PassthroughAfterEsc;
                } else {
                    // Non-digit, non-`;` byte. Bogus OSC — flush prefix
                    // and that byte, switch to passthrough until ST.
                    out.push(0x1b);
                    out.push(b']');
                    out.extend_from_slice(&self.digits);
                    out.push(b);
                    self.digits.clear();
                    self.state = OscState::PassthroughOscBody;
                }
            }
            OscState::SwallowOscBody => match b {
                0x07 => self.state = OscState::Normal,
                0x1b => self.state = OscState::SwallowAfterEsc,
                _ => {}
            },
            OscState::SwallowAfterEsc => match b {
                b'\\' | 0x07 => self.state = OscState::Normal,
                0x1b => {} // stay
                _ => self.state = OscState::SwallowOscBody,
            },
            OscState::PassthroughOscBody => {
                out.push(b);
                match b {
                    0x07 => self.state = OscState::Normal,
                    0x1b => self.state = OscState::PassthroughAfterEsc,
                    _ => {}
                }
            }
            OscState::PassthroughAfterEsc => {
                out.push(b);
                if b == b'\\' || b == 0x07 {
                    self.state = OscState::Normal;
                } else if b != 0x1b {
                    self.state = OscState::PassthroughOscBody;
                }
            }
        }
    }
}

impl Default for OscTitleStripper {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn title_uses_clud_prefix_with_cwd_name() {
        assert_eq!(title_for_cwd_name("clud"), "clud clud");
        assert_eq!(title_for_cwd_name("my-app"), "clud my-app");
    }

    #[test]
    fn title_handles_non_ascii_cwd_name() {
        // Cyrillic + emoji to verify the formatter doesn't choke on
        // non-ASCII paths (which then flow through SetConsoleTitleW's
        // wide-string conversion on Windows).
        assert_eq!(title_for_cwd_name("проект"), "clud проект");
        assert_eq!(title_for_cwd_name("🚀"), "clud 🚀");
    }

    #[test]
    fn title_passes_through_empty_name() {
        // Defensive: even an empty cwd name produces a well-formed
        // title rather than panicking.
        assert_eq!(title_for_cwd_name(""), "clud ");
    }

    #[test]
    fn title_does_not_trim_or_normalize_input() {
        // We're explicit about what we format — no surprise trims, no
        // case changes. The cwd basename is shown verbatim.
        assert_eq!(title_for_cwd_name(" spaced "), "clud  spaced ");
        assert_eq!(title_for_cwd_name("MIXED-Case"), "clud MIXED-Case");
    }

    #[test]
    fn current_cwd_basename_returns_some_in_test_env() {
        // Cargo runs tests with the manifest dir as cwd, which always
        // has a leaf component (`clud-bin`). Smoke test the helper
        // without asserting the specific value.
        let got = current_cwd_basename();
        assert!(got.is_some(), "expected Some(_) cwd basename in test env");
        assert!(!got.unwrap().is_empty(), "basename should not be empty");
    }

    #[test]
    fn set_for_current_cwd_does_not_panic() {
        // Smoke test on every platform — the POSIX stub is a no-op,
        // and on Windows SetConsoleTitleW returns silently when there
        // is no console (e.g. inside `cargo test` under a CI runner).
        set_for_current_cwd();
    }

    #[test]
    fn set_for_current_cwd_records_desired_title() {
        // The keeper thread reads from this cell to decide whether to
        // re-stamp; if the cell stays empty after `set_for_current_cwd`,
        // the keeper would idle forever. Verify the value is captured.
        set_for_current_cwd();
        let stored = desired_title_cell()
            .lock()
            .expect("desired title mutex")
            .clone();
        assert!(
            stored.starts_with("clud "),
            "desired title should be the formatted form, got {stored:?}"
        );
        assert!(
            !stored.trim_end().eq("clud"),
            "desired title should include a basename component, got {stored:?}"
        );
    }

    #[test]
    fn keep_setting_in_background_is_idempotent_and_does_not_panic() {
        // Calling more than once must not spawn duplicate keeper
        // threads (OnceLock guard) and must not panic on POSIX where
        // the spawn helper is a no-op.
        keep_setting_in_background();
        keep_setting_in_background();
    }

    // ─── OscTitleStripper ──────────────────────────────────────────────

    #[test]
    fn osc_stripper_passthrough_for_plain_bytes() {
        let mut s = OscTitleStripper::new();
        assert_eq!(s.process(b"hello world\n"), b"hello world\n");
    }

    #[test]
    fn osc_stripper_drops_osc_0_with_bel_terminator() {
        let mut s = OscTitleStripper::new();
        let chunk = b"before\x1b]0;child-title\x07after";
        assert_eq!(s.process(chunk), b"beforeafter");
    }

    #[test]
    fn osc_stripper_drops_osc_2_with_st_terminator() {
        let mut s = OscTitleStripper::new();
        // ST = ESC \ (0x1B 0x5C)
        let chunk = b"x\x1b]2;another-title\x1b\\y";
        assert_eq!(s.process(chunk), b"xy");
    }

    #[test]
    fn osc_stripper_passes_through_osc_10_color_query() {
        // OSC 10 is the foreground-color query; vt100-class TUIs send
        // it to discover the terminal palette. Stripping it would hang
        // the child waiting for a reply that never comes.
        let mut s = OscTitleStripper::new();
        let chunk = b"\x1b]10;?\x07";
        assert_eq!(s.process(chunk), b"\x1b]10;?\x07");
    }

    #[test]
    fn osc_stripper_passes_through_osc_8_hyperlink() {
        let mut s = OscTitleStripper::new();
        // OSC 8 ; ; <url> ST <text> OSC 8 ; ; ST
        let chunk = b"\x1b]8;;https://example.com\x1b\\link\x1b]8;;\x1b\\";
        assert_eq!(s.process(chunk), chunk);
    }

    #[test]
    fn osc_stripper_passes_through_osc_133_prompt_marks() {
        let mut s = OscTitleStripper::new();
        let chunk = b"\x1b]133;A\x07$ ls\n\x1b]133;B\x07";
        assert_eq!(s.process(chunk), chunk);
    }

    #[test]
    fn osc_stripper_handles_split_across_chunks() {
        // Worst-case: OSC sequence split into many tiny chunks. The
        // filter must reassemble the prefix decision and the ST
        // detection across calls.
        let mut s = OscTitleStripper::new();
        let mut got = Vec::new();
        for piece in [
            &b"abc\x1b"[..],
            b"]",
            b"0",
            b";",
            b"split-title",
            b"\x07",
            b"xyz",
        ] {
            got.extend(s.process(piece));
        }
        assert_eq!(got, b"abcxyz");
    }

    #[test]
    fn osc_stripper_handles_st_split_across_chunks() {
        let mut s = OscTitleStripper::new();
        let mut got = Vec::new();
        got.extend(s.process(b"\x1b]0;t\x1b"));
        got.extend(s.process(b"\\tail"));
        assert_eq!(got, b"tail");
    }

    #[test]
    fn osc_stripper_handles_back_to_back_title_oscs() {
        let mut s = OscTitleStripper::new();
        let chunk = b"a\x1b]0;t1\x07b\x1b]2;t2\x1b\\c";
        assert_eq!(s.process(chunk), b"abc");
    }

    #[test]
    fn osc_stripper_lone_esc_is_buffered_until_resolved() {
        // A bare ESC by itself isn't emitted until we know whether it's
        // starting an OSC — but if the next byte isn't `]`, both bytes
        // must surface. This protects CSI sequences (ESC [ …) from
        // being eaten.
        let mut s = OscTitleStripper::new();
        assert_eq!(s.process(b"\x1b[31mred\x1b[0m"), b"\x1b[31mred\x1b[0m");
    }

    #[test]
    fn osc_stripper_double_esc_does_not_eat_either() {
        // ESC ESC ] 0 ; … BEL: the first ESC isn't OSC-related, the
        // second one starts an OSC. We must emit the first ESC.
        let mut s = OscTitleStripper::new();
        assert_eq!(s.process(b"\x1b\x1b]0;x\x07"), b"\x1b");
    }

    #[test]
    fn osc_stripper_malformed_osc_with_letter_passes_through() {
        // OSC `]X;` is bogus — but conservatively pass it through
        // rather than swallowing arbitrary bytes that might be
        // user-visible. Real terminals would also pass it through.
        let mut s = OscTitleStripper::new();
        let chunk = b"\x1b]X;weird\x07";
        assert_eq!(s.process(chunk), chunk);
    }

    #[test]
    fn osc_stripper_multidigit_non_title_passthrough() {
        // OSC 52 (clipboard) starts with `5` — must not be confused
        // with `2`. The digit accumulator handles multi-digit numbers.
        let mut s = OscTitleStripper::new();
        let chunk = b"\x1b]52;c;SGVsbG8=\x07";
        assert_eq!(s.process(chunk), chunk);
    }
}
