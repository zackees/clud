//! Explicit Windows launch through the packaged native WezTerm GUI.

use crate::args::Args;
use crate::subprocess::ManagedSubprocess;
use std::path::{Path, PathBuf};

const ENV_KITTY_TERM: &str = "CLUD_KITTY_TERM";
const ENV_BINARY_OVERRIDE: &str = "CLUD_KITTY_TERM_BINARY";
const ENV_CONFIG_OVERRIDE: &str = "CLUD_KITTY_TERM_CONFIG";

/// Handle only an explicit Kitty terminal request before ordinary startup.
pub fn handle(args: &Args) -> Option<i32> {
    if !args.kitty_term {
        return None;
    }
    if args.command.is_some() {
        eprintln!("[clud] --kitty-term only applies to a backend launch, not a clud subcommand");
        return Some(2);
    }
    if std::env::var_os(ENV_KITTY_TERM).is_some()
        || std::env::var_os(crate::webterm::ENV_WEBTERM).is_some()
    {
        eprintln!("[clud] --kitty-term cannot launch inside an existing desktop terminal");
        return Some(2);
    }
    if let Some(error) = platform_error(cfg!(windows), std::env::consts::ARCH) {
        eprintln!("[clud] {error}");
        return Some(1);
    }
    Some(match launch(args) {
        Ok(exit_code) => exit_code,
        Err(error) => {
            eprintln!("[clud] Kitty terminal unavailable: {error}");
            1
        }
    })
}

fn platform_error(is_windows: bool, architecture: &str) -> Option<&'static str> {
    if !is_windows {
        return Some("--kitty-term is currently supported on Windows only");
    }
    if architecture == "aarch64" {
        return Some(
            "--kitty-term does not support Windows ARM64; no ARM64 WezTerm GUI bundle is packaged",
        );
    }
    if architecture != "x86_64" {
        return Some("--kitty-term currently requires x86_64 Windows");
    }
    None
}

fn launch(args: &Args) -> Result<i32, String> {
    let executable = std::env::current_exe().map_err(|error| format!("locating clud: {error}"))?;
    let cwd =
        std::env::current_dir().map_err(|error| format!("reading current directory: {error}"))?;
    let (binary, config) = companion_paths(&executable)?;
    let argv = launch_argv(args, &binary, &config, &executable, &cwd);
    let process = ManagedSubprocess::start_inheriting_env(argv, None, false, None)?;
    process.wait(None)
}

fn companion_paths(executable: &Path) -> Result<(PathBuf, PathBuf), String> {
    let (default_binary, default_config) = default_paths(executable)?;
    let binary = std::env::var_os(ENV_BINARY_OVERRIDE)
        .map(PathBuf::from)
        .unwrap_or(default_binary);
    let config = std::env::var_os(ENV_CONFIG_OVERRIDE)
        .map(PathBuf::from)
        .unwrap_or(default_config);
    for (name, path) in [("WezTerm GUI", &binary), ("Kitty terminal config", &config)] {
        if !path.is_file() {
            return Err(format!(
                "{name} is missing at {}; install the Windows Kitty terminal bundle",
                path.display()
            ));
        }
    }
    Ok((binary, config))
}

fn default_paths(executable: &Path) -> Result<(PathBuf, PathBuf), String> {
    let directory = executable
        .parent()
        .and_then(Path::parent)
        .ok_or_else(|| {
            format!(
                "clud executable has no parent directory: {}",
                executable.display()
            )
        })?
        .join("clud-kittyterm");
    Ok((
        directory.join("wezterm-gui.exe"),
        directory.join("clud-kittyterm.lua"),
    ))
}

fn launch_argv(
    args: &Args,
    binary: &Path,
    config: &Path,
    executable: &Path,
    cwd: &Path,
) -> Vec<String> {
    let mut argv = vec![
        binary.to_string_lossy().into_owned(),
        "--config-file".into(),
        config.to_string_lossy().into_owned(),
        "start".into(),
        "--always-new-process".into(),
        "--no-auto-connect".into(),
        "--return-initial-exit-code".into(),
        "--cwd".into(),
        cwd.to_string_lossy().into_owned(),
        "--".into(),
        executable.to_string_lossy().into_owned(),
    ];
    let mut backend_arguments = false;
    for item in args.raw_argv.iter().skip(1) {
        if item == "--" {
            backend_arguments = true;
        }
        if !backend_arguments && item == "--kitty-term" {
            continue;
        }
        argv.push(item.clone());
    }
    argv
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    fn parse(raw: &[&str]) -> Args {
        Args::parse_from_raw(raw.iter().map(|item| item.to_string()).collect())
    }

    #[test]
    fn forwards_original_argv_with_native_wezterm_launch_flags() {
        let args = parse(&[
            "clud",
            "--kitty-term",
            "--codex",
            "-p",
            "hello",
            "--",
            "--kitty-term",
        ]);
        let argv = launch_argv(
            &args,
            Path::new("C:/bin/clud-kittyterm/wezterm-gui.exe"),
            Path::new("C:/bin/clud-kittyterm/clud-kittyterm.lua"),
            Path::new("C:/bin/clud.exe"),
            Path::new("C:/work"),
        );
        assert_eq!(
            argv,
            [
                "C:/bin/clud-kittyterm/wezterm-gui.exe",
                "--config-file",
                "C:/bin/clud-kittyterm/clud-kittyterm.lua",
                "start",
                "--always-new-process",
                "--no-auto-connect",
                "--return-initial-exit-code",
                "--cwd",
                "C:/work",
                "--",
                "C:/bin/clud.exe",
                "--codex",
                "-p",
                "hello",
                "--",
                "--kitty-term",
            ]
        );
    }

    #[test]
    fn companion_defaults_to_wheel_data_directory() {
        let (binary, config) = default_paths(Path::new("C:/venv/Scripts/clud.exe")).unwrap();
        assert_eq!(binary, Path::new("C:/venv/clud-kittyterm/wezterm-gui.exe"));
        assert_eq!(
            config,
            Path::new("C:/venv/clud-kittyterm/clud-kittyterm.lua")
        );
    }

    #[test]
    fn rejects_explicit_subcommand_before_launcher_resolution() {
        let args = parse(&["clud", "--kitty-term", "auth", "status"]);
        assert_eq!(handle(&args), Some(2));
    }

    #[test]
    fn windows_arm64_reports_unsupported_architecture_before_bundle_lookup() {
        assert_eq!(
            platform_error(true, "aarch64"),
            Some("--kitty-term does not support Windows ARM64; no ARM64 WezTerm GUI bundle is packaged")
        );
        assert_eq!(platform_error(true, "x86_64"), None);
        assert_eq!(
            platform_error(false, "x86_64"),
            Some("--kitty-term is currently supported on Windows only")
        );
    }
}
