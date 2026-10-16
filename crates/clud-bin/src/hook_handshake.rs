//! Hook build handshake (#1954): the hook command is the bare
//! `clud-cmd-scan`, resolved from PATH, so a stale or foreign copy earlier on
//! PATH would enforce an old rule set without anyone noticing. At launch the
//! session's `clud` exports its own path and build fingerprint; each hook run
//! compares them with its own and, on a mismatch, logs `HOOK-SKEW` and relays
//! the call to the launcher's `clud __cmd-scan`, so the session always runs
//! the rules of the clud that started it.

use std::ffi::OsString;
use std::path::{Path, PathBuf};

/// The launching `clud`'s absolute path.
pub const LAUNCHER_ENV: &str = "CLUD_HOOK_LAUNCHER";
/// The launching `clud`'s build fingerprint (see [`fingerprint`]).
pub const BUILD_ENV: &str = "CLUD_HOOK_BUILD";
/// Set on a relayed hook run so a second mismatch never relays again.
pub const RELAYED_ENV: &str = "CLUD_HOOK_RELAYED";

/// This build's fingerprint for the executable at `exe`: the crate version
/// plus the file size. Cheap enough for every hook call (one `stat`), and a
/// different build of the same version almost always differs in size.
pub fn fingerprint(exe: &Path) -> Option<String> {
    let len = std::fs::metadata(exe).ok()?.len();
    Some(format!("{}+{len}", env!("CARGO_PKG_VERSION")))
}

/// What a hook run should do.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Decision {
    /// Evaluate here: no launcher identity, the same build, or already relayed.
    Run,
    /// Relay to this launcher; `own` and `launcher` are the two fingerprints.
    Relay {
        launcher: PathBuf,
        own: String,
        expected: String,
    },
}

/// Facts a decision is made from, injected so it is unit-testable.
#[derive(Debug, Clone, Default)]
pub struct Facts {
    pub own: Option<String>,
    pub expected: Option<String>,
    pub launcher: Option<PathBuf>,
    pub launcher_exists: bool,
    pub relayed: bool,
}

pub fn decide(facts: &Facts) -> Decision {
    let (own, expected, launcher) = match (&facts.own, &facts.expected, &facts.launcher) {
        (Some(own), Some(expected), Some(launcher)) => (own, expected, launcher),
        _ => return Decision::Run,
    };
    if facts.relayed || own == expected || !facts.launcher_exists {
        return Decision::Run;
    }
    Decision::Relay {
        launcher: launcher.clone(),
        own: own.clone(),
        expected: expected.clone(),
    }
}

/// The facts for this process, from its own executable and environment.
pub fn probe() -> Facts {
    let var = |name: &str| std::env::var_os(name).filter(|v| !v.is_empty());
    let launcher = var(LAUNCHER_ENV).map(PathBuf::from);
    let exe = std::env::current_exe().ok();
    Facts {
        own: exe.as_deref().and_then(fingerprint),
        expected: var(BUILD_ENV).map(|v| v.to_string_lossy().into_owned()),
        launcher_exists: launcher.as_deref().is_some_and(Path::is_file),
        launcher,
        relayed: var(RELAYED_ENV).is_some(),
    }
}

/// Export this `clud`'s identity for every hook its session runs.
pub fn export_for_session() {
    let Ok(exe) = std::env::current_exe() else {
        return;
    };
    let Some(build) = fingerprint(&exe) else {
        return;
    };
    // SAFETY: called once at startup, before clud starts any threads that
    // read the environment, like the PATH update in the same startup path.
    unsafe {
        std::env::set_var(LAUNCHER_ENV, &exe);
        std::env::set_var(BUILD_ENV, build);
    }
}

/// Relay a hook run (`args` without argv[0]) to `launcher __cmd-scan`, with
/// [`RELAYED_ENV`] set. `None` when the relay could not start, so the caller
/// evaluates locally rather than failing the tool call.
pub fn relay(launcher: &Path, args: &[OsString]) -> Option<i32> {
    // SAFETY: the hook process is single-threaded at this point.
    unsafe { std::env::set_var(RELAYED_ENV, "1") };
    let mut argv: Vec<OsString> = vec![crate::multicall::CMD_SCAN_SUBCOMMAND.into()];
    argv.extend(args.iter().cloned());
    crate::trampoline::relay_child_and_wait(launcher, &argv).ok()
}

#[cfg(test)]
#[path = "hook_handshake_tests.rs"]
mod tests;
