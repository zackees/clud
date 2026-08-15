//! Launch handoff for clud's owned Tauri terminal companion.
//!
//! The companion owns the WebView and PTY. This module deliberately owns only
//! the CLI contract: persistent preference, recursion protection, exact argv
//! forwarding, and a useful error when a headless wheel has no companion.

use std::path::PathBuf;

use crate::args::Args;
use crate::{clud_settings, subprocess};

pub const ENV_WEBTERM: &str = "CLUD_WEBTERM";
const ENV_COMPANION_OVERRIDE: &str = "CLUD_WEBTERM_BINARY";

/// Handle web-terminal-only flags before normal clud startup. `None` means the
/// caller should continue with the ordinary launch path.
pub fn handle(args: &Args) -> Option<i32> {
    if let Some(preference) = args.set_web_term {
        return Some(
            match clud_settings::save_web_term_enabled(preference.enabled()) {
                Ok(()) => {
                    println!(
                        "[clud] web terminal preference is now {}",
                        if preference.enabled() { "on" } else { "off" }
                    );
                    0
                }
                Err(error) => {
                    eprintln!("[clud] failed to save web terminal preference: {error}");
                    1
                }
            },
        );
    }

    if std::env::var_os(ENV_WEBTERM).is_some() {
        return None;
    }

    // Ordinary clud subcommands must remain completely side-effect free with
    // respect to this optional preference (notably, settings reads acquire the
    // settings lock). Only an explicit --web-term may turn a subcommand into a
    // usage error below.
    if !args.web_term && args.command.is_some() {
        return None;
    }

    let persisted = match clud_settings::load_web_term_enabled() {
        Ok(enabled) => enabled,
        Err(error) => {
            eprintln!("[clud] warning: failed to load web terminal preference: {error}");
            false
        }
    };
    let requested = args.web_term || (persisted && args.command.is_none());
    if !requested {
        return None;
    }
    if args.command.is_some() {
        eprintln!("[clud] --web-term only applies to a backend launch, not a clud subcommand");
        return Some(2);
    }

    Some(match launch(companion_argv(args)) {
        Ok(()) => 0,
        Err(error) => {
            eprintln!("[clud] web terminal unavailable: {error}");
            1
        }
    })
}

fn launch(argv: Vec<String>) -> Result<(), String> {
    let companion = companion_path()?;
    let process = subprocess::ManagedSubprocess::start_inheriting_env(
        [vec![companion.to_string_lossy().to_string()], argv].concat(),
        None,
        false,
        None,
    )?;
    let exit_code = process.wait(None)?;
    if exit_code == 0 {
        Ok(())
    } else {
        Err(format!("web terminal exited with {exit_code}"))
    }
}

fn companion_path() -> Result<PathBuf, String> {
    if let Some(override_path) = std::env::var_os(ENV_COMPANION_OVERRIDE) {
        let path = PathBuf::from(override_path);
        if path.is_file() {
            return Ok(path);
        }
        return Err(format!(
            "{ENV_COMPANION_OVERRIDE} points to a missing file: {}",
            path.display()
        ));
    }
    let current = std::env::current_exe().map_err(|error| format!("locating clud: {error}"))?;
    let file_name = if cfg!(windows) {
        "clud-webterm.exe"
    } else {
        "clud-webterm"
    };
    let path = current
        .parent()
        .ok_or_else(|| {
            format!(
                "clud executable has no parent directory: {}",
                current.display()
            )
        })?
        .join(file_name);
    if path.is_file() {
        Ok(path)
    } else {
        Err(format!(
            "this build does not include {file_name}; install a desktop clud wheel or use the normal console"
        ))
    }
}

/// Arguments supplied to the companion after its executable name. They start
/// with the clud executable and retain every user argument except the webterm
/// control flags, so the inner clud process sees the same backend launch.
pub fn companion_argv(args: &Args) -> Vec<String> {
    let mut forwarded = Vec::new();
    let mut index = 1;
    while index < args.raw_argv.len() {
        let item = &args.raw_argv[index];
        if item == "--web-term" {
            index += 1;
            continue;
        }
        if item == "--set-web-term" {
            index += 2;
            continue;
        }
        if item.starts_with("--set-web-term=") {
            index += 1;
            continue;
        }
        forwarded.push(item.clone());
        index += 1;
    }
    // The companion command line is `clud-webterm -- <clud> <args...>`.
    // This sentinel belongs to the companion; any later `--` remains part of
    // the original clud invocation and therefore must not affect this prefix.
    forwarded.insert(0, "--".to_string());
    let executable = std::env::current_exe()
        .map(|path| path.to_string_lossy().to_string())
        .unwrap_or_else(|_| "clud".to_string());
    forwarded.insert(1, executable);
    forwarded
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::args::Args;

    fn parse(raw: &[&str]) -> Args {
        Args::parse_from_raw(raw.iter().map(|item| item.to_string()).collect())
    }

    #[test]
    fn preference_parser_supports_default_on_and_explicit_off() {
        assert_eq!(
            parse(&["clud", "--set-web-term"]).set_web_term,
            Some(crate::args::WebTermPreference::On)
        );
        assert_eq!(
            parse(&["clud", "--set-web-term", "off"]).set_web_term,
            Some(crate::args::WebTermPreference::Off)
        );
    }

    #[test]
    fn companion_argv_removes_only_webterm_controls() {
        let args = parse(&["clud", "--web-term", "--codex", "-p", "hello"]);
        let argv = companion_argv(&args);
        assert_eq!(argv[0], "--");
        assert!(argv.iter().any(|item| item == "--codex"));
        assert!(argv.iter().any(|item| item == "hello"));
        assert!(!argv.iter().any(|item| item == "--web-term"));
    }

    #[test]
    fn companion_argv_keeps_a_backend_separator_after_its_own_prefix() {
        let args = parse(&["clud", "--web-term", "--codex", "--", "hello"]);
        assert_eq!(
            companion_argv(&args)[0],
            "--",
            "the companion's separator must always be first"
        );
        assert_eq!(
            companion_argv(&args)[2..],
            ["--codex", "--", "hello"],
            "the backend separator is forwarded unchanged"
        );
    }
}
