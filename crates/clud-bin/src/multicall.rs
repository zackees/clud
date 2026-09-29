//! `clud` is multicall (#1551): the one executable clud ships answers to every
//! helper name it serves, busybox-style, by dispatching on argv\[0\].
//!
//! [`dispatch`] runs first in `main.rs`, before console setup, clap, tracing
//! or any runtime, so an alias pays only for its own work:
//!
//! ```text
//! stem(argv[0])                                   -> applet
//!   clud-cmd-scan | clud-block-bad-cmd            -> block_bad_cmd::run
//!   clud-shim | every shim_registry::SHIMS name   -> clud_shim::run
//!   anything else                                 -> normal clud
//! ```
//!
//! The stem is argv\[0\]'s file name split on either separator with `.exe`
//! dropped, compared case-insensitively on Windows (NTFS is). It keys on
//! argv\[0\], never `current_exe()`: on Linux that resolves symlinks back to
//! `clud`. `clud __cmd-scan ...` and `clud __shim <name> ...` reach the same
//! applets without depending on argv\[0\].
//!
//! Aliases are materialized by [`place_alias`] into clud-owned directories
//! under `~/.clud/state/` (never the pip scripts dir, which may be
//! read-only): hardlink first, then symlink, then a plain copy as the
//! guaranteed fallback. See DD-121 in `docs/DESIGN_DECISIONS.md`.

use std::ffi::{OsStr, OsString};
use std::io;
use std::path::Path;

use crate::shim_registry;

/// The scanner's canonical alias.
pub const CMD_SCAN: &str = "clud-cmd-scan";
/// The pre-#532 scanner name, still answered so an unmigrated hook config
/// keeps working.
pub const LEGACY_CMD_SCAN: &str = "clud-block-bad-cmd";
/// The shim's own name (`clud-shim --registry`).
pub const SHIM: &str = "clud-shim";
/// `clud __cmd-scan ...`: the scanner without an alias.
pub const CMD_SCAN_SUBCOMMAND: &str = "__cmd-scan";
/// `clud __shim <name> ...`: the shim applet `<name>` without an alias.
pub const SHIM_SUBCOMMAND: &str = "__shim";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Applet {
    CmdScan,
    Shim,
}

/// The applet argv\[0\] selects, or `None` for plain `clud`.
pub fn applet_for(argv0: &OsStr) -> Option<Applet> {
    let name = shim_registry::invoked_name(argv0)?;
    let name = if cfg!(windows) {
        name.to_ascii_lowercase()
    } else {
        name
    };
    match name.as_str() {
        CMD_SCAN | LEGACY_CMD_SCAN => Some(Applet::CmdScan),
        SHIM => Some(Applet::Shim),
        _ if shim_registry::SHIMS.iter().any(|spec| spec.name == name) => Some(Applet::Shim),
        _ => None,
    }
}

/// Run the applet `argv` selects and return its exit code, or `None` when
/// this is an ordinary `clud` invocation.
pub fn dispatch(argv: &[OsString]) -> Option<i32> {
    let argv0 = argv.first()?;
    match applet_for(argv0) {
        Some(Applet::CmdScan) => return Some(run_cmd_scan(&argv[1..])),
        Some(Applet::Shim) => return Some(crate::clud_shim::run(argv)),
        None => {}
    }
    let sub = argv.get(1)?;
    if sub == CMD_SCAN_SUBCOMMAND {
        return Some(run_cmd_scan(&argv[2..]));
    }
    if sub == SHIM_SUBCOMMAND {
        if argv.len() < 3 {
            eprintln!("usage: clud {SHIM_SUBCOMMAND} <name> [args...]");
            return Some(2);
        }
        // `<name>` becomes the shim's argv[0].
        return Some(crate::clud_shim::run(&argv[2..]));
    }
    None
}

fn run_cmd_scan(args: &[OsString]) -> i32 {
    crate::block_bad_cmd::run_with_args(
        args.iter()
            .map(|arg| arg.to_string_lossy().into_owned())
            .collect::<Vec<_>>(),
    )
}

