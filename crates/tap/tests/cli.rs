//! End-to-end coverage of the `tap` binary (#1067 acceptance criteria 1-2).
//!
//! The unit tests in `decision_tests.rs` prove the argv -> decision function.
//! These prove the *binary* behaves as the gate contract requires: that a
//! refusal actually stops the program, and that an allowed command is
//! indistinguishable from running it unwrapped.
//!
//! Nothing here removes anything. #1067: "Never validate a removal guard by
//! performing a removal -- not on a host, not in a container." The refusal
//! cases use paths that are never touched *because* they are refused, and the
//! passthrough cases wrap `echo` and a shell exiting with a chosen code.

use std::path::{Path, PathBuf};
use std::time::Duration;

use running_process::{
    CommandSpec, NativeProcess, ProcessConfig, ReadStatus, StderrMode, StdinMode, StreamKind,
};

/// What `tap` did: its own streams, kept apart.
///
/// stdout and stderr must not be merged here. The contract under test is that
/// an allowed command's stdout is byte-for-byte what it would be unwrapped,
/// while refusals go to stderr -- merging them would make both assertions
/// vacuous.
struct Run {
    code: Option<i32>,
    stdout: String,
    stderr: String,
}

/// Locate the built `tap`.
///
/// `CARGO_BIN_EXE_tap` is baked in at *compile* time, and CI compiles the
/// harnesses on one runner then executes them on another, where that path does
/// not exist -- which is precisely how this test first failed, on all three
/// unit lanes at once with `Spawn(NotFound)`. `CLUD_TEST_BIN_DIR` is the
/// runtime override the exec runner sets for exactly this; the compile-time
/// constant stays as the local fallback, so a plain `cargo test` is unchanged.
/// Same precedence as `crates/clud-bin/tests/common/exe.rs`.
fn tap_binary() -> PathBuf {
    if let Some(dir) = std::env::var_os("CLUD_TEST_BIN_DIR") {
        let name = if cfg!(windows) { "tap.exe" } else { "tap" };
        let candidate = PathBuf::from(dir).join(name);
        if candidate.is_file() {
            return candidate;
        }
    }
    PathBuf::from(env!("CARGO_BIN_EXE_tap"))
}

/// Run the built `tap` rooted at `session_root`.
fn tap(session_root: &Path, args: &[&str]) -> Run {
    let mut argv = vec![tap_binary().display().to_string()];
    argv.extend(args.iter().map(|a| (*a).to_string()));
    run_argv(session_root, argv)
}

/// Spawn `argv` rooted at `session_root` and collect its separated streams.
fn run_argv(session_root: &Path, argv: Vec<String>) -> Run {
    let mut env: Vec<(String, String)> = std::env::vars().collect();
    env.retain(|(key, _)| key != "CLUD_SESSION_ROOT");
    env.push((
        "CLUD_SESSION_ROOT".to_string(),
        session_root.display().to_string(),
    ));

    let process = NativeProcess::new(ProcessConfig {
        command: CommandSpec::Argv(argv),
        cwd: Some(session_root.to_path_buf()),
        env: Some(env),
        capture: true,
        stderr_mode: StderrMode::Pipe,
        creationflags: None,
        create_process_group: false,
        stdin_mode: StdinMode::Null,
        nice: None,
    });
    process.start().expect("tap binary must start");

    let stdout = drain(&process, StreamKind::Stdout);
    let stderr = drain(&process, StreamKind::Stderr);
    let code = process.wait(Some(Duration::from_secs(60))).ok();

    Run {
        code,
        stdout,
        stderr,
    }
}

/// Run an argv **without** `tap`, for comparison.
///
/// The criterion is that an allowed command is indistinguishable from running
/// it unwrapped. Comparing against a hardcoded string would instead assert
/// what `echo` prints, which differs by platform; comparing against the
/// unwrapped run states the actual property and is portable.
fn bare(session_root: &Path, args: &[&str]) -> Run {
    run_argv(
        session_root,
        args.iter().map(|a| (*a).to_string()).collect(),
    )
}

/// The shell that exists on this platform, and how it takes a command.
///
/// `sh` and `echo` are not executables on Windows -- `echo` is a `cmd`
/// builtin -- so spawning them by name fails with NotFound there.
#[cfg(windows)]
const SHELL: [&str; 2] = ["cmd", "/c"];
#[cfg(not(windows))]
const SHELL: [&str; 2] = ["sh", "-c"];

/// Read one stream to EOF, mirroring `clud_hooks_run::drain`.
fn drain(process: &NativeProcess, kind: StreamKind) -> String {
    let mut out = String::new();
    let deadline = std::time::Instant::now() + Duration::from_secs(60);
    loop {
        match process.read_stream(kind, Some(Duration::from_millis(50))) {
            ReadStatus::Line(bytes) => {
                out.push_str(&String::from_utf8_lossy(&bytes));
                out.push('\n');
            }
            ReadStatus::Eof => break,
            ReadStatus::Timeout => {
                if process.returncode().is_some() || std::time::Instant::now() >= deadline {
                    break;
                }
            }
        }
    }
    out
}

