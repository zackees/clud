//! Every case here is synthetic argv. #1067 is explicit: "Never validate a
//! removal guard by performing a removal -- not on a host, not in a
//! container." Nothing in this file touches a filesystem.

use super::*;

fn root() -> PathBuf {
    PathBuf::from("/srv/project")
}

fn home() -> PathBuf {
    PathBuf::from("/home/dev")
}

fn argv(parts: &[&str]) -> Vec<String> {
    parts.iter().map(|s| (*s).to_string()).collect()
}

fn decide(parts: &[&str]) -> Decision {
    classify(&argv(parts), &root(), &root(), Some(&home()))
}

fn refusal(parts: &[&str]) -> String {
    match decide(parts) {
        Decision::Refuse(message) => message,
        Decision::Exec => panic!("expected a refusal for {parts:?}"),
    }
}

// -- the acceptance criteria from #1067 ---------------------------------

/// "`tap rm -rf /` and its `$VAR`-expanded equivalents refuse."
///
/// The expanded equivalents are the point: by the time tap sees the argv,
/// `rm -rf "$SP"/` with an unset `SP` *is* `rm -rf /`. There is no variable
/// left to interpret, which is why this check can be exact where
/// `block_bad_cmd_rm_vars` had to be heuristic.
#[test]
fn a_removal_of_the_filesystem_root_is_refused() {
    for parts in [
        &["rm", "-rf", "/"][..],
        &["rm", "-rf", "/."][..],
        &["rm", "-rf", "//"][..],
        &["rm", "-rf", "/srv/.."][..],
        &["rm", "-rf", "/srv/project/../.."][..],
    ] {
        let message = refusal(parts);
        assert!(message.contains("filesystem root"), "{parts:?}: {message}");
        assert!(message.contains("#1064"), "{parts:?}: {message}");
    }
}

/// "`tap rm -rf ./scratch` execs."
#[test]
fn a_removal_inside_the_session_root_is_allowed() {
    for parts in [
        &["rm", "-rf", "./scratch"][..],
        &["rm", "-rf", "scratch"][..],
        &["rm", "-rf", "/srv/project/scratch"][..],
        &["rm", "-r", "--", "scratch"][..],
        &["rm", "-rf", "nested/deep/dir"][..],
    ] {
        assert_eq!(decide(parts), Decision::Exec, "{parts:?}");
    }
}

#[test]
fn a_removal_of_the_home_directory_is_refused() {
    assert!(refusal(&["rm", "-rf", "/home/dev"]).contains("home directory"));
}

#[test]
fn a_removal_outside_the_session_root_is_refused() {
    let message = refusal(&["rm", "-rf", "/etc/passwd"]);
    assert!(message.contains("outside the session root"), "{message}");
    // The message must carry both paths: an agent restating the command needs
    // to know what its argument became and what it is allowed to be.
    assert!(message.contains("/etc/passwd"), "{message}");
    assert!(message.contains("/srv/project"), "{message}");
}

/// A sibling directory sharing a name prefix is *outside* the root.
///
/// String-prefix containment would allow `/srv/project-old`, which is a real
/// directory someone could lose.
#[test]
fn a_sibling_with_a_shared_prefix_is_not_inside_the_root() {
    assert!(refusal(&["rm", "-rf", "/srv/project-old"]).contains("outside"));
}

/// Escaping the root with `..` is resolved before the check, not after.
#[test]
fn climbing_out_of_the_root_is_refused() {
    assert!(refusal(&["rm", "-rf", "../other"]).contains("outside"));
    assert!(refusal(&["rm", "-rf", "scratch/../../other"]).contains("outside"));
}

// -- fail closed --------------------------------------------------------

/// An empty operand list is the #1064 shape one step earlier: a variable that
/// expanded to nothing at all rather than to `/`.
#[test]
fn a_removal_with_no_target_is_refused() {
    let message = refusal(&["rm", "-rf"]);
    assert!(message.contains("no target"), "{message}");
    assert!(message.contains("expanded to nothing"), "{message}");
}

#[test]
fn an_empty_argv_is_refused_rather_than_treated_as_a_no_op() {
    assert!(matches!(
        classify(&[], &root(), &root(), Some(&home())),
        Decision::Refuse(_)
    ));
}

