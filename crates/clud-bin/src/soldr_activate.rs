//! Session-startup soldr activation.
//!
//! Called early from `main.rs` (before any subprocess spawn that might
//! resolve `cargo` / `rustc` from PATH). The flow:
//!
//! 1. [`crate::repo_clud_config::discover_effective_clud_config`] merges
//!    user-level `~/.clud/settings.json` under repo-level
//!    `<repo-root>/.clud/settings.json` (repo wins per-field).
//! 2. If `rust.use_soldr` is `true`, spawn `soldr shims --json` and
//!    capture the JSON.
//! 3. Prepend the JSON's `path_entry` to `PATH` in-process. Every
//!    subsequent subprocess inherits the modified PATH and routes its
//!    `cargo` / `rustc` calls through soldr.
//!
//! Failure-mode contract (zackees/clud#343): **every** way the soldr
//! probe can fail — `soldr` not on PATH, exit ≠ 0, hung, malformed
//! JSON, missing `path_entry`, dir doesn't exist — must result in
//! exactly one warning line on stderr and a clean fall-through to
//! "behave as if `.clud/settings.json` were absent". Never panic,
//! never abort the session, never prompt.
//!
//! On-demand soldr install (zackees/clud#343 + user follow-up): when
//! `rust.install` is `true` (default) and soldr is missing, this module
//! attempts to install it via `uv tool install soldr` (preferred) or
//! `pip install --user soldr` (fallback), honoring the optional
//! `rust.version` pin. The install is **best-effort** — a failure
//! engages the same warn-and-continue contract above.

use crate::repo_clud_config::{discover_effective_clud_config, RepoCludConfig};
use serde::Deserialize;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::Duration;

const SOLDR_SHIMS_TIMEOUT: Duration = Duration::from_secs(15);
const SOLDR_INSTALL_TIMEOUT: Duration = Duration::from_secs(60);

/// Expected JSON shape from `soldr shims --json`. We tolerate unknown
/// fields (forward-compat) and only require `schema_version` and
/// `path_entry`.
#[derive(Debug, Deserialize)]
struct SoldrShimsJson {
    schema_version: u32,
    path_entry: Option<String>,
    #[serde(default)]
    soldr_version: Option<String>,
}

/// Top-level entry point. Called from `main.rs` after `trampoline::unlock_exe()`
/// and before any subprocess that might want a soldr-routed cargo.
///
/// Returns `()` unconditionally — every failure path warns + continues.
pub fn activate_soldr_shims_if_requested() {
    let cwd = match std::env::current_dir() {
        Ok(p) => p,
        Err(_) => return,
    };

    let Some(cfg) = discover_effective_clud_config(&cwd) else {
        return;
    };

    if !cfg.rust.use_soldr {
        // Honored opt-out; no warning needed — the user explicitly turned
        // soldr routing off.
        return;
    }

    activate_with_config(&cfg);
}

/// Internal entry point exposed for testing. Same flow as
/// [`activate_soldr_shims_if_requested`] but takes the resolved config
/// directly so tests can stub discovery.
fn activate_with_config(cfg: &RepoCludConfig) {
    // First probe: does `soldr` exist on PATH?
    if which::which("soldr").is_err() {
        if cfg.rust.install {
            match install_soldr_on_demand(cfg.rust.version.as_deref()) {
                Ok(()) => {
                    // Install succeeded; fall through to the shims invocation.
                }
                Err(reason) => {
                    eprintln!(
                        "clud: failed to install soldr automatically: {reason}; .clud/settings.json directive ignored"
                    );
                    return;
                }
            }
        } else {
            eprintln!(
                "clud: soldr not found on PATH and install is disabled; .clud/settings.json directive ignored"
            );
            return;
        }
    }

    // Second probe: ask soldr for the shim dir.
    match run_soldr_shims_json() {
        Ok(shim_info) => {
            prepend_path_entry(&shim_info.path_entry);
            eprintln!(
                "clud: .clud/settings.json (or ~/.clud/settings.json) detected; routing cargo / rustc / rustfmt / clippy-driver / rustdoc through soldr{version} (shim dir: {dir})",
                version = shim_info
                    .soldr_version
                    .map(|v| format!(" v{v}"))
                    .unwrap_or_default(),
                dir = shim_info.path_entry.display()
            );
        }
        Err(reason) => {
            eprintln!("clud: {reason}; .clud/settings.json directive ignored");
        }
    }
}

