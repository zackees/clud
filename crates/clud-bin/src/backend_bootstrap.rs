//! Backend executable resolution plus first-run bootstrap helpers.
//!
//! Keep installer prompting here rather than in the runner so every launch
//! path consumes the same resolved backend path before `LaunchPlan` is built.

use std::fmt;
use std::io::{BufRead, Read, Write};
use std::path::{Path, PathBuf};
use std::time::Duration;

use crate::backend::{self, Backend};
use crate::subprocess;
use crate::win_creation_flags::invisible_helper_creationflags;
use running_process::{
    CommandSpec, NativeProcess, ProcessConfig, ReadStatus, StderrMode, StdinMode,
};

const CLAUDE_POSIX_INSTALL_COMMAND: &str = "curl -fsSL https://claude.ai/install.sh | bash";
const CLAUDE_WINDOWS_POWERSHELL_INSTALL_COMMAND: &str = "irm https://claude.ai/install.ps1 | iex";
const CLAUDE_WINDOWS_CMD_INSTALL_COMMAND: &str =
    "curl -fsSL https://claude.ai/install.cmd -o install.cmd && install.cmd && del install.cmd";

const CODEX_POSIX_INSTALL_COMMAND: &str = "curl -fsSL https://chatgpt.com/codex/install.sh | sh";
const CODEX_LINUX_MANAGED_INSTALL_COMMAND: &str = "clud codex-update";
const CODEX_TRUSTED_INSTALLER_URL: &str = "https://releases.openai.com/codex/install.sh";
// Audited 2026-09-22. Update this only after reviewing the new installer.
const CODEX_TRUSTED_INSTALLER_SHA256: &str =
    "150e3cf675682efeaac115aa3747add3f27887896d04ce6d0b56478d8b428bf6";
const CODEX_WINDOWS_POWERSHELL_INSTALL_COMMAND: &str =
    "irm https://chatgpt.com/codex/install.ps1 | iex";
const DEEPSEEK_RUN_COMMAND: &str = "npx @deepseek-ai/dsh web";

const MIN_UNIFIED_CLAUDE_CODE_VERSION: ClaudeCodeVersion = ClaudeCodeVersion {
    major: 2,
    minor: 1,
    patch: 223,
};

