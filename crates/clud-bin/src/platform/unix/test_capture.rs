//! Unix privacy checks and pipe handle conversion for test capture.

#[cfg(not(target_os = "macos"))]
use std::fs;
use std::fs::File;
use std::io::{self, Read};
use std::os::fd::{AsRawFd, OwnedFd};
#[cfg(not(target_os = "macos"))]
use std::os::unix::fs::MetadataExt;
use std::path::Path;

/// Refuse a writable, untrusted or symlinked path before creating artifacts.
#[cfg(not(target_os = "macos"))]
pub fn verify_root(root: &Path) -> io::Result<()> {
    if !root.is_absolute() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "artifact root must be absolute",
        ));
    }
    // SAFETY: geteuid takes no arguments and has no side effects.
    let uid = unsafe { libc::geteuid() };
    for (index, path) in root.ancestors().enumerate() {
        let meta = fs::symlink_metadata(path)?;
        if !meta.is_dir() || meta.file_type().is_symlink() || (meta.uid() != uid && meta.uid() != 0)
        {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "untrusted artifact directory",
            ));
        }
        if index == 0 {
            if meta.uid() != uid || meta.mode() & 0o077 != 0 {
                return Err(io::Error::new(
                    io::ErrorKind::PermissionDenied,
                    "artifact root is not private",
                ));
            }
        } else if meta.mode() & 0o022 != 0 && meta.mode() & 0o1000 == 0 {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "writable artifact ancestor",
            ));
        }
    }
    Ok(())
}

/// Verify the opened regular file before any output reaches it.
#[cfg(not(target_os = "macos"))]
pub fn verify_file(file: &File) -> io::Result<()> {
    // SAFETY: geteuid has no arguments and no side effects.
    let uid = unsafe { libc::geteuid() };
    let meta = file.metadata()?;
    if !meta.is_file() || meta.uid() != uid || meta.mode() & 0o077 != 0 || meta.nlink() != 1 {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "artifact file is not private",
        ));
    }
    Ok(())
}

/// Refuse macOS roots until extended ACLs are verified.
#[cfg(target_os = "macos")]
pub fn verify_root(_root: &Path) -> io::Result<()> {
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "macOS private artifact ACLs are unavailable",
    ))
}

/// Refuse macOS files until extended ACLs are verified.
#[cfg(target_os = "macos")]
pub fn verify_file(_file: &File) -> io::Result<()> {
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "macOS private artifact ACLs are unavailable",
    ))
}

/// Poll the capture pipe so a retained writer cannot prevent reader shutdown.
pub fn read_pipe(reader: &mut os_pipe::PipeReader, data: &mut [u8]) -> io::Result<Option<usize>> {
    let mut descriptor = libc::pollfd {
        fd: reader.as_raw_fd(),
        events: libc::POLLIN,
        revents: 0,
    };
    // SAFETY: descriptor points to one initialized pollfd for the duration of poll.
    let ready = unsafe { libc::poll(&mut descriptor, 1, 100) };
    if ready < 0 {
        return Err(io::Error::last_os_error());
    }
    if ready == 0 {
        return Ok(None);
    }
    if descriptor.revents & libc::POLLNVAL != 0 {
        return Err(io::Error::other("invalid capture pipe"));
    }
    reader.read(data).map(Some)
}

/// Convert the writer to the file type accepted by running-process.
pub fn pipe_writer_file(writer: os_pipe::PipeWriter) -> File {
    let handle: OwnedFd = writer.into();
    File::from(handle)
}

#[cfg(all(test, not(target_os = "macos")))]
mod tests {
    use super::*;
    use crate::test_runtime::capture::capture;
    use std::os::unix::fs::PermissionsExt;
    use std::time::Duration;

    fn private_root() -> tempfile::TempDir {
        let root = tempfile::tempdir().unwrap();
        fs::set_permissions(root.path(), fs::Permissions::from_mode(0o700)).unwrap();
        root
    }

    #[test]
    fn captures_merged_binary_output_and_exact_failure_status() {
        let root = private_root();
        let command = vec![
            "sh".into(),
            "-c".into(),
            "printf 'a\\r\\n\\377'; printf 'err' >&2; exit 7".into(),
        ];
        let result = capture(root.path(), Duration::from_secs(5), &command).unwrap();
        assert_eq!(result.state, "complete");
        assert_eq!(result.exit_code, Some(7));
        assert_eq!(fs::read(result.artifact).unwrap(), b"a\r\n\xfferr");
    }

