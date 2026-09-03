//! `tap` v0 -- the post-expansion command gate (#1067, Phase 4 of #1064).
//!
//! `tap <program> [args...]` inspects the **already-expanded** argv, refuses a
//! removal whose target resolves to a filesystem root, `$HOME`, or anywhere
//! outside the session root, and otherwise runs the program with stdio and
//! exit code passed straight through.
//!
//! Scope is v0 deliberately. [`zackees/tap`](https://github.com/zackees/tap)'s
//! full design -- KDL grammar, FlatBuffers compiler, shim farm, profiles,
//! audit log -- is explicitly not this. What `block_bad_cmd_gate` already
//! expects is a wrapper that can be required as a prefix, and that is what
//! this is: one static binary, no profiles, no shims, no daemon.
//!
//! # What it does not do
//!
//! Depth 1 only. `tap make` does not confine the Makefile; genuine containment
//! of descendants is a sandbox's job (Landlock, a container) and is Phase 5.
//! Redirections belong to the shell, not to the wrapped program, and are
//! covered by `set -u` from Phase 2 instead.

mod decision;

use std::path::PathBuf;
use std::time::Duration;

use decision::{classify, Decision};
use running_process::{CommandSpec, NativeProcess, ProcessConfig, StderrMode, StdinMode};

/// Env var naming the session root a removal must stay inside.
///
/// Set by clud when it launches a gated session. Absent means the caller ran
/// `tap` by hand, and the working directory is the honest default -- refusing
/// outright would make the binary untestable and unusable standalone.
const SESSION_ROOT_ENV: &str = "CLUD_SESSION_ROOT";

/// Exit code for a refusal.
///
/// 126 is "command found but not executable", which is the closest POSIX
/// meaning to "this was not run" and is distinct from any exit code the
/// wrapped program could return for its own reasons.
const REFUSED_EXIT: i32 = 126;

fn main() -> std::process::ExitCode {
    let argv: Vec<String> = std::env::args().skip(1).collect();

    let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    let session_root = std::env::var_os(SESSION_ROOT_ENV)
        .map(PathBuf::from)
        .unwrap_or_else(|| cwd.clone());
    let home = home_dir();

    match classify(&argv, &cwd, &session_root, home.as_deref()) {
        Decision::Refuse(message) => {
            eprintln!("{message}");
            std::process::ExitCode::from(REFUSED_EXIT as u8)
        }
        Decision::Exec => exec(&argv),
    }
}

/// Run the program, passing stdio through and returning its exit code.
///
/// `capture: false` with inherited stdin is what makes this transparent: the
/// child writes to the same terminal this process has, so output is
/// byte-for-byte what it would be unwrapped. A true `execve` would be tidier
/// on Unix, but the repo routes all process execution through
/// `running_process` and a spawn-and-wait is indistinguishable from the
/// caller's side.
fn exec(argv: &[String]) -> std::process::ExitCode {
    let process = NativeProcess::new(ProcessConfig {
        command: CommandSpec::Argv(argv.to_vec()),
        cwd: None,
        env: None,
        capture: false,
        stderr_mode: StderrMode::Pipe,
        creationflags: None,
        create_process_group: false,
        stdin_mode: StdinMode::Inherit,
        nice: None,
    });

    if let Err(error) = process.start() {
        eprintln!("tap: failed to run {}: {error}", argv[0]);
        // 127 is "command not found", the shell's own code for this.
        return std::process::ExitCode::from(127);
    }

    match process.wait(Some(Duration::from_secs(60 * 60 * 24))) {
        Ok(code) => std::process::ExitCode::from(exit_byte(code)),
        Err(error) => {
            eprintln!("tap: {} did not exit cleanly: {error}", argv[0]);
            std::process::ExitCode::from(127)
        }
    }
}

/// Narrow an exit code to the byte a process can actually return.
///
/// A signal death arrives as a negative code on Unix; the shell reports those
/// as `128 + signal`, and matching that keeps `tap cmd` and `cmd` reporting
/// the same thing.
fn exit_byte(code: i32) -> u8 {
    if code < 0 {
        let signal = (-code) as u8;
        return 128u8.saturating_add(signal);
    }
    (code & 0xFF) as u8
}

/// The user's home directory.
///
/// Read directly rather than via a crate: `tap` has one dependency on purpose,
/// and this is the whole of what it needs. `USERPROFILE` first on Windows,
/// where `HOME` is often absent or points somewhere else entirely.
fn home_dir() -> Option<PathBuf> {
    if cfg!(windows) {
        if let Some(profile) = std::env::var_os("USERPROFILE") {
            return Some(PathBuf::from(profile));
        }
    }
    std::env::var_os("HOME").map(PathBuf::from)
}

#[cfg(test)]
mod tests {
    use super::exit_byte;

    /// `tap cmd` must report what `cmd` would have reported.
    #[test]
    fn exit_codes_pass_through() {
        assert_eq!(exit_byte(0), 0);
        assert_eq!(exit_byte(1), 1);
        assert_eq!(exit_byte(2), 2);
        assert_eq!(exit_byte(42), 42);
    }

    /// A signal death is reported the way a shell reports it, so a wrapped
    /// segfault still looks like a segfault.
    #[test]
    fn signal_deaths_become_128_plus_signal() {
        assert_eq!(exit_byte(-11), 139);
        assert_eq!(exit_byte(-9), 137);
    }

    /// Codes outside a byte are truncated the way the OS truncates them,
    /// rather than panicking on the cast.
    #[test]
    fn oversized_codes_are_truncated_not_panicked_on() {
        assert_eq!(exit_byte(256), 0);
        assert_eq!(exit_byte(257), 1);
    }
}
