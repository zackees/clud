//! Windows-only tests of the subprocess-mode injection path (#1370):
//! `write_records_to_handle`, `write_to_console_input` and
//! `subprocess_console_injector` write into a real console input
//! buffer, and the tests read the records back with `ReadConsoleInputW`.
//!
//! The buffer is this test process's own `CONIN$`. A headless CI runner
//! may start the test binary without a console, so [`conin`] allocates
//! one with `AllocConsole` when opening `CONIN$` fails. If neither
//! works the tests fail loudly instead of skipping: the whole point is
//! that this path runs in CI.
//!
//! The console input buffer is process-wide, so every test holds
//! [`console_lock`] for its whole body.

use std::fs::{File, OpenOptions};
use std::os::windows::io::AsRawHandle;
use std::sync::{Mutex, MutexGuard, OnceLock};

use windows::Win32::Foundation::HANDLE;
use windows::Win32::System::Console::{
    AllocConsole, FlushConsoleInputBuffer, GetNumberOfConsoleInputEvents, GetStdHandle,
    ReadConsoleInputW, SetStdHandle, INPUT_RECORD, STD_INPUT_HANDLE,
};

use super::*;

/// Serializes every test that touches the console input buffer or the
/// standard input handle.
pub(crate) fn console_lock() -> MutexGuard<'static, ()> {
    static LOCK: Mutex<()> = Mutex::new(());
    LOCK.lock().unwrap_or_else(|p| p.into_inner())
}

fn open_conin() -> std::io::Result<File> {
    OpenOptions::new().read(true).write(true).open("CONIN$")
}

/// This process's console input buffer, allocating a console first
/// when the test binary was started without one.
pub(crate) fn conin() -> HANDLE {
    static CONIN: OnceLock<File> = OnceLock::new();
    let file = CONIN.get_or_init(|| {
        open_conin()
            .or_else(|first| {
                // SAFETY: no preconditions; fails harmlessly when a
                // console is already attached.
                let alloc = unsafe { AllocConsole() };
                open_conin().map_err(|second| {
                    std::io::Error::other(format!(
                        "no console input buffer: CONIN$ failed ({first}), \
                         AllocConsole returned {alloc:?}, retry failed ({second})"
                    ))
                })
            })
            .expect("the subprocess-injection tests need a console input buffer")
    });
    HANDLE(file.as_raw_handle())
}

/// A resolver for [`console_input_injector`].
pub(crate) fn conin_resolver() -> std::io::Result<HANDLE> {
    Ok(conin())
}

/// Empty the console input buffer before a test writes to it.
pub(crate) fn flush(handle: HANDLE) {
    // SAFETY: `handle` is a live console input handle.
    unsafe { FlushConsoleInputBuffer(handle) }.expect("FlushConsoleInputBuffer");
}

/// Read every record currently queued, without blocking.
pub(crate) fn drain(handle: HANDLE) -> Vec<INPUT_RECORD> {
    let mut pending: u32 = 0;
    // SAFETY: `handle` is a live console input handle.
    unsafe { GetNumberOfConsoleInputEvents(handle, &mut pending) }
        .expect("GetNumberOfConsoleInputEvents");
    if pending == 0 {
        return Vec::new();
    }
    let mut records = vec![INPUT_RECORD::default(); pending as usize];
    let mut read: u32 = 0;
    // SAFETY: `records` holds `pending` records, which are queued, so the
    // read does not block.
    unsafe { ReadConsoleInputW(handle, &mut records, &mut read) }.expect("ReadConsoleInputW");
    records.truncate(read as usize);
    records
}

/// What a backend reading the console would see typed: the key-down
/// records' characters, with `VK_RETURN` as `\n`.
pub(crate) fn typed_text(records: &[INPUT_RECORD]) -> String {
    let mut units = Vec::new();
    for r in records.iter().filter(|r| r.EventType == KEY_EVENT) {
        // SAFETY: `EventType == KEY_EVENT` selects the `KeyEvent` variant.
        let key = unsafe { r.Event.KeyEvent };
        if !key.bKeyDown.as_bool() {
            continue;
        }
        if key.wVirtualKeyCode == VK_RETURN {
            units.push(u16::from(b'\n'));
        } else {
            // SAFETY: the unicode arm is what `build_input_records` sets.
            units.push(unsafe { key.uChar.UnicodeChar });
        }
    }
    String::from_utf16(&units).expect("typed text is valid UTF-16")
}

