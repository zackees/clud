//! Windows User Path edits and OS-built fresh-environment verification.

use super::activation::{ActivationPlan, WindowsPathValue};
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::process::Command; // running-process: command-builder
use std::thread;
use std::time::{Duration, Instant};

use running_process::{spawn_with_env_policy, EnvironmentPolicy, SpawnStdio, StdioSource};
use winreg::enums::{RegType, HKEY_CURRENT_USER, KEY_READ, KEY_WRITE};
use winreg::RegKey;

const USER_ENVIRONMENT: &str = "Environment";

pub(super) fn plan(destination: &Path) -> Result<ActivationPlan, String> {
    let bin = destination.parent().ok_or("missing install directory")?;
    let directory = bin.to_str().ok_or("install directory is not UTF-8")?;
    if directory.contains([';', '\0', '\r', '\n']) {
        return Err("install directory contains a Windows Path separator".into());
    }
    let baseline = running_process::environment::user_baseline_environment()
        .map_err(|error| format!("read OS-built User environment: {error}"))?;
    let local = baseline
        .into_iter()
        .find(|(key, _)| key.to_string_lossy().eq_ignore_ascii_case("LOCALAPPDATA"))
        .map(|(_, value)| PathBuf::from(value))
        .ok_or("OS-built User environment lacks LOCALAPPDATA")?;
    let expected = local.join("Programs/clud/bin/clud.exe");
    if !same_path(destination, &expected) {
        return Err(format!(
            "install destination {} differs from the OS User profile {}; use the normal User environment",
            destination.display(), expected.display()
        ));
    }
    let prior_user_path = read_user_path()?;
    let raw = prior_user_path
        .as_ref()
        .map(|value| decode_path(&value.bytes))
        .transpose()?
        .unwrap_or_default();
    let next = prepend_path(directory, &raw);
    if next.encode_utf16().count() > 32766 {
        return Err("User Path would exceed the Windows environment limit".into());
    }
    let next_user_path = WindowsPathValue {
        bytes: encode_path(&next),
        kind: prior_user_path
            .as_ref()
            .map(|value| value.kind.clone())
            .unwrap_or(RegType::REG_EXPAND_SZ),
    };
    Ok(ActivationPlan {
        destination: destination.to_path_buf(),
        prior_user_path,
        next_user_path,
    })
}

fn same_path(left: &Path, right: &Path) -> bool {
    left.to_string_lossy()
        .replace('/', "\\")
        .trim_end_matches('\\')
        .eq_ignore_ascii_case(
            right
                .to_string_lossy()
                .replace('/', "\\")
                .trim_end_matches('\\'),
        )
}

fn prepend_path(directory: &str, raw: &str) -> String {
    if raw.split(';').next().is_some_and(|first| {
        first
            .trim_end_matches(['\\', '/'])
            .eq_ignore_ascii_case(directory.trim_end_matches(['\\', '/']))
    }) {
        return raw.to_owned();
    }
    if raw.is_empty() {
        directory.to_owned()
    } else {
        format!("{directory};{raw}")
    }
}

fn decode_path(bytes: &[u8]) -> Result<String, String> {
    if !bytes.len().is_multiple_of(2) {
        return Err("User Path has malformed UTF-16 bytes".into());
    }
    let words: Vec<u16> = bytes
        .chunks_exact(2)
        .map(|pair| u16::from_le_bytes([pair[0], pair[1]]))
        .collect();
    let words = words.strip_suffix(&[0]).unwrap_or(&words);
    if words.contains(&0) {
        return Err("User Path contains an embedded NUL".into());
    }
    String::from_utf16(words).map_err(|error| format!("User Path is not UTF-16: {error}"))
}

fn encode_path(value: &str) -> Vec<u8> {
    value
        .encode_utf16()
        .chain(std::iter::once(0))
        .flat_map(u16::to_le_bytes)
        .collect()
}

