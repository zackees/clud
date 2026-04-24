use std::path::Path;

use running_process_core::CommandSpec;

/// Convert an argv-style launch into the most compatible subprocess form for
/// the current platform.
///
/// Windows `CreateProcess*` does not execute `.cmd` / `.bat` scripts directly;
/// they must run through `cmd.exe`. `running-process-core` already provides a
/// Windows shell path that wraps `cmd /D /S /C ...`, so batch wrappers are
/// rendered as a shell command there while native executables stay argv-based.
pub fn command_spec_for_subprocess(argv: Vec<String>) -> CommandSpec {
    #[cfg(windows)]
    if is_windows_batch_wrapper(argv.first().map(String::as_str)) {
        return CommandSpec::Shell(render_windows_batch_command(&argv));
    }

    CommandSpec::Argv(argv)
}

#[cfg(windows)]
fn is_windows_batch_wrapper(program: Option<&str>) -> bool {
    let Some(program) = program else {
        return false;
    };
    matches!(
        Path::new(program)
            .extension()
            .and_then(|ext| ext.to_str())
            .map(|ext| ext.to_ascii_lowercase())
            .as_deref(),
        Some("cmd" | "bat")
    )
}

#[cfg(windows)]
fn render_windows_batch_command(argv: &[String]) -> String {
    argv.iter()
        .map(|arg| quote_for_cmd(arg))
        .collect::<Vec<_>>()
        .join(" ")
}

#[cfg(windows)]
fn quote_for_cmd(arg: &str) -> String {
    let mut out = String::with_capacity(arg.len() + 2);
    out.push('"');
    for ch in arg.chars() {
        match ch {
            // Prevent cmd.exe from treating `%FOO%` as env expansion before the
            // batch wrapper receives the argument.
            '%' => out.push_str("%%"),
            '"' => out.push_str("\"\""),
            _ => out.push(ch),
        }
    }
    out.push('"');
    out
}

#[cfg(test)]
mod tests {
    use super::command_spec_for_subprocess;

    #[test]
    fn native_executable_stays_argv() {
        let spec =
            command_spec_for_subprocess(vec!["codex.exe".into(), "exec".into(), "hello".into()]);
        match spec {
            running_process_core::CommandSpec::Argv(argv) => {
                assert_eq!(argv, vec!["codex.exe", "exec", "hello"]);
            }
            other => panic!("expected Argv for native executable, got {other:?}"),
        }
    }

    #[cfg(windows)]
    #[test]
    fn cmd_wrapper_uses_shell_command() {
        let spec = command_spec_for_subprocess(vec![
            r"C:\Users\me\AppData\Roaming\npm\codex.cmd".into(),
            "exec".into(),
            "100% \"quoted\" prompt".into(),
        ]);
        match spec {
            running_process_core::CommandSpec::Shell(command) => {
                assert!(
                    command.starts_with(r#""C:\Users\me\AppData\Roaming\npm\codex.cmd" "exec" "#),
                    "unexpected shell command: {command}"
                );
                assert!(
                    command.contains(r#""100%% ""quoted"" prompt""#),
                    "percent signs and quotes must be escaped for cmd.exe: {command}"
                );
            }
            other => panic!("expected Shell for .cmd wrapper, got {other:?}"),
        }
    }

    #[cfg(not(windows))]
    #[test]
    fn cmd_suffix_is_not_special_off_windows() {
        let spec = command_spec_for_subprocess(vec!["codex.cmd".into(), "exec".into()]);
        match spec {
            running_process_core::CommandSpec::Argv(argv) => {
                assert_eq!(argv, vec!["codex.cmd", "exec"]);
            }
            other => panic!("expected Argv off Windows, got {other:?}"),
        }
    }
}