/// Every key record as (down, vk, repeat, unicode char).
fn key_fields(records: &[INPUT_RECORD]) -> Vec<(bool, u16, u16, u16)> {
    records
        .iter()
        .filter(|r| r.EventType == KEY_EVENT)
        .map(|r| {
            // SAFETY: filtered to `KEY_EVENT` records.
            let key = unsafe { r.Event.KeyEvent };
            (
                key.bKeyDown.as_bool(),
                key.wVirtualKeyCode,
                key.wRepeatCount,
                // SAFETY: the unicode arm is what `build_input_records` sets.
                unsafe { key.uChar.UnicodeChar },
            )
        })
        .collect()
}

/// Point `STD_INPUT_HANDLE` at `handle` for the duration of `body`.
fn with_stdin(handle: HANDLE, body: impl FnOnce()) {
    // SAFETY: no preconditions.
    let saved = unsafe { GetStdHandle(STD_INPUT_HANDLE) }.unwrap_or_default();
    // SAFETY: `handle` stays open for the process lifetime.
    unsafe { SetStdHandle(STD_INPUT_HANDLE, handle) }.expect("SetStdHandle");
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(body));
    // SAFETY: restores the handle saved above.
    let _ = unsafe { SetStdHandle(STD_INPUT_HANDLE, saved) };
    if let Err(panic) = result {
        std::panic::resume_unwind(panic);
    }
}

const SAMPLE: &str = "C:\\Users\\me\\Документы\\b c.txt\n😀.png ";

#[test]
fn records_written_to_the_console_read_back_unchanged() {
    let _guard = console_lock();
    let h = conin();
    flush(h);

    let bytes = build_input_records(SAMPLE);
    write_records_to_handle(h, &bytes).expect("write to CONIN$");

    let back = drain(h);
    let expected = decode_input_records(&bytes).unwrap();
    assert_eq!(key_fields(&back), key_fields(&expected));
    assert_eq!(typed_text(&back), SAMPLE);
}

#[test]
fn a_misaligned_buffer_is_decoded_not_reinterpreted() {
    let _guard = console_lock();
    let h = conin();
    flush(h);

    let bytes = build_input_records("ab");
    // Shift the records one byte so they start off INPUT_RECORD's 4-byte
    // alignment, as a `&[u8]` is allowed to.
    let mut shifted = vec![0u8; bytes.len() + 4];
    let offset = (1..=4)
        .find(|o| (shifted.as_ptr() as usize + o) % 4 != 0)
        .unwrap();
    shifted[offset..offset + bytes.len()].copy_from_slice(&bytes);
    let misaligned = &shifted[offset..offset + bytes.len()];
    assert_ne!(misaligned.as_ptr() as usize % 4, 0);

    write_records_to_handle(h, misaligned).expect("write misaligned records");
    assert_eq!(typed_text(&drain(h)), "ab");
}

#[test]
fn a_partial_record_is_rejected_and_nothing_is_written() {
    let _guard = console_lock();
    let h = conin();
    flush(h);

    let mut bytes = build_input_records("x");
    bytes.pop();
    let err = write_records_to_handle(h, &bytes).unwrap_err();
    assert_eq!(err.kind(), std::io::ErrorKind::InvalidInput);
    assert!(drain(h).is_empty(), "a rejected buffer must write nothing");
}

#[test]
fn write_to_console_input_targets_standard_input() {
    let _guard = console_lock();
    let h = conin();
    flush(h);

    with_stdin(h, || {
        write_to_console_input(&build_input_records("hi\n")).expect("write via stdin");
        write_to_console_input(&[]).expect("an empty buffer is a no-op");
    });
    assert_eq!(typed_text(&drain(h)), "hi\n");
}

#[test]
fn write_to_console_input_fails_when_stdin_is_not_a_console() {
    let _guard = console_lock();
    let file = tempfile::tempfile().expect("temp file");
    with_stdin(HANDLE(file.as_raw_handle()), || {
        assert!(write_to_console_input(&build_input_records("x")).is_err());
    });
}

#[test]
fn subprocess_console_injector_types_the_joined_paths_into_stdin() {
    let _guard = console_lock();
    let h = conin();
    flush(h);

    let injector = subprocess_console_injector();
    with_stdin(h, || {
        injector(&[
            r"C:\a.txt".to_string(),
            r"D:\dir with space\b.rs".to_string(),
        ]);
    });
    assert_eq!(
        typed_text(&drain(h)),
        "C:\\a.txt\nD:\\dir with space\\b.rs "
    );
}

#[test]
fn subprocess_console_injector_ignores_an_empty_drop() {
    let _guard = console_lock();
    let h = conin();
    flush(h);

    with_stdin(h, || subprocess_console_injector()(&[]));
    assert!(drain(h).is_empty());
}

#[test]
fn console_input_injector_tolerates_an_unresolvable_handle() {
    let injector = console_input_injector(|| Err(std::io::Error::other("no console")));
    injector(&[r"C:\a.txt".to_string()]);
}