fn read_user_path() -> Result<Option<WindowsPathValue>, String> {
    let key = RegKey::predef(HKEY_CURRENT_USER)
        .open_subkey_with_flags(USER_ENVIRONMENT, KEY_READ)
        .map_err(|error| format!("open User environment: {error}"))?;
    let value = match key.get_raw_value("Path") {
        Ok(value) => value,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(format!("read User Path: {error}")),
    };
    if !matches!(value.vtype, RegType::REG_SZ | RegType::REG_EXPAND_SZ) {
        return Err("User Path is not REG_SZ or REG_EXPAND_SZ".into());
    }
    Ok(Some(WindowsPathValue {
        bytes: value.bytes,
        kind: value.vtype,
    }))
}

pub(super) fn description(plan: &ActivationPlan) -> String {
    let Some(bin) = plan.destination.parent() else {
        return "invalid planned destination".into();
    };
    let before = plan
        .prior_user_path
        .as_ref()
        .map(|value| decode_path(&value.bytes).unwrap_or_default())
        .unwrap_or_default();
    let after = decode_path(&plan.next_user_path.bytes).unwrap_or_default();
    format!(
        "{} HKCU\\Environment\\Path ({:?}) from {:?} to {:?}; prepend {}; broadcast Environment change; verify OS-built User environment",
        if plan.prior_user_path.as_ref() == Some(&plan.next_user_path) { "keep" } else { "write" },
        plan.next_user_path.kind,
        before,
        after,
        bin.display()
    )
}

pub(super) fn apply_and_verify(plan: &ActivationPlan, version: &str) -> Result<(), String> {
    if read_user_path()? != plan.prior_user_path {
        return Err("User Path changed after consent".into());
    }
    let changed = plan.prior_user_path.as_ref() != Some(&plan.next_user_path);
    if changed {
        write_user_path(Some(&plan.next_user_path))?;
        broadcast();
    }
    if let Err(error) = verify(plan, version) {
        if changed && read_user_path()? == Some(plan.next_user_path.clone()) {
            write_user_path(plan.prior_user_path.as_ref())?;
            broadcast();
        }
        return Err(error);
    }
    Ok(())
}

fn write_user_path(value: Option<&WindowsPathValue>) -> Result<(), String> {
    let key = RegKey::predef(HKEY_CURRENT_USER)
        .open_subkey_with_flags(USER_ENVIRONMENT, KEY_READ | KEY_WRITE)
        .map_err(|error| format!("open User environment for update: {error}"))?;
    match value {
        Some(value) => key
            .set_raw_value(
                "Path",
                &winreg::RegValue {
                    bytes: value.bytes.clone(),
                    vtype: value.kind.clone(),
                },
            )
            .map_err(|error| format!("write User Path: {error}")),
        None => key
            .delete_value("Path")
            .map_err(|error| format!("remove User Path: {error}")),
    }
}

fn broadcast() {
    use windows::Win32::Foundation::{LPARAM, WPARAM};
    use windows::Win32::UI::WindowsAndMessaging::{
        SendMessageTimeoutW, HWND_BROADCAST, SMTO_ABORTIFHUNG, WM_SETTINGCHANGE,
    };

    let area: Vec<u16> = "Environment\0".encode_utf16().collect();
    // SAFETY: the UTF-16 buffer remains live until the bounded synchronous send returns.
    unsafe {
        let _ = SendMessageTimeoutW(
            HWND_BROADCAST,
            WM_SETTINGCHANGE,
            WPARAM(0),
            LPARAM(area.as_ptr() as isize),
            SMTO_ABORTIFHUNG,
            5000,
            None,
        );
    }
}

