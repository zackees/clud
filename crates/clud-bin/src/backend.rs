use std::path::PathBuf;

use clap::ValueEnum;
use serde::{Deserialize, Serialize};

/// Model API/provider selected by the user.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ModelProvider {
    Claude,
    Codex,
    #[serde(rename = "deepseek")]
    DeepSeek,
}

impl ModelProvider {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Claude => "claude",
            Self::Codex => "codex",
            Self::DeepSeek => "deepseek",
        }
    }

    pub fn from_settings_str(value: &str) -> Option<Self> {
        match value.to_ascii_lowercase().as_str() {
            "claude" => Some(Self::Claude),
            "codex" => Some(Self::Codex),
            "deepseek" => Some(Self::DeepSeek),
            _ => None,
        }
    }

    pub fn native_harness(self) -> Backend {
        match self {
            Self::Claude | Self::DeepSeek => Backend::Claude,
            Self::Codex => Backend::Codex,
        }
    }
}

impl std::fmt::Display for ModelProvider {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// User-requested harness preference. `Default` resolves to the provider's
/// native executable.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, ValueEnum)]
#[serde(rename_all = "snake_case")]
#[value(rename_all = "lower")]
pub enum HarnessSelection {
    #[default]
    Default,
    Claude,
    Codex,
}

impl HarnessSelection {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Default => "default",
            Self::Claude => "claude",
            Self::Codex => "codex",
        }
    }

    pub fn from_settings_str(value: &str) -> Option<Self> {
        match value.to_ascii_lowercase().as_str() {
            "default" => Some(Self::Default),
            "claude" => Some(Self::Claude),
            "codex" => Some(Self::Codex),
            _ => None,
        }
    }

    pub fn resolve(self, provider: ModelProvider) -> Backend {
        match self {
            Self::Default => provider.native_harness(),
            Self::Claude => Backend::Claude,
            Self::Codex => Backend::Codex,
        }
    }
}

impl std::fmt::Display for HarnessSelection {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PreferenceSource {
    Cli,
    GlobalSetting,
    BuiltInDefault,
}

impl PreferenceSource {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Cli => "cli",
            Self::GlobalSetting => "global_setting",
            Self::BuiltInDefault => "built_in_default",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ResolvedLaunchTarget {
    pub model_provider: ModelProvider,
    pub requested_harness: HarnessSelection,
    pub effective_harness: Backend,
    pub provider_source: PreferenceSource,
    pub harness_source: PreferenceSource,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LaunchTargetError {
    ClaudeViaCodexUnsupported,
    DeepSeekViaCodexUnsupported,
    DeepSeekModelUnsupported,
}

impl std::fmt::Display for LaunchTargetError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::ClaudeViaCodexUnsupported => write!(
                f,
                "unsupported launch target: Claude provider cannot use the Codex harness"
            ),
            Self::DeepSeekViaCodexUnsupported => write!(
                f,
                "unsupported launch target: DeepSeek provider requires the Claude harness"
            ),
            Self::DeepSeekModelUnsupported => write!(
                f,
                "unsupported launch option: DeepSeek uses its fixed provider profile and does not accept --model"
            ),
        }
    }
}

