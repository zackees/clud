use std::path::PathBuf;

use clap::ValueEnum;
use serde::{Deserialize, Serialize};

/// Model API/provider selected by the user.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ValueEnum)]
#[serde(rename_all = "snake_case")]
#[value(rename_all = "lower")]
pub enum ModelProvider {
    Claude,
    Codex,
    #[serde(rename = "deepseek")]
    DeepSeek,
    Kimi,
    #[serde(rename = "openrouter")]
    OpenRouter,
}

/// Whether one provider owns the launch or the Claude harness routes among
/// several providers through the launch-scoped gateway.
///
/// This is deliberately separate from [`ModelProvider`] and from
/// [`LaunchMode`] (subprocess versus PTY).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RoutingMode {
    #[default]
    Direct,
    Unified,
}

impl RoutingMode {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Direct => "direct",
            Self::Unified => "unified",
        }
    }
}

impl ModelProvider {
    /// Every provider variant. Registry guardrail tests iterate this so a new
    /// variant added without updating it fails loudly instead of silently
    /// falling through provider-inference/settings lookups.
    pub const ALL: &'static [ModelProvider] = &[
        Self::Claude,
        Self::Codex,
        Self::DeepSeek,
        Self::Kimi,
        Self::OpenRouter,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Claude => "claude",
            Self::Codex => "codex",
            Self::DeepSeek => "deepseek",
            Self::Kimi => "kimi",
            Self::OpenRouter => "openrouter",
        }
    }

    pub fn from_settings_str(value: &str) -> Option<Self> {
        let lowered = value.to_ascii_lowercase();
        Self::ALL
            .iter()
            .copied()
            .find(|provider| provider.as_str() == lowered)
    }

    pub fn native_harness(self) -> Backend {
        match self {
            Self::Claude | Self::DeepSeek | Self::Kimi | Self::OpenRouter => Backend::Claude,
            Self::Codex => Backend::Codex,
        }
    }

    /// Provider-native wire-ID prefixes, used to infer a provider from a raw
    /// model wire ID that isn't in the catalog. Exhaustive match is
    /// deliberate: adding a provider must extend this or the build breaks.
    pub fn wire_prefixes(self) -> &'static [&'static str] {
        match self {
            Self::Claude => &["claude-"],
            Self::Codex => &["gpt-", "codex-"],
            Self::DeepSeek => &["deepseek-"],
            Self::Kimi => &["kimi-"],
            // OpenRouter's `anthropic/*` aliases are not an unambiguous
            // provider identity; require --openrouter or its clud catalog ID.
            Self::OpenRouter => &[],
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
    #[serde(rename = "deepseek")]
    DeepSeek,
}

impl HarnessSelection {
    pub const fn for_backend(backend: Backend) -> Self {
        match backend {
            Backend::Claude => Self::Claude,
            Backend::Codex => Self::Codex,
            Backend::DeepSeek => Self::DeepSeek,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Default => "default",
            Self::Claude => "claude",
            Self::Codex => "codex",
            Self::DeepSeek => "deepseek",
        }
    }

    pub fn from_settings_str(value: &str) -> Option<Self> {
        match value.to_ascii_lowercase().as_str() {
            "default" => Some(Self::Default),
            "claude" => Some(Self::Claude),
            "codex" => Some(Self::Codex),
            "deepseek" | "dsh" => Some(Self::DeepSeek),
            _ => None,
        }
    }

    pub fn resolve(self, provider: ModelProvider) -> Backend {
        match self {
            Self::Default => provider.native_harness(),
            Self::Claude => Backend::Claude,
            Self::Codex => Backend::Codex,
            Self::DeepSeek => Backend::DeepSeek,
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
    ProviderSetting,
    CredentialFallback,
    BuiltInDefault,
}

impl PreferenceSource {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Cli => "cli",
            Self::GlobalSetting => "global_setting",
            Self::ProviderSetting => "provider_setting",
            Self::CredentialFallback => "credential_fallback",
            Self::BuiltInDefault => "built_in_default",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ResolvedLaunchTarget {
    pub routing_mode: RoutingMode,
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
    KimiViaCodexUnsupported,
    OpenRouterViaCodexUnsupported,
    UnifiedViaCodexUnsupported,
    UnifiedViaDeepSeekUnsupported,
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
                "unsupported launch target: DeepSeek provider cannot use the Codex harness"
            ),
            Self::KimiViaCodexUnsupported => write!(
                f,
                "unsupported launch target: Kimi provider cannot use the Codex harness"
            ),
            Self::OpenRouterViaCodexUnsupported => write!(
                f,
                "unsupported launch target: OpenRouter provider cannot use the Codex harness"
            ),
            Self::UnifiedViaCodexUnsupported => write!(
                f,
                "unsupported launch target: unified routing requires the Claude harness"
            ),
            Self::UnifiedViaDeepSeekUnsupported => write!(
                f,
                "unsupported launch target: unified routing requires the Claude harness"
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
    DeepSeek,
}

impl Backend {
    pub const ALL: [Self; 3] = [Self::Claude, Self::Codex, Self::DeepSeek];

    /// The executable name to search for on PATH.
    pub fn executable_name(&self) -> &'static str {
        match self {
            Backend::Claude => "claude",
            Backend::Codex => "codex",
            Backend::DeepSeek => "dsh",
        }
    }

    pub fn from_settings_str(value: &str) -> Option<Self> {
        if value.eq_ignore_ascii_case("claude") {
            Some(Backend::Claude)
        } else if value.eq_ignore_ascii_case("codex") {
            Some(Backend::Codex)
        } else if value.eq_ignore_ascii_case("deepseek") || value.eq_ignore_ascii_case("dsh") {
            Some(Backend::DeepSeek)
        } else {
            None
        }
    }

    pub fn as_model_provider(self) -> ModelProvider {
        match self {
            Self::Claude => ModelProvider::Claude,
            Self::Codex => ModelProvider::Codex,
            Self::DeepSeek => ModelProvider::DeepSeek,
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
    let cli_provider = if deepseek {
        Some(ModelProvider::DeepSeek)
    } else if codex {
        Some(ModelProvider::Codex)
    } else if claude {
        Some(ModelProvider::Claude)
    } else {
        None
    };
    resolve_launch_target_with_provider(cli_provider, cli_harness, global_provider, global_harness)
}

/// Resolve a launch target from already-normalized CLI provider intent.
///
/// This is the provider-neutral entry point used by `--provider` and by a
/// qualified `--model` that infers its provider. The boolean wrapper above is
/// retained for existing callers and compatibility tests.
pub fn resolve_launch_target_with_provider(
    cli_provider: Option<ModelProvider>,
    cli_harness: Option<HarnessSelection>,
    global_provider: Option<ModelProvider>,
    global_harness: Option<HarnessSelection>,
) -> Result<ResolvedLaunchTarget, LaunchTargetError> {
    resolve_routed_launch_target(
        RoutingMode::Direct,
        cli_provider,
        cli_harness,
        global_provider,
        global_harness,
    )
}

pub fn resolve_routed_launch_target(
    routing_mode: RoutingMode,
    cli_provider: Option<ModelProvider>,
    cli_harness: Option<HarnessSelection>,
    global_provider: Option<ModelProvider>,
    global_harness: Option<HarnessSelection>,
) -> Result<ResolvedLaunchTarget, LaunchTargetError> {
    if routing_mode == RoutingMode::Unified {
        let requested_harness = cli_harness.unwrap_or(HarnessSelection::Default);
        if requested_harness == HarnessSelection::Codex {
            return Err(LaunchTargetError::UnifiedViaCodexUnsupported);
        }
        if requested_harness == HarnessSelection::DeepSeek {
            return Err(LaunchTargetError::UnifiedViaDeepSeekUnsupported);
        }
        return Ok(ResolvedLaunchTarget {
            routing_mode,
            model_provider: cli_provider.unwrap_or(ModelProvider::Claude),
            requested_harness,
            effective_harness: Backend::Claude,
            provider_source: if cli_provider.is_some() {
                PreferenceSource::Cli
            } else {
                PreferenceSource::BuiltInDefault
            },
            harness_source: if cli_harness.is_some() {
                PreferenceSource::Cli
            } else {
                PreferenceSource::BuiltInDefault
            },
        });
    }
    let (model_provider, provider_source) = if let Some(provider) = cli_provider {
        (provider, PreferenceSource::Cli)
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
            ModelProvider::Kimi => return Err(LaunchTargetError::KimiViaCodexUnsupported),
            ModelProvider::OpenRouter => {
                return Err(LaunchTargetError::OpenRouterViaCodexUnsupported)
            }
            ModelProvider::Codex => {}
        }
    }

    Ok(ResolvedLaunchTarget {
        routing_mode,
        model_provider,
        requested_harness,
        effective_harness,
        provider_source,
        harness_source,
    })
}

/// Validate options whose meaning depends on the resolved model provider.
pub fn validate_provider_options(
    _target: ResolvedLaunchTarget,
    _model: Option<&str>,
) -> Result<(), LaunchTargetError> {
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
        Backend::DeepSeek => "DeepSeek",
    };
    Some(format!(
        "\x1b[32m[clud] Harness override: {name} (global setting)\x1b[0m"
    ))
}

/// What [`resolve_launch_mode`] decides from. Named fields rather than
/// positional bools: most of these are `bool`, and a swapped pair compiles.
#[derive(Debug, Clone, Copy)]
pub struct LaunchModeRequest {
    /// `--pty` was passed.
    pub pty: bool,
    /// `--subprocess` was passed.
    pub subprocess: bool,
    pub backend: Backend,
    /// Codex runs as `codex exec` (non-interactive).
    pub codex_uses_exec: bool,
    /// A `clud loop` launch.
    pub is_loop: bool,
    /// clud's own stdin **and** stdout are terminals
    /// (`session::terminals_are_interactive`).
    pub parent_has_tty: bool,
    /// The backend will run its interactive TUI: no non-interactive prompt
    /// (`-p`, `loop`, …) and no `--repeat` schedule.
    pub interactive_session: bool,
}

/// Resolve how the backend should be launched.
///
/// Explicit `--pty` / `--subprocess` always wins. Otherwise:
/// - Interactive Claude with a real terminal runs through the PTY pump
///   (#691, [DD-086]). Both stdin and stdout must be terminals: ConPTY stops
///   relaying child output when clud's stdout is redirected, so
///   `clud > out.txt` would hang. Everything else — piped or redirected
///   stdio, `-p`, `--repeat` — stays a subprocess. `CLUD_PTY_DEFAULT=0`
///   restores the old subprocess default for interactive launches, and
///   `CLUD_PTY_DEFAULT=1` forces PTY for every Claude launch.
/// - In `clud loop` mode, non-Windows Claude defaults to PTY so the user sees
///   live token streaming. Loop iterations take long enough that the
///   subprocess's silent-until-EOF buffering makes it impossible to tell if
///   the agent is working or hung; see #32. Windows loops stay subprocess:
///   the stream-json progress fallback (`command/builder.rs`) only exists on
///   the subprocess path.
/// - Codex `exec` (non-interactive) always uses subprocess.
/// - Codex interactive TUI (#1181, [DD-070]): on Linux/macOS it runs through
///   the PTY pump even when clud already has a real terminal, so clud owns
///   the byte stream and can apply the Codex-only bare-LF normalizer
///   (`codex_lf.rs`). On Windows with a real terminal it still runs as a
///   subprocess inheriting that console: ConPTY adds its own repaint layer
///   (#515) and the console's LF->CRLF output processing already masks the
///   Codex rendering bug, so there is nothing to gain from wrapping. When
///   clud has no TTY on any platform (piped stdin or headless host), the
///   child is wrapped in a PTY so the TUI has a pseudo-console to talk to.
///   History: PR #47 (titled `(#46)`, but #46 is an unrelated CI issue —
///   see #737) introduced "Codex + TTY => subprocess" because the old
///   crossterm event loop dropped Codex's startup `\x1b[6n` reply; the same
///   PR replaced that loop with the raw byte pump that forwards replies
///   verbatim, which removed the hang's mechanism.
pub fn resolve_launch_mode(request: LaunchModeRequest) -> LaunchMode {
    resolve_launch_mode_with_pty_default(
        request,
        parse_pty_default(std::env::var_os("CLUD_PTY_DEFAULT").as_deref()),
    )
}

/// `CLUD_PTY_DEFAULT` as a tri-state: unset or empty is `None` (use the
/// built-in rule), `0`/`false`/`off` is `Some(false)`, and any other value is
/// `Some(true)`.
fn parse_pty_default(value: Option<&std::ffi::OsStr>) -> Option<bool> {
    let value = value?.to_string_lossy();
    let value = value.trim();
    if value.is_empty() {
        return None;
    }
    Some(value != "0" && !value.eq_ignore_ascii_case("false") && !value.eq_ignore_ascii_case("off"))
}

fn resolve_launch_mode_with_pty_default(
    request: LaunchModeRequest,
    pty_default: Option<bool>,
) -> LaunchMode {
    let LaunchModeRequest {
        pty,
        subprocess,
        backend,
        codex_uses_exec,
        is_loop,
        parent_has_tty,
        interactive_session,
    } = request;
    if pty {
        return LaunchMode::Pty;
    }
    if subprocess {
        return LaunchMode::Subprocess;
    }
    match backend {
        Backend::Claude if pty_default == Some(true) => LaunchMode::Pty,
        Backend::Claude if parent_has_tty && interactive_session && pty_default != Some(false) => {
            LaunchMode::Pty
        }
        Backend::Claude if is_loop && !cfg!(target_os = "windows") => LaunchMode::Pty,
        Backend::Claude => LaunchMode::Subprocess,
        Backend::Codex if codex_uses_exec => LaunchMode::Subprocess,
        Backend::Codex if parent_has_tty && cfg!(target_os = "windows") => LaunchMode::Subprocess,
        Backend::Codex => LaunchMode::Pty,
        Backend::DeepSeek => LaunchMode::Subprocess,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn model_provider_all_has_no_duplicates_and_matches_variant_count() {
        // Fails loudly if a variant is added without updating `ALL` -- the
        // exact silent-fallback failure mode this registry hoist removes.
        let all = ModelProvider::ALL;
        for (index, provider) in all.iter().enumerate() {
            for other in &all[index + 1..] {
                assert_ne!(provider, other, "ModelProvider::ALL has a duplicate entry");
            }
        }
        // Compare against clap's derive-generated variant list rather than a
        // hardcoded count: a hardcoded `3` would silently keep passing if a
        // future variant were added without updating `ALL`, which is exactly
        // the silent-fallback failure mode this registry exists to remove.
        assert_eq!(
            all.len(),
            ModelProvider::value_variants().len(),
            "ModelProvider::ALL must list every enum variant exactly once"
        );
    }

    #[test]
    fn every_model_provider_round_trips_through_settings_str() {
        for provider in ModelProvider::ALL {
            let value = provider.as_str();
            assert_eq!(ModelProvider::from_settings_str(value), Some(*provider));
            // Case-insensitivity must survive the table-driven rewrite.
            assert_eq!(
                ModelProvider::from_settings_str(&value.to_ascii_uppercase()),
                Some(*provider)
            );
        }
        assert_eq!(ModelProvider::from_settings_str("not-a-provider"), None);
    }

    #[test]
    fn wire_prefixes_are_exhaustive_and_match_the_original_prefix_ladder() {
        assert_eq!(ModelProvider::Claude.wire_prefixes(), &["claude-"]);
        assert_eq!(ModelProvider::Codex.wire_prefixes(), &["gpt-", "codex-"]);
        assert_eq!(ModelProvider::DeepSeek.wire_prefixes(), &["deepseek-"]);
        assert_eq!(ModelProvider::Kimi.wire_prefixes(), &["kimi-"]);
    }

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
            Some(HarnessSelection::DeepSeek),
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
        assert_eq!(
            HarnessSelection::Default.resolve(ModelProvider::Kimi),
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
    fn deepseek_rejects_the_codex_harness_but_accepts_a_qualified_model() {
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
            validate_provider_options(target, Some("deepseek-v4-pro")),
            Ok(())
        );
        assert_eq!(validate_provider_options(target, None), Ok(()));
    }

    #[test]
    fn kimi_rejects_the_codex_harness_but_accepts_the_claude_harness() {
        let target = resolve_launch_target_with_provider(
            Some(ModelProvider::Kimi),
            Some(HarnessSelection::Codex),
            None,
            None,
        );
        assert_eq!(target, Err(LaunchTargetError::KimiViaCodexUnsupported));
        assert_eq!(
            target.unwrap_err().to_string(),
            "unsupported launch target: Kimi provider cannot use the Codex harness"
        );

        let target =
            resolve_launch_target_with_provider(Some(ModelProvider::Kimi), None, None, None)
                .unwrap();
        assert_eq!(target.effective_harness, Backend::Claude);
        assert_eq!(validate_provider_options(target, Some("kimi-k3")), Ok(()));
    }

    #[test]
    fn normalized_cli_provider_keeps_cli_source_metadata() {
        let target = resolve_launch_target_with_provider(
            Some(ModelProvider::Codex),
            None,
            Some(ModelProvider::Claude),
            None,
        )
        .unwrap();
        assert_eq!(target.model_provider, ModelProvider::Codex);
        assert_eq!(target.provider_source, PreferenceSource::Cli);
    }

    #[test]
    fn unified_routing_uses_claude_harness_without_importing_saved_preferences() {
        let target = resolve_routed_launch_target(
            RoutingMode::Unified,
            Some(ModelProvider::Codex),
            None,
            Some(ModelProvider::DeepSeek),
            Some(HarnessSelection::Codex),
        )
        .unwrap();
        assert_eq!(target.routing_mode, RoutingMode::Unified);
        assert_eq!(target.model_provider, ModelProvider::Codex);
        assert_eq!(target.effective_harness, Backend::Claude);
        assert_eq!(target.provider_source, PreferenceSource::Cli);
        assert_eq!(target.harness_source, PreferenceSource::BuiltInDefault);
    }

    #[test]
    fn unified_routing_rejects_an_explicit_codex_harness() {
        assert_eq!(
            resolve_routed_launch_target(
                RoutingMode::Unified,
                None,
                Some(HarnessSelection::Codex),
                None,
                None,
            ),
            Err(LaunchTargetError::UnifiedViaCodexUnsupported)
        );
    }

    #[test]
    fn unified_routing_rejects_an_explicit_deepseek_harness() {
        assert_eq!(
            resolve_routed_launch_target(
                RoutingMode::Unified,
                None,
                Some(HarnessSelection::DeepSeek),
                None,
                None,
            ),
            Err(LaunchTargetError::UnifiedViaDeepSeekUnsupported)
        );
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

    /// An interactive launch with a real terminal and no flags or env
    /// override; each test changes only the fields it is about.
    fn request(backend: Backend) -> LaunchModeRequest {
        LaunchModeRequest {
            pty: false,
            subprocess: false,
            backend,
            codex_uses_exec: false,
            is_loop: false,
            parent_has_tty: true,
            interactive_session: true,
        }
    }

    fn mode(request: LaunchModeRequest) -> LaunchMode {
        resolve_launch_mode_with_pty_default(request, None)
    }

    /// #691: interactive Claude with a real terminal runs through the PTY
    /// pump on every platform.
    #[test]
    fn test_interactive_claude_with_tty_uses_pty() {
        assert_eq!(mode(request(Backend::Claude)), LaunchMode::Pty);
    }

    /// ConPTY stops relaying child output when clud's stdout is redirected,
    /// so anything short of a real terminal on both ends keeps the subprocess
    /// path (`parent_has_tty` is stdin **and** stdout).
    #[test]
    fn test_claude_without_tty_stays_subprocess() {
        assert_eq!(
            mode(LaunchModeRequest {
                parent_has_tty: false,
                ..request(Backend::Claude)
            }),
            LaunchMode::Subprocess
        );
    }

    /// `-p`, `--repeat` and the other non-interactive shapes never run the
    /// TUI, so a PTY would only add per-byte cost.
    #[test]
    fn test_noninteractive_claude_with_tty_stays_subprocess() {
        assert_eq!(
            mode(LaunchModeRequest {
                interactive_session: false,
                ..request(Backend::Claude)
            }),
            LaunchMode::Subprocess
        );
    }

    #[test]
    fn test_pty_default_env_parses_as_tri_state() {
        use std::ffi::OsStr;
        assert_eq!(parse_pty_default(None), None);
        assert_eq!(parse_pty_default(Some(OsStr::new(""))), None);
        assert_eq!(parse_pty_default(Some(OsStr::new("  "))), None);
        for off in ["0", "false", "FALSE", "off", " Off "] {
            assert_eq!(
                parse_pty_default(Some(OsStr::new(off))),
                Some(false),
                "{off:?}"
            );
        }
        for on in ["1", "true", "on", "yes"] {
            assert_eq!(
                parse_pty_default(Some(OsStr::new(on))),
                Some(true),
                "{on:?}"
            );
        }
    }

    /// `CLUD_PTY_DEFAULT=0` is the environment-level escape hatch back to the
    /// pre-#691 subprocess default, without passing `--subprocess` each time.
    #[test]
    fn test_pty_default_off_restores_subprocess_for_interactive_claude() {
        assert_eq!(
            resolve_launch_mode_with_pty_default(request(Backend::Claude), Some(false)),
            LaunchMode::Subprocess
        );
    }

    #[test]
    fn test_claude_loop_uses_pty_for_streaming() {
        // #32: subprocess silence during long loop iterations makes it
        // impossible to tell if claude is working or hung. Loop mode opts
        // into PTY so token output streams live.
        //
        // Windows loops stay subprocess. Not for #38's ConPTY
        // handle-inheritance, which the old comment cited: #38 is closed and
        // was about daemon-worker PTY sessions, not this pump. The reason on
        // record now (DD-086) is that a loop is non-interactive, and the
        // stream-json progress view plus the `<<<CLUD_LOOP_DONE>>>` token
        // fallback exist only on the subprocess path (`command/builder.rs`).
        let expected = if cfg!(target_os = "windows") {
            LaunchMode::Subprocess
        } else {
            LaunchMode::Pty
        };
        let loop_request = LaunchModeRequest {
            is_loop: true,
            interactive_session: false,
            ..request(Backend::Claude)
        };
        assert_eq!(mode(loop_request), expected);
    }

    #[test]
    fn test_claude_loop_respects_explicit_subprocess_override() {
        // --subprocess still wins for users who want the old behavior.
        let loop_request = LaunchModeRequest {
            subprocess: true,
            is_loop: true,
            interactive_session: false,
            ..request(Backend::Claude)
        };
        assert_eq!(mode(loop_request), LaunchMode::Subprocess);
        // ...and for interactive launches, the documented escape hatch.
        let interactive = LaunchModeRequest {
            subprocess: true,
            ..request(Backend::Claude)
        };
        assert_eq!(mode(interactive), LaunchMode::Subprocess);
    }

    /// `CLUD_PTY_DEFAULT=1` forces PTY even where the built-in rule would
    /// not: no terminal, or a non-interactive loop.
    #[test]
    fn test_claude_pty_default_on_forces_pty() {
        let headless = LaunchModeRequest {
            parent_has_tty: false,
            interactive_session: false,
            ..request(Backend::Claude)
        };
        assert_eq!(
            resolve_launch_mode_with_pty_default(headless, Some(true)),
            LaunchMode::Pty
        );
        let loop_request = LaunchModeRequest {
            is_loop: true,
            interactive_session: false,
            ..request(Backend::Claude)
        };
        assert_eq!(
            resolve_launch_mode_with_pty_default(loop_request, Some(true)),
            LaunchMode::Pty
        );
    }

    #[test]
    fn test_claude_pty_default_respects_explicit_subprocess_override() {
        let explicit = LaunchModeRequest {
            subprocess: true,
            ..request(Backend::Claude)
        };
        assert_eq!(
            resolve_launch_mode_with_pty_default(explicit, Some(true)),
            LaunchMode::Subprocess
        );
    }

    #[test]
    fn test_pty_default_audit_flag_does_not_change_codex_exec() {
        let exec = LaunchModeRequest {
            codex_uses_exec: true,
            parent_has_tty: false,
            ..request(Backend::Codex)
        };
        assert_eq!(
            resolve_launch_mode_with_pty_default(exec, Some(true)),
            LaunchMode::Subprocess
        );
    }

    #[test]
    fn test_codex_interactive_no_tty_uses_pty() {
        // When clud has no real terminal (piped stdin / headless), wrap the
        // child in a PTY so its TUI has a pseudo-console to talk to.
        let headless = LaunchModeRequest {
            parent_has_tty: false,
            ..request(Backend::Codex)
        };
        assert_eq!(mode(headless), LaunchMode::Pty);
    }

    /// Windows keeps inheriting the real console (#1181): ConPTY brings its
    /// own repaint layer (#515) and the console already translates LF to
    /// CRLF, so the Codex goal-cell rendering bug never shows there. This
    /// test runs on the Windows exec lane and is the guard that the flip
    /// for Linux/macOS did not change Windows behavior.
    ///
    /// Cite PR #47, not issue #46, for the original rule. The PR is titled
    /// `... (#46)`, so the number is not wrong -- but the *issue* is "CI:
    /// macos-15-intel integration test can't locate mock-agent", which
    /// concluded it was not a PTY regression (#737).
    #[cfg(windows)]
    #[test]
    fn test_codex_interactive_with_tty_uses_subprocess_on_windows() {
        assert_eq!(mode(request(Backend::Codex)), LaunchMode::Subprocess);
    }

    /// Linux/macOS run interactive Codex through the PTY pump even with a
    /// real terminal (#1181), so clud is in the byte path and
    /// `codex_lf::CodexLfNormalizer` can mask Codex's bare-LF goal cell.
    #[cfg(not(windows))]
    #[test]
    fn test_codex_interactive_with_tty_uses_pty_off_windows() {
        assert_eq!(mode(request(Backend::Codex)), LaunchMode::Pty);
    }

    /// Explicit `--subprocess` still wins for Codex with a TTY on every
    /// platform, so the old inherit-the-console behavior stays reachable.
    #[test]
    fn test_codex_interactive_with_tty_explicit_subprocess_wins() {
        let explicit = LaunchModeRequest {
            subprocess: true,
            ..request(Backend::Codex)
        };
        assert_eq!(mode(explicit), LaunchMode::Subprocess);
    }

    #[test]
    fn test_codex_exec_defaults_to_subprocess() {
        // `clud --codex -p "..."` -> `codex exec` -> non-interactive, pipeable.
        for parent_has_tty in [true, false] {
            let exec = LaunchModeRequest {
                codex_uses_exec: true,
                parent_has_tty,
                interactive_session: false,
                ..request(Backend::Codex)
            };
            assert_eq!(mode(exec), LaunchMode::Subprocess);
        }
    }

    #[test]
    fn test_launch_mode_pty_override() {
        let headless_claude = LaunchModeRequest {
            pty: true,
            parent_has_tty: false,
            interactive_session: false,
            ..request(Backend::Claude)
        };
        assert_eq!(mode(headless_claude), LaunchMode::Pty);
        let codex_exec = LaunchModeRequest {
            pty: true,
            codex_uses_exec: true,
            ..request(Backend::Codex)
        };
        assert_eq!(mode(codex_exec), LaunchMode::Pty);
    }

    #[test]
    fn test_launch_mode_subprocess_override() {
        for backend in [Backend::Claude, Backend::Codex] {
            let explicit = LaunchModeRequest {
                subprocess: true,
                ..request(backend)
            };
            assert_eq!(mode(explicit), LaunchMode::Subprocess);
        }
    }

    /// The env override is read by the public entry point, which production
    /// launches use; with it unset the public and injected paths agree.
    #[test]
    fn test_public_resolver_matches_injected_default_when_env_unset() {
        if std::env::var_os("CLUD_PTY_DEFAULT").is_some() {
            return;
        }
        let interactive = request(Backend::Claude);
        assert_eq!(resolve_launch_mode(interactive), mode(interactive));
    }
}