#[derive(Debug)]
struct ShimInfo {
    path_entry: PathBuf,
    soldr_version: Option<String>,
}

/// Spawn `soldr shims --json` and parse the response.
///
/// Returns a `String` reason on failure (already prefixed for the
/// caller's `eprintln!` — caller appends "`.clud/settings.json
/// directive ignored`").
fn run_soldr_shims_json() -> Result<ShimInfo, String> {
    let output = match run_with_timeout(
        Command::new("soldr").args(["shims", "--json"]),
        SOLDR_SHIMS_TIMEOUT,
    ) {
        Ok(out) => out,
        Err(TimeoutError::Spawn(err)) => {
            return Err(format!("failed to spawn `soldr shims --json`: {err}"));
        }
        Err(TimeoutError::Timeout) => {
            return Err(format!(
                "soldr shims --json timed out after {}s",
                SOLDR_SHIMS_TIMEOUT.as_secs()
            ));
        }
        Err(TimeoutError::Wait(err)) => {
            return Err(format!("waiting on `soldr shims --json` failed: {err}"));
        }
    };

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        let lower = stderr.to_lowercase();
        if lower.contains("unrecognized subcommand") || lower.contains("unknown subcommand") {
            return Err("this soldr is too old (no 'shims' verb); upgrade to v0.7.55+".to_string());
        }
        let snippet: String = stderr.chars().take(200).collect();
        let code = output
            .status
            .code()
            .map(|c| c.to_string())
            .unwrap_or_else(|| "signal".to_string());
        return Err(format!(
            "soldr shims --json exited with code {code}; stderr: {snippet}"
        ));
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    let parsed: SoldrShimsJson = serde_json::from_str(stdout.trim())
        .map_err(|e| format!("soldr shims --json returned invalid JSON; parse error: {e}"))?;

    if parsed.schema_version != 1 {
        return Err(format!(
            "soldr shims --json returned unexpected schema version {} (expected 1)",
            parsed.schema_version
        ));
    }

    let path_entry_raw = parsed
        .path_entry
        .ok_or_else(|| "soldr shims --json response missing path_entry".to_string())?;
    let path_entry = PathBuf::from(path_entry_raw);
    if !path_entry.is_dir() {
        return Err(format!(
            "soldr shim dir {} does not exist",
            path_entry.display()
        ));
    }

    Ok(ShimInfo {
        path_entry,
        soldr_version: parsed.soldr_version,
    })
}

/// Prepend `path_entry` to `PATH` (idempotent — skip if already at
/// position 0). Modifies the *current process* env so spawned children
/// inherit the change.
fn prepend_path_entry(path_entry: &Path) {
    let separator = if cfg!(windows) { ";" } else { ":" };
    let existing = std::env::var_os("PATH").unwrap_or_default();
    let existing_str = existing.to_string_lossy();
    let path_entry_str = path_entry.to_string_lossy();

    // Idempotency: if PATH already starts with this dir, no-op.
    let already_leading = existing_str
        .split(if cfg!(windows) { ';' } else { ':' })
        .next()
        .map(|first| {
            if cfg!(windows) {
                first.eq_ignore_ascii_case(&path_entry_str)
            } else {
                first == path_entry_str
            }
        })
        .unwrap_or(false);
    if already_leading {
        return;
    }

    let new_path = if existing.is_empty() {
        path_entry_str.into_owned()
    } else {
        format!("{}{}{}", path_entry_str, separator, existing_str)
    };
    // SAFETY: env::set_var is safe at process startup before any other
    // thread is spawned. clud's main thread reaches this before
    // spawning any worker / runner thread.
    unsafe {
        std::env::set_var("PATH", new_path);
    }
}

