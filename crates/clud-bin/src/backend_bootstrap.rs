//! Backend executable resolution plus first-run bootstrap helpers.
//!
//! Keep installer prompting here rather than in the runner so every launch
//! path consumes the same resolved backend path before `LaunchPlan` is built.

use std::fmt;
use std::io::{BufRead, Write};
use std::path::{Path, PathBuf};
use std::process::Command;

use crate::backend::{self, Backend};

#[cfg(not(windows))]
const CLAUDE_POSIX_INSTALL_COMMAND: &str = "curl -fsSL https://claude.ai/install.sh | bash";

#[cfg(windows)]
const CLAUDE_WINDOWS_POWERSHELL_INSTALL_COMMAND: &str = "irm https://claude.ai/install.ps1 | iex";

#[cfg(windows)]
const CLAUDE_WINDOWS_CMD_INSTALL_COMMAND: &str =
    "curl -fsSL https://claude.ai/install.cmd -o install.cmd && install.cmd && del install.cmd";

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BackendBootstrapError {
    BackendMissing {
        backend: Backend,
    },
    ClaudeMissingNonInteractive {
        install_command: &'static str,
    },
    ClaudeInstallDeclined {
        install_command: &'static str,
    },
    ClaudeInstallerFailed {
        message: String,
        install_command: &'static str,
    },
    ClaudeVerificationFailed {
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
            Self::ClaudeMissingNonInteractive { install_command } => write!(
                f,
                "error: claude not found on PATH. Install Claude Code with: {install_command}"
            ),
            Self::ClaudeInstallDeclined { install_command } => write!(
                f,
                "error: Claude Code install declined. Install manually with: {install_command}"
            ),
            Self::ClaudeInstallerFailed {
                message,
                install_command,
            } => write!(
                f,
                "error: Claude Code install failed: {message}. Install manually with: {install_command}"
            ),
            Self::ClaudeVerificationFailed {
                message,
                install_command,
            } => write!(
                f,
                "error: Claude Code installer completed, but clud could not verify claude: {message}. Install manually with: {install_command}"
            ),
        }
    }
}

pub trait BackendBootstrapHost {
    fn find_backend(&mut self, backend: Backend) -> Option<PathBuf>;
    fn run_claude_native_installer(&mut self) -> Result<(), String>;
    fn native_claude_path(&self) -> Option<PathBuf>;
    fn verify_claude(&mut self, path: &Path) -> Result<(), String>;
}

pub struct ProductionBackendBootstrapHost;

impl BackendBootstrapHost for ProductionBackendBootstrapHost {
    fn find_backend(&mut self, backend: Backend) -> Option<PathBuf> {
        backend::find_backend(backend)
    }

    fn run_claude_native_installer(&mut self) -> Result<(), String> {
        run_claude_native_installer()
    }

    fn native_claude_path(&self) -> Option<PathBuf> {
        native_claude_path()
    }

