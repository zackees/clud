use std::collections::BTreeSet;
use std::path::{Component, Path, PathBuf};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RmFlavor {
    Gnu,
    Bsd,
    BusyBox,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Plan {
    pub program: PathBuf,
    pub argv: Vec<String>,
    pub operands: Vec<PathBuf>,
}

fn operands(args: &[String]) -> Result<Vec<String>, String> {
    let mut found = Vec::new();
    let mut options = true;
    for arg in args {
        if options && arg == "--" {
            options = false;
        } else if options && arg == "--no-preserve-root" {
            return Err("the platform root protection cannot be disabled".into());
        } else if !(options && arg.starts_with('-') && arg != "-") {
            found.push(arg.clone());
        }
    }
    Ok(found)
}

pub fn protected_reason(path: &Path, home: Option<&Path>) -> Option<String> {
    if crate::platform::host::WINDOWS {
        if let Some(reason) = windows_catastrophe_reason(&path.to_string_lossy()) {
            return Some(reason.into());
        }
    }
    let mut parts = path.components();
    if matches!(
        (parts.next(), parts.next()),
        (Some(Component::RootDir | Component::Prefix(_)), None)
    ) {
        return Some("filesystem root".into());
    }
    if path.is_absolute() && path.components().count() == 2 {
        return Some("top-level directory".into());
    }
    if let Some(home) = home.and_then(|h| std::fs::canonicalize(h).ok()) {
        if home.starts_with(path) {
            return Some("home directory or its ancestor".into());
        }
    }
    if is_mount_point(path) {
        return Some("mount point".into());
    }
    None
}

fn windows_catastrophe_reason(raw: &str) -> Option<&'static str> {
    let normalized = crate::path_norm::slash_separators(raw);
    let trimmed = normalized.trim_end_matches('/');
    if trimmed.eq_ignore_ascii_case("%USERPROFILE%") {
        return Some("home directory");
    }
    if let Some(unc) = trimmed.strip_prefix("//") {
        let depth = unc.split('/').filter(|part| !part.is_empty()).count();
        return match depth {
            0..=2 => Some("filesystem root"),
            3 => Some("top-level directory"),
            _ => None,
        };
    }
    let bytes = normalized.as_bytes();
    let tail = if bytes.len() >= 3
        && bytes[0].is_ascii_alphabetic()
        && bytes[1] == b':'
        && bytes[2] == b'/'
    {
        &normalized[3..]
    } else if bytes.len() >= 2
        && bytes[0] == b'/'
        && bytes[1].is_ascii_alphabetic()
        && (bytes.len() == 2 || bytes[2] == b'/')
    {
        &normalized[2..]
    } else {
        return None;
    };
    let depth = tail.split('/').filter(|part| !part.is_empty()).count();
    match depth {
        0 => Some("filesystem root"),
        1 => Some("top-level directory"),
        _ => None,
    }
}

fn is_mount_point(path: &Path) -> bool {
    crate::platform::rm_guard::is_mount_point(path)
}

fn contains_nested_mount(path: &Path) -> Result<bool, String> {
    crate::platform::rm_guard::contains_nested_mount(path)
}

fn canonical_operand(raw: &str, cwd: &Path) -> Result<PathBuf, String> {
    let native = if crate::platform::host::WINDOWS {
        crate::rm_tool::msys_drive_path(raw).unwrap_or_else(|| raw.to_owned())
    } else {
        raw.to_owned()
    };
    let given = Path::new(&native);
    if crate::platform::host::WINDOWS && is_mount_point(&cwd.join(given)) {
        return Err("operand is a junction or mount point".into());
    }
    let absolute = if given.is_absolute() {
        given.to_path_buf()
    } else {
        cwd.join(given)
    };
    if raw.ends_with('/') || raw.ends_with('\\') {
        return std::fs::canonicalize(&absolute)
            .map_err(|e| format!("cannot resolve {raw:?}: {e}"));
    }
    if std::fs::symlink_metadata(&absolute).is_ok_and(|m| m.file_type().is_symlink()) {
        let parent = std::fs::canonicalize(absolute.parent().ok_or("operand has no parent")?)
            .map_err(|e| format!("cannot resolve {raw:?}: {e}"))?;
        return Ok(parent.join(absolute.file_name().ok_or("operand has no name")?));
    }
    let mut tail = Vec::new();
    let mut cursor = absolute.as_path();
    loop {
        match std::fs::canonicalize(cursor) {
            Ok(base) => return Ok(tail.iter().rev().fold(base, |path, name| path.join(name))),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                tail.push(
                    cursor
                        .file_name()
                        .ok_or("operand has no existing ancestor")?,
                );
                cursor = cursor.parent().ok_or("operand has no existing ancestor")?;
            }
            Err(error) => return Err(format!("cannot resolve operand: {error}")),
        }
    }
}