impl std::error::Error for LaunchTargetError {}

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

    pub fn from_settings_str(value: &str) -> Option<Self> {
        if value.eq_ignore_ascii_case("claude") {
            Some(Backend::Claude)
        } else if value.eq_ignore_ascii_case("codex") {
            Some(Backend::Codex)
        } else {
            None
        }
    }

    pub fn as_model_provider(self) -> ModelProvider {
        match self {
            Self::Claude => ModelProvider::Claude,
            Self::Codex => ModelProvider::Codex,
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
pub fn resolve_backend(claude: bool, codex: bool) -> Backend {
    resolve_backend_with_default(claude, codex, None)
}

/// Resolve which backend to use based on CLI flags and a persisted default.
///
/// Explicit CLI backend flags always win. The persisted default only applies
/// to bare `clud` launches.
pub fn resolve_backend_with_default(
    claude: bool,
    codex: bool,
    default_backend: Option<Backend>,
) -> Backend {
    if codex {
        Backend::Codex
    } else if claude {
        Backend::Claude
    } else {
        default_backend.unwrap_or(Backend::Claude)
    }
}

/// Resolve model provider and harness independently.
///
/// Each dimension follows the same precedence: CLI > global setting >
/// built-in default. The concrete harness is validated after resolution so an
/// unsupported route never silently falls back.
pub fn resolve_launch_target(
    claude: bool,
    codex: bool,
    deepseek: bool,
    cli_harness: Option<HarnessSelection>,
    global_provider: Option<ModelProvider>,
    global_harness: Option<HarnessSelection>,
) -> Result<ResolvedLaunchTarget, LaunchTargetError> {
    let (model_provider, provider_source) = if deepseek {
        (ModelProvider::DeepSeek, PreferenceSource::Cli)
    } else if codex {
        (ModelProvider::Codex, PreferenceSource::Cli)
    } else if claude {
        (ModelProvider::Claude, PreferenceSource::Cli)
    } else if let Some(provider) = global_provider {
        (provider, PreferenceSource::GlobalSetting)
    } else {
        (ModelProvider::Claude, PreferenceSource::BuiltInDefault)
    };

    let (requested_harness, harness_source) = if let Some(harness) = cli_harness {
        (harness, PreferenceSource::Cli)
    } else if let Some(harness) = global_harness {
        (harness, PreferenceSource::GlobalSetting)
    } else {
        (HarnessSelection::Default, PreferenceSource::BuiltInDefault)
    };
    let effective_harness = requested_harness.resolve(model_provider);
    if effective_harness == Backend::Codex {
        match model_provider {
            ModelProvider::Claude => return Err(LaunchTargetError::ClaudeViaCodexUnsupported),
            ModelProvider::DeepSeek => return Err(LaunchTargetError::DeepSeekViaCodexUnsupported),
            ModelProvider::Codex => {}
        }
    }

    Ok(ResolvedLaunchTarget {
        model_provider,
        requested_harness,
        effective_harness,
        provider_source,
        harness_source,
    })
}

/// Validate options whose meaning depends on the resolved model provider.
pub fn validate_provider_options(
    target: ResolvedLaunchTarget,
    model: Option<&str>,
) -> Result<(), LaunchTargetError> {
    if target.model_provider == ModelProvider::DeepSeek && model.is_some() {
        return Err(LaunchTargetError::DeepSeekModelUnsupported);
    }
    Ok(())
}

pub fn saved_harness_override_notice(
    target: ResolvedLaunchTarget,
    stderr_is_terminal: bool,
    structured_output: bool,
) -> Option<String> {
    if structured_output
        || !stderr_is_terminal
        || target.harness_source != PreferenceSource::GlobalSetting
        || target.requested_harness == HarnessSelection::Default
    {
        return None;
    }
    let name = match target.effective_harness {
        Backend::Claude => "Claude",
        Backend::Codex => "Codex",
    };
    Some(format!(
        "\x1b[32m[clud] Harness override: {name} (global setting)\x1b[0m"
    ))
}

/// Resolve how the backend should be launched.
///
/// Explicit `--pty` / `--subprocess` always wins. Otherwise:
/// - Claude defaults to subprocess. #328 is **closed** (2026-06-16) and did
///   not flip this: it landed the Ctrl+V / Shift+Enter work plus the opt-in
///   flag below, but its audit criterion — flip the default and run the
///   platform matrix — was never carried out. The open proposal to flip is
///   #691; until that lands, subprocess stays the default by inertia rather
///   than by an in-progress audit.
///   `CLUD_PTY_DEFAULT=1` opts Claude into PTY by default so the matrix and
///   manual Windows checks can exercise the keyboard-interception path without
///   changing the stable default yet. In `clud loop` mode, non-Windows already
///   defaults to PTY so the user sees live token streaming. Loop iterations
///   take long enough that the subprocess-default's silent-until-EOF buffering
///   makes it impossible to tell if the agent is working or hung; see #32.
/// - Codex `exec` (non-interactive) always uses subprocess.
/// - Codex interactive TUI uses subprocess when clud is already running in
///   a real terminal so the child inherits that TTY directly. The terminal
///   emulator answers DSR/cursor queries natively, avoiding the ConPTY-wrapped
///   hang where codex's Ink TUI writes `\x1b[6n` on startup and never gets a
///   reply. When clud has no TTY (piped stdin or headless host), we still wrap
///   the child in a PTY so the TUI has some pseudo-console to talk to.
pub fn resolve_launch_mode(
    pty: bool,
    subprocess: bool,
    backend: Backend,
    codex_uses_exec: bool,
    is_loop: bool,
    parent_has_tty: bool,
) -> LaunchMode {
    resolve_launch_mode_with_pty_default(
        pty,
        subprocess,
        backend,
        codex_uses_exec,
        is_loop,
        parent_has_tty,
        env_pty_default_enabled(),
    )
}

fn env_pty_default_enabled() -> bool {
    std::env::var_os("CLUD_PTY_DEFAULT").is_some_and(|value| {
        let value = value.to_string_lossy();
        let value = value.trim();
        !value.is_empty()
            && value != "0"
            && !value.eq_ignore_ascii_case("false")
            && !value.eq_ignore_ascii_case("off")
    })
}

fn resolve_launch_mode_with_pty_default(
    pty: bool,
    subprocess: bool,
    backend: Backend,
    codex_uses_exec: bool,
    is_loop: bool,
    parent_has_tty: bool,
    pty_default: bool,
) -> LaunchMode {
    if pty {
        return LaunchMode::Pty;
    }
    if subprocess {
        return LaunchMode::Subprocess;
    }
    match backend {
        Backend::Claude if pty_default => LaunchMode::Pty,
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
    fn test_persisted_default_backend_wins_for_bare_launch() {
        assert_eq!(
            resolve_backend_with_default(false, false, Some(Backend::Codex)),
            Backend::Codex
        );
    }

    #[test]
    fn test_explicit_backend_flags_override_persisted_default() {
        assert_eq!(
            resolve_backend_with_default(true, false, Some(Backend::Codex)),
            Backend::Claude
        );
        assert_eq!(
            resolve_backend_with_default(false, true, Some(Backend::Claude)),
            Backend::Codex
        );
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
    fn launch_target_precedence_is_independent_per_dimension() {
        let cli_providers = [
            (false, false, false, None),
            (true, false, false, Some(ModelProvider::Claude)),
            (false, true, false, Some(ModelProvider::Codex)),
            (false, false, true, Some(ModelProvider::DeepSeek)),
        ];
        let global_providers = [
            None,
            Some(ModelProvider::Claude),
            Some(ModelProvider::Codex),
            Some(ModelProvider::DeepSeek),
        ];
        let harnesses = [
            None,
            Some(HarnessSelection::Default),
            Some(HarnessSelection::Claude),
            Some(HarnessSelection::Codex),
        ];

        for (claude, codex, deepseek, cli_provider) in cli_providers {
            for global_provider in global_providers {
                for cli_harness in harnesses {
                    for global_harness in harnesses {
                        let provider = cli_provider
                            .or(global_provider)
                            .unwrap_or(ModelProvider::Claude);
                        let provider_source = if cli_provider.is_some() {
                            PreferenceSource::Cli
                        } else if global_provider.is_some() {
                            PreferenceSource::GlobalSetting
                        } else {
                            PreferenceSource::BuiltInDefault
                        };
                        let requested = cli_harness
                            .or(global_harness)
                            .unwrap_or(HarnessSelection::Default);
                        let harness_source = if cli_harness.is_some() {
                            PreferenceSource::Cli
                        } else if global_harness.is_some() {
                            PreferenceSource::GlobalSetting
                        } else {
                            PreferenceSource::BuiltInDefault
                        };
                        let result = resolve_launch_target(
                            claude,
                            codex,
                            deepseek,
                            cli_harness,
                            global_provider,
                            global_harness,
                        );
                        let effective = requested.resolve(provider);

                        if provider == ModelProvider::Claude && effective == Backend::Codex {
                            assert_eq!(
                                result,
                                Err(LaunchTargetError::ClaudeViaCodexUnsupported),
                                "cli_provider={cli_provider:?}, global_provider={global_provider:?}, \
                                 cli_harness={cli_harness:?}, global_harness={global_harness:?}"
                            );
                            continue;
                        }
                        if provider == ModelProvider::DeepSeek && effective == Backend::Codex {
                            assert_eq!(
                                result,
                                Err(LaunchTargetError::DeepSeekViaCodexUnsupported),
                                "cli_provider={cli_provider:?}, global_provider={global_provider:?}, \
                                 cli_harness={cli_harness:?}, global_harness={global_harness:?}"
                            );
                            continue;
                        }

                        let target = result.unwrap();
                        assert_eq!(target.model_provider, provider);
                        assert_eq!(target.requested_harness, requested);
                        assert_eq!(target.effective_harness, effective);
                        assert_eq!(target.provider_source, provider_source);
                        assert_eq!(target.harness_source, harness_source);
                    }
                }
            }
        }
    }

    #[test]
    fn default_harness_maps_to_provider_native_executable() {
        assert_eq!(
            HarnessSelection::Default.resolve(ModelProvider::Claude),
            Backend::Claude
        );
        assert_eq!(
            HarnessSelection::Default.resolve(ModelProvider::Codex),
            Backend::Codex
        );
        assert_eq!(
            HarnessSelection::Default.resolve(ModelProvider::DeepSeek),
            Backend::Claude
        );
    }

    #[test]
    fn unsupported_cross_route_is_an_error() {
        let error = resolve_launch_target(
            true,
            false,
            false,
            Some(HarnessSelection::Codex),
            None,
            None,
        )
        .unwrap_err();
        assert_eq!(error, LaunchTargetError::ClaudeViaCodexUnsupported);
        assert_eq!(
            error.to_string(),
            "unsupported launch target: Claude provider cannot use the Codex harness"
        );
    }

    #[test]
    fn deepseek_rejects_the_codex_harness_and_model_override() {
        let target = resolve_launch_target(
            false,
            false,
            true,
            Some(HarnessSelection::Codex),
            None,
            None,
        );
        assert_eq!(target, Err(LaunchTargetError::DeepSeekViaCodexUnsupported));

        let target = resolve_launch_target(false, false, true, None, None, None).unwrap();
        assert_eq!(target.effective_harness, Backend::Claude);
        assert_eq!(
            validate_provider_options(target, Some("anything")),
            Err(LaunchTargetError::DeepSeekModelUnsupported)
        );
        assert_eq!(validate_provider_options(target, None), Ok(()));
    }

    #[test]
    fn saved_non_default_harness_notice_is_green_and_tty_only() {
        let target = resolve_launch_target(
            false,
            false,
            false,
            None,
            Some(ModelProvider::Codex),
            Some(HarnessSelection::Claude),
        )
        .unwrap();
        let notice = saved_harness_override_notice(target, true, false).unwrap();
        assert!(notice.starts_with("\x1b[32m"));
        assert!(notice.contains("Harness override: Claude (global setting)"));
        assert!(notice.ends_with("\x1b[0m"));
        assert_eq!(saved_harness_override_notice(target, false, false), None);
        assert_eq!(saved_harness_override_notice(target, true, true), None);

        let saved_default = resolve_launch_target(
            false,
            false,
            false,
            None,
            Some(ModelProvider::Codex),
            Some(HarnessSelection::Default),
        )
        .unwrap();
        assert_eq!(
            saved_harness_override_notice(saved_default, true, false),
            None
        );

        let cli_override = resolve_launch_target(
            false,
            true,
            false,
            Some(HarnessSelection::Claude),
            None,
            None,
        )
        .unwrap();
        assert_eq!(
            saved_harness_override_notice(cli_override, true, false),
            None
        );
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
        // into PTY so token output streams live.
        //
        // Still gated to non-Windows, but NOT for the reason this comment
        // used to give. It cited #38's "Windows ConPTY handle-inheritance",
        // and #38 is closed (2026-04-19) and was about `clud attach` hanging
        // on *daemon-worker* PTY sessions spawned through
        // `spawn_detached_self(bInheritHandles=TRUE)` — a different code path
        // from this foreground pump, which inherits nothing. So the gate has
        // no verified justification on record; whether Windows loop-PTY
        // actually misbehaves is untested. #691 owns re-testing it.
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
    fn test_claude_pty_default_audit_flag_uses_pty() {
        assert_eq!(
            resolve_launch_mode_with_pty_default(
                false,
                false,
                Backend::Claude,
                false,
                false,
                true,
                true,
            ),
            LaunchMode::Pty
        );
        assert_eq!(
            resolve_launch_mode_with_pty_default(
                false,
                false,
                Backend::Claude,
                false,
                true,
                true,
                true
            ),
            LaunchMode::Pty
        );
    }

    #[test]
    fn test_claude_pty_default_respects_explicit_subprocess_override() {
        assert_eq!(
            resolve_launch_mode_with_pty_default(
                false,
                true,
                Backend::Claude,
                false,
                false,
                true,
                true
            ),
            LaunchMode::Subprocess
        );
    }

    #[test]
    fn test_pty_default_audit_flag_does_not_change_codex_exec() {
        assert_eq!(
            resolve_launch_mode_with_pty_default(
                false,
                false,
                Backend::Codex,
                true,
                false,
                false,
                true
            ),
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
        // When clud already runs in a real terminal, inherit that TTY
        // directly instead of wrapping in ConPTY. The terminal answers DSR
        // queries natively; the ConPTY path was leaving codex's Ink TUI
        // hung on startup waiting for a reply.
        //
        // Cite PR #47, not issue #46. The PR is titled `... (#46)`, so the
        // number is not wrong -- but the *issue* is "CI: macos-15-intel
        // integration test can't locate mock-agent", which concluded it was
        // not a PTY regression. A reader chasing #46 for this mechanism finds
        // a CI path bug and reasonably concludes the comment is nonsense. The
        // fix that made TTY inheritance correct is PR #47.
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
