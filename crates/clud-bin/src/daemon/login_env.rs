//! Materialize the stable, login-level environment used as a daemon session floor.
//!
//! A daemon must not use the shell of whichever client happened to start it as
//! its permanent environment. Session-specific values arrive in the request;
//! this module supplies the refreshable OS/login layer below them.

use std::io;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, OnceLock, RwLock};
use std::thread;
use std::time::Duration;

#[cfg(unix)]
use std::time::Instant;

#[cfg(unix)]
use running_process::{
    CommandSpec, NativeProcess, ProcessConfig, ReadStatus, StderrMode, StdinMode, StreamKind,
};

const REFRESH_INTERVAL: Duration = Duration::from_secs(5 * 60);
#[cfg(unix)]
const POSIX_LOGIN_ENV_TIMEOUT: Duration = Duration::from_secs(3);

static LOGIN_ENV: OnceLock<LoginEnvironment> = OnceLock::new();

/// A daemon-owned snapshot of the operating system's login environment.
///
/// The snapshot is replaced atomically enough for this use: a session receives
/// one complete clone, while an already-running worker retains the clone it was
/// created with. A refresh therefore cannot alter a live session's child env.
#[derive(Clone)]
pub(super) struct LoginEnvironment {
    values: Arc<RwLock<Vec<(String, String)>>>,
}

impl LoginEnvironment {
    fn new() -> Self {
        let initial = materialize().unwrap_or_else(|_| std::env::vars().collect());
        Self {
            values: Arc::new(RwLock::new(initial)),
        }
    }

    fn refresh_and_snapshot(&self) -> Vec<(String, String)> {
        if let Ok(fresh) = materialize() {
            *self
                .values
                .write()
                .expect("login environment lock poisoned") = fresh;
        }
        self.snapshot()
    }

    fn snapshot(&self) -> Vec<(String, String)> {
        self.values
            .read()
            .expect("login environment lock poisoned")
            .clone()
    }
}

/// Initialize the one daemon-local login environment and keep it converging
/// while the daemon is alive. Calling this in the daemon entry, rather than the
/// client, makes the baseline independent of the client that won auto-start.
pub(super) fn start_refreshing(shutdown_requested: Arc<AtomicBool>) {
    let environment = LOGIN_ENV.get_or_init(LoginEnvironment::new).clone();
    let _ = thread::Builder::new()
        .name("clud-login-env-refresh".to_string())
        .spawn(move || {
            while !shutdown_requested.load(Ordering::SeqCst) {
                for _ in 0..REFRESH_INTERVAL.as_secs() {
                    if shutdown_requested.load(Ordering::SeqCst) {
                        return;
                    }
                    thread::sleep(Duration::from_secs(1));
                }
                let _ = environment.refresh_and_snapshot();
            }
        });
}

/// Refresh immediately at session admission, then return the immutable base to
/// persist in that worker's spec. The fallback is intentionally the daemon's
/// inherited env only for direct unit dispatches that do not run `run_daemon`.
pub(super) fn snapshot_for_new_session() -> Vec<(String, String)> {
    let mut base = LOGIN_ENV
        .get()
        .map(LoginEnvironment::refresh_and_snapshot)
        .unwrap_or_else(|| std::env::vars().collect());

    // These values describe the daemon's own protocol and lifecycle rather
    // than the client shell. Keep them above the login floor, but do not carry
    // arbitrary inherited session exports into every future worker.
    for (key, value) in std::env::vars() {
        if key.starts_with("CLUD_DAEMON_") || key.starts_with("RUNNING_PROCESS_") {
            replace(&mut base, key, value);
        }
    }
    base
}

fn materialize() -> io::Result<Vec<(String, String)>> {
    #[cfg(windows)]
    {
        materialize_windows()
    }
    #[cfg(unix)]
    {
        materialize_posix()
    }
    #[cfg(not(any(unix, windows)))]
    {
        Ok(std::env::vars().collect())
    }
}

