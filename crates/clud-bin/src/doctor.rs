//! `clud doctor` (#1954): report hook and alias health that a launch cannot
//! see. Every alias in clud's own alias dirs must be the running build, no
//! retired alias may remain, and the first `clud-cmd-scan` on PATH must be
//! this build too. The hook matcher coverage is printed for reference.

use std::path::{Path, PathBuf};

use crate::hook_handshake::fingerprint;

/// One problem `clud doctor` reports.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Finding {
    /// An alias whose build differs from the running `clud`.
    Stale { path: PathBuf, found: String },
    /// A retired alias name still on disk.
    Retired { path: PathBuf },
    /// The first `clud-cmd-scan` on PATH is another build.
    ForeignScanner { path: PathBuf, found: String },
}

impl Finding {
    /// The file the finding is about.
    pub fn path(&self) -> &Path {
        match self {
            Self::Stale { path, .. } | Self::Retired { path } => path,
            Self::ForeignScanner { path, .. } => path,
        }
    }
}

impl std::fmt::Display for Finding {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let path = self.path().display();
        match self {
            Self::Stale { found, .. } => {
                let note = format!("build {found}");
                write!(f, "stale alias {path} ({note})")
            }
            Self::Retired { .. } => {
                let note = "remains";
                write!(f, "retired alias {path} {note}")
            }
            Self::ForeignScanner { found, .. } => {
                let note = "hooks relay to the launcher";
                write!(f, "first clud-cmd-scan on PATH is {path} (build {found}); {note}")
            }
        }
    }
}

/// Check `names` and `retired` in `dir` against the running build `own`.
pub fn inspect_dir(dir: &Path, names: &[String], retired: &[&str], own: &str) -> Vec<Finding> {
    let mut findings = Vec::new();
    for name in names {
        let path = dir.join(crate::shim_registry::file_name(name));
        if let Some(found) = fingerprint(&path).filter(|found| found != own) {
            findings.push(Finding::Stale { path, found });
        }
    }
    for name in retired {
        let path = dir.join(crate::shim_registry::file_name(name));
        if std::fs::symlink_metadata(&path).is_ok() {
            findings.push(Finding::Retired { path });
        }
    }
    findings
}

/// Every finding for this machine.
pub fn inspect(exe: &Path, own: &str, home: Option<&Path>, path_env: &str) -> Vec<Finding> {
    let scanners = vec![
        crate::multicall::CMD_SCAN.to_string(),
        crate::multicall::LEGACY_CMD_SCAN.to_string(),
    ];
    let retired = crate::shim_install::RETIRED_ALIASES;
    let mut findings = Vec::new();
    if let Some(home) = home {
        let shims = home.join(crate::shim_install::SHIMS_SUBDIR);
        let names = crate::shim_install::alias_names();
        findings.extend(inspect_dir(&shims, &names, retired, own));
        let helper = home.join(".clud/state/helper-bin");
        findings.extend(inspect_dir(&helper, &scanners, retired, own));
    }
    if let Some(beside) = exe.parent() {
        let mut names = scanners.clone();
        names.push(crate::multicall::SHIM.to_string());
        findings.extend(inspect_dir(beside, &names, &[], own));
    }
    let scanner = crate::shim_registry::file_name(crate::multicall::CMD_SCAN);
    if let Some(first) = crate::shim_resolve::which(&scanner, path_env) {
        if let Some(found) = fingerprint(&first).filter(|found| found != own) {
            if !findings.iter().any(|f| f.path() == first) {
                findings.push(Finding::ForeignScanner { path: first, found });
            }
        }
    }
    findings
}

/// `clud doctor`: print the report; exit 1 when anything was found.
pub fn run() -> i32 {
    let Ok(exe) = std::env::current_exe() else {
        eprintln!("clud doctor: cannot resolve the running executable");
        return 2;
    };
    let Some(own) = fingerprint(&exe) else {
        eprintln!("clud doctor: cannot stat {}", exe.display());
        return 2;
    };
    println!("clud {} (build {own})", exe.display());
    println!("hook matcher: {}", crate::clud_hooks_compile::guarded_tools_matcher());
    let home = crate::home::user_home();
    let path_env = std::env::var("PATH").unwrap_or_default();
    let findings = inspect(&exe, &own, home.as_deref(), &path_env);
    if findings.is_empty() {
        println!("aliases: all current");
        return 0;
    }
    for finding in &findings {
        println!("problem: {finding}");
    }
    println!("a normal `clud` launch refreshes clud's own aliases and removes retired ones");
    1
}

#[cfg(test)]
#[path = "doctor_tests.rs"]
mod tests;
