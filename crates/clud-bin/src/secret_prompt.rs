//! #1708: acquire credentials in a masked window and deliver directly to a sink.
//! Secret bytes have no Debug, serialization, command-line or status surface.

use std::path::PathBuf;

use clap::{ArgGroup, Parser, ValueEnum};
use zeroize::Zeroizing;

#[path = "secret_prompt/gui.rs"]
mod gui;
#[path = "secret_prompt/sink.rs"]
mod sink;

/// Public options shared by `clud secret` and its multicall alias.
#[derive(Debug, Clone, Parser)]
#[command(group(ArgGroup::new("sink").required(true).args(["to_file", "vault", "to_command"])))]
pub struct Options {
    /// Window title identifying the credential request.
    #[arg(long, default_value = "clud credential request")]
    pub title: String,
    /// Describe the credential and host to the user.
    #[arg(long, default_value = "Enter the requested credential")]
    pub label: String,
    /// Accept a masked multiline value, such as a private key.
    #[arg(long)]
    pub multiline: bool,
    /// Write to an owner-only file (0600 on Unix, protected DACL on Windows).
    #[arg(long, value_name = "PATH")]
    pub to_file: Option<PathBuf>,
    /// Store a provider credential in clud's native vault.
    #[arg(long, value_enum)]
    pub vault: Option<Vault>,
    /// Deliver the credential to this command's stdin; suppress its output.
    #[arg(long, num_args = 1.., allow_hyphen_values = true, value_name = "COMMAND")]
    pub to_command: Vec<String>,
}

#[derive(Debug, Clone, Copy, ValueEnum)]
pub enum Vault {
    Deepseek,
    Kimi,
    Openrouter,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Failure {
    Cancelled,
    Unavailable,
    Invalid,
    Sink,
}

type Secret = Zeroizing<String>;

/// Self-contained dispatch: no harness, daemon, transcript or model call.
pub fn run(options: &Options) -> i32 {
    let result = acquire(options).and_then(|secret| sink::deliver(options, &secret));
    report(result)
}

fn report(result: Result<(), Failure>) -> i32 {
    match result {
        Ok(()) => {
            println!("stored");
            0
        }
        Err(Failure::Cancelled) => {
            println!("cancelled");
            3
        }
        Err(error) => {
            println!("failed");
            eprintln!(
                "{}",
                match error {
                    Failure::Unavailable =>
                        "No graphical secret prompt is available; use a desktop with its native graphics libraries installed.",
                    Failure::Invalid =>
                        "The credential is empty, too large or invalid for this input mode.",
                    _ => "Could not deliver the credential to the requested sink.",
                }
            );
            1
        }
    }
}

fn acquire(options: &Options) -> Result<Secret, Failure> {
    if test_mode() {
        if std::env::var_os("CLUD_TEST_SECRET_PROMPT_CANCEL").is_some() {
            return Err(Failure::Cancelled);
        }
        let fixture = std::env::var_os("CLUD_TEST_SECRET_PROMPT_FILE")
            .filter(|_| std::env::var_os("CLUD_TEST_SECRET_PROMPT_GUI").is_none());
        if let Some(path) = fixture {
            let text = std::fs::read_to_string(path).map_err(|_| Failure::Unavailable)?;
            return validate_secret(Zeroizing::new(text), options.multiline);
        }
    }
    gui::prompt(options).and_then(|secret| validate_secret(secret, options.multiline))
}

fn validate_secret(secret: Secret, multiline: bool) -> Result<Secret, Failure> {
    if secret.is_empty() || secret.len() > 1024 * 1024 || secret.contains('\0') {
        return Err(Failure::Invalid);
    }
    if !multiline && secret.contains(['\r', '\n']) {
        return Err(Failure::Invalid);
    }
    Ok(secret)
}

fn test_mode() -> bool {
    cfg!(debug_assertions)
        && std::env::var_os("CLUD_INTEGRATION_TESTS").is_some_and(|value| value == "1")
}

/// The multicall prompt has exactly the same validated sink options.
pub fn run_alias(argv: &[std::ffi::OsString]) -> i32 {
    match Options::try_parse_from(argv) {
        Ok(options) => run(&options),
        Err(error) => {
            let code = error.exit_code();
            let _ = error.print();
            code
        }
    }
}

/// Askpass is a separate, caller-checked protocol, never the agent-facing CLI.
pub fn run_askpass(argv: &[std::ffi::OsString]) -> i32 {
    if !sink::askpass_caller_allowed() {
        eprintln!("failed: askpass must be invoked by git, ssh or sudo");
        return 2;
    }
    let options = Options {
        title: "clud authentication".to_string(),
        label: argv.get(1).map_or_else(
            || "Enter the requested credential".to_string(),
            |value| value.to_string_lossy().into_owned(),
        ),
        multiline: false,
        to_file: None,
        vault: None,
        to_command: Vec::new(),
    };
    match acquire(&options) {
        Ok(secret) => {
            use std::io::Write;
            // Only the authenticated calling tool receives this protocol output.
            let mut stdout = std::io::stdout().lock();
            if stdout
                .write_all(secret.as_bytes())
                .and_then(|()| stdout.write_all(b"\n"))
                .is_ok()
            {
                0
            } else {
                1
            }
        }
        Err(error) => {
            eprintln!(
                "{}",
                if error == Failure::Cancelled {
                    "cancelled"
                } else {
                    "failed"
                }
            );
            if error == Failure::Cancelled {
                3
            } else {
                1
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn secret_validation_preserves_multiline_and_refuses_invalid_values() {
        let text = "private\nkey\n";
        assert_eq!(
            &*validate_secret(Zeroizing::new(text.into()), true).unwrap(),
            text
        );
        assert!(validate_secret(Zeroizing::new(text.into()), false).is_err());
        assert!(validate_secret(Zeroizing::new(String::new()), true).is_err());
        assert!(validate_secret(Zeroizing::new("a\0b".into()), true).is_err());
    }

    #[test]
    fn a_sink_is_required_and_mutually_exclusive() {
        assert!(Options::try_parse_from(["secret"]).is_err());
        assert!(Options::try_parse_from(["secret", "--to-file", "x", "--vault", "kimi"]).is_err());
        let parsed = Options::try_parse_from([
            "secret",
            "--to-command",
            "gh",
            "auth",
            "login",
            "--with-token",
        ])
        .unwrap();
        assert_eq!(parsed.to_command, ["gh", "auth", "login", "--with-token"]);
    }
}
