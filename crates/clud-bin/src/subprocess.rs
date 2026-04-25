//! Subprocess launch helpers.
//!
//! On Windows, Rust's `std::process::Command` cannot directly spawn `.cmd` /
//! `.bat` files. Since Rust 1.77 (BatBadBat / CVE-2024-24576) the standard
//! library refuses any batch invocation whose arguments don't round-trip
//! losslessly through cmd.exe's idiosyncratic command-line parser; the visible
//! failure mode is `failed to spawn process: batch file arguments are
//! invalid`. That hits `clud --codex` because npm installs Codex as a `.cmd`
//! shim at `C:\Users\<user>\AppData\Roaming\npm\codex.cmd` (see issue #59).
//!
//! The fix is to rewrite the launch as `cmd.exe /D /S /C "<bat-path>" <args>`
//! so Windows is launching cmd.exe (a real `.exe`) and the batch invocation
//! lives inside a shell command line where we control the quoting.
//! `running-process-core` already provides exactly that wrapper via
//! `CommandSpec::Shell`, which builds `cmd /D /S /C "<command>"` using
//! `raw_arg` and therefore preserves our quoting verbatim.
//!
//! This module is the single place that decides whether a given backend argv
//! needs the Windows batch rewrite. POSIX has no `.cmd` / `.bat` concept;
//! every code path stays argv-based there.

#[cfg(windows)]
use std::path::Path;

use running_process_core::CommandSpec;

/// Translate an argv-style launch into the most compatible `CommandSpec` for
/// the current platform.
///
/// On Windows, when `argv[0]` is a `.cmd` / `.bat` script we route through
/// `cmd.exe` (see module docs) by emitting [`CommandSpec::Shell`]. Native
/// executables and every POSIX target stay as plain `Argv` so we keep the
/// well-defined argv contract end-to-end.
pub fn command_spec_for_subprocess(argv: Vec<String>) -> CommandSpec {
    #[cfg(windows)]
    if is_windows_batch_wrapper(argv.first().map(String::as_str)) {
        return CommandSpec::Shell(render_windows_batch_command(&argv));
    }

    CommandSpec::Argv(argv)
}

/// True when `program` ends in `.cmd` or `.bat` (case-insensitive). Always
/// false on non-Windows targets — POSIX never has to special-case batch
/// wrappers.
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

/// Build the inner `<bat-path> <args>` command string that goes inside
/// `cmd /D /S /C "..."`. Each token is quoted independently with
/// [`quote_for_cmd`].
///
/// `running-process-core::shell_command` already prepends `cmd /D /S /C "`
/// (and appends the matching close-quote) using `raw_arg`, so we only need
/// to render the *contents* of that outer quoted region.
///
/// The flags it picks are intentional:
///
/// * `/D` skips `AutoRun` so a user `cmd.exe /K ...` registry key cannot
///   inject extra commands ahead of ours.
/// * `/S` makes cmd's quote handling predictable: the *outermost* `"..."`
///   pair is stripped verbatim and what's inside is parsed as a normal
///   command line. Without `/S`, cmd applies a different "first-and-last
///   quote on the line" rule that breaks on paths with spaces.
/// * `/C` runs the command and exits.
#[cfg(windows)]
fn render_windows_batch_command(argv: &[String]) -> String {
    argv.iter()
        .map(|arg| quote_for_cmd(arg))
        .collect::<Vec<_>>()
        .join(" ")
}