fn names_current_or_parent(raw: &str) -> bool {
    matches!(
        raw.trim_end_matches(['/', '\\']).rsplit(['/', '\\']).next(),
        Some("." | "..")
    )
}

/// The first `rm` after the shim's directory on PATH, via the resolver every
/// shim shares ([`crate::shim_registry::next_on_path`]), in strict mode: the
/// in-session floor refuses rather than guess when its own directory is
/// missing from PATH.
pub fn find_handoff(path: &str, shim_exe: &Path) -> Result<PathBuf, String> {
    let session_dir = std::env::var_os(crate::shim_registry::SESSION_DIR_KEY).map(PathBuf::from);
    let home_key = if crate::platform::host::WINDOWS {
        "USERPROFILE"
    } else {
        "HOME"
    };
    let home = std::env::var_os(home_key).map(PathBuf::from);
    find_handoff_with(path, shim_exe, session_dir.as_deref(), home.as_deref())
}

/// [`find_handoff`] over an injected session shim directory and home.
pub fn find_handoff_with(
    path: &str,
    shim_exe: &Path,
    session_dir: Option<&Path>,
    home: Option<&Path>,
) -> Result<PathBuf, String> {
    use crate::shim_registry::{self as registry, NextOnPathError};
    let executable = registry::file_name(concat!("r", "m"));
    // A symlinked alias runs as `clud` (#1746), so its own directory is not
    // an alias directory; the session and home shim directories are.
    let dirs = registry::shim_dirs(shim_exe, session_dir, home);
    registry::next_on_path(
        &executable,
        std::ffi::OsStr::new(path),
        shim_exe,
        &dirs,
        true,
    )
    .map_err(|error| match error {
        NextOnPathError::ShimDirMissing => "the clud shim directory is missing from PATH".into(),
        NextOnPathError::NotFound => {
            "no handoff executable exists after the clud shim on PATH".into()
        }
    })
}

pub fn handoff_args(args: &[String], flavor: RmFlavor) -> Vec<String> {
    let mut out = Vec::new();
    let options: Vec<_> = args.iter().take_while(|arg| arg.as_str() != "--").collect();
    match flavor {
        RmFlavor::Gnu => {
            if !options.iter().any(|a| a.as_str() == "--preserve-root=all") {
                out.push("--preserve-root=all".into());
            }
            if !options.iter().any(|a| a.as_str() == "--one-file-system") {
                out.push("--one-file-system".into());
            }
        }
        RmFlavor::Bsd if !options.iter().any(|a| a.as_str() == "-x") => out.push("-x".into()),
        RmFlavor::Bsd | RmFlavor::BusyBox => {}
    }
    out.extend_from_slice(args);
    out
}

fn covers_visible_home_children(operands: &[PathBuf], home: Option<&Path>) -> bool {
    let Some(home) = home.and_then(|path| std::fs::canonicalize(path).ok()) else {
        return false;
    };
    let Ok(entries) = std::fs::read_dir(&home) else {
        return false;
    };
    let visible: BTreeSet<_> = entries
        .flatten()
        .filter_map(|entry| {
            let name = entry.file_name();
            (!name.to_string_lossy().starts_with('.')).then_some(home.join(name))
        })
        .collect();
    !visible.is_empty() && visible.iter().all(|path| operands.contains(path))
}

