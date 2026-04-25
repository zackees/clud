//! Per-launch-mode byte injectors for the IDropTarget callback.
//!
//! When a drop arrives at our `IDropTarget` (see [`super::console_drop_target`]),
//! the parsed-and-normalized list of paths needs to be delivered to the
//! backend (claude / codex). The backend can be running in one of two
//! modes:
//!
//! 1. **Subprocess mode** — `clud` spawned the backend as an ordinary
//!    child process inheriting our stdin handle. The drop bytes need to
//!    be written into the *console input buffer* via
//!    `WriteConsoleInputW` so the backend's `ReadFile`/`ReadConsole`
//!    sees them as if the user had typed them.
//! 2. **PTY mode** — `clud` is acting as a PTY pump (see
//!    `crate::session`). The drop bytes need to be written into the PTY
//!    master so the slave side sees them on its TTY.
//!
//! Both modes share the same string contract: paths are joined with
//! newlines (`\n`) and a single trailing space is appended so the
//! backend's prompt does not eat the next user keystroke.
//!
//! ## Why `Vec<u8>` for input records?
//!
//! [`build_input_records`] returns a `Vec<u8>` rather than a
//! `Vec<INPUT_RECORD>` so the unit tests can assert exact byte patterns
//! on every CI host (Linux, macOS, Windows). The actual
//! `WriteConsoleInputW` call lives in the Windows-only thin wrapper
//! [`write_to_console_input`].

use std::io::Write;
use std::sync::{Arc, Mutex};

use super::console_drop_target::DropInjector;

/// Width of one Win32 [`INPUT_RECORD`] on the wire. The struct is:
///
/// ```text
/// struct INPUT_RECORD {
///     WORD EventType;          // 2 bytes  (KEY_EVENT = 0x0001)
///     // 2 bytes of padding for alignment of the union
///     union {
///         KEY_EVENT_RECORD Key {
///             BOOL  bKeyDown;       // 4 bytes
///             WORD  wRepeatCount;   // 2
///             WORD  wVirtualKeyCode;// 2
///             WORD  wVirtualScanCode;// 2
///             union {               // 2 bytes
///                 WCHAR UnicodeChar;
///                 CHAR  AsciiChar;
///             } uChar;
///             DWORD dwControlKeyState; // 4
///         }                          // 16 bytes
///         // …other variants padded to 16 bytes
///     } Event;
/// }
/// ```
///
/// Total = `2 (EventType) + 2 (padding) + 16 (largest union variant) = 20`.
pub const INPUT_RECORD_SIZE: usize = 20;

const KEY_EVENT: u16 = 0x0001;
const VK_RETURN: u16 = 0x0D;

/// Synthesize Win32 `INPUT_RECORD` entries for each character of `s`,
/// returning a `Vec<u8>` ready to hand to `WriteConsoleInputW`.
///
/// Two records per char: `KEY_EVENT` key-down then key-up. Newlines in
/// `s` synthesize a `VK_RETURN` sequence; all other chars use the
/// unicode-char path (`uChar.UnicodeChar`) with `wVirtualKeyCode = 0`.
///
/// Pure function — testable on every host.
pub fn build_input_records(s: &str) -> Vec<u8> {
    if s.is_empty() {
        return Vec::new();
    }

    let utf16: Vec<u16> = s.encode_utf16().collect();
    let mut out: Vec<u8> = Vec::with_capacity(utf16.len() * 2 * INPUT_RECORD_SIZE);

    for &unit in &utf16 {
        let (vk, ch) = if unit == u16::from(b'\n') {
            (VK_RETURN, 0u16)
        } else if unit == u16::from(b'\r') {
            // Skip lone CR — we always emit a VK_RETURN for the LF that
            // follows in normal CRLF text. If a backend sends a bare CR
            // it will be treated as a no-op rather than producing a
            // double-newline.
            continue;
        } else {
            (0u16, unit)
        };
        push_key_record(&mut out, /*key_down=*/ true, vk, ch);
        push_key_record(&mut out, /*key_down=*/ false, vk, ch);
    }

    out
}

fn push_key_record(out: &mut Vec<u8>, key_down: bool, vk: u16, ch: u16) {
    let start = out.len();
    out.resize(start + INPUT_RECORD_SIZE, 0u8);
    let r = &mut out[start..start + INPUT_RECORD_SIZE];

    // EventType: WORD at offset 0
    r[0..2].copy_from_slice(&KEY_EVENT.to_le_bytes());
    // 2 bytes padding at [2..4]
    // bKeyDown: BOOL (4 bytes) at offset 4
    let kd: i32 = if key_down { 1 } else { 0 };
    r[4..8].copy_from_slice(&kd.to_le_bytes());
    // wRepeatCount: WORD at offset 8
    r[8..10].copy_from_slice(&1u16.to_le_bytes());
    // wVirtualKeyCode: WORD at offset 10
    r[10..12].copy_from_slice(&vk.to_le_bytes());
    // wVirtualScanCode: WORD at offset 12
    r[12..14].copy_from_slice(&0u16.to_le_bytes());
    // uChar.UnicodeChar: WCHAR at offset 14
    r[14..16].copy_from_slice(&ch.to_le_bytes());
    // dwControlKeyState: DWORD at offset 16
    r[16..20].copy_from_slice(&0u32.to_le_bytes());
}

