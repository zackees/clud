//! #2197: external masked-prompt helpers for a Linux desktop where the native
//! egui window cannot load (for example KDE Wayland). Tried in order:
//! `zenity --password`, `kdialog --password`, `ksshaskpass`, then `pinentry`
//! (Assuan `GETPIN`). A helper is only used when it is on PATH and a display
//! exists. The value goes from the helper's stdout straight back to the
//! caller's sink; it is never logged, echoed or placed on a command line.

use std::time::{Duration, Instant};

use running_process::{
    CommandSpec, NativeProcess, ProcessConfig, ReadStatus, StderrMode, StdinMode, StreamKind,
};
use zeroize::{Zeroize, Zeroizing};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Helper {
    Zenity,
    Kdialog,
    Ksshaskpass,
    Pinentry,
}

impl Helper {
    const ORDER: [Helper; 4] = [
        Helper::Zenity,
        Helper::Kdialog,
        Helper::Ksshaskpass,
        Helper::Pinentry,
    ];

    fn program(self) -> &'static str {
        match self {
            Helper::Zenity => "zenity",
            Helper::Kdialog => "kdialog",
            Helper::Ksshaskpass => "ksshaskpass",
            Helper::Pinentry => "pinentry",
        }
    }
}

/// Helpers to try, in order, given injected facts (no PATH or env access).
pub fn select_helpers(display: bool, on_path: impl Fn(&str) -> bool) -> Vec<Helper> {
    if !display {
        return Vec::new();
    }
    Helper::ORDER
        .into_iter()
        .filter(|helper| on_path(helper.program()))
        .collect()
}

/// Argument vector after the program name. Prompt text only; never a secret.
fn args(helper: Helper, title: &str, label: &str) -> Vec<String> {
    match helper {
        Helper::Zenity => vec![
            "--password".into(),
            format!("--title={title}"),
            format!("--text={label}"),
        ],
        Helper::Kdialog => vec![
            "--title".into(),
            title.into(),
            "--password".into(),
            label.into(),
        ],
        Helper::Ksshaskpass => vec![label.into()],
        Helper::Pinentry => Vec::new(),
    }
}

fn assuan_escape(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for c in text.chars() {
        match c {
            '%' => out.push_str("%25"),
            '\n' => out.push_str("%0A"),
            '\r' => out.push_str("%0D"),
            _ => out.push(c),
        }
    }
    out
}

fn pinentry_script(title: &str, label: &str) -> String {
    format!(
        "SETTITLE {}\nSETDESC {}\nSETPROMPT Secret:\nGETPIN\nBYE\n",
        assuan_escape(title),
        assuan_escape(label)
    )
}

