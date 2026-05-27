//! Backend executable resolution plus first-run bootstrap helpers.
//!
//! Keep installer prompting here rather than in the runner so every launch
//! path consumes the same resolved backend path before `LaunchPlan` is built.

use std::fmt;
use std::io::{BufRead, Write};
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
const CODEX_WINDOWS_POWERSHELL_INSTALL_COMMAND: &str =
    "irm https://chatgpt.com/codex/install.ps1 | iex";

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
    PosixShell {
        command: &'static str,
    },
    WindowsPowerShell {
        command: &'static str,
        cmd_fallback: Option<&'static str>,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InstallLocation {
    HomeLocalBin { executable: &'static str },
    UserProfileLocalBin { executable: &'static str },
    LocalAppDataCodexBin,
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
                manual_install_command: CODEX_POSIX_INSTALL_COMMAND,
                installer: InstallerPlan::PosixShell {
                    command: CODEX_POSIX_INSTALL_COMMAND,
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
    if let Some(path) = host.find_backend(backend) {
        return Ok(path.to_string_lossy().to_string());
    }

    if dry_run {
        return Ok(backend.executable_name().to_string());
    }

    let spec = BackendInstallSpec::for_backend(backend, host.platform());
    if !interactive {
        return Err(BackendBootstrapError::BackendMissingNonInteractive {
            backend,
            product_name: spec.product_name,
            install_command: spec.manual_install_command,
        });
    }

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

    let installed_path = host.find_backend(backend).or_else(|| {
        host.native_backend_path(&spec)
            .filter(|path| path.is_file())
    });
    let Some(path) = installed_path else {
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

fn run_backend_installer(spec: &BackendInstallSpec) -> Result<(), String> {
    match spec.installer {
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
    let command = subprocess::command_spec_for_subprocess(vec![
        path.to_string_lossy().to_string(),
        "--version".to_string(),
    ]);
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

fn run_captured_command(command: CommandSpec) -> Result<(i32, String), String> {
    let process = NativeProcess::new(ProcessConfig {
        command,
        cwd: None,
        env: None,
        capture: true,
        stderr_mode: StderrMode::Stdout,
        creationflags: invisible_helper_creationflags(),
        create_process_group: false,
        stdin_mode: StdinMode::Null,
        nice: None,
    });
    process
        .start()
        .map_err(|err| format!("failed to start command: {err}"))?;

    let mut buf = Vec::<u8>::new();
    loop {
        match process.read_combined(Some(Duration::from_millis(100))) {
            ReadStatus::Line(event) => {
                buf.extend_from_slice(&event.line);
                buf.push(b'\n');
            }
            ReadStatus::Timeout => {
                if process.returncode().is_some() {
                    break;
                }
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

    struct MockHost {
        platform: InstallPlatform,
        find_results: VecDeque<Option<PathBuf>>,
        find_calls: Vec<Backend>,
        installer_runs: Vec<Backend>,
        installer_result: Result<(), String>,
        native_path: Option<PathBuf>,
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
    fn codex_linux_install_spec_uses_standalone_command_and_home_local_bin() {
        let spec = BackendInstallSpec::for_backend(Backend::Codex, InstallPlatform::Linux);
        assert_eq!(spec.manual_install_command, CODEX_POSIX_INSTALL_COMMAND);
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
        for backend in [Backend::Claude, Backend::Codex] {
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
                install_command: CODEX_POSIX_INSTALL_COMMAND
            }
        );
        assert!(err.to_string().contains("chatgpt.com/codex/install.sh"));
        assert!(host.installer_runs.is_empty());
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
            let expected_prompt =
                BackendInstallSpec::for_backend(backend, InstallPlatform::Linux).prompt() + "\n";
            assert_eq!(prompt, expected_prompt);
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
        assert!(err.to_string().contains("chatgpt.com/codex/install"));
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
            std::fs::write(&native, b"mock").expect("write native path");
            let mut host = MockHost {
                find_results: VecDeque::from([None, None]),
                installer_result: Ok(()),
                native_path: Some(native.clone()),
                verify_result: Ok(()),
                ..Default::default()
            };
            let (path, _) = resolve_with(backend, false, true, "Y\n", &mut host).expect("path");
            assert_eq!(path, native.to_string_lossy());
            assert_eq!(host.verified, vec![(backend, native)]);
        }
    }
}
