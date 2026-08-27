use std::io::{BufRead, Write};

use crate::args::{Args, Command};

pub const DO_TARGET_PROMPT: &str = "[clud] Enter an issue URL or goal: ";
const NONINTERACTIVE_ERROR: &str =
    "`clud do` requires a URL or goal in non-interactive mode; pass `clud do <url-or-goal>`";

/// Resolve and write back a `Command::Do` target before launch orchestration.
/// Other commands are untouched.
pub fn resolve_do_command_target<R: BufRead, W: Write>(
    args: &mut Args,
    stdin_is_terminal: bool,
    stderr_is_terminal: bool,
    input: &mut R,
    output: &mut W,
) -> Result<(), String> {
    let Some(Command::Do { target }) = args.command.clone() else {
        return Ok(());
    };
    let may_prompt = stdin_is_terminal
        && stderr_is_terminal
        && !args.dry_run
        && !args.detach
        && !args.detachable;
    let target = resolve_do_target(target.as_deref(), may_prompt, input, output)?;
    args.command = Some(Command::Do {
        target: Some(target),
    });
    Ok(())
}

/// Resolve `clud do`'s optional positional before any backend process starts.
///
/// The terminal checks are supplied by the caller so this stays deterministic
/// in tests and never tries to consume piped input that belongs to automation.
pub fn resolve_do_target<R: BufRead, W: Write>(
    provided: Option<&str>,
    may_prompt: bool,
    input: &mut R,
    output: &mut W,
) -> Result<String, String> {
    if let Some(target) = provided.map(str::trim).filter(|target| !target.is_empty()) {
        return Ok(target.to_string());
    }
    if !may_prompt {
        return Err(NONINTERACTIVE_ERROR.to_string());
    }

    output
        .write_all(DO_TARGET_PROMPT.as_bytes())
        .and_then(|_| output.flush())
        .map_err(|error| format!("failed to write `clud do` prompt: {error}"))?;

    let mut target = String::new();
    let bytes = input
        .read_line(&mut target)
        .map_err(|error| format!("failed to read `clud do` target: {error}"))?;
    let target = target.trim();
    if bytes == 0 || target.is_empty() {
        return Err("`clud do` requires a non-empty URL or goal".to_string());
    }
    Ok(target.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    fn parse(argv: &[&str]) -> Args {
        Args::parse_from_raw(argv.iter().map(|value| (*value).to_string()).collect())
    }

    #[test]
    fn command_resolver_prompts_and_writes_back_missing_target() {
        let mut args = parse(&["clud", "do"]);
        let mut input = Cursor::new(b"fix the launch classifier\n".to_vec());
        let mut output = Vec::new();
        resolve_do_command_target(&mut args, true, true, &mut input, &mut output).unwrap();
        match args.command {
            Some(Command::Do { target }) => {
                assert_eq!(target.as_deref(), Some("fix the launch classifier"));
            }
            other => panic!("expected resolved do command, got {other:?}"),
        }
    }

    #[test]
    fn command_resolver_rejects_missing_target_in_dry_run_without_reading() {
        let mut args = parse(&["clud", "--dry-run", "do"]);
        let mut input = Cursor::new(b"must remain unread\n".to_vec());
        let mut output = Vec::new();
        let error =
            resolve_do_command_target(&mut args, true, true, &mut input, &mut output).unwrap_err();
        assert_eq!(error, NONINTERACTIVE_ERROR);
        assert_eq!(input.position(), 0);
        assert!(output.is_empty());
    }

    #[test]
    fn command_resolver_accepts_explicit_target_in_dry_run() {
        let mut args = parse(&["clud", "--dry-run", "do", "https://example.com/issue/1"]);
        let mut input = Cursor::new(Vec::new());
        let mut output = Vec::new();
        resolve_do_command_target(&mut args, false, false, &mut input, &mut output).unwrap();
        match args.command {
            Some(Command::Do { target }) => {
                assert_eq!(target.as_deref(), Some("https://example.com/issue/1"));
            }
            other => panic!("expected resolved do command, got {other:?}"),
        }
        assert!(output.is_empty());
    }

    #[test]
    fn provided_target_is_trimmed_without_prompting() {
        let mut input = Cursor::new(b"ignored\n".to_vec());
        let mut output = Vec::new();
        let target = resolve_do_target(
            Some("  https://github.com/zackees/clud/issues/1036  "),
            false,
            &mut input,
            &mut output,
        )
        .unwrap();
        assert_eq!(target, "https://github.com/zackees/clud/issues/1036");
        assert!(output.is_empty());
    }

    #[test]
    fn missing_target_prompts_and_accepts_free_form_input() {
        let mut input = Cursor::new(b"  improve the launch classifier  \r\n".to_vec());
        let mut output = Vec::new();
        let target = resolve_do_target(None, true, &mut input, &mut output).unwrap();
        assert_eq!(target, "improve the launch classifier");
        assert_eq!(String::from_utf8(output).unwrap(), DO_TARGET_PROMPT);
    }

    #[test]
    fn missing_target_never_reads_or_prompts_in_noninteractive_mode() {
        let mut input = Cursor::new(b"must remain unread\n".to_vec());
        let mut output = Vec::new();
        let error = resolve_do_target(None, false, &mut input, &mut output).unwrap_err();
        assert_eq!(error, NONINTERACTIVE_ERROR);
        assert_eq!(input.position(), 0);
        assert!(output.is_empty());
    }

    #[test]
    fn eof_and_blank_input_are_clear_errors() {
        for bytes in [b"".as_slice(), b"  \r\n".as_slice()] {
            let mut input = Cursor::new(bytes.to_vec());
            let mut output = Vec::new();
            let error = resolve_do_target(None, true, &mut input, &mut output).unwrap_err();
            assert_eq!(error, "`clud do` requires a non-empty URL or goal");
        }
    }
}