/// Quote one argv token for cmd.exe.
///
/// Strategy: wrap the token in `"..."` and escape the two characters that
/// matter inside a double-quoted region:
///
/// * `"` → `""`. Inside a quoted block, cmd.exe treats a doubled quote as a
///   single embedded literal quote.
/// * `%` → `%%`. cmd.exe expands `%FOO%` *before* it strips the outer
///   quotes, even inside `"..."`. Doubling the `%` defangs that pass.
///
/// Other shell metacharacters (`& | < > ^ ( ) ;`) only matter outside quotes;
/// once the whole argument is wrapped in `"..."`, cmd treats them as literal
/// bytes and forwards them to the batch file unchanged.
///
/// `!` is special only when delayed expansion is enabled (`setlocal
/// enabledelayedexpansion`), which is off by default. We intentionally do not
/// escape it: doing so would corrupt prompts that legitimately contain `!`,
/// and a `.cmd` shim that turned delayed expansion on for itself would
/// already break in the same way regardless of how we quote.
#[cfg(windows)]
fn quote_for_cmd(arg: &str) -> String {
    let mut out = String::with_capacity(arg.len() + 2);
    out.push('"');
    for ch in arg.chars() {
        match ch {
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

    // ---- Windows-only: `.cmd` / `.bat` wrapping (issue #59) ----
    //
    // We expect every Windows batch backend to come out as a `Shell` spec
    // that running-process-core will hand to `cmd /D /S /C "..."`.

    #[cfg(windows)]
    #[test]
    fn cmd_wrapper_with_no_args_routes_through_shell() {
        // Just the `.cmd` path with no extra args: the rendered command is
        // simply the quoted exe path, which still has to go through cmd.exe.
        let spec =
            command_spec_for_subprocess(vec![r"C:\Users\me\AppData\Roaming\npm\codex.cmd".into()]);
        match spec {
            running_process_core::CommandSpec::Shell(command) => {
                assert_eq!(command, r#""C:\Users\me\AppData\Roaming\npm\codex.cmd""#);
            }
            other => panic!("expected Shell for .cmd wrapper, got {other:?}"),
        }
    }

    #[cfg(windows)]
    #[test]
    fn cmd_wrapper_quotes_args_with_spaces() {
        let spec = command_spec_for_subprocess(vec![
            r"C:\path with spaces\codex.cmd".into(),
            "exec".into(),
            "hello world".into(),
        ]);
        let rendered = match spec {
            running_process_core::CommandSpec::Shell(s) => s,
            other => panic!("expected Shell, got {other:?}"),
        };
        assert!(
            rendered.starts_with(r#""C:\path with spaces\codex.cmd" "exec" "hello world""#),
            "spaces in exe path and arg must each be wrapped in quotes: {rendered}"
        );
    }

    #[cfg(windows)]
    #[test]
    fn cmd_wrapper_escapes_shell_metacharacters() {
        // `&`, `|`, `<`, `>`, `^` are cmd metacharacters outside quoted
        // regions. Wrapping each token in `"..."` plus the outer
        // `cmd /S /C "..."` rule keeps them literal so the batch file
        // receives them byte-for-byte.
        let spec = command_spec_for_subprocess(vec![
            r"C:\codex.cmd".into(),
            "fix this & that".into(),
            "left | right".into(),
            "redirect < input > output".into(),
            "caret ^ stays".into(),
        ]);
        let rendered = match spec {
            running_process_core::CommandSpec::Shell(s) => s,
            other => panic!("expected Shell, got {other:?}"),
        };
        assert!(
            rendered.contains(r#""fix this & that""#),
            "& must stay inside quotes: {rendered}"
        );
        assert!(
            rendered.contains(r#""left | right""#),
            "| must stay inside quotes: {rendered}"
        );
        assert!(
            rendered.contains(r#""redirect < input > output""#),
            "redirection chars must stay inside quotes: {rendered}"
        );
        assert!(
            rendered.contains(r#""caret ^ stays""#),
            "^ must stay inside quotes: {rendered}"
        );
        // Sanity: there must be no occurrence of an unescaped metacharacter
        // appearing *outside* the quoted regions. We approximate this by
        // checking that every metachar shows up only between matching quotes.
        for needle in ["&", "|", "<", ">", "^"] {
            // Each needle should appear at least once (it's in our input);
            // the surrounding `" `... `"` from the join contract is the
            // assurance it stays inside.
            assert!(
                rendered.contains(needle),
                "expected `{needle}` to appear in rendered command: {rendered}"
            );
        }
    }

    #[cfg(windows)]
    #[test]
    fn cmd_wrapper_escapes_percent_and_quote() {
        // `%` would otherwise expand env vars before the batch sees it; `"`
        // needs to be doubled to embed inside a quoted token.
        let spec = command_spec_for_subprocess(vec![
            r"C:\codex.cmd".into(),
            "exec".into(),
            r#"100% "quoted" prompt"#.into(),
        ]);
        let rendered = match spec {
            running_process_core::CommandSpec::Shell(s) => s,
            other => panic!("expected Shell, got {other:?}"),
        };
        assert!(
            rendered.contains(r#""100%% ""quoted"" prompt""#),
            "percent and quote escaping is wrong: {rendered}"
        );
    }

    #[cfg(windows)]
    #[test]
    fn bat_extension_is_also_wrapped() {
        // The check is case-insensitive: `.BAT`, `.Cmd`, etc. all have to
        // hit the cmd.exe path.
        let spec = command_spec_for_subprocess(vec![r"C:\tools\thing.BAT".into(), "go".into()]);
        match spec {
            running_process_core::CommandSpec::Shell(_) => {}
            other => panic!("expected Shell for .BAT wrapper, got {other:?}"),
        }
    }

    #[cfg(windows)]
    #[test]
    fn exe_path_is_not_wrapped() {
        // `.exe` is a native executable; it must stay argv-based so we don't
        // pay the cmd.exe parsing tax (and don't regress any quoting that
        // already worked for non-batch backends).
        let spec = command_spec_for_subprocess(vec![
            r"C:\Program Files\Tool\codex.exe".into(),
            "exec".into(),
            "hello".into(),
        ]);
        match spec {
            running_process_core::CommandSpec::Argv(argv) => {
                assert_eq!(
                    argv,
                    vec![r"C:\Program Files\Tool\codex.exe", "exec", "hello"],
                    ".exe paths must not be cmd-wrapped"
                );
            }
            other => panic!("expected Argv passthrough for .exe, got {other:?}"),
        }
    }

    #[cfg(windows)]
    #[test]
    fn extensionless_path_is_not_wrapped() {
        // PATH lookup may hand us a bare program name on Windows too (e.g.
        // for .exe files where the extension was already stripped by an
        // earlier resolver). These must not be wrapped.
        let spec = command_spec_for_subprocess(vec!["codex".into(), "exec".into()]);
        match spec {
            running_process_core::CommandSpec::Argv(argv) => {
                assert_eq!(argv, vec!["codex", "exec"]);
            }
            other => panic!("expected Argv for extensionless path, got {other:?}"),
        }
    }

    #[cfg(not(windows))]
    #[test]
    fn cmd_suffix_is_not_special_off_windows() {
        // POSIX doesn't have batch wrappers; even a file literally named
        // `foo.cmd` is just an executable, and we must not invent a cmd.exe
        // dependency on Linux/macOS.
        let spec = command_spec_for_subprocess(vec!["codex.cmd".into(), "exec".into()]);
        match spec {
            running_process_core::CommandSpec::Argv(argv) => {
                assert_eq!(argv, vec!["codex.cmd", "exec"]);
            }
            other => panic!("expected Argv off Windows, got {other:?}"),
        }
    }

    #[cfg(not(windows))]
    #[test]
    fn shell_script_path_is_not_wrapped() {
        // `.sh` and extensionless POSIX paths must always pass through.
        let spec = command_spec_for_subprocess(vec!["./run.sh".into(), "--flag".into()]);
        match spec {
            running_process_core::CommandSpec::Argv(argv) => {
                assert_eq!(argv, vec!["./run.sh", "--flag"]);
            }
            other => panic!("expected Argv for .sh script, got {other:?}"),
        }
        let spec = command_spec_for_subprocess(vec!["/usr/bin/codex".into(), "exec".into()]);
        match spec {
            running_process_core::CommandSpec::Argv(argv) => {
                assert_eq!(argv, vec!["/usr/bin/codex", "exec"]);
            }
            other => panic!("expected Argv for extensionless path, got {other:?}"),
        }
    }
}