fn verify(plan: &ActivationPlan, version: &str) -> Result<(), String> {
    let output = run_baseline(
        "$ErrorActionPreference='Stop'; [Console]::OutputEncoding=[Text.UTF8Encoding]::new($false); $c=(Get-Command clud -CommandType Application -ErrorAction Stop).Source; [Console]::WriteLine($c); & clud --version",
    )?;
    let mut lines = output.trim().lines();
    let resolved = lines.next().unwrap_or_default().trim();
    let observed = lines.next().unwrap_or_default().trim();
    if !same_path(Path::new(resolved), &plan.destination) {
        return Err(format!(
            "fresh User environment resolves clud to {resolved:?}, expected {}",
            plan.destination.display()
        ));
    }
    if observed != format!("clud {version}") {
        return Err(format!(
            "fresh User environment returned {observed:?}, expected clud {version}"
        ));
    }
    Ok(())
}

fn run_baseline(script: &str) -> Result<String, String> {
    let system_root = running_process::environment::user_baseline_environment()
        .map_err(|error| format!("read OS-built User environment: {error}"))?
        .into_iter()
        .find(|(key, _)| key.to_string_lossy().eq_ignore_ascii_case("SystemRoot"))
        .map(|(_, value)| PathBuf::from(value))
        .ok_or("OS-built User environment lacks SystemRoot")?;
    let program = system_root.join("System32/WindowsPowerShell/v1.0/powershell.exe");
    let mut command = Command::new(program); // running-process: command-builder
    command.args([
        "-NoLogo",
        "-NoProfile",
        "-NonInteractive",
        "-Command",
        script,
    ]);
    let cwd = tempfile::tempdir().map_err(|error| error.to_string())?;
    command.current_dir(cwd.path());
    let mut output = tempfile::tempfile().map_err(|error| error.to_string())?;
    let mut child = spawn_with_env_policy(
        &mut command,
        SpawnStdio {
            stdin: StdioSource::Null,
            stdout: StdioSource::File(&output),
            stderr: StdioSource::Null,
            drain_timeout: Some(Duration::from_secs(1)),
            show_console: false,
        },
        EnvironmentPolicy::UserBaseline,
    )
    .map_err(|error| format!("launch OS-built User environment probe: {error}"))?;
    let deadline = Instant::now() + Duration::from_secs(15);
    let code = loop {
        if let Some(code) = child.try_wait().map_err(|error| error.to_string())? {
            break code;
        }
        if Instant::now() >= deadline {
            let _ = child.kill();
            return Err("OS-built User environment probe timed out".into());
        }
        thread::sleep(Duration::from_millis(20));
    };
    if code != 0 {
        return Err(format!("OS-built User environment probe exited {code}"));
    }
    output
        .seek(SeekFrom::Start(0))
        .map_err(|error| error.to_string())?;
    let mut bytes = Vec::new();
    (&mut output)
        .take(32 * 1024)
        .read_to_end(&mut bytes)
        .map_err(|error| error.to_string())?;
    String::from_utf8(bytes)
        .map_err(|error| format!("OS-built User environment output is not UTF-8: {error}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn user_path_prepend_keeps_long_expandable_value() {
        let prior = format!("%SystemRoot%\\System32;{}", "C:\\other;".repeat(500));
        let next = prepend_path(r"C:\Users\Ada\Programs\clud\bin", &prior);
        assert!(next.starts_with(r"C:\Users\Ada\Programs\clud\bin;%SystemRoot%\System32;"));
        assert!(next.contains(&"C:\\other;".repeat(499)));
        assert_eq!(decode_path(&encode_path(&next)).unwrap(), next);
    }

    #[test]
    fn user_path_prepend_preserves_existing_entries_and_is_idempotent() {
        let next = prepend_path(
            r"C:\Users\Ada\Programs\clud\bin",
            r"%Tools%;C:\Users\Ada\Programs\clud\bin;D:\keep",
        );
        assert_eq!(
            next,
            r"C:\Users\Ada\Programs\clud\bin;%Tools%;C:\Users\Ada\Programs\clud\bin;D:\keep"
        );
        assert_eq!(prepend_path(r"C:\Users\Ada\Programs\clud\bin", &next), next);
    }
}
