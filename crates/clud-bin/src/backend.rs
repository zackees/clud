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
/// Explicit `--pty` / `--subprocess` always wins. Otherwise:
/// - Claude defaults to subprocess while PTY issues are being investigated,
///   **except** in `clud loop` mode on non-Windows where we use PTY so the
///   user sees live token streaming. Loop iterations take long enough that
///   the subprocess-default's silent-until-EOF buffering makes it impossible
///   to tell if the agent is working or hung — see #32. Windows stays on
///   subprocess for now because ConPTY handle-inheritance under loops still
///   hangs (see #38); once that's fixed, the gate can be removed.
/// - Codex `exec` (non-interactive) always uses subprocess.
/// - Codex interactive TUI uses subprocess when clud is already running in
///   a real terminal so the child inherits that TTY directly — the terminal
///   emulator answers DSR/cursor queries natively, avoiding the ConPTY-
///   wrapped hang where codex's Ink TUI writes `\x1b[6n` on startup and
///   never gets a reply (see #46). When clud has no TTY (piped stdin or
///   headless host), we still wrap the child in a PTY so the TUI has
///   *some* pseudo-console to talk to.
pub fn resolve_launch_mode(
    pty: bool,
    subprocess: bool,
    backend: Backend,
    codex_uses_exec: bool,
    is_loop: bool,
    parent_has_tty: bool,
) -> LaunchMode {
    if pty {
        return LaunchMode::Pty;
    }
    if subprocess {
        return LaunchMode::Subprocess;
    }
    match backend {
        Backend::Claude if is_loop && !cfg!(target_os = "windows") => LaunchMode::Pty,
        Backend::Claude => LaunchMode::Subprocess,
        Backend::Codex if codex_uses_exec => LaunchMode::Subprocess,
        Backend::Codex if parent_has_tty => LaunchMode::Subprocess,
        Backend::Codex => LaunchMode::Pty,
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
    fn test_claude_defaults_to_subprocess() {
        assert_eq!(
            resolve_launch_mode(false, false, Backend::Claude, false, false, true),
            LaunchMode::Subprocess
        );
        assert_eq!(
            resolve_launch_mode(false, false, Backend::Claude, true, false, true),
            LaunchMode::Subprocess
        );
    }

    #[test]
    fn test_claude_loop_uses_pty_for_streaming() {
        // #32: subprocess silence during long loop iterations makes it
        // impossible to tell if claude is working or hung. Loop mode opts
        // into PTY so token output streams live. Gated to non-Windows
        // until #38's Windows ConPTY handle-inheritance is fixed.
        let expected = if cfg!(target_os = "windows") {
            LaunchMode::Subprocess
        } else {
            LaunchMode::Pty
        };
        assert_eq!(
            resolve_launch_mode(false, false, Backend::Claude, false, true, true),
            expected
        );
    }

    #[test]
    fn test_claude_loop_respects_explicit_subprocess_override() {
        // --subprocess still wins for users who want the old behavior.
        assert_eq!(
            resolve_launch_mode(false, true, Backend::Claude, false, true, true),
            LaunchMode::Subprocess
        );
    }

    #[test]
    fn test_codex_interactive_no_tty_uses_pty() {
        // When clud has no real terminal (piped stdin / headless), wrap the
        // child in a PTY so its TUI has a pseudo-console to talk to.
        assert_eq!(
            resolve_launch_mode(false, false, Backend::Codex, false, false, false),
            LaunchMode::Pty
        );
    }

    #[test]
    fn test_codex_interactive_with_tty_uses_subprocess() {
        // #46: when clud already runs in a real terminal, inherit that TTY
        // directly instead of wrapping in ConPTY. The terminal answers DSR
        // queries natively; the ConPTY path was leaving codex's Ink TUI
        // hung on startup waiting for a reply.
        assert_eq!(
            resolve_launch_mode(false, false, Backend::Codex, false, false, true),
            LaunchMode::Subprocess
        );
    }

    #[test]
    fn test_codex_exec_defaults_to_subprocess() {
        // `clud --codex -p "..."` -> `codex exec` -> non-interactive, pipeable.
        assert_eq!(
            resolve_launch_mode(false, false, Backend::Codex, true, false, true),
            LaunchMode::Subprocess
        );
        assert_eq!(
            resolve_launch_mode(false, false, Backend::Codex, true, false, false),
            LaunchMode::Subprocess
        );
    }

    #[test]
    fn test_launch_mode_pty_override() {
        assert_eq!(
            resolve_launch_mode(true, false, Backend::Claude, false, false, true),
            LaunchMode::Pty
        );
        assert_eq!(
            resolve_launch_mode(true, false, Backend::Codex, true, false, true),
            LaunchMode::Pty
        );
    }

    #[test]
    fn test_launch_mode_subprocess_override() {
        assert_eq!(
            resolve_launch_mode(false, true, Backend::Claude, false, false, true),
            LaunchMode::Subprocess
        );
        assert_eq!(
            resolve_launch_mode(false, true, Backend::Codex, false, false, true),
            LaunchMode::Subprocess
        );
    }
}