pub fn prepare_with(
    args: &[String],
    cwd: &Path,
    home: Option<&Path>,
    path: &str,
    shim: &Path,
    flavor: RmFlavor,
) -> Result<Plan, String> {
    let mut resolved = Vec::new();
    for raw in operands(args)? {
        if crate::platform::host::WINDOWS {
            if let Some(reason) = windows_catastrophe_reason(&raw) {
                return Err(format!("unsafe deletion operand {raw:?}: {reason}"));
            }
            let given = Path::new(&raw);
            let direct = if given.is_absolute() {
                given.to_path_buf()
            } else {
                cwd.join(given)
            };
            if is_mount_point(&direct) {
                return Err(format!("unsafe deletion operand {raw:?}: mount point"));
            }
        }
        if names_current_or_parent(&raw) {
            return Err(format!(
                "unsafe deletion operand {raw:?}: current or parent directory"
            ));
        }
        let target = canonical_operand(&raw, cwd)?;
        if let Some(reason) = protected_reason(&target, home) {
            return Err(format!("unsafe deletion operand {raw:?}: {reason}"));
        }
        if target.is_dir() && contains_nested_mount(&target)? {
            return Err(format!(
                "unsafe deletion operand {raw:?}: directory contains a mount point"
            ));
        }
        resolved.push(target);
    }
    if covers_visible_home_children(&resolved, home) {
        return Err("all visible children of the home directory were selected".into());
    }
    Ok(Plan {
        program: find_handoff(path, shim)?,
        argv: handoff_args(args, flavor),
        operands: resolved,
    })
}

pub fn prepare(args: &[String]) -> Result<Plan, String> {
    let cwd = std::env::current_dir().map_err(|e| format!("cannot resolve cwd: {e}"))?;
    let home_key = if crate::platform::host::WINDOWS {
        "USERPROFILE"
    } else {
        "HOME"
    };
    let home = std::env::var_os(home_key).map(PathBuf::from);
    let path = std::env::var("PATH").unwrap_or_default();
    let shim = std::env::current_exe().map_err(|e| format!("cannot resolve shim: {e}"))?;
    let handoff = find_handoff(&path, &shim)?;
    let resolved_handoff = std::fs::canonicalize(&handoff).unwrap_or_else(|_| handoff.clone());
    let is_busybox = resolved_handoff.file_name().is_some_and(|name| {
        name.to_string_lossy()
            .to_ascii_lowercase()
            .contains("busybox")
    });
    let flavor = if is_busybox {
        RmFlavor::BusyBox
    } else if crate::platform::host::LINUX {
        RmFlavor::Gnu
    } else if crate::platform::host::UNIX {
        RmFlavor::Bsd
    } else {
        RmFlavor::BusyBox
    };
    prepare_with(args, &cwd, home.as_deref(), &path, &shim, flavor)
}

pub fn deny(reason: &str) -> i32 {
    println!(
        "{}",
        serde_json::json!({"decision":"deny", "reason":reason})
    );
    2
}

pub fn audit(args: &[String], plan: Option<&Plan>, exit: i32, reason: Option<&str>) {
    let Ok(state) = crate::daemon::default_state_dir() else {
        return;
    };
    let now = std::time::SystemTime::now();
    let handoff = plan.map(|plan| {
        let mut command = vec![plan.program.to_string_lossy().into_owned()];
        command.extend(plan.argv.iter().cloned());
        command
    });
    let record = serde_json::json!({
        "ts_unix": now.duration_since(std::time::UNIX_EPOCH).unwrap_or_default().as_secs(),
        "command": "r".to_owned() + "m",
        "role": "child",
        "cwd": std::env::current_dir().ok(),
        "operands": args,
        "handoff": handoff,
        "exit": exit,
        "reason": reason,
    });
    crate::rm_tool::append_audit(&state.join("logs").join("rm"), now, &record);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn current_and_parent_operands_are_refused_before_handoff() {
        for operand in [".", "..", "./", "../", "child/.", "child/..", "child\\.."] {
            assert!(names_current_or_parent(operand), "{operand}");
        }
        for operand in ["child", "..hidden", "child/../sibling"] {
            assert!(!names_current_or_parent(operand), "{operand}");
        }
    }

    #[test]
    fn windows_drive_msys_and_unc_roots_have_catastrophe_reasons() {
        for operand in [
            "C:/",
            "C:\\",
            "C:/Windows",
            "C:/Users",
            "/c",
            "/c/Windows",
            "//server/share",
            "%USERPROFILE%",
        ] {
            assert!(windows_catastrophe_reason(operand).is_some(), "{operand}");
        }
        for operand in [
            "C:/Users/alice/build",
            "/c/projects/build",
            "//server/share/work/build",
        ] {
            assert_eq!(windows_catastrophe_reason(operand), None, "{operand}");
        }
    }
}