/// Re-run the account's login shell from a deliberately tiny bootstrap env.
///
/// `ProcessConfig::env` uses running-process's `EnvironmentPolicy::Clear`, so
/// session-local exports (virtualenv, shims, tokens, and prior daemon state)
/// cannot leak into the base. Those values are instead supplied by the client
/// overlay when a session is admitted. The static command is important: no
/// caller-controlled text reaches a shell.
#[cfg(unix)]
fn materialize_posix() -> io::Result<Vec<(String, String)>> {
    let shell = login_shell();
    let config = ProcessConfig {
        command: CommandSpec::Argv(vec![
            shell.clone(),
            "-l".to_string(),
            "-c".to_string(),
            "printf '\\0'; command env -0".to_string(),
        ]),
        cwd: None,
        env: Some(posix_bootstrap_env(&shell)),
        capture: true,
        stderr_mode: StderrMode::Pipe,
        creationflags: None,
        create_process_group: false,
        stdin_mode: StdinMode::Null,
        nice: None,
    };
    let process = NativeProcess::new(config);
    process
        .start()
        .map_err(|error| io::Error::other(format!("start login shell: {error}")))?;

    let deadline = Instant::now() + POSIX_LOGIN_ENV_TIMEOUT;
    let mut stdout = Vec::new();
    loop {
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            let _ = process.kill();
            let _ = process.wait(Some(Duration::from_secs(1)));
            return Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "login shell environment materialization timed out",
            ));
        }
        match process.read_combined(Some(remaining.min(Duration::from_millis(100)))) {
            ReadStatus::Line(event) if event.stream == StreamKind::Stdout => {
                stdout.extend(event.line);
            }
            ReadStatus::Line(_) | ReadStatus::Timeout => {}
            ReadStatus::Eof => break,
        }
    }
    let exit_code = process
        .wait(Some(Duration::from_secs(1)))
        .map_err(|error| io::Error::other(format!("wait for login shell: {error}")))?;
    if exit_code != 0 {
        return Err(io::Error::other(format!(
            "login shell exited with status {exit_code}"
        )));
    }
    parse_nul_environment(after_login_marker(&stdout)?)
}

/// Login profiles sometimes print a banner. The static command emits one NUL
/// before `env -0`, making that arbitrary stdout unambiguously discardable.
#[cfg(unix)]
fn after_login_marker(stdout: &[u8]) -> io::Result<&[u8]> {
    let Some(marker) = stdout.iter().position(|byte| *byte == 0) else {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "login shell did not emit the environment marker",
        ));
    };
    Ok(&stdout[marker + 1..])
}

#[cfg(unix)]
fn login_shell() -> String {
    let candidate = std::env::var("SHELL").unwrap_or_else(|_| "/bin/sh".to_string());
    let path = std::path::Path::new(&candidate);
    if path.is_absolute() && path.is_file() {
        candidate
    } else {
        "/bin/sh".to_string()
    }
}

#[cfg(unix)]
fn posix_bootstrap_env(shell: &str) -> Vec<(String, String)> {
    let home = dirs::home_dir()
        .or_else(|| std::env::var_os("HOME").map(Into::into))
        .unwrap_or_else(|| std::path::PathBuf::from("/"));
    let mut env = vec![
        ("HOME".to_string(), home.to_string_lossy().into_owned()),
        (
            "PATH".to_string(),
            "/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin".to_string(),
        ),
        ("SHELL".to_string(), shell.to_string()),
    ];
    for key in ["USER", "LOGNAME"] {
        if let Ok(value) = std::env::var(key) {
            env.push((key.to_string(), value));
        }
    }
    env
}

/// Parse `env -0` output without treating newlines in a value as record
/// separators. Empty malformed records are ignored, but an output without one
/// usable key is rejected so callers retain the prior known-good snapshot.
#[cfg(any(unix, test))]
fn parse_nul_environment(bytes: &[u8]) -> io::Result<Vec<(String, String)>> {
    let mut entries = Vec::new();
    for record in bytes
        .split(|byte| *byte == 0)
        .filter(|record| !record.is_empty())
    {
        let Some(separator) = record.iter().position(|byte| *byte == b'=') else {
            continue;
        };
        let (key, value) = (&record[..separator], &record[separator + 1..]);
        if key.is_empty() {
            continue;
        }
        let key = String::from_utf8(key.to_vec()).map_err(|_| {
            io::Error::new(
                io::ErrorKind::InvalidData,
                "login environment key was not UTF-8",
            )
        })?;
        let value = String::from_utf8(value.to_vec()).map_err(|_| {
            io::Error::new(
                io::ErrorKind::InvalidData,
                "login environment value was not UTF-8",
            )
        })?;
        replace(&mut entries, key, value);
    }
    if entries.is_empty() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "login shell produced no environment entries",
        ));
    }
    Ok(entries)
}

fn replace(entries: &mut Vec<(String, String)>, key: String, value: String) {
    if let Some((_, existing)) = entries
        .iter_mut()
        .find(|(existing, _)| same_key(existing, &key))
    {
        *existing = value;
    } else {
        entries.push((key, value));
    }
}

fn same_key(left: &str, right: &str) -> bool {
    #[cfg(windows)]
    {
        left.eq_ignore_ascii_case(right)
    }
    #[cfg(not(windows))]
    {
        left == right
    }
}

