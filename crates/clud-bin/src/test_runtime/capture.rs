//! Bounded parent-owned capture for the local test-report tool.

use std::ffi::OsString;
use std::fs::File;
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, RecvTimeoutError};
use std::time::{Duration, Instant};

use running_process::{spawn_argv_with_stdio, SpawnStdio, SpawnedChild, StdioSource};
use serde::Serialize;

const MAX_CAPTURE_BYTES: u64 = 1024 * 1024 * 1024;
const CHUNK_BYTES: usize = 8192;

enum Event {
    Data(Vec<u8>),
    Eof,
    Error,
}

#[derive(Serialize)]
pub(crate) struct CaptureResult {
    schema_version: u8,
    pub(crate) artifact: PathBuf,
    pub(crate) state: &'static str,
    pub(crate) exit_code: Option<i32>,
    pub(crate) bytes: Option<u64>,
    pub(crate) pipe_eof: bool,
    pub(crate) child_exited: bool,
    pub(crate) termination_requested: Option<bool>,
    pub(crate) tree_cleanup: &'static str,
}

/// Capture stdout and stderr to one verified file without passing that file to
/// the child. Success requires both a terminal child status and pipe EOF.
pub(crate) fn capture(
    root: &Path,
    timeout: Duration,
    command: &[String],
) -> io::Result<CaptureResult> {
    capture_with_limit(root, timeout, command, MAX_CAPTURE_BYTES)
}

pub(crate) fn capture_with_limit(
    root: &Path,
    timeout: Duration,
    command: &[String],
    limit: u64,
) -> io::Result<CaptureResult> {
    if command.is_empty() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "missing command",
        ));
    }
    let (mut artifact, path) = create_artifact(root)?;
    let (reader, writer) = os_pipe::pipe()?;
    let writer = crate::platform::test_capture::pipe_writer_file(writer);
    let argv: Vec<OsString> = command.iter().map(OsString::from).collect();
    let stdio = SpawnStdio {
        stdin: StdioSource::Parent,
        stdout: StdioSource::File(&writer),
        stderr: StdioSource::File(&writer),
        ..SpawnStdio::default()
    };
    let mut child = spawn_argv_with_stdio(&argv, None, None, stdio)?;
    drop(writer);
    let rx = capture_reader(reader);
    let deadline = Instant::now()
        .checked_add(timeout)
        .ok_or_else(|| io::Error::from(io::ErrorKind::InvalidInput))?;
    let mut exit_code = None;
    let mut eof = false;
    let mut bytes = 0u64;
    let mut state = "complete";
    while exit_code.is_none() || !eof {
        if Instant::now() >= deadline {
            state = "timeout";
            break;
        }
        if exit_code.is_none() {
            match child.try_wait() {
                Ok(status) => exit_code = status,
                Err(_) => {
                    state = "wait_error";
                    break;
                }
            }
        }
        if eof {
            std::thread::sleep(Duration::from_millis(25));
            continue;
        }
        match rx.recv_timeout(Duration::from_millis(25)) {
            Ok(Event::Data(data)) => {
                let remaining = limit.saturating_sub(bytes);
                let write_len = usize::try_from(remaining)
                    .unwrap_or(usize::MAX)
                    .min(data.len());
                if artifact.write_all(&data[..write_len]).is_err() {
                    state = "write_error";
                    break;
                }
                bytes += write_len as u64;
                if write_len < data.len() {
                    state = "output_limit";
                    break;
                }
            }
            Ok(Event::Eof) => eof = true,
            Ok(Event::Error) => {
                state = "read_error";
                break;
            }
            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) if eof => {}
            Err(RecvTimeoutError::Disconnected) => {
                state = "read_error";
                break;
            }
        }
    }
    if artifact.flush().is_err() {
        state = "write_error";
    }
    let actual_bytes = artifact.metadata().ok().map(|metadata| metadata.len());
    if actual_bytes.is_none() && state == "complete" {
        state = "artifact_metadata_error";
    }
    let termination_requested = if state == "complete" {
        None
    } else {
        Some(stop_incomplete(&mut child, &rx, &mut exit_code, &mut eof))
    };
    drop(rx);
    Ok(CaptureResult {
        schema_version: 1,
        artifact: path,
        state,
        exit_code,
        bytes: actual_bytes,
        pipe_eof: eof,
        child_exited: exit_code.is_some(),
        termination_requested,
        tree_cleanup: "unverified",
    })
}

fn create_artifact(root: &Path) -> io::Result<(File, PathBuf)> {
    crate::platform::test_capture::verify_root(root)?;
    let temporary = tempfile::Builder::new()
        .prefix("local-test-")
        .suffix(".log")
        .tempfile_in(root)?;
    let (artifact, path) = temporary.keep().map_err(|error| error.error)?;
    crate::platform::test_capture::verify_file(&artifact)?;
    Ok((artifact, path))
}

fn capture_reader(mut reader: os_pipe::PipeReader) -> mpsc::Receiver<Event> {
    let (tx, rx) = mpsc::sync_channel::<Event>(4);
    std::thread::spawn(move || loop {
        let mut data = vec![0; CHUNK_BYTES];
        match reader.read(&mut data) {
            Ok(0) => {
                let _ = tx.send(Event::Eof);
                break;
            }
            Ok(n) => {
                data.truncate(n);
                if tx.send(Event::Data(data)).is_err() {
                    break;
                }
            }
            Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
            Err(_) => {
                let _ = tx.send(Event::Error);
                break;
            }
        }
    });
    rx
}

fn stop_incomplete(
    child: &mut SpawnedChild,
    rx: &mpsc::Receiver<Event>,
    exit_code: &mut Option<i32>,
    eof: &mut bool,
) -> bool {
    let requested = child.kill_tree().is_ok();
    let grace = Instant::now() + Duration::from_millis(500);
    while Instant::now() < grace && (exit_code.is_none() || !*eof) {
        if exit_code.is_none() {
            if let Ok(status) = child.try_wait() {
                *exit_code = status;
            }
        }
        if *eof {
            std::thread::sleep(Duration::from_millis(25));
        } else {
            match rx.recv_timeout(Duration::from_millis(25)) {
                Ok(Event::Eof) => *eof = true,
                Ok(Event::Error) | Err(RecvTimeoutError::Disconnected) => break,
                Ok(Event::Data(_)) | Err(RecvTimeoutError::Timeout) => {}
            }
        }
    }
    requested
}

/// Internal JSON interface consumed by the bundled report tool.
pub fn run_cli(root: &Path, timeout_secs: u64, command: &[String]) -> i32 {
    let path = root.to_string_lossy();
    if path.len() > 200 || path.chars().any(|c| c.is_control() || matches!(c, '\u{061c}' | '\u{200e}' | '\u{200f}' | '\u{2028}'..='\u{202e}' | '\u{2066}'..='\u{206f}')) {
        eprintln!("clud test capture: invalid artifact root");
        return 2;
    }
    if timeout_secs == 0 || timeout_secs > 86_400 {
        eprintln!("clud test capture: invalid timeout");
        return 2;
    }
    match capture(root, Duration::from_secs(timeout_secs), command) {
        Ok(result) => match serde_json::to_string(&result) {
            Ok(json) => {
                println!("{json}");
                0
            }
            Err(_) => {
                eprintln!("clud test capture: failed to encode result");
                1
            }
        },
        Err(_) => {
            eprintln!("clud test capture: unable to capture command safely");
            1
        }
    }
}