// -- argv grammar -------------------------------------------------------

/// `--` ends flag parsing, so a target that looks like a flag is still a
/// target. Missing this would skip the check on `rm -rf -- /`.
#[test]
fn operands_after_a_double_dash_are_still_targets() {
    assert!(refusal(&["rm", "-rf", "--", "/"]).contains("filesystem root"));
    // A flag-shaped operand after `--` is a target like any other. This one
    // resolves inside the root, so it is allowed -- the property under test is
    // that it was *considered*, not skipped as a flag.
    assert_eq!(decide(&["rm", "--", "-weird-name"]), Decision::Exec);
}

/// A lone `-` is an operand, not a flag; `-rf` is a flag.
#[test]
fn flag_detection_does_not_swallow_a_bare_dash() {
    // `-` resolves under the cwd, so it is allowed -- the point is that it is
    // treated as an operand at all rather than skipped as a flag.
    assert_eq!(decide(&["rm", "-"]), Decision::Exec);
}

#[test]
fn the_program_is_matched_by_name_not_by_full_path() {
    for program in ["/bin/rm", "/usr/bin/rm", r"C:\Windows\rm.exe", "rm.exe"] {
        let message = refusal(&[program, "-rf", "/"]);
        assert!(message.contains("filesystem root"), "{program}: {message}");
    }
}

#[test]
fn rmdir_and_unlink_are_guarded_too() {
    assert!(refusal(&["rmdir", "/"]).contains("filesystem root"));
    assert!(refusal(&["unlink", "/etc/hosts"]).contains("outside"));
}

/// Anything that is not a removal passes through untouched. v0's scope is the
/// incident class in #1064, and pretending to guard `mv` or `dd` without
/// modelling their argument grammars would be false confidence.
#[test]
fn non_removal_programs_are_not_inspected() {
    for parts in [
        &["ls", "/"][..],
        &["cat", "/etc/passwd"][..],
        &["echo", "rm -rf /"][..],
        &["git", "status"][..],
    ] {
        assert_eq!(decide(parts), Decision::Exec, "{parts:?}");
    }
}

// -- windows shapes -----------------------------------------------------

/// A drive root is a filesystem root. Checked structurally, so the several
/// spellings all answer the same.
///
/// Windows-only, and that is a real constraint rather than test tidiness:
/// `std::path` only parses a `C:\` prefix on Windows, so on Unix the same
/// string is an ordinary relative path. `tap` inspects paths on the platform
/// it runs on, so this is sound -- but a cross-platform assertion here would
/// be testing the host's path parser, not the guard.
#[test]
#[cfg(windows)]
fn a_windows_drive_root_is_a_filesystem_root() {
    let win_root = PathBuf::from(r"C:\srv\project");
    for target in [r"C:\", "C:/"] {
        let decision = classify(&argv(&["rm", "-rf", target]), &win_root, &win_root, None);
        match decision {
            Decision::Refuse(message) => {
                assert!(
                    message.contains("filesystem root") || message.contains("outside"),
                    "{target}: {message}"
                );
            }
            Decision::Exec => panic!("{target} must not be allowed"),
        }
    }
}

/// No home means the home check simply does not fire; it must not panic or
/// refuse everything.
#[test]
fn an_absent_home_does_not_break_the_other_checks() {
    let decision = classify(&argv(&["rm", "-rf", "scratch"]), &root(), &root(), None);
    assert_eq!(decision, Decision::Exec);
    assert!(matches!(
        classify(&argv(&["rm", "-rf", "/"]), &root(), &root(), None),
        Decision::Refuse(_)
    ));
}

// -- message quality ----------------------------------------------------

/// The reader is an agent that has to restate the command. A refusal that does
/// not say what to do next produces another guess.
#[test]
fn every_refusal_says_how_to_proceed() {
    for parts in [
        &["rm", "-rf", "/"][..],
        &["rm", "-rf", "/etc"][..],
        &["rm", "-rf", "/home/dev"][..],
    ] {
        let message = refusal(parts);
        assert!(
            message.contains("Restate the command"),
            "{parts:?} must tell the agent what to do: {message}"
        );
    }
}