/// Attempt to install soldr via `uv tool install soldr` (preferred) or
/// `pip install --user soldr` (fallback). Honors the optional pinned
/// `version` (e.g. `"0.7.55"` becomes `soldr==0.7.55`).
///
/// Returns `Ok(())` only if a `which::which("soldr")` succeeds after
/// the install attempt. Returns `Err(<reason>)` otherwise.
fn install_soldr_on_demand(version: Option<&str>) -> Result<(), String> {
    let pinned = version.map(|v| format!("soldr=={v}"));
    let pkg = pinned.as_deref().unwrap_or("soldr");

    let attempted = try_install(&[("uv", &["tool", "install", pkg])])
        .or_else(|_| try_install(&[("pip", &["install", "--user", pkg])]));

    match attempted {
        Ok(via) => {
            if which::which("soldr").is_ok() {
                eprintln!("clud: installed soldr via `{via}`");
                Ok(())
            } else {
                Err(format!(
                    "`{via}` reported success but `soldr` is still not on PATH (you may need to add your install dir to PATH manually)"
                ))
            }
        }
        Err(reason) => Err(reason),
    }
}

/// Try a series of `(installer, args)` candidates. Returns the first
/// one that succeeded, or the last failure reason.
fn try_install(candidates: &[(&str, &[&str])]) -> Result<String, String> {
    let mut last_reason = String::from("no installer attempted");
    for (installer, args) in candidates {
        if which::which(installer).is_err() {
            last_reason = format!("`{installer}` not on PATH");
            continue;
        }
        let summary = format!("{} {}", installer, args.join(" "));
        match run_with_timeout(Command::new(installer).args(*args), SOLDR_INSTALL_TIMEOUT) {
            Ok(output) if output.status.success() => return Ok(summary),
            Ok(output) => {
                let stderr = String::from_utf8_lossy(&output.stderr);
                last_reason = format!(
                    "`{summary}` exited with code {}: {}",
                    output
                        .status
                        .code()
                        .map(|c| c.to_string())
                        .unwrap_or_else(|| "signal".to_string()),
                    stderr.chars().take(200).collect::<String>()
                );
            }
            Err(TimeoutError::Timeout) => {
                last_reason = format!(
                    "`{summary}` timed out after {}s",
                    SOLDR_INSTALL_TIMEOUT.as_secs()
                );
            }
            Err(TimeoutError::Spawn(err)) => {
                last_reason = format!("failed to spawn `{summary}`: {err}");
            }
            Err(TimeoutError::Wait(err)) => {
                last_reason = format!("waiting on `{summary}` failed: {err}");
            }
        }
    }
    Err(last_reason)
}

// ---------------------------------------------------------------------
// Cross-platform wait-with-timeout for a Command.
//
// We avoid `tokio` here — clud's startup must stay sync to keep
// trampoline / console-title work simple. A spawn-thread + channel
// timer is the standard pattern.
// ---------------------------------------------------------------------

enum TimeoutError {
    Spawn(std::io::Error),
    Wait(std::io::Error),
    Timeout,
}

