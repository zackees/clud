//! Post-expansion rm safety. Decisions never spawn; unit builds have no executor.
use std::path::{Path, PathBuf};

use crate::deletion_policy::unsafe_delete_base_reason;

#[derive(Debug, PartialEq, Eq)]
pub struct Approved {
    pub operands: Vec<PathBuf>,
    pub recursive: bool,
    pub force: bool,
    pub verbose: bool,
}

/// Parse only options whose semantics we implement. Never forward raw options.
pub fn decide(args: &[String], cwd: &Path, home: Option<&Path>) -> Result<Approved, String> {
    let mut result = Approved {
        operands: vec![],
        recursive: false,
        force: false,
        verbose: false,
    };
    let mut options = true;
    for arg in args {
        if options && arg == "--" {
            options = false;
            continue;
        }
        if options && arg.starts_with('-') && arg != "-" {
            match arg.as_str() {
                "--recursive" => result.recursive = true,
                "--force" => result.force = true,
                "--verbose" => result.verbose = true,
                "--preserve-root" | "--one-file-system" => {}
                value if !value.starts_with("--") => {
                    for flag in value[1..].chars() {
                        match flag {
                            'r' | 'R' => result.recursive = true,
                            'f' => result.force = true,
                            'v' => result.verbose = true,
                            _ => return Err(format!("unsupported rm option: {arg}")),
                        }
                    }
                }
                _ => return Err(format!("unsupported rm option: {arg}")),
            }
            continue;
        }
        result.operands.push(validate_operand(arg, cwd, home)?);
    }
    if result.operands.is_empty() {
        return Err("rm requires a provable operand".into());
    }
    Ok(result)
}

fn validate_operand(arg: &str, cwd: &Path, home: Option<&Path>) -> Result<PathBuf, String> {
    if let Some(reason) = unsafe_delete_base_reason(arg) {
        return Err(format!("unsafe rm operand {arg:?}: {reason}"));
    }
    // Do not turn unlinking a symlink into deleting its referent. Refuse all
    // final symlinks, including trailing-slash spellings and dangling links.
    let raw = Path::new(arg);
    if raw
        .components()
        .any(|c| c == std::path::Component::ParentDir)
    {
        return Err("rm parent traversal is not provable".into());
    }

    let absolute = if raw.is_absolute() {
        raw.to_path_buf()
    } else {
        cwd.join(raw)
    };
    if !absolute.is_absolute() {
        return Err("rm cwd must be absolute".into());
    }
    if std::fs::symlink_metadata(absolute.as_os_str().to_string_lossy().trim_end_matches('/'))
        .is_ok_and(|m| m.file_type().is_symlink())
    {
        return Err("rm symlink operand is not provable".into());
    }
    // Resolve existing ancestors, including symlinks. NotFound is the only
    // error that permits walking up; permission errors and loops fail closed.
    let mut ancestor = absolute.as_path();
    let mut missing = vec![];
    let canonical = loop {
        match std::fs::canonicalize(ancestor) {
            Ok(path) => break path,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                // A dangling symlink must not be mistaken for a missing file.
                if std::fs::symlink_metadata(ancestor).is_ok() {
                    return Err("unresolvable rm symlink".into());
                }
                missing.push(
                    ancestor
                        .file_name()
                        .ok_or("unprovable rm ancestor")?
                        .to_os_string(),
                );
                ancestor = ancestor.parent().ok_or("unprovable rm ancestor")?;
            }
            Err(error) => return Err(format!("cannot resolve rm operand: {error}")),
        }
    };
    let mut target = canonical;
    for part in missing.iter().rev() {
        target.push(part);
    }
    let text = target.to_str().ok_or("non-UTF8 rm target")?;
    if let Some(reason) = unsafe_delete_base_reason(text) {
        return Err(format!("unsafe canonical rm target: {reason}"));
    }
    if let Some(home) = home {
        let home = std::fs::canonicalize(home).map_err(|e| format!("cannot resolve home: {e}"))?;
        if home.starts_with(&target) {
            return Err("rm targets a home root or its ancestor".into());
        }
    }
    reject_mounts(&target)?;
    Ok(target)
}

#[cfg(target_os = "linux")]
fn reject_mounts(target: &Path) -> Result<(), String> {
    let mounts = std::fs::read_to_string("/proc/self/mountinfo")
        .map_err(|e| format!("cannot establish mount boundaries: {e}"))?;
    for line in mounts.lines() {
        let field = line.split_whitespace().nth(4).ok_or("invalid mountinfo")?;
        let mount = field
            .replace("\\040", " ")
            .replace("\\011", "\t")
            .replace("\\012", "\n")
            .replace("\\134", "\\");
        if Path::new(&mount).starts_with(target) {
            return Err("rm targets or crosses a mount boundary".into());
        }
    }
    Ok(())
}
#[cfg(not(target_os = "linux"))]
fn reject_mounts(_: &Path) -> Result<(), String> {
    Err("rm shim execution is unsupported on this platform".into())
}