/// Join paths into the byte string that is shipped to the backend.
///
/// Contract: each path on its own line (newline-separated), with a
/// **trailing space** so a subsequent backend prompt doesn't fuse with
/// the last path. Empty input yields an empty string.
pub fn join_paths_for_injection(paths: &[String]) -> String {
    if paths.is_empty() {
        return String::new();
    }
    let mut s = paths.join("\n");
    s.push(' ');
    s
}

/// Build a PTY-mode injector: writes the joined paths into the supplied
/// PTY master. The master must be `Send` so the OLE callback (which
/// runs on the GUI thread that owns the console window) can invoke it.
pub fn pty_master_injector(master: Arc<Mutex<Box<dyn Write + Send>>>) -> DropInjector {
    Box::new(move |paths: &[String]| {
        if paths.is_empty() {
            return;
        }
        let payload = join_paths_for_injection(paths);
        if let Ok(mut guard) = master.lock() {
            // Best-effort: a failed write here can't be surfaced to the
            // user from inside the OLE callback. Drop silently.
            let _ = guard.write_all(payload.as_bytes());
            let _ = guard.flush();
        }
    })
}

/// Build a subprocess-mode injector that writes the joined paths into
/// the *console input buffer* via `WriteConsoleInputW`. The backend's
/// `ReadFile`/`ReadConsole` then sees them as if typed.
#[cfg(windows)]
pub fn subprocess_console_injector() -> DropInjector {
    Box::new(move |paths: &[String]| {
        if paths.is_empty() {
            return;
        }
        let payload = join_paths_for_injection(paths);
        let records = build_input_records(&payload);
        // Best-effort — like the PTY path, we can't surface failures
        // from inside the OLE callback.
        let _ = write_to_console_input(&records);
    })
}