    fn verify_claude(&mut self, path: &Path) -> Result<(), String> {
        verify_claude(path)
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

    if backend != Backend::Claude {
        return Err(BackendBootstrapError::BackendMissing { backend });
    }

    let install_command = official_claude_install_command();
    if !interactive {
        return Err(BackendBootstrapError::ClaudeMissingNonInteractive { install_command });
    }

    writeln!(
        err,
        "Claude Code is not installed. Install Anthropic's native Claude Code binary now? [y/N]"
    )
    .ok();
    err.flush().ok();

    if !read_yes(input) {
        return Err(BackendBootstrapError::ClaudeInstallDeclined { install_command });
    }

    host.run_claude_native_installer().map_err(|message| {
        BackendBootstrapError::ClaudeInstallerFailed {
            message,
            install_command,
        }
    })?;

    let installed_path = host
        .find_backend(Backend::Claude)
        .or_else(|| host.native_claude_path().filter(|path| path.is_file()));
    let Some(path) = installed_path else {
        return Err(BackendBootstrapError::ClaudeVerificationFailed {
            message: "installed binary was not found on PATH or in the native install directory"
                .to_string(),
            install_command,
        });
    };

    host.verify_claude(&path).map_err(|message| {
        BackendBootstrapError::ClaudeVerificationFailed {
            message,
            install_command,
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

pub fn official_claude_install_command() -> &'static str {
    #[cfg(windows)]
    {
        CLAUDE_WINDOWS_POWERSHELL_INSTALL_COMMAND
    }
    #[cfg(not(windows))]
    {
        CLAUDE_POSIX_INSTALL_COMMAND
    }
}

fn run_claude_native_installer() -> Result<(), String> {
    #[cfg(windows)]
    {
        let mut command = Command::new("powershell.exe");
        command
            .arg("-NoProfile")
            .arg("-ExecutionPolicy")
            .arg("Bypass")
            .arg("-Command")
            .arg(CLAUDE_WINDOWS_POWERSHELL_INSTALL_COMMAND);
        match run_command(&mut command) {
            Ok(()) => Ok(()),
            Err(first) => run_windows_cmd_installer().map_err(|second| {
                format!("PowerShell installer failed ({first}); CMD fallback failed ({second})")
            }),
        }
    }
    #[cfg(not(windows))]
    {
        let mut command = Command::new("sh");
        command.arg("-c").arg(CLAUDE_POSIX_INSTALL_COMMAND);
        run_command(&mut command)
    }
}

#[cfg(windows)]
fn run_windows_cmd_installer() -> Result<(), String> {
    let dir = std::env::temp_dir().join(format!("clud-claude-install-{}", std::process::id()));
    std::fs::create_dir_all(&dir)
        .map_err(|err| format!("failed to create temp dir {}: {err}", dir.display()))?;
    let mut command = Command::new("cmd.exe");
    command
        .arg("/D")
        .arg("/S")
        .arg("/C")
        .arg(CLAUDE_WINDOWS_CMD_INSTALL_COMMAND)
        .current_dir(&dir);
    let result = run_command(&mut command);
    let _ = std::fs::remove_dir_all(&dir);
    result
}

fn run_command(command: &mut Command) -> Result<(), String> {
    let status = command
        .status()
        .map_err(|err| format!("failed to start command: {err}"))?;
    if status.success() {
        Ok(())
    } else {
        Err(format!("command exited with {status}"))
    }
}

fn native_claude_path() -> Option<PathBuf> {
    #[cfg(windows)]
    {
        std::env::var_os("USERPROFILE")
            .map(PathBuf::from)
            .map(|home| home.join(".local").join("bin").join("claude.exe"))
    }
    #[cfg(not(windows))]
    {
        std::env::var_os("HOME")
            .map(PathBuf::from)
            .map(|home| home.join(".local").join("bin").join("claude"))
    }
}

fn verify_claude(path: &Path) -> Result<(), String> {
    let output = Command::new(path)
        .arg("--version")
        .output()
        .map_err(|err| format!("failed to run {} --version: {err}", path.display()))?;
    if output.status.success() {
        Ok(())
    } else {
        let stderr = String::from_utf8_lossy(&output.stderr);
        Err(format!(
            "{} --version exited with {}; stderr: {}",
            path.display(),
            output.status,
            stderr.trim()
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::VecDeque;
    use std::io;

    struct MockHost {
        find_results: VecDeque<Option<PathBuf>>,
        installer_runs: usize,
        installer_result: Result<(), String>,
        native_path: Option<PathBuf>,
        verified: Vec<PathBuf>,
        verify_result: Result<(), String>,
    }

    impl Default for MockHost {
        fn default() -> Self {
            Self {
                find_results: VecDeque::new(),
                installer_runs: 0,
                installer_result: Ok(()),
                native_path: None,
                verified: Vec::new(),
                verify_result: Ok(()),
            }
        }
    }

    impl BackendBootstrapHost for MockHost {
        fn find_backend(&mut self, _backend: Backend) -> Option<PathBuf> {
            self.find_results.pop_front().unwrap_or(None)
        }

        fn run_claude_native_installer(&mut self) -> Result<(), String> {
            self.installer_runs += 1;
            self.installer_result.clone()
        }

        fn native_claude_path(&self) -> Option<PathBuf> {
            self.native_path.clone()
        }

        fn verify_claude(&mut self, path: &Path) -> Result<(), String> {
            self.verified.push(path.to_path_buf());
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
        assert_eq!(host.installer_runs, 0);
    }

    #[test]
    fn dry_run_uses_placeholder_without_installing() {
        let mut host = MockHost::default();
        let (path, prompt) =
            resolve_with(Backend::Claude, true, true, "y\n", &mut host).expect("path");
        assert_eq!(path, "claude");
        assert!(prompt.is_empty());
        assert_eq!(host.installer_runs, 0);
    }

    #[test]
    fn codex_missing_does_not_install_claude() {
        let mut host = MockHost::default();
        let err = resolve_with(Backend::Codex, false, true, "y\n", &mut host).unwrap_err();
        assert_eq!(
            err,
            BackendBootstrapError::BackendMissing {
                backend: Backend::Codex
            }
        );
        assert_eq!(host.installer_runs, 0);
    }

    #[test]
    fn claude_missing_noninteractive_prints_install_command() {
        let mut host = MockHost::default();
        let err = resolve_with(Backend::Claude, false, false, "", &mut host).unwrap_err();
        assert_eq!(
            err,
            BackendBootstrapError::ClaudeMissingNonInteractive {
                install_command: official_claude_install_command()
            }
        );
        assert!(err.to_string().contains("claude.ai/install"));
        assert_eq!(host.installer_runs, 0);
    }

    #[test]
    fn interactive_decline_does_not_install() {
        let mut host = MockHost::default();
        let err = resolve_with(Backend::Claude, false, true, "n\n", &mut host).unwrap_err();
        assert_eq!(
            err,
            BackendBootstrapError::ClaudeInstallDeclined {
                install_command: official_claude_install_command()
            }
        );
        assert_eq!(host.installer_runs, 0);
    }

    #[test]
    fn interactive_accept_installs_verifies_and_returns_path_from_path() {
        let installed = PathBuf::from("/home/me/.local/bin/claude");
        let mut host = MockHost {
            find_results: VecDeque::from([None, Some(installed.clone())]),
            installer_result: Ok(()),
            verify_result: Ok(()),
            ..Default::default()
        };
        let (path, prompt) =
            resolve_with(Backend::Claude, false, true, "yes\n", &mut host).expect("path");
        assert_eq!(path, installed.to_string_lossy());
        assert!(prompt.contains("Install Anthropic's native Claude Code binary now?"));
        assert_eq!(host.installer_runs, 1);
        assert_eq!(host.verified, vec![installed]);
    }

    #[test]
    fn installer_failure_reports_manual_command() {
        let mut host = MockHost {
            installer_result: Err("network down".to_string()),
            ..Default::default()
        };
        let err = resolve_with(Backend::Claude, false, true, "y\n", &mut host).unwrap_err();
        assert!(matches!(
            err,
            BackendBootstrapError::ClaudeInstallerFailed { .. }
        ));
        assert!(err.to_string().contains("network down"));
        assert!(err.to_string().contains("claude.ai/install"));
    }

    #[test]
    fn path_fallback_uses_documented_native_install_location() {
        let temp = tempfile::tempdir().expect("tempdir");
        let native = temp.path().join(if cfg!(windows) {
            "claude.exe"
        } else {
            "claude"
        });
        std::fs::write(&native, b"mock").expect("write native path");
        let mut host = MockHost {
            find_results: VecDeque::from([None, None]),
            installer_result: Ok(()),
            native_path: Some(native.clone()),
            verify_result: Ok(()),
            ..Default::default()
        };
        let (path, _) = resolve_with(Backend::Claude, false, true, "Y\n", &mut host).expect("path");
        assert_eq!(path, native.to_string_lossy());
        assert_eq!(host.verified, vec![native]);
    }
}