/// Whether `target` still stands for `source`: a symlink to exactly
/// `source`, the same file (a hardlink), or a copy of the same size written
/// no earlier than `source`. Any other symlink is stale, so an installer
/// never writes through a planted link. Replacing
/// `clud` on upgrade gives it a new file identity and a newer mtime, so
/// every alias reads stale on the next launch. Cheap: no 30 MB hash.
pub fn alias_is_current(source: &Path, target: &Path) -> bool {
    if std::fs::symlink_metadata(target).is_ok_and(|meta| meta.file_type().is_symlink()) {
        return std::fs::read_link(target).is_ok_and(|link| link == source);
    }
    let (Ok(src), Ok(dst)) = (std::fs::metadata(source), std::fs::metadata(target)) else {
        return false;
    };
    if !dst.is_file() || src.len() != dst.len() {
        return false;
    }
    if same_file(&src, &dst) {
        return true;
    }
    match (src.modified(), dst.modified()) {
        (Ok(src_time), Ok(dst_time)) => dst_time >= src_time,
        _ => false,
    }
}

#[cfg(unix)]
fn symlink(source: &Path, link: &Path) -> io::Result<()> {
    std::os::unix::fs::symlink(source, link)
}

#[cfg(windows)]
fn symlink(source: &Path, link: &Path) -> io::Result<()> {
    std::os::windows::fs::symlink_file(source, link)
}

#[cfg(unix)]
fn same_file(a: &std::fs::Metadata, b: &std::fs::Metadata) -> bool {
    use std::os::unix::fs::MetadataExt;
    a.dev() == b.dev() && a.ino() == b.ino()
}

#[cfg(not(unix))]
fn same_file(_: &std::fs::Metadata, _: &std::fs::Metadata) -> bool {
    // Hardlinks share size and mtime, so the mtime rule covers them.
    false
}

/// How [`place_alias`] materialized an alias.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Placement {
    Hardlink,
    Symlink,
    Copy,
}

/// Put `source` at `target` (hardlink, else symlink, else copy), atomically
/// through a staging name in `target`'s directory, executable on POSIX. Only
/// a failed copy is an error; a failed hardlink (cross-device, FAT, network
/// share) or symlink (Windows without Developer Mode) falls through silently.
pub fn place_alias(source: &Path, target: &Path) -> io::Result<Placement> {
    place_alias_with(source, target, true)
}

