#![cfg(windows)]

//! Issue #753 guardrail: prove that the completion suppression actually works
//! against a **real** Git-Bash login shell, by counting the functions the
//! Claude Code snapshot generator would capture.
//!
//! This deliberately asserts an observed function *count* rather than "we set
//! `WINELOADERNOEXEC`". The lever is a variable Git for Windows consults in
//! `/etc/profile.d/git-prompt.sh` (`if test -z "$WINELOADERNOEXEC"`), not a
//! documented API — if they rename or drop that guard, an env-var-presence
//! assertion would keep passing while the 170-spawn-per-tool-call tax silently
//! came back. Counting functions is the only assertion that fails loudly.
//!
//! `NativeProcess` is used per the repo subprocess policy; unlike the reaper
//! fixtures there is nothing here a Job Object would distort — we only read
//! the child's stdout.

use std::path::PathBuf;
use std::time::Duration;

use clud::shell::completion_guard::{env_overrides_for, SUPPRESS_KEY};
use clud::win_creation_flags::invisible_helper_creationflags;
use running_process::{
    CommandSpec, NativeProcess, ProcessConfig, ReadStatus, StderrMode, StdinMode,
};

/// The exact capture pipeline Claude Code's snapshot generator runs, recovered
/// from the shipped `claude.exe`:
///
/// ```text
/// declare -F | cut -d' ' -f3 | grep -vE '^_[^_]' | while read func; do
///   encoded_func=$(declare -f "$func" | base64 )
///   echo "eval \"\$(echo '$encoded_func' | base64 -d)\" ..." >> "$SNAPSHOT_FILE"
/// done
/// ```
///
/// Every surviving name costs two process spawns at replay time, so the count
/// this emits *is* the tax, in units of 2 spawns.
const COUNT_CAPTURED_FUNCTIONS: &str =
    "declare -f >/dev/null 2>&1; declare -F | cut -d' ' -f3 | grep -vE '^_[^_]' | wc -l";

fn find_git_bash() -> Option<PathBuf> {
    if let Ok(explicit) = std::env::var("CLUD_TEST_GIT_BASH") {
        let path = PathBuf::from(explicit);
        if path.is_file() {
            return Some(path);
        }
    }
    let candidates = [
        r"C:\Program Files\Git\bin\bash.exe",
        r"C:\Program Files (x86)\Git\bin\bash.exe",
    ];
    candidates
        .iter()
        .map(PathBuf::from)
        .find(|path| path.is_file())
}

/// Run `script` in a **login** shell (`-l`), the mode Claude Code uses to build
/// the snapshot, and return trimmed stdout.
fn login_shell_output(bash: &PathBuf, script: &str, extra_env: &[(String, String)]) -> String {
    // Inherit the ambient environment, then layer the override. A cleared env
    // would leave bash without SystemRoot/PATH and tell us nothing.
    let mut env: Vec<(String, String)> = std::env::vars()
        .filter(|(key, _)| !extra_env.iter().any(|(k, _)| k == key))
        .collect();
    env.extend(extra_env.iter().cloned());

    let config = ProcessConfig {
        command: CommandSpec::Argv(vec![
            bash.to_string_lossy().into_owned(),
            "-l".to_string(),
            "-c".to_string(),
            script.to_string(),
        ]),
        cwd: None,
        env: Some(env),
        capture: true,
        stderr_mode: StderrMode::Stdout,
        // Piped helper — nobody interacts with this child's console, so
        // suppress the conhost popup (issue #55 pattern).
        creationflags: invisible_helper_creationflags(),
        create_process_group: false,
        stdin_mode: StdinMode::Null,
        nice: None,
    };

    let process = NativeProcess::new(config);
    process.start().expect("failed to start git bash");

    let mut out = String::new();
    loop {
        match process.read_combined(Some(Duration::from_millis(200))) {
            ReadStatus::Line(event) => {
                out.push_str(&String::from_utf8_lossy(&event.line));
                out.push('\n');
            }
            ReadStatus::Timeout => {
                if process.returncode().is_some() {
                    break;
                }
            }
            _ => break,
        }
    }
    let _ = process.wait(Some(Duration::from_secs(30)));
    out.trim().to_string()
}

fn captured_function_count(bash: &PathBuf, extra_env: &[(String, String)]) -> usize {
    let raw = login_shell_output(bash, COUNT_CAPTURED_FUNCTIONS, extra_env);
    raw.lines()
        .rev()
        .find_map(|line| line.trim().parse::<usize>().ok())
        .unwrap_or_else(|| panic!("could not parse a function count from bash output: {raw:?}"))
}

#[test]
fn suppression_collapses_the_snapshot_function_count() {
    let Some(bash) = find_git_bash() else {
        eprintln!("skipping: no Git Bash found (set CLUD_TEST_GIT_BASH to override)");
        return;
    };

    let baseline = captured_function_count(&bash, &[]);

    // A Git Bash whose profile doesn't load completions has no tax to remove,
    // so there is nothing for this guardrail to prove. Skip rather than fail:
    // CI images vary, and a false red here would be noise, not signal.
    if baseline < 20 {
        eprintln!(
            "skipping: login shell captured only {baseline} functions — no completions to suppress"
        );
        return;
    }

    // Use the production policy, not a hand-written literal, so this test
    // breaks if the override we actually ship ever stops matching.
    let overrides = env_overrides_for(true, false);
    assert_eq!(
        overrides,
        vec![(SUPPRESS_KEY.to_string(), "1".to_string())],
        "production override changed — update this guardrail deliberately"
    );

    let guarded = captured_function_count(&bash, &overrides);

    assert!(
        guarded <= 5,
        "expected the login shell to capture almost nothing with {SUPPRESS_KEY} set, \
         got {guarded} (baseline {baseline}). Git for Windows most likely changed the \
         `test -z \"$WINELOADERNOEXEC\"` guard in /etc/profile.d/git-prompt.sh — \
         see crates/clud-bin/src/shell/completion_guard.rs and issue #753."
    );
    assert!(
        guarded * 4 < baseline,
        "suppression must remove the bulk of the captured functions: \
         baseline {baseline}, guarded {guarded}"
    );
}

/// The opt-out has to genuinely restore the stock behaviour, or users who hit
/// a completion-dependent workflow have no way out.
#[test]
fn opt_out_restores_completion_capture() {
    let Some(bash) = find_git_bash() else {
        eprintln!("skipping: no Git Bash found (set CLUD_TEST_GIT_BASH to override)");
        return;
    };

    let baseline = captured_function_count(&bash, &[]);
    if baseline < 20 {
        eprintln!("skipping: nothing to restore on this shell");
        return;
    }

    // Opted out => no overrides => the shell behaves exactly as it would
    // without clud in the picture.
    assert!(env_overrides_for(true, true).is_empty());
    let opted_out = captured_function_count(&bash, &env_overrides_for(true, true));
    assert_eq!(
        opted_out, baseline,
        "opting out must leave the login shell untouched"
    );
}