/// Literal owner contract: a set key containing uppercase CI, regardless of value.
pub fn ci_present<'a>(keys: impl Iterator<Item = &'a str>) -> bool {
    keys.into_iter().any(|key| key.contains("CI"))
}

/// No environment override: inspect this process's running filesystem.
pub fn docker_detected() -> bool {
    #[cfg(target_os = "linux")]
    {
        use std::os::unix::fs::OpenOptionsExt;
        // Require accessible evidence from this process's actual filesystem.
        // O_NOFOLLOW prevents a marker symlink from supplying evidence.
        std::fs::OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NOFOLLOW)
            .open("/.dockerenv")
            .is_ok_and(|file| file.metadata().is_ok_and(|m| m.is_file()))
    }
    #[cfg(not(target_os = "linux"))]
    {
        false
    }
}

#[derive(Debug, PartialEq, Eq)]
pub enum Action {
    DryRun,
    Execute,
    Deny,
}

pub fn gate(dry_run: bool, ci: bool, docker: bool) -> Action {
    if dry_run {
        Action::DryRun
    } else if ci && docker {
        Action::Execute
    } else {
        Action::Deny
    }
}

/// Read real process facts, then decide. This library never executes rm.
pub fn prepare(args: &[String]) -> Result<(Approved, Action), String> {
    let dry = std::env::var("CLUD_RM_DRY_RUN").ok().as_deref() == Some("1");
    let ci = std::env::vars_os().any(|(key, _)| key.to_string_lossy().contains("CI"));
    let action = gate(dry, ci, docker_detected());
    if action == Action::Deny {
        return Err("real rm requires both a set CI-named variable and detected Docker".into());
    }
    let cwd = std::env::current_dir().map_err(|e| format!("cannot resolve cwd: {e}"))?;
    let home = std::env::var_os("HOME")
        .map(PathBuf::from)
        .ok_or("rm cannot establish the home root")?;
    Ok((decide(args, &cwd, Some(&home))?, action))
}

pub fn deny(reason: &str) -> i32 {
    println!(
        "{}",
        serde_json::json!({"decision":"deny", "reason":reason})
    );
    2
}

pub fn report_dry_run(approved: Approved, action: Action) -> i32 {
    let _ = action;
    println!(
        "{}",
        serde_json::json!({"decision":"allow", "dry_run":true, "operands":approved.operands})
    );
    0
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn complete_gate_truth_table_and_unit_executor_is_always_dry() {
        for dry in [false, true] {
            for ci in [false, true] {
                for docker in [false, true] {
                    let action = gate(dry, ci, docker);
                    let expected = if dry {
                        Action::DryRun
                    } else if ci && docker {
                        Action::Execute
                    } else {
                        Action::Deny
                    };
                    assert_eq!(action, expected);
                    if action != Action::Deny {
                        assert_eq!(
                            report_dry_run(
                                Approved {
                                    operands: vec![],
                                    recursive: false,
                                    force: false,
                                    verbose: false
                                },
                                action
                            ),
                            0
                        );
                    }
                }
            }
        }
        assert!(ci_present(["SPECIAL", "MY_CI_JOB"].into_iter()));
        assert!(ci_present(["SPECIAL"].into_iter()));
        assert!(!ci_present(["ci", "HOME"].into_iter()));
    }
    #[test]
    fn refuses_roots_and_unknown_options_in_guaranteed_dry_run() {
        for operand in [
            "/",
            "//server/share",
            "C:/",
            "/c/",
            "../escape",
            "",
            ".",
            "/tmp/..",
            "/tmp",
            "--no-preserve-root",
            "-z",
        ] {
            assert!(
                decide(&[operand.into()], Path::new("/tmp/work"), None).is_err(),
                "{operand}"
            );
        }
    }
    #[cfg(target_os = "linux")]
    #[test]
    fn validates_every_operand_and_canonical_ancestors() {
        let tmp = tempfile::tempdir().unwrap();
        let args = vec!["-rfv".into(), "--".into(), "missing/leaf".into()];
        let good = decide(&args, tmp.path(), None).unwrap();
        assert!(good.recursive && good.force && good.verbose);
        assert_eq!(report_dry_run(good, Action::DryRun), 0);
        assert!(decide(&["leaf".into(), "/".into()], tmp.path(), None).is_err());
        let safe = tmp.path().join("safe-file");
        std::fs::write(&safe, b"preserve referent").unwrap();
        std::os::unix::fs::symlink(&safe, tmp.path().join("link")).unwrap();
        assert!(decide(&["link".into()], tmp.path(), None).is_err());
        assert!(safe.exists());
        std::os::unix::fs::symlink("/", tmp.path().join("root")).unwrap();
        assert!(decide(&["root".into()], tmp.path(), None).is_err());
        assert!(decide(
            &[tmp.path().to_str().unwrap().into()],
            tmp.path(),
            Some(tmp.path())
        )
        .is_err());
        assert!(reject_mounts(Path::new("/proc")).is_err());
    }
}