fn place_alias_with(source: &Path, target: &Path, try_links: bool) -> io::Result<Placement> {
    let dir = target
        .parent()
        .ok_or_else(|| io::Error::other("alias target has no parent"))?;
    let file_name = target
        .file_name()
        .ok_or_else(|| io::Error::other("alias target has no file name"))?;
    let staging = dir.join(format!(
        ".{}.{}.tmp",
        file_name.to_string_lossy(),
        std::process::id()
    ));
    let _ = std::fs::remove_file(&staging);
    let placement = if try_links && std::fs::hard_link(source, &staging).is_ok() {
        Placement::Hardlink
    } else if try_links && symlink(source, &staging).is_ok() {
        Placement::Symlink
    } else {
        std::fs::copy(source, &staging)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&staging, std::fs::Permissions::from_mode(0o755))?;
        }
        Placement::Copy
    };
    if let Err(error) = std::fs::rename(&staging, target) {
        let _ = std::fs::remove_file(&staging);
        return Err(error);
    }
    Ok(placement)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_alias_spelling_selects_its_applet() {
        let mut cases: Vec<(String, Option<Applet>)> = vec![
            (CMD_SCAN.into(), Some(Applet::CmdScan)),
            (LEGACY_CMD_SCAN.into(), Some(Applet::CmdScan)),
            (SHIM.into(), Some(Applet::Shim)),
            ("clud".into(), None),
            ("clud-dev".into(), None),
        ];
        for spec in shim_registry::SHIMS {
            cases.push((spec.name.to_string(), Some(Applet::Shim)));
        }
        for (name, expected) in cases {
            let mut spellings = vec![
                name.clone(),
                format!("{name}.exe"),
                format!("{name}.EXE"),
                format!("/usr/local/bin/{name}"),
                format!(r"C:\Users\me\.clud\state\rm-shim\{name}.exe"),
                format!("C:/mixed\\sep/{name}"),
            ];
            if cfg!(windows) {
                spellings.push(name.to_ascii_uppercase());
                spellings.push(format!("{}.Exe", name.to_ascii_uppercase()));
            }
            for spelling in spellings {
                assert_eq!(
                    applet_for(OsStr::new(&spelling)),
                    expected,
                    "argv[0] = {spelling}"
                );
            }
        }
    }

    #[test]
    fn plain_clud_is_not_dispatched() {
        assert_eq!(dispatch(&[OsString::from("clud")]), None);
        assert_eq!(
            dispatch(&[OsString::from("clud"), OsString::from("--version")]),
            None
        );
        assert_eq!(dispatch(&[]), None);
    }

    #[test]
    fn place_alias_hardlinks_and_detects_upgrade() {
        let tmp = tempfile::tempdir().unwrap();
        let source = tmp.path().join("clud");
        std::fs::write(&source, b"v1").unwrap();
        let alias_dir = tmp.path().join("aliases");
        std::fs::create_dir_all(&alias_dir).unwrap();
        let target = alias_dir.join(CMD_SCAN);

        assert!(!alias_is_current(&source, &target));
        let placement = place_alias(&source, &target).unwrap();
        assert_eq!(placement, Placement::Hardlink);
        assert!(alias_is_current(&source, &target));
        assert_eq!(std::fs::read(&target).unwrap(), b"v1");

        // An upgrade writes a new file in place of the old one.
        std::fs::remove_file(&source).unwrap();
        std::fs::write(&source, b"v2-longer").unwrap();
        assert!(!alias_is_current(&source, &target));
        place_alias(&source, &target).unwrap();
        assert!(alias_is_current(&source, &target));
        assert_eq!(std::fs::read(&target).unwrap(), b"v2-longer");
    }

    #[test]
    fn a_copy_is_current_until_the_source_changes() {
        let tmp = tempfile::tempdir().unwrap();
        let source = tmp.path().join("clud");
        std::fs::write(&source, b"v1").unwrap();
        let target = tmp.path().join("copy");
        std::fs::copy(&source, &target).unwrap();
        assert!(alias_is_current(&source, &target));

        std::fs::write(&source, b"v2").unwrap();
        let later = std::time::SystemTime::now() + std::time::Duration::from_secs(60);
        std::fs::File::options()
            .write(true)
            .open(&source)
            .unwrap()
            .set_modified(later)
            .unwrap();
        assert!(!alias_is_current(&source, &target));
    }

    #[test]
    fn copy_fallback_is_a_fresh_executable_copy() {
        let tmp = tempfile::tempdir().unwrap();
        let source = tmp.path().join("clud");
        std::fs::write(&source, b"bin").unwrap();
        let target = tmp.path().join("alias");
        assert_eq!(
            place_alias_with(&source, &target, false).unwrap(),
            Placement::Copy
        );
        assert_eq!(std::fs::read(&target).unwrap(), b"bin");
        assert!(shim_registry::executable(&target));
        assert!(alias_is_current(&source, &target));
    }

    #[cfg(unix)]
    #[test]
    fn a_symlink_alias_is_current_only_when_it_points_at_clud() {
        let tmp = tempfile::tempdir().unwrap();
        let source = tmp.path().join("clud");
        std::fs::write(&source, b"bin").unwrap();
        let other = tmp.path().join("other");
        std::fs::write(&other, b"bin").unwrap();
        let alias = tmp.path().join("rm");
        std::os::unix::fs::symlink(&source, &alias).unwrap();
        assert!(alias_is_current(&source, &alias));
        let planted = tmp.path().join("gh");
        std::os::unix::fs::symlink(&other, &planted).unwrap();
        assert!(!alias_is_current(&source, &planted));
        // Replacing it swaps the entry, never writing through the link.
        place_alias(&source, &planted).unwrap();
        assert!(alias_is_current(&source, &planted));
        assert!(!std::fs::symlink_metadata(&planted)
            .unwrap()
            .file_type()
            .is_symlink());
    }
}