#[cfg(windows)]
fn materialize_windows() -> io::Result<Vec<(String, String)>> {
    use winreg::enums::{HKEY_CURRENT_USER, HKEY_LOCAL_MACHINE};
    use winreg::RegKey;

    const MACHINE_PATH: &str = r"SYSTEM\CurrentControlSet\Control\Session Manager\Environment";
    const USER_PATH: &str = r"Environment";

    let mut environment = Vec::new();
    // A normal Windows process has these loader-owned values even when they
    // are absent from the persisted user/machine keys. Seed them before
    // reading registry values so `%SystemRoot%` can expand in a machine PATH.
    for key in ["SystemRoot", "SystemDrive", "USERPROFILE"] {
        if let Ok(value) = std::env::var(key) {
            replace(&mut environment, key.to_string(), value);
        }
    }
    let machine = RegKey::predef(HKEY_LOCAL_MACHINE).open_subkey(MACHINE_PATH)?;
    append_registry_values(&mut environment, machine)?;
    let user = RegKey::predef(HKEY_CURRENT_USER).open_subkey(USER_PATH)?;
    append_registry_values(&mut environment, user)?;
    if environment.is_empty() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "Windows registry environment was empty",
        ));
    }
    Ok(environment)
}

#[cfg(windows)]
fn append_registry_values(
    environment: &mut Vec<(String, String)>,
    key: winreg::RegKey,
) -> io::Result<()> {
    use winreg::enums::RegType;
    use winreg::types::FromRegValue;

    for item in key.enum_values() {
        let (name, value) = item?;
        if !matches!(value.vtype, RegType::REG_SZ | RegType::REG_EXPAND_SZ) {
            continue;
        }
        let raw = String::from_reg_value(&value)
            .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
        let expanded = expand_windows_variables(&raw, environment);
        if name.eq_ignore_ascii_case("PATH") {
            if let Some(existing) = value_of(environment, "PATH") {
                replace(environment, name, format!("{existing};{expanded}"));
            } else {
                replace(environment, name, expanded);
            }
        } else {
            replace(environment, name, expanded);
        }
    }
    Ok(())
}

#[cfg(windows)]
fn value_of<'a>(entries: &'a [(String, String)], key: &str) -> Option<&'a str> {
    entries
        .iter()
        .find(|(existing, _)| same_key(existing, key))
        .map(|(_, value)| value.as_str())
}

#[cfg(windows)]
fn expand_windows_variables(value: &str, entries: &[(String, String)]) -> String {
    let mut result = String::new();
    let mut remainder = value;
    while let Some(start) = remainder.find('%') {
        result.push_str(&remainder[..start]);
        let after_start = &remainder[start + 1..];
        let Some(end) = after_start.find('%') else {
            result.push('%');
            result.push_str(after_start);
            return result;
        };
        let name = &after_start[..end];
        if name.is_empty() {
            result.push_str("%%");
        } else if let Some(expansion) = value_of(entries, name) {
            result.push_str(expansion);
        } else if let Ok(expansion) = std::env::var(name) {
            result.push_str(&expansion);
        } else {
            result.push('%');
            result.push_str(name);
            result.push('%');
        }
        remainder = &after_start[end + 1..];
    }
    result.push_str(remainder);
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_nul_delimited_environment_with_newlines_in_values() {
        let parsed = parse_nul_environment(b"PATH=/bin\0MULTI=first\nsecond\0HOME=/home/a\0")
            .expect("valid env");
        assert_eq!(parsed[0], ("PATH".to_string(), "/bin".to_string()));
        assert_eq!(
            parsed[1],
            ("MULTI".to_string(), "first\nsecond".to_string())
        );
        assert_eq!(parsed[2], ("HOME".to_string(), "/home/a".to_string()));
    }

    #[test]
    fn last_value_for_a_key_wins() {
        let parsed = parse_nul_environment(b"PATH=/old\0PATH=/new\0").expect("valid env");
        assert_eq!(parsed, vec![("PATH".to_string(), "/new".to_string())]);
    }

    #[test]
    fn rejects_empty_or_malformed_output() {
        assert!(parse_nul_environment(b"").is_err());
        assert!(parse_nul_environment(b"not-an-entry\0").is_err());
    }

    #[cfg(unix)]
    #[test]
    fn login_marker_discards_profile_banners_without_touching_env_records() {
        let bytes = b"welcome to the shell\n\0PATH=/bin\0";
        assert_eq!(after_login_marker(bytes).unwrap(), b"PATH=/bin\0");
    }

    #[cfg(unix)]
    #[test]
    fn bootstrap_env_is_minimal_and_keeps_the_chosen_shell() {
        let env = posix_bootstrap_env("/bin/bash");
        assert_eq!(
            env.iter().find(|(key, _)| key == "SHELL").unwrap().1,
            "/bin/bash"
        );
        assert!(env.iter().any(|(key, _)| key == "HOME"));
        assert!(env.iter().any(|(key, _)| key == "PATH"));
        assert!(!env.iter().any(|(key, _)| key == "VIRTUAL_ENV"));
    }
}
