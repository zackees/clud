//! Unix half of `crate::self_install::transaction`'s filesystem checks:
//! owner/mode/link-count checks against the effective uid, private (0700)
//! directory creation, and the `HOME`-based approved destination.

use std::fs::{self, OpenOptions};
use std::os::unix::fs::{DirBuilderExt, MetadataExt, OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};

use crate::self_install::catalog::{Arch, GnuEligibility};

fn euid() -> u32 {
    // SAFETY: geteuid has no preconditions and cannot fail.
    unsafe { libc::geteuid() }
}

/// Opens installer lock files owner-only and without following a symlink.
pub fn restrict_lock_options(options: &mut OpenOptions) {
    options.mode(0o600).custom_flags(libc::O_NOFOLLOW);
}

/// Always false: POSIX has no reparse points (symlinks are checked separately).
pub fn is_reparse_point(_meta: &fs::Metadata) -> bool {
    false
}

/// True when the effective user owns the file and it has exactly one link.
pub fn owned_exclusively(meta: &fs::Metadata) -> bool {
    meta.uid() == euid() && meta.nlink() == 1
}

/// True when the effective user owns the directory and no one else may write it.
pub fn user_controlled_dir(meta: &fs::Metadata) -> bool {
    meta.uid() == euid() && meta.mode() & 0o022 == 0
}

/// The device/inode pair that pins a file's identity across a consent prompt.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileIdentity {
    dev: u64,
    ino: u64,
}

pub fn file_identity(meta: &fs::Metadata) -> FileIdentity {
    FileIdentity {
        dev: meta.dev(),
        ino: meta.ino(),
    }
}

/// Creates the install directory chain with private (0700) permissions.
pub fn create_install_dir(parent: &Path) -> Result<(), String> {
    create_private_dirs(parent)
}

pub(crate) fn create_private_dirs(parent: &Path) -> Result<(), String> {
    let mut missing = Vec::new();
    let mut current = Some(parent);
    while let Some(directory) = current {
        match fs::symlink_metadata(directory) {
            Ok(_) => break,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                missing.push(directory.to_path_buf());
                current = directory.parent();
            }
            Err(error) => return Err(format!("inspect {}: {error}", directory.display())),
        }
    }
    for directory in missing.iter().rev() {
        fs::DirBuilder::new()
            .mode(0o700)
            .create(directory)
            .map_err(|error| {
                format!("create private directory {}: {error}", directory.display())
            })?;
    }
    Ok(())
}

pub fn approved_destination() -> Result<PathBuf, String> {
    let home = std::env::var_os("HOME")
        .map(PathBuf::from)
        .ok_or("HOME is unavailable")?;
    if !home.is_absolute() {
        return Err("HOME must be absolute".into());
    }
    let path = std::env::var_os("PATH").unwrap_or_default();
    for directory in [home.join(".local/bin"), home.join("bin")] {
        let on_path = std::env::split_paths(&path).any(|entry| entry == directory);
        if !on_path {
            continue;
        }
        if let Ok(meta) = fs::symlink_metadata(&directory) {
            if meta.is_dir() && !meta.file_type().is_symlink() && user_controlled_dir(&meta) {
                return Ok(directory.join("clud"));
            }
        }
    }
    Ok(home.join(".local/bin/clud"))
}

pub fn make_executable(path: &Path) -> Result<(), String> {
    fs::set_permissions(path, fs::Permissions::from_mode(0o700))
        .map_err(|error| error.to_string())?;
    Ok(())
}

