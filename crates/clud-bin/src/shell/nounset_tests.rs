use super::*;
use tempfile::tempdir;

#[test]
fn arms_nounset_by_default() {
    let tmp = tempdir().unwrap();
    let overrides = env_overrides_at(tmp.path(), false, None);

    let bash_env = overrides
        .iter()
        .find(|(key, _)| key == BASH_ENV_KEY)
        .expect("BASH_ENV must be set");
    assert!(Path::new(&bash_env.1).is_file(), "the file must exist");
    assert!(
        std::fs::read_to_string(&bash_env.1)
            .unwrap()
            .contains("set -u"),
        "the whole point is that the sourced file enables nounset"
    );
    // Nothing to chain, so no stash key.
    assert!(!overrides.iter().any(|(key, _)| key == PREV_KEY));
}

#[test]
fn opting_out_leaves_the_shell_alone() {
    let tmp = tempdir().unwrap();
    assert!(
        env_overrides_at(tmp.path(), true, None).is_empty(),
        "{OPT_OUT_KEY} must restore stock shell behaviour"
    );
    // And it must not have written a file the shell could still find.
    assert!(!tmp.path().join(FILE_NAME).exists());
}

#[test]
fn an_inherited_bash_env_is_chained_not_clobbered() {
    // Skipping when the user has their own would be a silent coverage gap;
    // overwriting it would break their setup. The file sources theirs after
    // arming nounset.
    let tmp = tempdir().unwrap();
    let overrides = env_overrides_at(tmp.path(), false, Some("/home/u/mine.sh".to_string()));

    assert_eq!(
        overrides
            .iter()
            .find(|(key, _)| key == PREV_KEY)
            .map(|(_, value)| value.as_str()),
        Some("/home/u/mine.sh"),
        "the previous BASH_ENV must be stashed for the generated file to source"
    );
    let body = script_body();
    assert!(body.contains(&format!(". \"${{{PREV_KEY}}}\"")), "{body}");
}

#[test]
fn our_own_path_is_never_chained_to_itself() {
    // A clud session launched from inside a clud session would otherwise stash
    // our path as "previous" and the file would source itself forever.
    let tmp = tempdir().unwrap();
    let first = env_overrides_at(tmp.path(), false, None);
    let ours = first
        .iter()
        .find(|(key, _)| key == BASH_ENV_KEY)
        .map(|(_, value)| value.clone())
        .unwrap();

    let second = env_overrides_at(tmp.path(), false, Some(ours));

    assert!(
        !second.iter().any(|(key, _)| key == PREV_KEY),
        "our own path must not be chained as the previous one"
    );
}

#[test]
fn an_empty_inherited_value_is_not_chained() {
    let tmp = tempdir().unwrap();
    let overrides = env_overrides_at(tmp.path(), false, Some(String::new()));
    assert!(!overrides.iter().any(|(key, _)| key == PREV_KEY));
}

/// The user's own startup file must be sourced *before* nounset is armed.
///
/// Their file was written for a stock shell, and stock startup files routinely
/// test unset variables (`[ -z "$PS1" ]`, `$SSH_AUTH_SOCK`). Arming first would
/// abort every tool call with an error pointing at *their* file, for a policy
/// they never opted into — a worse outcome than not shipping this at all.
/// Ordering is the entire mitigation, so it gets a test.
#[test]
fn the_users_own_file_is_sourced_before_nounset_is_armed() {
    let body = script_body();
    let set_u = body.find("set -u").expect("nounset must be enabled");
    let chain = body
        .find(&format!(". \"${{{PREV_KEY}}}\""))
        .expect("the chained source must be present");
    assert!(
        chain < set_u,
        "sourcing theirs after `set -u` breaks stock startup files: {body}"
    );
    // The `:-` form costs nothing and stays correct if their file (or a later
    // edit) turns nounset on above this line.
    assert!(
        body.contains(&format!("${{{PREV_KEY}:-}}")),
        "the readability test must use the :- form: {body}"
    );
}

#[test]
fn opt_out_parsing_matches_the_completion_guard_precedent() {
    for truthy in ["1", "true", "YES", " on "] {
        assert!(is_truthy(truthy), "{truthy} should opt out");
    }
    for falsy in ["0", "false", "no", "off", "", "maybe"] {
        assert!(!is_truthy(falsy), "{falsy} should not opt out");
    }
}

/// Run `bash -c <script>` under exactly `env`, returning `(exit_code, output)`.
///
/// The repo routes all subprocess execution through `running_process`, so this
/// does too. Streams are merged because every assertion below is about what
/// the shell said, not which pipe it said it on.
#[cfg(unix)]
fn bash_under(env: Vec<(String, String)>, script: &str) -> (i32, String) {
    use running_process::{
        CommandSpec, NativeProcess, ProcessConfig, ReadStatus, StderrMode, StdinMode,
    };
    use std::time::Duration;

    let process = NativeProcess::new(ProcessConfig {
        command: CommandSpec::Argv(vec![
            "bash".to_string(),
            "-c".to_string(),
            script.to_string(),
        ]),
        cwd: None,
        env: Some(env),
        capture: true,
        stderr_mode: StderrMode::Stdout,
        creationflags: None,
        create_process_group: false,
        stdin_mode: StdinMode::Null,
        nice: None,
    });
    process.start().expect("bash must be runnable");

    let mut buf = Vec::<u8>::new();
    loop {
        match process.read_combined(Some(Duration::from_millis(100))) {
            ReadStatus::Line(event) => {
                buf.extend_from_slice(&event.line);
                buf.push(b'\n');
            }
            ReadStatus::Timeout => {
                if process.returncode().is_some() {
                    break;
                }
            }
            ReadStatus::Eof => break,
        }
    }
    let code = process
        .wait(Some(Duration::from_secs(30)))
        .expect("bash must exit");
    (code, String::from_utf8_lossy(&buf).trim().to_string())
}