/// Windows-only: write a pre-built `INPUT_RECORD` byte buffer to the
/// console input buffer attached to the current process.
///
/// `records_bytes` must be a multiple of [`INPUT_RECORD_SIZE`]; partial
/// records are an error.
#[cfg(windows)]
pub fn write_to_console_input(records_bytes: &[u8]) -> std::io::Result<()> {
    use windows::Win32::System::Console::{
        GetStdHandle, WriteConsoleInputW, INPUT_RECORD, STD_INPUT_HANDLE,
    };

    if records_bytes.is_empty() {
        return Ok(());
    }
    if records_bytes.len() % INPUT_RECORD_SIZE != 0 {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            format!(
                "records buffer length {} is not a multiple of {}",
                records_bytes.len(),
                INPUT_RECORD_SIZE
            ),
        ));
    }

    // SAFETY: INPUT_RECORD is repr(C) and our byte layout matches its
    // wire format exactly (verified by build_input_records tests).
    // Reinterpret the slice for the FFI call.
    let count = records_bytes.len() / INPUT_RECORD_SIZE;
    let records: &[INPUT_RECORD] =
        unsafe { std::slice::from_raw_parts(records_bytes.as_ptr() as *const INPUT_RECORD, count) };

    let stdin = unsafe { GetStdHandle(STD_INPUT_HANDLE) }.map_err(|e| {
        std::io::Error::other(format!("GetStdHandle(STD_INPUT_HANDLE) failed: {e}"))
    })?;

    let mut written: u32 = 0;
    unsafe {
        WriteConsoleInputW(stdin, records, &mut written)
            .map_err(|e| std::io::Error::other(format!("WriteConsoleInputW failed: {e}")))?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn build_input_records_for_simple_ascii() {
        // "hi" → 4 records: h-down, h-up, i-down, i-up.
        let records = build_input_records("hi");
        assert_eq!(records.len(), 4 * INPUT_RECORD_SIZE);

        // Record 0: 'h' down
        assert_eq!(&records[0..2], &KEY_EVENT.to_le_bytes());
        assert_eq!(&records[4..8], &1i32.to_le_bytes()); // bKeyDown = TRUE
        assert_eq!(&records[10..12], &0u16.to_le_bytes()); // VK = 0
        assert_eq!(&records[14..16], &(b'h' as u16).to_le_bytes());

        // Record 1: 'h' up
        let r1 = &records[INPUT_RECORD_SIZE..2 * INPUT_RECORD_SIZE];
        assert_eq!(&r1[0..2], &KEY_EVENT.to_le_bytes());
        assert_eq!(&r1[4..8], &0i32.to_le_bytes()); // bKeyDown = FALSE
        assert_eq!(&r1[14..16], &(b'h' as u16).to_le_bytes());

        // Record 3: 'i' up
        let r3 = &records[3 * INPUT_RECORD_SIZE..4 * INPUT_RECORD_SIZE];
        assert_eq!(&r3[4..8], &0i32.to_le_bytes());
        assert_eq!(&r3[14..16], &(b'i' as u16).to_le_bytes());
    }

    #[test]
    fn build_input_records_handles_unicode() {
        // BMP non-ASCII char — single UTF-16 unit.
        let records = build_input_records("é");
        assert_eq!(records.len(), 2 * INPUT_RECORD_SIZE);
        let down = &records[0..INPUT_RECORD_SIZE];
        let expected_unit: u16 = 0x00E9; // 'é'
        assert_eq!(&down[14..16], &expected_unit.to_le_bytes());
        assert_eq!(&down[10..12], &0u16.to_le_bytes()); // VK = 0 for unicode-char path
    }

    #[test]
    fn build_input_records_handles_supplementary_plane_emoji() {
        // 🚀 (U+1F680) encodes to a surrogate pair in UTF-16. We emit
        // four records — down/up for each surrogate code unit.
        let records = build_input_records("🚀");
        assert_eq!(records.len(), 4 * INPUT_RECORD_SIZE);
        // High surrogate first
        let high = u16::from_le_bytes([records[14], records[15]]);
        assert!((0xD800..=0xDBFF).contains(&high));
        let low = u16::from_le_bytes([
            records[2 * INPUT_RECORD_SIZE + 14],
            records[2 * INPUT_RECORD_SIZE + 15],
        ]);
        assert!((0xDC00..=0xDFFF).contains(&low));
    }

    #[test]
    fn build_input_records_handles_newline() {
        // "a\nb" produces a-down, a-up, RETURN-down, RETURN-up, b-down, b-up.
        let records = build_input_records("a\nb");
        assert_eq!(records.len(), 6 * INPUT_RECORD_SIZE);

        // Record 2 should be VK_RETURN down with UnicodeChar = 0.
        let r2 = &records[2 * INPUT_RECORD_SIZE..3 * INPUT_RECORD_SIZE];
        assert_eq!(&r2[4..8], &1i32.to_le_bytes()); // key down
        assert_eq!(&r2[10..12], &VK_RETURN.to_le_bytes());
        assert_eq!(&r2[14..16], &0u16.to_le_bytes());
    }

    #[test]
    fn build_input_records_skips_bare_cr() {
        // CRLF — we should emit ONE VK_RETURN sequence (for the LF),
        // not two and not a bare-CR no-op.
        let crlf = build_input_records("\r\n");
        assert_eq!(crlf.len(), 2 * INPUT_RECORD_SIZE); // just RETURN-down + RETURN-up
        let r0 = &crlf[0..INPUT_RECORD_SIZE];
        assert_eq!(&r0[10..12], &VK_RETURN.to_le_bytes());
    }

    #[test]
    fn build_input_records_empty_string() {
        assert!(build_input_records("").is_empty());
    }

    #[test]
    fn join_paths_for_injection_single_path_has_trailing_space() {
        let s = join_paths_for_injection(&[r"C:\a.txt".to_string()]);
        assert_eq!(s, "C:\\a.txt ");
    }

    #[test]
    fn join_paths_for_injection_multi_path_uses_newlines_with_trailing_space() {
        let s = join_paths_for_injection(&[r"C:\a.txt".to_string(), r"C:\b.txt".to_string()]);
        assert_eq!(s, "C:\\a.txt\nC:\\b.txt ");
    }

    #[test]
    fn join_paths_for_injection_empty_returns_empty() {
        assert!(join_paths_for_injection(&[]).is_empty());
    }

    #[test]
    fn pty_master_injector_writes_normalized_path_with_trailing_space() {
        // Fake PTY master: an in-memory Vec<u8> behind Arc<Mutex<Box<dyn Write + Send>>>.
        struct VecWriter(Arc<Mutex<Vec<u8>>>);
        impl Write for VecWriter {
            fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
                self.0.lock().unwrap().extend_from_slice(buf);
                Ok(buf.len())
            }
            fn flush(&mut self) -> std::io::Result<()> {
                Ok(())
            }
        }

        let captured: Arc<Mutex<Vec<u8>>> = Arc::new(Mutex::new(Vec::new()));
        let writer: Box<dyn Write + Send> = Box::new(VecWriter(Arc::clone(&captured)));
        let master = Arc::new(Mutex::new(writer));

        let injector = pty_master_injector(master);
        injector(&[r"C:\a.txt".to_string(), r"C:\b.txt".to_string()]);

        let got = captured.lock().unwrap().clone();
        assert_eq!(got, b"C:\\a.txt\nC:\\b.txt ");
    }

    #[test]
    fn pty_master_injector_empty_paths_writes_nothing() {
        struct VecWriter(Arc<Mutex<Vec<u8>>>);
        impl Write for VecWriter {
            fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
                self.0.lock().unwrap().extend_from_slice(buf);
                Ok(buf.len())
            }
            fn flush(&mut self) -> std::io::Result<()> {
                Ok(())
            }
        }
        let captured: Arc<Mutex<Vec<u8>>> = Arc::new(Mutex::new(Vec::new()));
        let writer: Box<dyn Write + Send> = Box::new(VecWriter(Arc::clone(&captured)));
        let master = Arc::new(Mutex::new(writer));

        let injector = pty_master_injector(master);
        injector(&[]);

        assert!(captured.lock().unwrap().is_empty());
    }
}