/// Whether this Linux host can run the GNU build: a non-NixOS host with the
/// standard loader and glibc 2.17+. Never verified off Linux.
#[cfg(all(target_os = "linux", target_env = "gnu"))]
pub fn gnu_eligibility(arch: Arch) -> GnuEligibility {
    use std::ffi::CStr;
    let nixos = std::fs::read_to_string("/etc/os-release")
        .ok()
        .is_some_and(|text| {
            text.lines()
                .any(|line| line.trim_matches('"') == "ID=nixos" || line == "ID=\"nixos\"")
        });
    if nixos {
        return GnuEligibility::Unverified;
    }
    let loader = match arch {
        Arch::X86_64 => "/lib64/ld-linux-x86-64.so.2",
        Arch::Aarch64 => "/lib/ld-linux-aarch64.so.1",
    };
    if !std::path::Path::new(loader).exists() {
        return GnuEligibility::Unverified;
    }
    // SAFETY: glibc returns a process-lifetime NUL-terminated version string.
    let version = unsafe { CStr::from_ptr(libc::gnu_get_libc_version()) }.to_string_lossy();
    let mut parts = version
        .split('.')
        .take(2)
        .filter_map(|part| part.parse::<u64>().ok());
    match (parts.next(), parts.next()) {
        (Some(major), Some(minor)) if (major, minor) >= (2, 17) => GnuEligibility::VerifiedGlibc217,
        _ => GnuEligibility::Unverified,
    }
}

#[cfg(all(target_os = "linux", target_env = "musl"))]
pub fn gnu_eligibility(arch: Arch) -> GnuEligibility {
    let nixos = std::fs::read_to_string("/etc/os-release")
        .ok()
        .is_some_and(|text| {
            text.lines()
                .any(|line| line.trim_matches('"') == "ID=nixos" || line == "ID=\"nixos\"")
        });
    if nixos {
        return GnuEligibility::Unverified;
    }
    let loader = match arch {
        Arch::X86_64 => "/lib64/ld-linux-x86-64.so.2",
        Arch::Aarch64 => "/lib/ld-linux-aarch64.so.1",
    };
    if !std::path::Path::new(loader).exists() {
        return GnuEligibility::Unverified;
    }
    let Ok(process) = crate::subprocess::ManagedSubprocess::start_inheriting_env(
        vec!["/usr/bin/getconf".into(), "GNU_LIBC_VERSION".into()],
        None,
        true,
        None,
    ) else {
        return GnuEligibility::Unverified;
    };
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
    let mut output = Vec::new();
    loop {
        if std::time::Instant::now() >= deadline || output.len() > 64 {
            let _ = process.kill();
            return GnuEligibility::Unverified;
        }
        match process.read_stdout(Some(std::time::Duration::from_millis(50))) {
            running_process::ReadStatus::Line(line) => output.extend_from_slice(&line),
            running_process::ReadStatus::Timeout => {
                let _ = process.poll();
            }
            running_process::ReadStatus::Eof => break,
        }
    }
    if process.wait(Some(std::time::Duration::from_secs(1))) != Ok(0) {
        return GnuEligibility::Unverified;
    }
    let Ok(text) = std::str::from_utf8(&output) else {
        return GnuEligibility::Unverified;
    };
    let Some(version) = text.trim().strip_prefix("glibc ") else {
        return GnuEligibility::Unverified;
    };
    let Some((major, minor)) = version.split_once('.') else {
        return GnuEligibility::Unverified;
    };
    match (major.parse::<u64>(), minor.parse::<u64>()) {
        (Ok(major), Ok(minor)) if (major, minor) >= (2, 17) => GnuEligibility::VerifiedGlibc217,
        _ => GnuEligibility::Unverified,
    }
}

#[cfg(not(target_os = "linux"))]
pub fn gnu_eligibility(_arch: Arch) -> GnuEligibility {
    GnuEligibility::Unverified
}

#[cfg(test)]
mod tests {
    use crate::self_install::transaction::{inspect_destination, recover};
    use std::fs;
    use std::os::unix::fs::symlink;

    #[test]
    fn symlink_destination_and_backup_are_rejected_without_following_them() {
        let dir = tempfile::tempdir().unwrap();
        let other = dir.path().join("other");
        fs::write(&other, b"owned bytes").unwrap();
        let destination = dir.path().join("clud");
        symlink(&other, &destination).unwrap();
        assert!(inspect_destination(&destination, false).is_err());
        fs::remove_file(&destination).unwrap();
        symlink(&other, destination.with_extension("clud-backup")).unwrap();
        assert!(recover(&destination).is_err());
        assert_eq!(fs::read(&other).unwrap(), b"owned bytes");
    }
}