/// The environment a bash inherits when clud has contributed nothing — the
/// ambient one minus anything this module owns, so an outer clud session
/// cannot arm the "unarmed" baseline.
#[cfg(unix)]
fn stock_env() -> Vec<(String, String)> {
    // Built explicitly rather than inherited.
    //
    // This used to be `std::env::vars()` minus the two keys we own. That reads
    // the whole process environment, which sibling tests in this binary mutate
    // (`HOME`, `USERPROFILE`, and others, from several files each holding its
    // own lock). Iterating it while another thread writes is a race by
    // construction, and it showed up as this file failing about 1 run in 8 of
    // the full suite while passing alone.
    //
    // Nothing here needs the ambient environment. A bash that runs `echo` and
    // sources a file needs a PATH and nothing else, so the fixture says so and
    // stops depending on what the rest of the suite is doing.
    vec![(
        "PATH".to_string(),
        std::env::var("PATH").unwrap_or_else(|_| "/usr/bin:/bin".to_string()),
    )]
}

/// A real non-interactive bash must actually abort, not merely be handed a
/// file with the right characters in it. `bash -c` is non-interactive, which
/// is both the only mode that reads `BASH_ENV` and the mode every Bash tool
/// call runs in — so this is the production path, not an approximation of it.
///
/// The unset-then-set pair is the point: the same command under the same shell
/// succeeds without our overrides and dies with them. Asserting only the
/// failure would pass against a bash that was broken for some other reason.
#[test]
#[cfg(unix)]
fn a_real_non_interactive_bash_aborts_on_an_unset_expansion() {
    // `rm -rf "$SP"/` from #1064, reduced to an echo. Under nounset the shell
    // dies before the command runs; without it, `$SP` becomes empty.
    const PROBE: &str = r#"echo "[${SP}]""#;

    let tmp = tempdir().unwrap();
    let overrides = env_overrides_at(tmp.path(), false, None);

    let (code, output) = bash_under(stock_env(), PROBE);
    assert_eq!(code, 0, "baseline: a stock shell survives this: {output}");
    assert_eq!(
        output, "[]",
        "the unset variable silently expands to nothing — the whole problem"
    );

    let mut armed = stock_env();
    armed.extend(overrides);
    let (code, output) = bash_under(armed, PROBE);
    assert_ne!(code, 0, "the same command must fail once armed: {output}");
    assert!(
        output.contains("SP") && output.contains("unbound variable"),
        "the failure must name the variable so it is diagnosable: {output}"
    );
    assert!(
        !output.contains("[]"),
        "the command must not have run at all: {output}"
    );
}

/// Chaining is only worth writing if the chained file is really sourced. A
/// generated file that silently dropped the user's `BASH_ENV` would look
/// identical to this one from the outside.
#[test]
#[cfg(unix)]
fn an_inherited_bash_env_is_really_sourced_by_the_generated_file() {
    let tmp = tempdir().unwrap();
    let theirs = tmp.path().join("theirs.sh");
    std::fs::write(&theirs, "export FROM_THEIRS=yes\n").unwrap();

    let mut env = stock_env();
    env.extend(env_overrides_at(
        tmp.path(),
        false,
        Some(theirs.display().to_string()),
    ));

    let (code, output) = bash_under(env, r#"echo "[${FROM_THEIRS}]""#);

    assert_eq!(
        code, 0,
        "sourcing theirs must not itself trip nounset: {output}"
    );
    assert_eq!(
        output, "[yes]",
        "the user's own BASH_ENV must still take effect"
    );
}

/// The ordering rule, proven against a real startup file rather than by
/// reading the generated text. A stock `BASH_ENV` that tests an unset variable
/// — which is what `[ -z "$PS1" ]` does, and it is everywhere — must still
/// work. Arming nounset before sourcing theirs would kill every tool call here
/// with an error pointing at a file the user never connected to clud.
#[test]
#[cfg(unix)]
fn a_stock_startup_file_that_tests_an_unset_variable_still_works() {
    let tmp = tempdir().unwrap();
    let theirs = tmp.path().join("theirs.sh");
    std::fs::write(
        &theirs,
        "if [ -z \"$PS1\" ]; then export FROM_THEIRS=batch; fi\n",
    )
    .unwrap();

    let mut env = stock_env();
    env.extend(env_overrides_at(
        tmp.path(),
        false,
        Some(theirs.display().to_string()),
    ));

    let (code, output) = bash_under(env, r#"echo "[${FROM_THEIRS}]""#);

    assert_eq!(
        code, 0,
        "their file must run under the shell it was written for: {output}"
    );
    assert_eq!(output, "[batch]", "{output}");
}
