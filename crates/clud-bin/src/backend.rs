use std::path::PathBuf;

use serde::{Deserialize, Serialize};

/// Supported backend agents.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Backend {
    Claude,
    Codex,
}

impl Backend {
    /// The executable name to search for on PATH.
    pub fn executable_name(&self) -> &'static str {
        match self {
            Backend::Claude => "claude",
            Backend::Codex => "codex",
        }
    }
}

impl std::fmt::Display for Backend {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.executable_name())
    }
}

/// Supported process launch modes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LaunchMode {
    Subprocess,
    Pty,
}

impl LaunchMode {
    pub fn as_str(&self) -> &'static str {
        match self {
            LaunchMode::Subprocess => "subprocess",
            LaunchMode::Pty => "pty",
        }
    }
}

impl std::fmt::Display for LaunchMode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Find the backend executable on PATH.
pub fn find_backend(backend: Backend) -> Option<PathBuf> {
    which::which(backend.executable_name()).ok()
}

/// Resolve which backend to use based on CLI flags.
/// Default is Claude.
pub fn resolve_backend(_claude: bool, codex: bool) -> Backend {
    if codex {
        Backend::Codex
    } else {
        Backend::Claude
    }
}

/// Resolve how the backend should be launched.
///
/// For now, both backends default to subprocess mode. PTY remains available as
/// an explicit override while Claude PTY issues are being investigated.
pub fn resolve_launch_mode(pty: bool, _subprocess: bool, _backend: Backend) -> LaunchMode {
    if pty {
        LaunchMode::Pty
    } else {
        LaunchMode::Subprocess
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_default_is_claude() {
        assert_eq!(resolve_backend(false, false), Backend::Claude);
    }

    #[test]
    fn test_claude_flag() {
        assert_eq!(resolve_backend(true, false), Backend::Claude);
    }

    #[test]
    fn test_codex_flag() {
        assert_eq!(resolve_backend(false, true), Backend::Codex);
    }

    #[test]
    fn test_executable_names() {
        assert_eq!(Backend::Claude.executable_name(), "claude");
        assert_eq!(Backend::Codex.executable_name(), "codex");
    }

    #[test]
    fn test_launch_mode_defaults_to_subprocess() {
        assert_eq!(
            resolve_launch_mode(false, false, Backend::Claude),
            LaunchMode::Subprocess
        );
        assert_eq!(
            resolve_launch_mode(false, false, Backend::Codex),
            LaunchMode::Subprocess
        );
    }

    #[test]
    fn test_launch_mode_pty_override() {
        assert_eq!(
            resolve_launch_mode(true, false, Backend::Claude),
            LaunchMode::Pty
        );
    }

    #[test]
    fn test_launch_mode_subprocess_override() {
        assert_eq!(
            resolve_launch_mode(false, true, Backend::Claude),
            LaunchMode::Subprocess
        );
    }
}