/// A directory to treat as the session root. Created, never removed by `tap`.
fn session_root() -> tempfile::TempDir {
    tempfile::tempdir().expect("tempdir")
}

/// Criterion 1: "`tap rm -rf /` and its `$VAR`-expanded equivalents refuse."
///
/// By the time `tap` sees the argv there is no variable left -- the shell
/// already turned `rm -rf "$SP"/` with an unset `SP` into `rm -rf /`. So the
/// expanded equivalent *is* this argv, which is the whole reason the check can
/// be exact.
#[test]
fn refuses_a_removal_of_the_filesystem_root() {
    let root = session_root();
    let output = tap(root.path(), &["rm", "-rf", "/"]);

    assert_ne!(output.code, Some(0), "must not succeed");
    let stderr = &output.stderr;
    assert!(stderr.contains("filesystem root"), "{stderr}");
    assert!(stderr.contains("Restate the command"), "{stderr}");
    // Nothing ran, so nothing was written to stdout.
    assert!(
        output.stdout.is_empty(),
        "a refused command must produce no output"
    );
}

/// The refusal exit code must be distinguishable from anything the wrapped
/// program could return, so a caller can tell "tap refused" from "the command
/// failed".
#[test]
fn a_refusal_exits_126() {
    let root = session_root();
    let output = tap(root.path(), &["rm", "-rf", "/"]);
    assert_eq!(output.code, Some(126));
}

/// Criterion 2: "`tap rm -rf ./scratch` execs and passes through exit code and
/// output byte-for-byte."
///
/// Wrapping `echo` rather than a real `rm`: the property under test is
/// transparency, and proving it does not require destroying anything.
#[test]
fn passes_stdout_through_byte_for_byte() {
    let root = session_root();
    let args = [SHELL[0], SHELL[1], "echo hello-from-tap"];

    let wrapped = tap(root.path(), &args);
    let unwrapped = bare(root.path(), &args);

    // The property is indistinguishability, asserted against the unwrapped
    // run rather than a literal -- `echo` differs by platform, the contract
    // does not.
    assert_eq!(wrapped.stdout, unwrapped.stdout);
    assert_eq!(wrapped.code, unwrapped.code);
    assert!(
        wrapped.stdout.contains("hello-from-tap"),
        "{:?}",
        wrapped.stdout
    );
}

#[test]
fn passes_a_nonzero_exit_code_through() {
    let root = session_root();
    for code in [1, 2, 42] {
        let script = format!("exit {code}");
        let output = tap(root.path(), &[SHELL[0], SHELL[1], &script]);
        assert_eq!(output.code, Some(code), "exit {code} must survive");
    }
}

/// A removal inside the session root is allowed through to the program.
///
/// `rm` is invoked, but on a path that does not exist, so the only thing
/// proven is that `tap` did not stop it -- `rm` reports its own "No such
/// file" and that exit code arrives intact. No file is created and none is
/// removed.
#[test]
fn allows_a_removal_inside_the_session_root() {
    let root = session_root();
    let output = tap(root.path(), &["rm", "definitely-not-present"]);

    let stderr = &output.stderr;
    assert!(
        !stderr.contains("tap: refusing"),
        "tap must not refuse a target inside the root: {stderr}"
    );
    // `rm` ran and made its own decision, which is the point.
    assert_ne!(output.code, Some(126), "126 is tap's refusal code");
}

/// A removal naming a path outside the root is refused, and the message names
/// both paths so an agent can restate the command.
#[test]
fn refuses_a_target_outside_the_session_root() {
    let root = session_root();
    let output = tap(root.path(), &["rm", "-rf", "/etc/passwd"]);

    assert_eq!(output.code, Some(126));
    let stderr = &output.stderr;
    assert!(stderr.contains("outside the session root"), "{stderr}");
    assert!(stderr.contains("/etc/passwd"), "{stderr}");
}

/// Fails closed: an operand list emptied by an expansion is refused rather
/// than passed to `rm`, which would then do nothing useful and mask the bug.
#[test]
fn refuses_a_removal_whose_operands_all_vanished() {
    let root = session_root();
    let output = tap(root.path(), &["rm", "-rf"]);

    assert_eq!(output.code, Some(126));
    let stderr = &output.stderr;
    assert!(stderr.contains("expanded to nothing"), "{stderr}");
}

/// A program `tap` cannot find reports the shell's own "command not found"
/// code, not a refusal -- the two are different failures and a reader has to
/// be able to tell them apart.
#[test]
fn a_missing_program_is_127_not_126() {
    let root = session_root();
    let output = tap(root.path(), &["definitely-not-a-real-program-xyz"]);
    assert_eq!(output.code, Some(127));
}