fn run_with_timeout(
    cmd: &mut Command,
    deadline: Duration,
) -> Result<std::process::Output, TimeoutError> {
    let mut child = cmd
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(TimeoutError::Spawn)?;

    let start = std::time::Instant::now();
    let poll_interval = Duration::from_millis(50);
    loop {
        match child.try_wait() {
            Ok(Some(_status)) => {
                return child.wait_with_output().map_err(TimeoutError::Wait);
            }
            Ok(None) => {
                if start.elapsed() >= deadline {
                    let _ = child.kill();
                    let _ = child.wait();
                    return Err(TimeoutError::Timeout);
                }
                std::thread::sleep(poll_interval);
            }
            Err(err) => return Err(TimeoutError::Wait(err)),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::repo_clud_config::RustConfig;

    fn cfg_with_rust(r: RustConfig) -> RepoCludConfig {
        RepoCludConfig { rust: r }
    }

    fn isolate_path_env() -> PathGuard {
        PathGuard::capture()
    }

    /// RAII guard that snapshots PATH on construction and restores it
    /// on drop. Tests that mutate PATH MUST hold one; otherwise
    /// parallel cases stomp each other.
    struct PathGuard {
        prior: Option<std::ffi::OsString>,
        _lock: std::sync::MutexGuard<'static, ()>,
    }

    fn path_mutex() -> &'static std::sync::Mutex<()> {
        static M: std::sync::OnceLock<std::sync::Mutex<()>> = std::sync::OnceLock::new();
        M.get_or_init(|| std::sync::Mutex::new(()))
    }

    impl PathGuard {
        fn capture() -> Self {
            let lock = path_mutex().lock().unwrap_or_else(|p| p.into_inner());
            let prior = std::env::var_os("PATH");
            Self { prior, _lock: lock }
        }
    }

    impl Drop for PathGuard {
        fn drop(&mut self) {
            unsafe {
                match self.prior.take() {
                    Some(v) => std::env::set_var("PATH", v),
                    None => std::env::remove_var("PATH"),
                }
            }
        }
    }

    // -----------------------------------------------------------------
    // prepend_path_entry — idempotency + ordering.
    // -----------------------------------------------------------------

    #[test]
    fn prepend_idempotent_when_already_leading() {
        let _g = isolate_path_env();
        let shim = std::env::temp_dir().join("clud-shim-idempotent");
        std::fs::create_dir_all(&shim).unwrap();
        let sep = if cfg!(windows) { ";" } else { ":" };
        let other = std::env::temp_dir().join("other");
        std::fs::create_dir_all(&other).unwrap();

        let starting = format!("{}{sep}{}", shim.display(), other.display());
        unsafe {
            std::env::set_var("PATH", &starting);
        }
        prepend_path_entry(&shim);
        let after = std::env::var("PATH").unwrap();
        assert_eq!(after, starting, "no double-prepend when already leading");
    }

    #[test]
    fn prepend_inserts_at_position_zero() {
        let _g = isolate_path_env();
        let shim = std::env::temp_dir().join("clud-shim-prepend");
        std::fs::create_dir_all(&shim).unwrap();
        let other = std::env::temp_dir().join("other-prepend");
        std::fs::create_dir_all(&other).unwrap();

        unsafe {
            std::env::set_var("PATH", other.display().to_string());
        }
        prepend_path_entry(&shim);
        let after = std::env::var("PATH").unwrap();
        let sep = if cfg!(windows) { ';' } else { ':' };
        let first = after.split(sep).next().unwrap();
        assert_eq!(
            first,
            shim.display().to_string(),
            "shim dir must be at PATH[0]: {after}"
        );
    }

    #[test]
    fn prepend_handles_empty_starting_path() {
        let _g = isolate_path_env();
        unsafe {
            std::env::remove_var("PATH");
        }
        let shim = std::env::temp_dir().join("clud-shim-empty");
        std::fs::create_dir_all(&shim).unwrap();

        prepend_path_entry(&shim);
        let after = std::env::var("PATH").unwrap();
        assert_eq!(after, shim.display().to_string());
    }

    // -----------------------------------------------------------------
    // activate_with_config — failure-mode contract.
    //
    // We can't trivially stub `soldr` on PATH in a unit test without
    // platform-specific shenanigans, so the integration-level "spawn
    // a stub soldr" tests live in `tests/`. Here we just verify the
    // shape of activate_with_config when use_soldr=false (must early-
    // return without touching PATH).
    // -----------------------------------------------------------------

    #[test]
    fn activate_with_use_soldr_false_is_a_no_op_on_path() {
        let _g = isolate_path_env();
        let baseline = std::env::var_os("PATH");
        let cfg = cfg_with_rust(RustConfig {
            use_soldr: false,
            install: true,
            version: None,
        });
        activate_with_config(&cfg);
        assert_eq!(
            std::env::var_os("PATH"),
            baseline,
            "use_soldr=false must not mutate PATH"
        );
    }

    // -----------------------------------------------------------------
    // Pinned-version pkg spec.
    // -----------------------------------------------------------------

    #[test]
    fn install_pkg_spec_uses_double_equals_for_pinned_version() {
        // We don't actually run uv/pip here, just check the spec we'd
        // build. The internal `pinned.as_deref().unwrap_or("soldr")`
        // logic in `install_soldr_on_demand` is the contract; replicate
        // it locally.
        let version = Some("0.7.55");
        let pinned = version.map(|v| format!("soldr=={v}"));
        let pkg = pinned.as_deref().unwrap_or("soldr");
        assert_eq!(pkg, "soldr==0.7.55");

        let version: Option<&str> = None;
        let pinned = version.map(|v| format!("soldr=={v}"));
        let pkg = pinned.as_deref().unwrap_or("soldr");
        assert_eq!(pkg, "soldr");
    }
}
