//! Unix privacy checks and pipe handle conversion for test capture.

#[cfg(not(target_os = "macos"))]
use std::fs;
use std::fs::File;
use std::io;
use std::os::fd::OwnedFd;
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

    #[cfg(target_os = "linux")]
    #[test]
    fn escaped_writer_makes_timeout_cleanup_uncertain() {
        let root = private_root();
        let command = vec![
            "sh".into(),
            "-c".into(),
            "setsid sh -c 'sleep 2' & printf ready".into(),
        ];
        let result = capture(root.path(), Duration::from_millis(250), &command).unwrap();
        assert_eq!(result.state, "timeout");
        assert!(!result.pipe_eof);
        assert_eq!(result.tree_cleanup, "unverified");
        assert_eq!(fs::read(result.artifact).unwrap(), b"ready");
    }

    #[test]
    fn retained_pipe_writer_causes_bounded_incomplete_result() {
        let root = private_root();
        let command = vec!["sh".into(), "-c".into(), "(sleep 5) & printf ready".into()];
        let result = capture(root.path(), Duration::from_millis(250), &command).unwrap();
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