/// Decode Assuan `%XX` escapes into raw bytes (UTF-8 is validated by the caller).
fn assuan_unescape(text: &str) -> Vec<u8> {
    let bytes = text.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            let decoded = std::str::from_utf8(&bytes[i + 1..i + 3])
                .ok()
                .and_then(|hex| u8::from_str_radix(hex, 16).ok());
            if let Some(byte) = decoded {
                out.push(byte);
                i += 3;
                continue;
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    out
}

/// What a helper's stdout lines mean once it has exited.
#[derive(Debug, PartialEq, Eq)]
enum Reply {
    Secret(String),
    Cancelled,
    Unusable,
}

fn interpret_pinentry(lines: &[String]) -> Reply {
    let mut data: Vec<u8> = Vec::new();
    let mut seen = false;
    for line in lines {
        if let Some(part) = line.strip_prefix("D ") {
            let mut piece = assuan_unescape(part);
            data.extend_from_slice(&piece);
            piece.zeroize();
            seen = true;
        }
    }
    if seen {
        return match String::from_utf8(data) {
            Ok(secret) => Reply::Secret(secret),
            Err(err) => {
                let mut bytes = err.into_bytes();
                bytes.zeroize();
                Reply::Unusable
            }
        };
    }
    // 83886179 is gpg-error "Operation cancelled".
    if lines.iter().any(|l| l.starts_with("ERR 83886179")) {
        Reply::Cancelled
    } else {
        Reply::Unusable
    }
}

/// `stderr_text` is whether the helper wrote anything to stderr. A dismissed
/// dialog exits 1 silently; exit 1 with diagnostics means it never worked
/// (no display, broken toolkit plugin), so the next helper should be tried.
fn interpret(helper: Helper, exit: i32, lines: &[String], stderr_text: bool) -> Reply {
    if helper == Helper::Pinentry {
        return interpret_pinentry(lines);
    }
    match (exit, lines.first()) {
        (0, Some(first)) => Reply::Secret(first.clone()),
        (1, None) if !stderr_text => Reply::Cancelled,
        _ => Reply::Unusable,
    }
}

/// Keep only absolute PATH entries so a relative or empty entry (`.`, `::`)
/// can never resolve a planted helper from the current directory.
fn absolute_path_entries(path_env: &str) -> String {
    path_env
        .split(':')
        .filter(|entry| std::path::Path::new(entry).is_absolute())
        .collect::<Vec<_>>()
        .join(":")
}

fn run_helper(helper: Helper, path: &std::path::Path, title: &str, label: &str) -> Reply {
    let mut argv = vec![path.to_string_lossy().into_owned()];
    argv.extend(args(helper, title, label));
    let process = NativeProcess::new(ProcessConfig {
        command: CommandSpec::Argv(argv),
        cwd: None,
        env: None,
        capture: true,
        stderr_mode: StderrMode::Pipe,
        stdin_mode: StdinMode::Piped,
        create_process_group: true,
        creationflags: None,
        nice: None,
        address_space_limit_bytes: None,
    });
    if process.start().is_err() {
        return Reply::Unusable;
    }
    if helper == Helper::Pinentry
        && process
            .write_stdin(pinentry_script(title, label).as_bytes())
            .is_err()
    {
        let _ = process.kill();
        return Reply::Unusable;
    }
    eprintln!(
        "clud: waiting for {} to collect the secret...",
        helper.program()
    );
    let deadline = Instant::now() + Duration::from_secs(600);
    let mut stderr_text = false;
    let mut lines: Vec<String> = Vec::new();
    let mut closed = [false; 2];
    while !closed.iter().all(|c| *c) && Instant::now() < deadline {
        for (i, stream) in [StreamKind::Stdout, StreamKind::Stderr]
            .into_iter()
            .enumerate()
        {
            if closed[i] {
                continue;
            }
            match process.read_stream(stream, Some(Duration::from_millis(50))) {
                ReadStatus::Line(mut bytes) => {
                    if stream == StreamKind::Stderr {
                        stderr_text |= !bytes.iter().all(u8::is_ascii_whitespace);
                    } else {
                        let text = String::from_utf8_lossy(&bytes)
                            .trim_end_matches(['\r', '\n'])
                            .to_string();
                        lines.push(text);
                    }
                    bytes.zeroize();
                }
                ReadStatus::Eof => closed[i] = true,
                ReadStatus::Timeout => {}
            }
        }
    }
    let exit = process
        .wait(Some(deadline.saturating_duration_since(Instant::now())))
        .unwrap_or(-1);
    let _ = process.kill();
    let reply = interpret(helper, exit, &lines, stderr_text);
    lines.iter_mut().for_each(Zeroize::zeroize);
    reply
}

/// Try each available helper. `Cancelled` ends the chain; an unusable helper
/// moves on to the next one.
pub fn prompt(
    title: &str,
    label: &str,
) -> Result<Zeroizing<String>, super::secret_prompt::ExternalFailure> {
    use super::secret_prompt::ExternalFailure;
    let present = |name: &str| {
        std::env::var(name)
            .ok()
            .is_some_and(|value| !value.is_empty())
    };
    let display = present("WAYLAND_DISPLAY") || present("DISPLAY");
    let path_env = absolute_path_entries(&std::env::var("PATH").unwrap_or_default());
    let found = |name: &str| crate::shim_resolve::which(name, &path_env);
    for helper in select_helpers(display, |name| found(name).is_some()) {
        let Some(path) = found(helper.program()) else {
            continue;
        };
        match run_helper(helper, &path, title, label) {
            Reply::Secret(value) => return Ok(Zeroizing::new(value)),
            Reply::Cancelled => return Err(ExternalFailure::Cancelled),
            Reply::Unusable => {}
        }
    }
    Err(ExternalFailure::Unavailable)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn selection_follows_fixed_order_and_requires_path_and_display() {
        let all = |_: &str| true;
        assert_eq!(select_helpers(true, all), Helper::ORDER.to_vec());
        assert_eq!(
            select_helpers(true, |n| n == "kdialog" || n == "pinentry"),
            vec![Helper::Kdialog, Helper::Pinentry]
        );
        assert!(select_helpers(false, all).is_empty());
        assert!(select_helpers(true, |_| false).is_empty());
    }

    #[test]
    fn prompt_text_is_never_a_secret_and_goes_to_the_right_argument() {
        assert_eq!(
            args(Helper::Ksshaskpass, "t", "Token for host"),
            vec!["Token for host"]
        );
        assert!(args(Helper::Zenity, "t", "l").contains(&"--password".to_string()));
        assert!(args(Helper::Pinentry, "t", "l").is_empty());
    }

    #[test]
    fn pinentry_script_escapes_and_replies_unescape() {
        let script = pinentry_script("a%b", "x\ny");
        assert!(script.contains("SETTITLE a%25b\n"));
        assert!(script.contains("SETDESC x%0Ay\n"));
        let lines = vec!["D p%25w%0A".to_string(), "OK".to_string()];
        assert_eq!(
            interpret(Helper::Pinentry, 0, &lines, false),
            Reply::Secret("p%w\n".into())
        );
        let cancel = vec!["ERR 83886179 Operation cancelled".to_string()];
        assert_eq!(
            interpret(Helper::Pinentry, 0, &cancel, false),
            Reply::Cancelled
        );
        assert_eq!(interpret(Helper::Pinentry, 0, &[], false), Reply::Unusable);
    }

    #[test]
    fn simple_helper_exit_codes_map_to_outcomes() {
        let secret = vec!["hunter2".to_string()];
        assert_eq!(
            interpret(Helper::Zenity, 0, &secret, false),
            Reply::Secret("hunter2".into())
        );
        assert_eq!(interpret(Helper::Zenity, 1, &[], false), Reply::Cancelled);
        assert_eq!(interpret(Helper::Kdialog, 127, &[], false), Reply::Unusable);
        assert_eq!(
            interpret(Helper::Ksshaskpass, 0, &[], false),
            Reply::Unusable
        );
    }

    #[test]
    fn exit_one_with_stderr_is_unusable_not_cancel() {
        assert_eq!(interpret(Helper::Zenity, 1, &[], true), Reply::Unusable);
        assert_eq!(interpret(Helper::Kdialog, 1, &[], true), Reply::Unusable);
    }

    #[test]
    fn relative_and_empty_path_entries_are_dropped() {
        assert_eq!(
            absolute_path_entries(".:/usr/bin::bin:/opt/x:"),
            "/usr/bin:/opt/x"
        );
        assert_eq!(absolute_path_entries(":"), "");
    }

    #[test]
    fn pinentry_utf8_and_multi_line_data_round_trip() {
        // "pässwörd" split over two D lines, UTF-8 bytes percent-escaped.
        let lines = vec![
            "D p%C3%A4ssw".to_string(),
            "D %C3%B6rd".to_string(),
            "OK".to_string(),
        ];
        assert_eq!(
            interpret(Helper::Pinentry, 0, &lines, false),
            Reply::Secret("pässwörd".into())
        );
        let bad = vec!["D %FF".to_string()];
        assert_eq!(interpret(Helper::Pinentry, 0, &bad, false), Reply::Unusable);
    }
}