    #[test]
    fn rejects_untrusted_root_before_start() {
        let root = private_root();
        fs::set_permissions(root.path(), fs::Permissions::from_mode(0o755)).unwrap();
        let command = vec!["sh".into(), "-c".into(), "exit 0".into()];
        assert!(capture(root.path(), Duration::from_secs(1), &command).is_err());
    }

    #[test]
    fn output_limit_reports_the_written_prefix_length() {
        let root = private_root();
        let command = vec!["sh".into(), "-c".into(), "printf 1234567890".into()];
        let result = crate::test_runtime::capture::capture_with_limit(
            root.path(),
            Duration::from_secs(5),
            &command,
            5,
        )
        .unwrap();
        assert_eq!(result.state, "output_limit");
        assert_eq!(result.bytes, Some(5));
        assert_eq!(fs::read(result.artifact).unwrap(), b"12345");
        assert_eq!(result.tree_cleanup, "unverified");
    }

    // Timeout comfortably above scheduler jitter so the child has printed
    // `ready` before the window closes; the writers sleep far longer (#2377).
    const TIMEOUT_WINDOW: Duration = Duration::from_secs(3);

    /// Kills the escaped sleeper recorded in `pid_file`, even if an assertion
    /// panics first, so no 30s process outlives the test.
    #[cfg(target_os = "linux")]
    struct EscapedWriterGuard(std::path::PathBuf);

    #[cfg(target_os = "linux")]
    impl Drop for EscapedWriterGuard {
        fn drop(&mut self) {
            let pid = fs::read_to_string(&self.0)
                .ok()
                .and_then(|text| text.trim().parse::<libc::pid_t>().ok());
            if let Some(pid) = pid.filter(|pid| *pid > 1) {
                // SAFETY: plain kill(2) on a pid this test's own writer recorded.
                unsafe { libc::kill(pid, libc::SIGKILL) };
            }
        }
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn escaped_writer_makes_timeout_cleanup_uncertain() {
        let root = private_root();
        let pid_dir = tempfile::tempdir().unwrap();
        let pid_file = pid_dir.path().join("pid");
        let _guard = EscapedWriterGuard(pid_file.clone());
        let command = vec![
            "sh".into(),
            "-c".into(),
            "setsid sh -c 'echo $$ > \"$1\"; exec sleep 30' sh \"$1\" & printf ready".into(),
            "sh".into(),
            pid_file.to_string_lossy().into_owned(),
        ];
        let result = capture(root.path(), TIMEOUT_WINDOW, &command).unwrap();
        assert_eq!(result.state, "timeout");
        assert!(!result.pipe_eof);
        assert_eq!(result.tree_cleanup, "unverified");
        assert_eq!(fs::read(result.artifact).unwrap(), b"ready");
    }

    #[test]
    fn retained_pipe_writer_causes_bounded_incomplete_result() {
        let root = private_root();
        let command = vec!["sh".into(), "-c".into(), "(sleep 30) & printf ready".into()];
        let result = capture(root.path(), TIMEOUT_WINDOW, &command).unwrap();
        assert_eq!(result.state, "timeout");
        assert_eq!(fs::read(result.artifact).unwrap(), b"ready");
    }
}

#[cfg(all(test, target_os = "macos"))]
mod macos_tests {
    #[test]
    fn private_capture_fails_closed_before_spawn() {
        let root = tempfile::tempdir().expect("temporary root");
        let error = super::verify_root(root.path()).expect_err("ACL verification is unavailable");
        assert_eq!(error.kind(), std::io::ErrorKind::Unsupported);
    }
}

#[cfg(test)]
mod reader_tests {
    use crate::test_runtime::capture::{capture_reader, Event};
    use std::io::Write;
    use std::sync::atomic::Ordering;
    use std::sync::mpsc;
    use std::time::Duration;

    #[test]
    fn reader_stops_and_joins_while_writer_remains_open() {
        let (reader, mut writer) = os_pipe::pipe().unwrap();
        let (receiver, stop, handle) = capture_reader(reader).unwrap();
        writer.write_all(b"ready").unwrap();
        assert!(
            matches!(receiver.recv_timeout(Duration::from_secs(2)), Ok(Event::Data(data)) if data == b"ready")
        );
        stop.store(true, Ordering::Release);
        drop(receiver);
        let (done_tx, done_rx) = mpsc::channel();
        std::thread::spawn(move || {
            let _ = done_tx.send(handle.join());
        });
        assert!(done_rx
            .recv_timeout(Duration::from_secs(2))
            .unwrap()
            .is_ok());
        drop(writer);
    }
}