/// The version floor for the `CwdChanged` hook event, which Claude Code
/// introduced in 2.1.83. Older clients never fire it, so the backstop line
/// would sit inert.
const MIN_CWD_CHANGED_CLAUDE_CODE_VERSION: ClaudeCodeVersion = ClaudeCodeVersion {
    major: 2,
    minor: 1,
    patch: 83,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct ClaudeCodeVersion {
    major: u64,
    minor: u64,
    patch: u64,
}

impl fmt::Display for ClaudeCodeVersion {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{}.{}.{}", self.major, self.minor, self.patch)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InstallPlatform {
    MacOs,
    Linux,
    Windows,
}

impl InstallPlatform {
    pub fn current() -> Self {
        if cfg!(target_os = "windows") {
            Self::Windows
        } else if cfg!(target_os = "macos") {
            Self::MacOs
        } else {
            Self::Linux
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InstallerPlan {
    TrustedCodex,
    PosixShell {
        command: &'static str,
    },
    WindowsPowerShell {
        command: &'static str,
        cmd_fallback: Option<&'static str>,
    },
    Unsupported,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InstallLocation {
    HomeLocalBin { executable: &'static str },
    UserProfileLocalBin { executable: &'static str },
    LocalAppDataCodexBin,
    PathOnly,
}

impl InstallLocation {
    pub fn resolve(&self, env: &InstallPathEnv) -> Option<PathBuf> {
        match self {
            Self::HomeLocalBin { executable } => env
                .home
                .as_ref()
                .map(|home| home.join(".local").join("bin").join(executable)),
            Self::UserProfileLocalBin { executable } => env
                .user_profile
                .as_ref()
                .map(|home| home.join(".local").join("bin").join(executable)),
            Self::LocalAppDataCodexBin => env.local_app_data.as_ref().map(|root| {
                root.join("Programs")
                    .join("OpenAI")
                    .join("Codex")
                    .join("bin")
                    .join("codex.exe")
            }),
            Self::PathOnly => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct InstallPathEnv {
    pub home: Option<PathBuf>,
    pub user_profile: Option<PathBuf>,
    pub local_app_data: Option<PathBuf>,
}

impl InstallPathEnv {
    pub fn current() -> Self {
        Self {
            home: std::env::var_os("HOME").map(PathBuf::from),
            user_profile: std::env::var_os("USERPROFILE").map(PathBuf::from),
            local_app_data: std::env::var_os("LOCALAPPDATA").map(PathBuf::from),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BackendInstallSpec {
    pub backend: Backend,
    pub platform: InstallPlatform,
    pub product_name: &'static str,
    pub vendor_name: &'static str,
    pub installer_kind: &'static str,
    pub prompt_text: &'static str,
    pub manual_install_command: &'static str,
    pub installer: InstallerPlan,
    pub fallback_location: InstallLocation,
}

impl BackendInstallSpec {
    pub fn for_backend(backend: Backend, platform: InstallPlatform) -> Self {
        match (backend, platform) {
            (Backend::Claude, InstallPlatform::MacOs | InstallPlatform::Linux) => Self {
                backend,
                platform,
                product_name: "Claude Code",
                vendor_name: "Anthropic",
                installer_kind: "native",
                prompt_text:
                    "Claude Code is not installed. Install Anthropic's native Claude Code binary now? [y/N]",
                manual_install_command: CLAUDE_POSIX_INSTALL_COMMAND,
                installer: InstallerPlan::PosixShell {
                    command: CLAUDE_POSIX_INSTALL_COMMAND,
                },
                fallback_location: InstallLocation::HomeLocalBin {
                    executable: "claude",
                },
            },
            (Backend::Claude, InstallPlatform::Windows) => Self {
                backend,
                platform,
                product_name: "Claude Code",
                vendor_name: "Anthropic",
                installer_kind: "native",
                prompt_text:
                    "Claude Code is not installed. Install Anthropic's native Claude Code binary now? [y/N]",
                manual_install_command: CLAUDE_WINDOWS_POWERSHELL_INSTALL_COMMAND,
                installer: InstallerPlan::WindowsPowerShell {
                    command: CLAUDE_WINDOWS_POWERSHELL_INSTALL_COMMAND,
                    cmd_fallback: Some(CLAUDE_WINDOWS_CMD_INSTALL_COMMAND),
                },
                fallback_location: InstallLocation::UserProfileLocalBin {
                    executable: "claude.exe",
                },
            },
            (Backend::Codex, InstallPlatform::MacOs | InstallPlatform::Linux) => Self {
                backend,
                platform,
                product_name: "Codex CLI",
                vendor_name: "OpenAI",
                installer_kind: "standalone",
                prompt_text:
                    "Codex CLI is not installed. Install OpenAI's standalone Codex CLI now? [y/N]",
                manual_install_command: if platform == InstallPlatform::Linux {
                    CODEX_LINUX_MANAGED_INSTALL_COMMAND
                } else {
                    CODEX_POSIX_INSTALL_COMMAND
                },
                installer: if platform == InstallPlatform::Linux {
                    InstallerPlan::TrustedCodex
                } else {
                    InstallerPlan::PosixShell {
                        command: CODEX_POSIX_INSTALL_COMMAND,
                    }
                },
                fallback_location: InstallLocation::HomeLocalBin {
                    executable: "codex",
                },
            },
            (Backend::Codex, InstallPlatform::Windows) => Self {
                backend,
                platform,
                product_name: "Codex CLI",
                vendor_name: "OpenAI",
                installer_kind: "standalone",
                prompt_text:
                    "Codex CLI is not installed. Install OpenAI's standalone Codex CLI now? [y/N]",
                manual_install_command: CODEX_WINDOWS_POWERSHELL_INSTALL_COMMAND,
                installer: InstallerPlan::WindowsPowerShell {
                    command: CODEX_WINDOWS_POWERSHELL_INSTALL_COMMAND,
                    cmd_fallback: None,
                },
                fallback_location: InstallLocation::LocalAppDataCodexBin,
            },
            (Backend::DeepSeek, platform) => Self {
                backend,
                platform,
                product_name: "DeepSeek Harness",
                vendor_name: "DeepSeek AI",
                installer_kind: "npm developer preview",
                prompt_text: "DeepSeek Harness is not installed.",
                manual_install_command: DEEPSEEK_RUN_COMMAND,
                installer: InstallerPlan::Unsupported,
                fallback_location: InstallLocation::PathOnly,
            },
        }
    }

    pub fn prompt(&self) -> String {
        self.prompt_text.to_string()
    }

    fn fallback_path(&self, env: &InstallPathEnv) -> Option<PathBuf> {
        self.fallback_location.resolve(env)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BackendBootstrapError {
    BackendMissing {
        backend: Backend,
    },
    BackendMissingNonInteractive {
        backend: Backend,
        product_name: &'static str,
        install_command: &'static str,
    },
    BackendInstallDeclined {
        product_name: &'static str,
        install_command: &'static str,
    },
    BackendInstallerFailed {
        product_name: &'static str,
        message: String,
        install_command: &'static str,
    },
    BackendVerificationFailed {
        backend: Backend,
        product_name: &'static str,
        message: String,
        install_command: &'static str,
    },
}

impl BackendBootstrapError {
    pub fn exit_code(&self) -> i32 {
        1
    }
}

impl fmt::Display for BackendBootstrapError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::BackendMissing { backend } => write!(
                f,
                "error: {} not found on PATH. Install it or use --dry-run.",
                backend.executable_name()
            ),
            Self::BackendMissingNonInteractive {
                backend,
                product_name,
                install_command,
            } => write!(
                f,
                "error: {} not found on PATH. Install {product_name} with: {install_command}",
                backend.executable_name()
            ),
            Self::BackendInstallDeclined {
                product_name,
                install_command,
            } => write!(
                f,
                "error: {product_name} install declined. Install manually with: {install_command}"
            ),
            Self::BackendInstallerFailed {
                product_name,
                message,
                install_command,
            } => write!(
                f,
                "error: {product_name} install failed: {message}. Install manually with: {install_command}"
            ),
            Self::BackendVerificationFailed {
                backend,
                product_name,
                message,
                install_command,
            } => write!(
                f,
                "error: {product_name} installer completed, but clud could not verify {}: {message}. Install manually with: {install_command}",
                backend.executable_name()
            ),
        }
    }
}

pub trait BackendBootstrapHost {
    fn platform(&self) -> InstallPlatform;
    fn find_backend(&mut self, backend: Backend) -> Option<PathBuf>;
    fn run_backend_installer(&mut self, spec: &BackendInstallSpec) -> Result<(), String>;
    fn native_backend_path(&self, spec: &BackendInstallSpec) -> Option<PathBuf>;
    fn verify_backend(&mut self, backend: Backend, path: &Path) -> Result<(), String>;
}

pub struct ProductionBackendBootstrapHost;

impl BackendBootstrapHost for ProductionBackendBootstrapHost {
    fn platform(&self) -> InstallPlatform {
        InstallPlatform::current()
    }

    fn find_backend(&mut self, backend: Backend) -> Option<PathBuf> {
        backend::find_backend(backend)
    }

    fn run_backend_installer(&mut self, spec: &BackendInstallSpec) -> Result<(), String> {
        run_backend_installer(spec)
    }

    fn native_backend_path(&self, spec: &BackendInstallSpec) -> Option<PathBuf> {
        spec.fallback_path(&InstallPathEnv::current())
    }

    fn verify_backend(&mut self, backend: Backend, path: &Path) -> Result<(), String> {
        verify_backend(backend, path)
    }
}

/// Locate an already-installed backend for callers outside the install
/// flow. Same PATH-then-native-location rule the bootstrap gate uses, so a
/// stale `PATH` doesn't make a present binary look missing (issue #934).
pub fn locate_installed_backend(backend: Backend) -> Option<PathBuf> {
    let mut host = ProductionBackendBootstrapHost;
    let spec = BackendInstallSpec::for_backend(backend, host.platform());
    locate_backend(&mut host, backend, &spec)
}

/// Locate an already-installed backend: `PATH` first, then the documented
/// native install location for this platform.
///
/// Issue #934: both the pre-prompt gate and the post-install verification
/// go through here, so "installed" means the same thing on both sides. When
/// they disagreed, a PATH-only miss escalated to a full reinstall of a
/// binary that was already on disk.
fn locate_backend<H>(host: &mut H, backend: Backend, spec: &BackendInstallSpec) -> Option<PathBuf>
where
    H: BackendBootstrapHost + ?Sized,
{
    if let Some(path) = host.find_backend(backend) {
        return Some(path);
    }
    host.native_backend_path(spec).filter(|path| path.is_file())
}

pub fn resolve_backend_path<R, W, H>(
    backend: Backend,
    dry_run: bool,
    interactive: bool,
    input: &mut R,
    err: &mut W,
    host: &mut H,
) -> Result<String, BackendBootstrapError>
where
    R: BufRead,
    W: Write,
    H: BackendBootstrapHost,
{
    let spec = BackendInstallSpec::for_backend(backend, host.platform());

    // Issue #934: PATH first, then the documented native install location.
    // Checking only PATH here meant a transient PATH miss (stale shell, an
    // updater mid-swap) prompted for a full reinstall of a binary already
    // sitting at the fallback location — which is the very path this
    // function accepts *after* the installer runs.
    if let Some(path) = locate_backend(host, backend, &spec) {
        return Ok(path.to_string_lossy().to_string());
    }

    if dry_run {
        return Ok(backend.executable_name().to_string());
    }

    // DeepSeek Harness is currently a rapidly changing npm developer preview.
    // Upstream documents npx execution rather than a stable native installer,
    // so clud reports that exact path instead of mutating global npm state.
    if backend == Backend::DeepSeek {
        return Err(BackendBootstrapError::BackendMissingNonInteractive {
            backend,
            product_name: spec.product_name,
            install_command: spec.manual_install_command,
        });
    }

    if !interactive {
        return Err(BackendBootstrapError::BackendMissingNonInteractive {
            backend,
            product_name: spec.product_name,
            install_command: spec.manual_install_command,
        });
    }

    // Issue #934: name what was actually checked, so a false "not installed"
    // is diagnosable from the transcript instead of being a mystery.
    match host.native_backend_path(&spec) {
        Some(fallback) => writeln!(
            err,
            "[clud] {} not found on PATH, and no binary at {}",
            backend.executable_name(),
            fallback.display()
        ),
        None => writeln!(
            err,
            "[clud] {} not found on PATH",
            backend.executable_name()
        ),
    }
    .ok();
    writeln!(err, "{}", spec.prompt()).ok();
    err.flush().ok();

    if !read_yes(input) {
        return Err(BackendBootstrapError::BackendInstallDeclined {
            product_name: spec.product_name,
            install_command: spec.manual_install_command,
        });
    }

    host.run_backend_installer(&spec).map_err(|message| {
        BackendBootstrapError::BackendInstallerFailed {
            product_name: spec.product_name,
            message,
            install_command: spec.manual_install_command,
        }
    })?;

    let Some(path) = locate_backend(host, backend, &spec) else {
        return Err(BackendBootstrapError::BackendVerificationFailed {
            backend,
            product_name: spec.product_name,
            message: format!(
                "installed binary was not found on PATH or at the default {} install location",
                spec.product_name
            ),
            install_command: spec.manual_install_command,
        });
    };

    host.verify_backend(backend, &path).map_err(|message| {
        BackendBootstrapError::BackendVerificationFailed {
            backend,
            product_name: spec.product_name,
            message,
            install_command: spec.manual_install_command,
        }
    })?;
    Ok(path.to_string_lossy().to_string())
}

fn read_yes<R: BufRead>(input: &mut R) -> bool {
    let mut line = String::new();
    if input.read_line(&mut line).is_err() {
        return false;
    }
    matches!(line.trim(), "y" | "Y" | "yes" | "YES" | "Yes")
}

pub fn official_install_command(backend: Backend, platform: InstallPlatform) -> &'static str {
    BackendInstallSpec::for_backend(backend, platform).manual_install_command
}

pub fn official_claude_install_command() -> &'static str {
    official_install_command(Backend::Claude, InstallPlatform::current())
}

/// Only this fixed, hash-checked installer is given a system-tool PATH. Session
/// shells retain the removal shim; neither shell text nor a script path is an
/// argument to this entry point.
pub fn run_trusted_codex_update() -> i32 {
    match trusted_codex_update() {
        Ok(()) => 0,
        Err(error) => {
            eprintln!("Codex update refused: {error}");
            1
        }
    }
}

fn trusted_codex_update() -> Result<(), String> {
    if !cfg!(target_os = "linux") {
        return Err("Codex standalone update is supported here only on Linux".into());
    }
    let home = std::env::var_os("HOME").ok_or("HOME is required")?;
    let home = std::fs::canonicalize(home).map_err(|e| format!("cannot resolve HOME: {e}"))?;
    let response = ureq::get(CODEX_TRUSTED_INSTALLER_URL)
        .timeout(Duration::from_secs(30))
        .call()
        .map_err(|e| format!("cannot fetch Codex installer: {e}"))?;
    if response.get_url() != CODEX_TRUSTED_INSTALLER_URL {
        return Err("Codex installer redirected away from the audited origin".to_string());
    }
    let mut script = Vec::new();
    response
        .into_reader()
        .take(128 * 1024 + 1)
        .read_to_end(&mut script)
        .map_err(|e| format!("cannot read Codex installer: {e}"))?;
    if script.len() > 128 * 1024 {
        return Err("Codex installer exceeds the reviewed size limit".to_string());
    }
    run_verified_codex_installer(&script, CODEX_TRUSTED_INSTALLER_SHA256, &home)
}

fn trusted_codex_update_env(
    home: &Path,
    original_path: Option<&str>,
    shell: Option<&str>,
) -> Vec<(String, String)> {
    let system_path = if Path::new("/run/current-system/sw/bin/mkdir").is_file() {
        "/run/current-system/sw/bin:/usr/bin:/bin"
    } else {
        "/usr/bin:/bin"
    };
    let mut path = system_path.to_string();
    let visible_bin = home.join(".local/bin");
    let visible_bin = visible_bin.to_string_lossy();
    if !visible_bin.contains(':')
        && original_path
            .is_some_and(|value| value.split(':').any(|part| part == visible_bin.as_ref()))
    {
        path.push(':');
        path.push_str(&visible_bin);
    }
    let mut env = vec![
        ("HOME".into(), home.to_string_lossy().into_owned()),
        ("PATH".into(), path),
        ("CODEX_NON_INTERACTIVE".into(), "1".into()),
    ];
    if let Some(shell) = shell {
        env.push(("SHELL".into(), shell.into()));
    }
    env
}

fn run_verified_codex_installer(
    script: &[u8],
    expected_sha256: &str,
    home: &Path,
) -> Result<(), String> {
    let parent_env: Vec<_> = std::env::vars().collect();
    run_verified_codex_installer_with_parent_env(script, expected_sha256, home, &parent_env)
}

fn run_verified_codex_installer_with_parent_env(
    script: &[u8],
    expected_sha256: &str,
    home: &Path,
    parent_env: &[(String, String)],
) -> Result<(), String> {
    use sha2::{Digest, Sha256};
    let actual = format!("{:x}", Sha256::digest(script));
    if actual != expected_sha256 {
        return Err(format!(
            "installer contents changed (sha256 {actual}); review the new script before updating the pin"
        ));
    }
    let temp = tempfile::tempdir().map_err(|e| format!("cannot stage installer: {e}"))?;
    let path = temp.path().join("install.sh");
    std::fs::write(&path, script).map_err(|e| format!("cannot write installer: {e}"))?;
    let process = NativeProcess::new(ProcessConfig {
        command: CommandSpec::Argv(vec!["/bin/sh".into(), path.to_string_lossy().into_owned()]),
        cwd: Some(home.to_path_buf()),
        env: Some(trusted_codex_update_env(
            home,
            parent_env
                .iter()
                .find(|(key, _)| key == "PATH")
                .map(|(_, value)| value.as_str()),
            parent_env
                .iter()
                .find(|(key, _)| key == "SHELL")
                .map(|(_, value)| value.as_str()),
        )),
        capture: false,
        stderr_mode: StderrMode::Stdout,
        creationflags: None,
        create_process_group: false,
        stdin_mode: StdinMode::Inherit,
        nice: None,
    });
    process
        .start()
        .map_err(|e| format!("cannot start installer: {e}"))?;
    let code = process
        .wait(None)
        .map_err(|e| format!("cannot wait for installer: {e}"))?;
    if code == 0 {
        Ok(())
    } else {
        Err(format!("installer exited with {code}"))
    }
}

fn run_backend_installer(spec: &BackendInstallSpec) -> Result<(), String> {
    match spec.installer {
        InstallerPlan::TrustedCodex => trusted_codex_update(),
        InstallerPlan::PosixShell { command } => run_interactive_command(
            CommandSpec::Argv(vec![
                "sh".to_string(),
                "-c".to_string(),
                command.to_string(),
            ]),
            None,
        ),
        InstallerPlan::WindowsPowerShell {
            command,
            cmd_fallback,
        } => run_windows_powershell_installer(command, cmd_fallback, spec.backend),
        InstallerPlan::Unsupported => Err(format!(
            "automatic installation is unavailable; run {}",
            spec.manual_install_command
        )),
    }
}

fn run_windows_powershell_installer(
    command: &'static str,
    cmd_fallback: Option<&'static str>,
    backend: Backend,
) -> Result<(), String> {
    let argv = vec![
        "powershell.exe".to_string(),
        "-NoProfile".to_string(),
        "-ExecutionPolicy".to_string(),
        "Bypass".to_string(),
        "-Command".to_string(),
        command.to_string(),
    ];

    match run_interactive_command(CommandSpec::Argv(argv), None) {
        Ok(()) => Ok(()),
        Err(first) => {
            let Some(fallback) = cmd_fallback else {
                return Err(first);
            };
            run_windows_cmd_installer(fallback, backend).map_err(|second| {
                format!("PowerShell installer failed ({first}); CMD fallback failed ({second})")
            })
        }
    }
}

#[cfg(windows)]
fn run_windows_cmd_installer(command: &'static str, backend: Backend) -> Result<(), String> {
    let dir = std::env::temp_dir().join(format!(
        "clud-{}-install-{}",
        backend.executable_name(),
        std::process::id()
    ));
    std::fs::create_dir_all(&dir)
        .map_err(|err| format!("failed to create temp dir {}: {err}", dir.display()))?;
    let result = run_interactive_command(
        CommandSpec::Argv(vec![
            "cmd.exe".to_string(),
            "/D".to_string(),
            "/S".to_string(),
            "/C".to_string(),
            command.to_string(),
        ]),
        Some(dir.clone()),
    );
    let _ = std::fs::remove_dir_all(&dir);
    result
}

#[cfg(not(windows))]
fn run_windows_cmd_installer(_command: &'static str, _backend: Backend) -> Result<(), String> {
    Err("CMD fallback is only available on Windows".to_string())
}

fn run_interactive_command(command: CommandSpec, cwd: Option<PathBuf>) -> Result<(), String> {
    let process = NativeProcess::new(ProcessConfig {
        command,
        cwd,
        env: None,
        capture: false,
        stderr_mode: StderrMode::Stdout,
        creationflags: None,
        create_process_group: false,
        stdin_mode: StdinMode::Inherit,
        nice: None,
    });
    process
        .start()
        .map_err(|err| format!("failed to start command: {err}"))?;
    let exit_code = process
        .wait(None)
        .map_err(|err| format!("failed to wait for command: {err}"))?;
    if exit_code == 0 {
        Ok(())
    } else {
        Err(format!("command exited with {exit_code}"))
    }
}

fn verify_backend(_backend: Backend, path: &Path) -> Result<(), String> {
    let command = vec![path.to_string_lossy().to_string(), "--version".to_string()];
    let (exit_code, output) = run_captured_command(command)
        .map_err(|err| format!("failed to run {} --version: {err}", path.display()))?;
    if exit_code == 0 {
        Ok(())
    } else {
        Err(format!(
            "{} --version exited with {}; output: {}",
            path.display(),
            exit_code,
            output.trim()
        ))
    }
}

/// Unified model discovery accepts clud's synthetic IDs only in Claude Code
/// 2.1.223 and newer. Probe the executable selected by bootstrap so a stale
/// client fails before the launch-scoped gateway or any paid request starts.
pub fn require_unified_claude_version(path: &Path) -> Result<ClaudeCodeVersion, String> {
    require_claude_discovery_version(path, "unified routing")
}

/// The direct Codex-through-Claude route now uses the same synthetic model
/// discovery surface as unified routing, and therefore has the same client
/// floor.
pub fn require_codex_bridge_claude_version(path: &Path) -> Result<ClaudeCodeVersion, String> {
    require_claude_discovery_version(path, "Codex-through-Claude model discovery")
}

fn require_claude_discovery_version(
    path: &Path,
    feature: &str,
) -> Result<ClaudeCodeVersion, String> {
    let command = vec![path.to_string_lossy().to_string(), "--version".to_string()];
    let (exit_code, output) = run_captured_command(command).map_err(|error| {
        format!(
            "{feature} requires Claude Code >= {MIN_UNIFIED_CLAUDE_CODE_VERSION}, but the installed client version could not be checked: {error}"
        )
    })?;
    if exit_code != 0 {
        return Err(format!(
            "{feature} requires Claude Code >= {MIN_UNIFIED_CLAUDE_CODE_VERSION}, but the installed client's --version command exited with {exit_code}"
        ));
    }
    validate_claude_discovery_version_output(&output, feature)
}

#[cfg(test)]
fn validate_unified_claude_version_output(output: &str) -> Result<ClaudeCodeVersion, String> {
    validate_claude_discovery_version_output(output, "unified routing")
}

fn validate_claude_discovery_version_output(
    output: &str,
    feature: &str,
) -> Result<ClaudeCodeVersion, String> {
    let installed = parse_claude_code_version(output).ok_or_else(|| {
        format!(
            "{feature} requires Claude Code >= {MIN_UNIFIED_CLAUDE_CODE_VERSION}, but no installed version could be parsed from `claude --version`"
        )
    })?;
    if installed < MIN_UNIFIED_CLAUDE_CODE_VERSION {
        return Err(format!(
            "{feature} requires Claude Code >= {MIN_UNIFIED_CLAUDE_CODE_VERSION}; installed version is {installed}. Run `claude update` and retry"
        ));
    }
    Ok(installed)
}

fn parse_claude_code_version(output: &str) -> Option<ClaudeCodeVersion> {
    output
        .split(|character: char| !(character.is_ascii_digit() || character == '.'))
        .find_map(|candidate| {
            let mut parts = candidate.split('.');
            let major = parts.next()?.parse().ok()?;
            let minor = parts.next()?.parse().ok()?;
            let patch = parts.next()?.parse().ok()?;
            Some(ClaudeCodeVersion {
                major,
                minor,
                patch,
            })
        })
}

/// Whether the installed Claude Code can fire the `CwdChanged` hook event
/// (zackees/clud#967 Phase 5).
///
/// The backstop is hygiene, never correctness (DD-064), so the probe is
/// deliberately conservative: any failure — the binary missing or hanging,
/// output that will not parse, an older version — answers `false`, and the
/// launch simply registers no `CwdChanged` line. It runs once per launch and
/// only for a repo that opted into `.clud/hooks.json`, so its budget is
/// bounded: the whole probe gives up after a few seconds and degrades to no
/// line rather than stall a launch.
#[must_use]
pub fn probe_claude_cwd_changed_support(claude_path: &Path) -> bool {
    const PROBE_BUDGET: Duration = Duration::from_secs(5);

    let process = match subprocess::ManagedSubprocess::start_inheriting_env(
        vec![
            claude_path.to_string_lossy().into_owned(),
            "--version".to_string(),
        ],
        None,
        true,
        invisible_helper_creationflags(),
    ) {
        Ok(process) => process,
        Err(_) => return false,
    };

    let deadline = std::time::Instant::now() + PROBE_BUDGET;
    let mut buf = Vec::<u8>::new();
    loop {
        if std::time::Instant::now() >= deadline {
            let _ = process.kill();
            return false;
        }
        match process.read_stdout(Some(Duration::from_millis(100))) {
            ReadStatus::Line(line) => buf.extend_from_slice(&line),
            ReadStatus::Timeout => {
                // Polling observes direct-root exit and closes its Job, while
                // reader threads may still be draining buffered final bytes —
                // same pattern as `run_captured_command`; the deadline above
                // is what bounds this loop.
                let _ = process.poll();
            }
            ReadStatus::Eof => break,
        }
    }

    let exit_code = process
        .wait(Some(Duration::from_secs(2)))
        .map_err(|_| ())
        .ok();
    if exit_code != Some(0) {
        return false;
    }

    let output = String::from_utf8_lossy(&buf);
    parse_claude_code_version(&output)
        .is_some_and(|version| version >= MIN_CWD_CHANGED_CLAUDE_CODE_VERSION)
}

fn run_captured_command(command: Vec<String>) -> Result<(i32, String), String> {
    let process = subprocess::ManagedSubprocess::start_inheriting_env(
        command,
        None,
        true,
        invisible_helper_creationflags(),
    )
    .map_err(|err| format!("failed to start command: {err}"))?;

    let mut buf = Vec::<u8>::new();
    loop {
        match process.read_stdout(Some(Duration::from_millis(100))) {
            ReadStatus::Line(line) => {
                buf.extend_from_slice(&line);
            }
            ReadStatus::Timeout => {
                // Polling observes direct-root exit and closes its Job, but
                // reader threads can still be draining buffered final bytes.
                // Continue until EOF synchronizes with both readers.
                let _ = process.poll();
            }
            ReadStatus::Eof => break,
        }
    }

    let exit_code = process
        .wait(Some(Duration::from_secs(30)))
        .map_err(|err| format!("failed to wait for command: {err}"))?;
    Ok((exit_code, String::from_utf8_lossy(&buf).into_owned()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::VecDeque;
    use std::io;

    #[cfg(target_os = "linux")]
    #[test]
    fn codex_update_env_does_not_inherit_session_shims_or_installer_overrides() {
        let home = Path::new("/tmp/clud-codex-update-home");
        let env =
            trusted_codex_update_env(home, Some("/untrusted/shims:/usr/bin"), Some("/bin/bash"));
        assert!(env
            .iter()
            .any(|(key, value)| key == "PATH" && value.ends_with("/usr/bin:/bin")));
        assert!(env
            .iter()
            .all(|(_, value)| !value.contains("/untrusted/shims")));
        assert!(env
            .iter()
            .any(|(key, value)| key == "SHELL" && value == "/bin/bash"));
        assert!(env
            .iter()
            .any(|(key, value)| key == "HOME" && value == &home.to_string_lossy()));
        assert!(env
            .iter()
            .any(|(key, value)| key == "CODEX_NON_INTERACTIVE" && value == "1"));
        assert!(!env.iter().any(|(key, _)| key == "CLUD_RM_DRY_RUN"));
        assert!(!env.iter().any(|(key, _)| key == "CODEX_INSTALL_DIR"));
        let with_visible_bin = trusted_codex_update_env(
            home,
            Some("/untrusted/shims:/tmp/clud-codex-update-home/.local/bin:/usr/bin"),
            None,
        );
        assert!(with_visible_bin.iter().any(|(key, value)| key == "PATH"
            && value.ends_with(":/tmp/clud-codex-update-home/.local/bin")));
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn codex_update_fixture_covers_fresh_complete_and_stale_staging() {
        use sha2::{Digest, Sha256};
        let home = tempfile::tempdir().unwrap();
        let home = home.path();
        std::fs::write(
            home.join("fixture-codex"),
            b"#!/bin/sh\nprintf 'codex-cli 0.156.0\\n'\n",
        )
        .unwrap();
        let script = br#"set -eu
root="$HOME/.codex/packages/standalone"
releases="$root/releases"
mkdir -p "$releases"
tmp_dir="$(mktemp -d "$HOME/.codex/tmp.XXXXXX")"
remove=rm
cleanup() { "$remove" -rf "$tmp_dir"; }
trap cleanup EXIT
find "$releases" -mindepth 1 -maxdepth 1 -name '.staging.*' -exec "$remove" -rf {} +
stage="$releases/.staging.fixture.$$"
"$remove" -rf "$stage"
if [ ! -f "$releases/0.156.0/version" ]; then
  mkdir -p "$stage/bin"
  cp "$HOME/fixture-codex" "$stage/bin/codex"
  chmod 0755 "$stage/bin/codex"
  printf '0.156.0' > "$stage/version"
  mv "$stage" "$releases/0.156.0"
fi
ln -sfn releases/0.156.0 "$root/current"
"#;
        let digest = format!("{:x}", Sha256::digest(script));
        let root = home.join(".codex/packages/standalone");
        let releases = root.join("releases");
        run_verified_codex_installer(script, &digest, home).unwrap();
        assert_eq!(
            std::fs::read_to_string(releases.join("0.156.0/version")).unwrap(),
            "0.156.0"
        );
        assert_eq!(
            std::fs::read_link(root.join("current")).unwrap(),
            Path::new("releases/0.156.0")
        );
        let (code, version) = run_captured_command(vec![
            root.join("current/bin/codex")
                .to_string_lossy()
                .into_owned(),
            "--version".into(),
        ])
        .unwrap();
        assert_eq!(code, 0);
        assert_eq!(version.trim(), "codex-cli 0.156.0");
        run_verified_codex_installer(script, &digest, home).unwrap();
        let stale = releases.join(".staging.old");
        std::fs::create_dir(&stale).unwrap();
        run_verified_codex_installer(script, &digest, home).unwrap();
        assert!(!stale.exists());
        assert!(std::fs::read_dir(home.join(".codex"))
            .unwrap()
            .all(|entry| !entry
                .unwrap()
                .file_name()
                .to_string_lossy()
                .starts_with("tmp.")));
        assert!(std::fs::read_dir(releases).unwrap().all(|entry| !entry
            .unwrap()
            .file_name()
            .to_string_lossy()
            .starts_with(".staging.")));
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn codex_update_removes_stale_files_from_both_session_routes() {
        use sha2::{Digest, Sha256};
        use std::os::unix::fs::PermissionsExt;
        let home = tempfile::tempdir().unwrap();
        let home_path = home.path().to_string_lossy().into_owned();
        let shim_dir = home.path().join("session-shim");
        std::fs::create_dir(&shim_dir).unwrap();
        let poisoned_rm = shim_dir.join("rm");
        std::fs::write(&poisoned_rm, b"#!/bin/sh\nexit 27\n").unwrap();
        std::fs::set_permissions(&poisoned_rm, std::fs::Permissions::from_mode(0o755)).unwrap();
        let base = vec![
            ("HOME".to_string(), home_path),
            (
                "PATH".to_string(),
                format!("{}:/usr/bin:/bin", shim_dir.display()),
            ),
        ];
        let routes = [
            (
                "foreground",
                crate::runner::apply_child_env_policy(base.clone()),
            ),
            ("daemon", crate::daemon::io_helpers::child_env_from(&base)),
        ];
        let script = b"set -eu\nrm -rf \"$HOME/stale\"\n";
        let digest = format!("{:x}", Sha256::digest(script));
        for (route, parent_env) in routes {
            let path = parent_env
                .iter()
                .find(|(key, _)| key == "PATH")
                .map(|(_, value)| value.as_str())
                .unwrap();
            assert!(
                path.contains("session-shim"),
                "{route} route lost the shim: {path}"
            );
            let stale = home.path().join("stale");
            std::fs::create_dir(&stale).unwrap();
            std::fs::write(stale.join("old"), b"old").unwrap();
            run_verified_codex_installer_with_parent_env(script, &digest, home.path(), &parent_env)
                .unwrap();
            assert!(!stale.exists(), "{route} route retained stale files");
        }
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn codex_update_rejects_changed_installer_before_execution() {
        let home = tempfile::tempdir().unwrap();
        let err =
            run_verified_codex_installer(b"touch \"$HOME/not-run\"\n", "wrong-digest", home.path())
                .unwrap_err();
        assert!(err.contains("installer contents changed"));
        assert!(!home.path().join("not-run").exists());
    }

    #[test]
    fn unified_claude_version_parser_accepts_supported_output_shapes() {
        assert_eq!(
            parse_claude_code_version("2.1.223 (Claude Code)"),
            Some(MIN_UNIFIED_CLAUDE_CODE_VERSION)
        );
        assert_eq!(
            parse_claude_code_version("Claude Code v2.2.0\n"),
            Some(ClaudeCodeVersion {
                major: 2,
                minor: 2,
                patch: 0,
            })
        );
        assert_eq!(parse_claude_code_version("Claude Code unknown"), None);
    }

    #[test]
    fn cwd_changed_probe_floor_is_the_event_introduction_version() {
        // 2.1.83 is the first Claude Code that fires `CwdChanged`; anything
        // older must not get the backstop line, and the probe must accept
        // exactly the floor and above.
        let floor = MIN_CWD_CHANGED_CLAUDE_CODE_VERSION;
        assert_eq!(
            floor,
            ClaudeCodeVersion {
                major: 2,
                minor: 1,
                patch: 83
            }
        );
        assert!(parse_claude_code_version("2.1.82").is_none_or(|v| v < floor));
        assert!(parse_claude_code_version("2.1.83").is_some_and(|v| v >= floor));
        assert!(parse_claude_code_version("2.2.0").is_some_and(|v| v >= floor));
        assert!(parse_claude_code_version("1.9.99").is_none_or(|v| v < floor));
        assert!(parse_claude_code_version("3.0.0").is_some_and(|v| v >= floor));
    }

    #[test]
    fn unified_claude_version_floor_rejects_every_older_discovery_build() {
        let error = validate_unified_claude_version_output("2.1.222").unwrap_err();
        assert!(error.contains("installed version is 2.1.222"), "{error}");
        assert!(error.contains("claude update"), "{error}");
        assert_eq!(
            validate_unified_claude_version_output("2.1.223").unwrap(),
            MIN_UNIFIED_CLAUDE_CODE_VERSION
        );
    }

    /// Issue #921's exact repro shape: a 2.1.212 client with the
    /// `(Claude Code)` suffix must be refused naming its version, the floor,
    /// and the remedy — and missing/unparseable output must refuse too, never
    /// a silent picker without connector rows.
    #[test]
    fn unified_claude_version_floor_rejects_the_212_repro_and_unparseable_output() {
        let error = validate_unified_claude_version_output("2.1.212 (Claude Code)").unwrap_err();
        assert!(error.contains("installed version is 2.1.212"), "{error}");
        assert!(error.contains("2.1.223"), "{error}");
        assert!(error.contains("claude update"), "{error}");
        for output in ["", "no version here"] {
            let error = validate_unified_claude_version_output(output).unwrap_err();
            assert!(
                error.contains("no installed version could be parsed"),
                "{error}"
            );
            assert!(error.contains("2.1.223"), "{error}");
        }
    }

    struct MockHost {
        platform: InstallPlatform,
        find_results: VecDeque<Option<PathBuf>>,
        find_calls: Vec<Backend>,
        installer_runs: Vec<Backend>,
        installer_result: Result<(), String>,
        native_path: Option<PathBuf>,
        /// Path the installer materializes when it runs. Models the real
        /// world, where the fallback location is empty *before* the install
        /// and populated after — the distinction the pre-prompt gate turns on.
        installer_creates: Option<PathBuf>,
        verified: Vec<(Backend, PathBuf)>,
        verify_result: Result<(), String>,
    }

    impl Default for MockHost {
        fn default() -> Self {
            Self {
                platform: InstallPlatform::Linux,
                find_results: VecDeque::new(),
                find_calls: Vec::new(),
                installer_runs: Vec::new(),
                installer_result: Ok(()),
                native_path: None,
                installer_creates: None,
                verified: Vec::new(),
                verify_result: Ok(()),
            }
        }
    }

    impl BackendBootstrapHost for MockHost {
        fn platform(&self) -> InstallPlatform {
            self.platform
        }

        fn find_backend(&mut self, backend: Backend) -> Option<PathBuf> {
            self.find_calls.push(backend);
            self.find_results.pop_front().unwrap_or(None)
        }

        fn run_backend_installer(&mut self, spec: &BackendInstallSpec) -> Result<(), String> {
            self.installer_runs.push(spec.backend);
            if self.installer_result.is_ok() {
                if let Some(path) = &self.installer_creates {
                    std::fs::write(path, b"mock").expect("installer writes binary");
                }
            }
            self.installer_result.clone()
        }

        fn native_backend_path(&self, spec: &BackendInstallSpec) -> Option<PathBuf> {
            let _ = spec;
            self.native_path.clone()
        }

        fn verify_backend(&mut self, backend: Backend, path: &Path) -> Result<(), String> {
            self.verified.push((backend, path.to_path_buf()));
            self.verify_result.clone()
        }
    }

    fn resolve_with(
        backend: Backend,
        dry_run: bool,
        interactive: bool,
        input: &str,
        host: &mut MockHost,
    ) -> Result<(String, String), BackendBootstrapError> {
        let mut input = io::Cursor::new(input.as_bytes().to_vec());
        let mut err = Vec::<u8>::new();
        let path = resolve_backend_path(backend, dry_run, interactive, &mut input, &mut err, host)?;
        Ok((path, String::from_utf8(err).expect("utf8")))
    }

    fn path_env() -> InstallPathEnv {
        InstallPathEnv {
            home: Some(PathBuf::from("/home/me")),
            user_profile: Some(PathBuf::from("C:/Users/me")),
            local_app_data: Some(PathBuf::from("C:/Users/me/AppData/Local")),
        }
    }

    #[test]
    fn claude_macos_install_spec_uses_posix_command_and_home_local_bin() {
        let spec = BackendInstallSpec::for_backend(Backend::Claude, InstallPlatform::MacOs);
        assert_eq!(spec.manual_install_command, CLAUDE_POSIX_INSTALL_COMMAND);
        assert_eq!(
            spec.prompt(),
            "Claude Code is not installed. Install Anthropic's native Claude Code binary now? [y/N]"
        );
        assert_eq!(
            spec.installer,
            InstallerPlan::PosixShell {
                command: CLAUDE_POSIX_INSTALL_COMMAND
            }
        );
        assert_eq!(
            spec.fallback_path(&path_env()).unwrap(),
            PathBuf::from("/home/me/.local/bin/claude")
        );
    }

    #[test]
    fn claude_linux_install_spec_uses_posix_command_and_home_local_bin() {
        let spec = BackendInstallSpec::for_backend(Backend::Claude, InstallPlatform::Linux);
        assert_eq!(spec.manual_install_command, CLAUDE_POSIX_INSTALL_COMMAND);
        assert_eq!(
            spec.installer,
            InstallerPlan::PosixShell {
                command: CLAUDE_POSIX_INSTALL_COMMAND
            }
        );
        assert_eq!(
            spec.fallback_path(&path_env()).unwrap(),
            PathBuf::from("/home/me/.local/bin/claude")
        );
    }

    #[test]
    fn claude_windows_install_spec_uses_powershell_cmd_fallback_and_user_profile_path() {
        let spec = BackendInstallSpec::for_backend(Backend::Claude, InstallPlatform::Windows);
        assert_eq!(
            spec.manual_install_command,
            CLAUDE_WINDOWS_POWERSHELL_INSTALL_COMMAND
        );
        assert_eq!(
            spec.installer,
            InstallerPlan::WindowsPowerShell {
                command: CLAUDE_WINDOWS_POWERSHELL_INSTALL_COMMAND,
                cmd_fallback: Some(CLAUDE_WINDOWS_CMD_INSTALL_COMMAND)
            }
        );
        assert_eq!(
            spec.fallback_path(&path_env()).unwrap(),
            PathBuf::from("C:/Users/me")
                .join(".local")
                .join("bin")
                .join("claude.exe")
        );
    }

    #[test]
    fn codex_macos_install_spec_uses_standalone_command_and_home_local_bin() {
        let spec = BackendInstallSpec::for_backend(Backend::Codex, InstallPlatform::MacOs);
        assert_eq!(spec.manual_install_command, CODEX_POSIX_INSTALL_COMMAND);
        assert_eq!(
            spec.prompt(),
            "Codex CLI is not installed. Install OpenAI's standalone Codex CLI now? [y/N]"
        );
        assert_eq!(
            spec.installer,
            InstallerPlan::PosixShell {
                command: CODEX_POSIX_INSTALL_COMMAND
            }
        );
        assert_eq!(
            spec.fallback_path(&path_env()).unwrap(),
            PathBuf::from("/home/me/.local/bin/codex")
        );
    }

    #[test]
    fn deepseek_install_spec_is_path_only_with_official_npx_guidance() {
        for platform in [
            InstallPlatform::MacOs,
            InstallPlatform::Linux,
            InstallPlatform::Windows,
        ] {
            let spec = BackendInstallSpec::for_backend(Backend::DeepSeek, platform);
            assert_eq!(spec.product_name, "DeepSeek Harness");
            assert_eq!(spec.manual_install_command, DEEPSEEK_RUN_COMMAND);
            assert_eq!(spec.installer, InstallerPlan::Unsupported);
            assert_eq!(spec.fallback_location, InstallLocation::PathOnly);
            assert_eq!(spec.fallback_path(&path_env()), None);
        }
    }

    #[test]
    fn codex_linux_install_spec_uses_standalone_command_and_home_local_bin() {
        let spec = BackendInstallSpec::for_backend(Backend::Codex, InstallPlatform::Linux);
        assert_eq!(
            spec.manual_install_command,
            CODEX_LINUX_MANAGED_INSTALL_COMMAND
        );
        assert_eq!(spec.installer, InstallerPlan::TrustedCodex);
        assert_eq!(
            spec.fallback_path(&path_env()).unwrap(),
            PathBuf::from("/home/me/.local/bin/codex")
        );
    }

    #[test]
    fn codex_windows_install_spec_uses_powershell_without_cmd_fallback_and_local_app_data_path() {
        let spec = BackendInstallSpec::for_backend(Backend::Codex, InstallPlatform::Windows);
        assert_eq!(
            spec.manual_install_command,
            CODEX_WINDOWS_POWERSHELL_INSTALL_COMMAND
        );
        assert_eq!(
            spec.installer,
            InstallerPlan::WindowsPowerShell {
                command: CODEX_WINDOWS_POWERSHELL_INSTALL_COMMAND,
                cmd_fallback: None
            }
        );
        assert_eq!(
            spec.fallback_path(&path_env()).unwrap(),
            PathBuf::from("C:/Users/me/AppData/Local")
                .join("Programs")
                .join("OpenAI")
                .join("Codex")
                .join("bin")
                .join("codex.exe")
        );
    }

    #[test]
    fn existing_backend_path_wins() {
        let mut host = MockHost {
            find_results: VecDeque::from([Some(PathBuf::from("/bin/claude"))]),
            ..Default::default()
        };
        let (path, prompt) =
            resolve_with(Backend::Claude, false, true, "", &mut host).expect("path");
        assert_eq!(path, "/bin/claude");
        assert!(prompt.is_empty());
        assert!(host.installer_runs.is_empty());
        assert!(host.verified.is_empty());
    }

    #[test]
    fn dry_run_uses_placeholder_without_installing() {
        for backend in Backend::ALL {
            let mut host = MockHost::default();
            let (path, prompt) = resolve_with(backend, true, true, "y\n", &mut host).expect("path");
            assert_eq!(path, backend.executable_name());
            assert!(prompt.is_empty());
            assert!(host.installer_runs.is_empty());
            assert!(host.verified.is_empty());
        }
    }

    #[test]
    fn claude_missing_noninteractive_prints_install_command() {
        let mut host = MockHost::default();
        let err = resolve_with(Backend::Claude, false, false, "", &mut host).unwrap_err();
        assert_eq!(
            err,
            BackendBootstrapError::BackendMissingNonInteractive {
                backend: Backend::Claude,
                product_name: "Claude Code",
                install_command: CLAUDE_POSIX_INSTALL_COMMAND
            }
        );
        assert!(err.to_string().contains("claude.ai/install"));
        assert!(host.installer_runs.is_empty());
    }

    #[test]
    fn codex_missing_noninteractive_prints_install_command() {
        let mut host = MockHost::default();
        let err = resolve_with(Backend::Codex, false, false, "", &mut host).unwrap_err();
        assert_eq!(
            err,
            BackendBootstrapError::BackendMissingNonInteractive {
                backend: Backend::Codex,
                product_name: "Codex CLI",
                install_command: CODEX_LINUX_MANAGED_INSTALL_COMMAND
            }
        );
        assert!(err.to_string().contains("clud codex-update"));
        assert!(host.installer_runs.is_empty());
    }

    #[test]
    fn deepseek_missing_even_interactively_reports_guidance_without_installing() {
        let mut host = MockHost::default();
        let err = resolve_with(Backend::DeepSeek, false, true, "y\n", &mut host).unwrap_err();
        assert_eq!(
            err,
            BackendBootstrapError::BackendMissingNonInteractive {
                backend: Backend::DeepSeek,
                product_name: "DeepSeek Harness",
                install_command: DEEPSEEK_RUN_COMMAND,
            }
        );
        assert!(err.to_string().contains("npx @deepseek-ai/dsh web"));
        assert!(host.installer_runs.is_empty());
        assert!(host.verified.is_empty());
    }

    #[test]
    fn interactive_decline_does_not_install() {
        for backend in [Backend::Claude, Backend::Codex] {
            let mut host = MockHost::default();
            let err = resolve_with(backend, false, true, "n\n", &mut host).unwrap_err();
            assert!(matches!(
                err,
                BackendBootstrapError::BackendInstallDeclined { .. }
            ));
            assert!(host.installer_runs.is_empty());
            assert!(host.verified.is_empty());
        }
    }

    #[test]
    fn interactive_accept_installs_verifies_and_returns_path_from_path() {
        for backend in [Backend::Claude, Backend::Codex] {
            let installed =
                PathBuf::from(format!("/home/me/.local/bin/{}", backend.executable_name()));
            let mut host = MockHost {
                find_results: VecDeque::from([None, Some(installed.clone())]),
                installer_result: Ok(()),
                verify_result: Ok(()),
                ..Default::default()
            };
            let (path, prompt) =
                resolve_with(backend, false, true, "yes\n", &mut host).expect("path");
            assert_eq!(path, installed.to_string_lossy());
            // stderr is the issue #934 diagnostic followed by the prompt.
            let expected_prompt =
                BackendInstallSpec::for_backend(backend, InstallPlatform::Linux).prompt() + "\n";
            assert!(
                prompt.ends_with(&expected_prompt),
                "prompt must be the last thing written, got: {prompt:?}"
            );
            assert!(
                prompt.contains("not found on PATH"),
                "diagnostic must precede the prompt, got: {prompt:?}"
            );
            assert_eq!(host.installer_runs, vec![backend]);
            assert_eq!(host.verified, vec![(backend, installed)]);
        }
    }

    #[test]
    fn selected_backend_isolation_installs_only_the_selected_backend() {
        let codex_path = PathBuf::from("/home/me/.local/bin/codex");
        let mut host = MockHost {
            find_results: VecDeque::from([None, Some(codex_path.clone())]),
            installer_result: Ok(()),
            verify_result: Ok(()),
            ..Default::default()
        };
        let (path, _) = resolve_with(Backend::Codex, false, true, "y\n", &mut host).expect("path");
        assert_eq!(path, codex_path.to_string_lossy());
        assert_eq!(host.find_calls, vec![Backend::Codex, Backend::Codex]);
        assert_eq!(host.installer_runs, vec![Backend::Codex]);
        assert_eq!(host.verified, vec![(Backend::Codex, codex_path)]);
    }

    #[test]
    fn installer_failure_reports_manual_command() {
        let mut host = MockHost {
            installer_result: Err("network down".to_string()),
            ..Default::default()
        };
        let err = resolve_with(Backend::Codex, false, true, "y\n", &mut host).unwrap_err();
        assert!(matches!(
            err,
            BackendBootstrapError::BackendInstallerFailed { .. }
        ));
        assert!(err.to_string().contains("network down"));
        assert!(err.to_string().contains("clud codex-update"));
    }

    #[test]
    fn verification_failure_rejects_install() {
        let installed = PathBuf::from("/home/me/.local/bin/claude");
        let mut host = MockHost {
            find_results: VecDeque::from([None, Some(installed.clone())]),
            installer_result: Ok(()),
            verify_result: Err("bad version".to_string()),
            ..Default::default()
        };
        let err = resolve_with(Backend::Claude, false, true, "y\n", &mut host).unwrap_err();
        assert!(matches!(
            err,
            BackendBootstrapError::BackendVerificationFailed { .. }
        ));
        assert!(err.to_string().contains("bad version"));
        assert_eq!(host.verified, vec![(Backend::Claude, installed)]);
    }

    #[test]
    fn path_fallback_uses_documented_native_install_location() {
        for backend in [Backend::Claude, Backend::Codex] {
            let temp = tempfile::tempdir().expect("tempdir");
            let native = temp.path().join(if cfg!(windows) {
                format!("{}.exe", backend.executable_name())
            } else {
                backend.executable_name().to_string()
            });
            // The binary does NOT exist yet — the installer materializes it.
            // (Pre-writing it here would now short-circuit at the pre-prompt
            // gate and never exercise the post-install fallback.)
            let mut host = MockHost {
                find_results: VecDeque::from([None, None]),
                installer_result: Ok(()),
                native_path: Some(native.clone()),
                installer_creates: Some(native.clone()),
                verify_result: Ok(()),
                ..Default::default()
            };
            let (path, _) = resolve_with(backend, false, true, "Y\n", &mut host).expect("path");
            assert_eq!(path, native.to_string_lossy());
            assert_eq!(host.verified, vec![(backend, native)]);
            assert_eq!(host.installer_runs, vec![backend]);
        }
    }

    /// Issue #934: the binary is already at the documented native install
    /// location but missing from `PATH` (stale shell env, an updater
    /// mid-swap). clud must use it as-is — no prompt, no reinstall.
    #[test]
    fn existing_native_install_is_used_when_path_lookup_misses() {
        for backend in [Backend::Claude, Backend::Codex] {
            let temp = tempfile::tempdir().expect("tempdir");
            let native = temp.path().join(if cfg!(windows) {
                format!("{}.exe", backend.executable_name())
            } else {
                backend.executable_name().to_string()
            });
            std::fs::write(&native, b"mock").expect("write native path");
            let mut host = MockHost {
                find_results: VecDeque::from([None]),
                native_path: Some(native.clone()),
                ..Default::default()
            };

            // Empty stdin: if this prompted, `read_yes` would see EOF and the
            // call would fail as declined rather than resolving.
            let (path, err) = resolve_with(backend, false, true, "", &mut host).expect("path");

            assert_eq!(path, native.to_string_lossy());
            assert!(
                host.installer_runs.is_empty(),
                "must not reinstall a backend that is already on disk"
            );
            assert!(
                !err.contains("is not installed"),
                "must not claim the backend is missing, got: {err:?}"
            );
        }
    }

    /// The diagnostic names the fallback location it checked, so a false
    /// "not installed" is debuggable from the transcript alone.
    #[test]
    fn missing_backend_prompt_reports_the_locations_checked() {
        let temp = tempfile::tempdir().expect("tempdir");
        let absent = temp.path().join("claude-not-here");
        let mut host = MockHost {
            find_results: VecDeque::from([None]),
            native_path: Some(absent.clone()),
            ..Default::default()
        };

        let mut input = io::Cursor::new(b"n\n".to_vec());
        let mut buf = Vec::<u8>::new();
        let _ = resolve_backend_path(
            Backend::Claude,
            false,
            true,
            &mut input,
            &mut buf,
            &mut host,
        );

        let text = String::from_utf8(buf).expect("utf8");
        assert!(text.contains("not found on PATH"), "got: {text:?}");
        assert!(
            text.contains(&absent.display().to_string()),
            "diagnostic must name the fallback path checked, got: {text:?}"
        );
    }
}
