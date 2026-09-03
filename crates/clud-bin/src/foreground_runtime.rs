//! Foreground child runtime for provider/harness cross-routes (issue #626).

use crate::backend::{Backend, ModelProvider, RoutingMode};
use crate::codex_bridge::{
    BridgeConfig, BridgeError, BridgeHandle, UnifiedGatewayConfig, UNIFIED_GATEWAY_TOKEN_HEADER,
};
use crate::codex_model::ModelSpec;
use crate::command::LaunchPlan;
use crate::subprocess::ManagedSubprocess;
use running_process::pty::NativePtyProcess;
use std::fmt;
use std::io::Write;
#[cfg(test)]
use std::net::SocketAddr;
use std::path::{Path, PathBuf};

const DEFAULT_API_TIMEOUT_MS: &str = "3000000";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SpawnMode {
    Subprocess,
    Pty,
}

/// Narrow environment-aware spawn seam shared by subprocess and PTY paths.
/// Tests record the exact child overlay without installing a Claude binary;
/// production adapters below delegate to the existing running-process types.
pub trait SpawnAdapter<Output> {
    type Error;

    fn spawn(
        &self,
        mode: SpawnMode,
        command: Vec<String>,
        cwd: Option<String>,
        env: Vec<(String, String)>,
    ) -> Result<Output, Self::Error>;
}

pub struct ForegroundRuntime {
    env: Vec<(String, String)>,
    bridge: Option<BridgeHandle>,
    claude_settings: Option<ClaudeSettings>,
    startup_notices: Vec<&'static str>,
}

struct ClaudeSettings {
    value: String,
    replaces_user_argument: bool,
    _temp_file: tempfile::NamedTempFile,
}

impl ForegroundRuntime {
    /// Validate the shared launch-admission requirements without creating a
    /// listener, settings file, child environment, or harness process. Daemon
    /// submission calls this before creating a durable session; `start` calls
    /// it again at the worker/foreground boundary to cover later credential
    /// loss and every non-daemon launch mode.
    pub fn preflight(plan: &LaunchPlan) -> Result<(), BridgeError> {
        if is_codex_via_claude(plan) && !is_unified(plan) {
            preflight_codex_bridge_credentials()?;
        }
        Ok(())
    }

    pub fn start(plan: &LaunchPlan, env: Vec<(String, String)>) -> Result<Self, BridgeError> {
        // `dsh` owns its provider configuration and credentials. A native
        // DeepSeek Harness launch needs no clud vault or bridge setup at all.
        if plan.effective_harness() == Backend::DeepSeek {
            return Ok(Self {
                env,
                bridge: None,
                claude_settings: None,
                startup_notices: Vec::new(),
            });
        }
        // Prefer a descriptor resolved from the plan's provider so the store
        // is built with that provider's own vault identifiers. Unified plans
        // (provider `Claude`, no descriptor) fall back to the DeepSeek-scoped
        // constructor: `start_with_secret_store`'s unified branch still needs
        // to probe for a DeepSeek key even though DeepSeek is not this plan's
        // routed provider.
        let store = match crate::provider_registry::descriptor_for(plan.model_provider()) {
            Some(descriptor) => crate::provider_auth::NativeSecretStore::new_for(
                descriptor.vault_service,
                descriptor.vault_account,
            ),
            None => crate::provider_auth::NativeSecretStore::new(),
        }
        .map_err(|_| BridgeError::AnthropicCompatCredentials)?;
        // This is the shared pre-child admission boundary for subprocess,
        // PTY, detached, and worker launches. It must precede BridgeHandle
        // construction so refusal cannot bind a listener or expose settings.
        Self::preflight(plan)?;
        let runtime = Self::start_with_secret_store(plan, env, &store)?;
        for notice in &runtime.startup_notices {
            eprintln!("{notice}");
        }
        Ok(runtime)
    }

    /// Routing core, seamed on the secret-store dependency so tests can
    /// exercise the DeepSeek direct route without touching the host's real
    /// native credential vault. `start` is the sole production entry point.
    fn start_with_secret_store(
        plan: &LaunchPlan,
        mut env: Vec<(String, String)>,
        store: &dyn crate::provider_auth::SecretStore,
    ) -> Result<Self, BridgeError> {
        let (bridge, claude_settings, startup_notices) = if is_unified(plan) {
            // Optional routes must never block native Claude. Resolve only
            // availability metadata here; the actual credentials stay inside
            // the launch-scoped bridge and are not serialized into the plan.
            let deepseek_key = store.get().ok().flatten();
            // OpenRouter keeps its own vault record, so it needs its own store
            // rather than the injected DeepSeek-scoped one. Probed inline for
            // the same reason `codex_available` is: an absent optional
            // credential must omit a discovery row, never fail the launch.
            let openrouter_key = crate::provider_auth::NativeSecretStore::new_for(
                crate::provider_auth::OPENROUTER_VAULT_SERVICE,
                crate::provider_auth::OPENROUTER_VAULT_ACCOUNT,
            )
            .ok()
            .and_then(|store| {
                use crate::provider_auth::SecretStore as _;
                store.get().ok().flatten()
            });
            let codex_available =
                crate::codex_upstream::ResolvedCredentials::resolve_default().is_ok();
            let mut startup_notices =
                unified_startup_notices(codex_available, deepseek_key.is_some());
            if openrouter_key.is_none() {
                startup_notices.push(
                    "[clud] unified: OpenRouter is not configured; \
                     run `clud auth login openrouter` to add its route",
                );
            }
            // An unroutable rung fails the launch rather than a turn: by the
            // time a request is in flight the user has already waited, and the
            // error would arrive wrapped in the harness's API-error framing.
            let failover = crate::failover::FailoverLadder::parse(
                plan.failover.as_deref().unwrap_or_default(),
                plan.failover_allow_metered,
            )
            .map_err(|error| BridgeError::Failover(error.to_string()))?;
            if !failover.withheld_for_consent().is_empty() {
                startup_notices.push(
                    "[clud] failover: metered rungs are listed but will not be taken; \
                     pass --failover-allow-metered to consent",
                );
            }
            let bridge = BridgeHandle::start(
                BridgeConfig::default().with_unified_gateway(
                    UnifiedGatewayConfig::new(deepseek_key, codex_available)
                        .with_openrouter(openrouter_key)
                        .with_failover(failover),
                ),
            )?;
            apply_unified_overlay(&mut env, &bridge)?;
            let settings = merged_unified_context_lifecycle_settings(plan, &bridge)?;
            (Some(bridge), Some(settings), startup_notices)
        } else if is_codex_via_claude(plan) {
            // A selection that does not parse fails the launch rather than
            // the first turn: by the time a request is in flight the user has
            // already waited, and the message would arrive wrapped in the
            // harness's own API-error framing.
            let selection = codex_selection_from_plan(plan)?;
            let bridge =
                BridgeHandle::start(BridgeConfig::default().with_default_model(selection.clone()))?;
            apply_cross_route_overlay(&mut env, &bridge)?;
            let settings = merged_context_lifecycle_settings(plan, &bridge)?;
            (Some(bridge), Some(settings), Vec::new())
        } else if is_anthropic_compat_via_claude(plan) {
            // `is_anthropic_compat_via_claude` only returns true when a
            // descriptor resolves, so this `expect` cannot fail in practice;
            // it documents that invariant rather than silently defaulting.
            let descriptor = crate::provider_registry::descriptor_for(plan.model_provider())
                .expect("is_anthropic_compat_via_claude already proved a descriptor resolves");
            let secret = store
                .get()
                .map_err(|_| BridgeError::AnthropicCompatCredentials)?
                .ok_or(BridgeError::AnthropicCompatCredentials)?;
            apply_anthropic_compat_overlay(
                &mut env,
                &secret,
                descriptor,
                plan.model_selection.as_ref(),
            );
            (None, declared_hooks_settings(plan)?, Vec::new())
        } else {
            (None, declared_hooks_settings(plan)?, Vec::new())
        };
        // #967 Phase 2b: tell the hook binary that compiled dispatcher lines
        // are registered for this session, so the bare `clud-cmd-scan` line
        // stops running declared hooks itself and each one fires exactly once.
        if claude_settings.is_some() && declared_hook_fragment(plan).is_some() {
            env.push((
                crate::clud_hooks_compile::DISPATCH_ENV.to_string(),
                "1".to_string(),
            ));
        }
        // #967 Phase 3b: carry roots the hook cannot rediscover -- `--add-dir`
        // targets and `permissions.additionalDirectories` appear in no hook
        // payload.
        if let Some(entry) = hook_roots_env_value(plan) {
            env.push(entry);
        }
        Ok(Self {
            env,
            bridge,
            claude_settings,
            startup_notices,
        })
    }

    pub fn env(&self) -> &[(String, String)] {
        &self.env
    }

    #[cfg(test)]
    pub fn has_bridge(&self) -> bool {
        self.bridge.is_some()
    }

    /// How many turns the harness asked this launch's bridge to serve, or
    /// `None` when the launch is not bridge-routed. Feeds
    /// [`crate::launch_log::silent_bridge_reason`] (#998).
    pub fn bridge_turn_requests(&self) -> Option<usize> {
        self.bridge.as_ref().map(BridgeHandle::turn_requests)
    }

    #[cfg(test)]
    pub fn socket_addr(&self) -> Option<SocketAddr> {
        self.bridge.as_ref().map(BridgeHandle::socket_addr)
    }

    #[cfg(test)]
    pub fn base_url(&self) -> Option<&str> {
        self.bridge.as_ref().map(BridgeHandle::base_url)
    }

    #[cfg(test)]
    pub fn bearer_token(&self) -> Option<&str> {
        self.bridge.as_ref().map(BridgeHandle::bearer_token)
    }

    pub fn spawn_with<Output, Adapter: SpawnAdapter<Output>>(
        &self,
        adapter: &Adapter,
        mode: SpawnMode,
        mut command: Vec<String>,
        cwd: Option<String>,
    ) -> Result<Output, Adapter::Error> {
        if let Some(settings) = &self.claude_settings {
            // Claude accepts a single --settings source. Compose an explicit
            // user source during startup, then replace it here so neither the
            // user's settings nor the lifecycle hooks can shadow the other.
            if settings.replaces_user_argument {
                remove_user_settings_argument(&mut command);
            }
            command.insert(1, settings.value.clone());
            command.insert(1, "--settings".to_string());
        }
        adapter.spawn(mode, command, cwd, self.env.clone())
    }

    pub fn spawn_subprocess(
        &self,
        command: Vec<String>,
        cwd: Option<PathBuf>,
        capture_stdout: bool,
        creation_flags: Option<u32>,
    ) -> Result<ManagedSubprocess, String> {
        let adapter = NativeSubprocessAdapter {
            capture_stdout,
            creation_flags,
        };
        self.spawn_with(
            &adapter,
            SpawnMode::Subprocess,
            command,
            cwd.map(|path| path.to_string_lossy().into_owned()),
        )
    }

    pub fn spawn_pty(
        &self,
        command: Vec<String>,
        cwd: Option<String>,
        rows: u16,
        cols: u16,
    ) -> Result<NativePtyProcess, running_process::pty::PtyError> {
        let adapter = NativePtyAdapter { rows, cols };
        self.spawn_with(&adapter, SpawnMode::Pty, command, cwd)
    }
}

// Unit routing tests intentionally construct bridge runtimes without a host
// credential. The compiled CLI regression exercises the production boundary
// with a real isolated process/home; keep its resolver separate from those
// structural unit tests so they never accidentally consult developer state.
#[cfg(not(test))]
fn preflight_codex_bridge_credentials() -> Result<(), BridgeError> {
    crate::codex_upstream::ResolvedCredentials::preflight_default()
        .map_err(BridgeError::CodexBridgeCredentials)
}

#[cfg(test)]
fn preflight_codex_bridge_credentials() -> Result<(), BridgeError> {
    Ok(())
}

impl fmt::Debug for ForegroundRuntime {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ForegroundRuntime")
            .field("bridge_active", &self.bridge.is_some())
            .field("environment_entries", &self.env.len())
            .finish()
    }
}

pub fn with_foreground_runtime<ResultValue>(
    plan: &LaunchPlan,
    env: Vec<(String, String)>,
    run: impl FnOnce(&ForegroundRuntime) -> ResultValue,
) -> Result<ResultValue, BridgeError> {
    let runtime = ForegroundRuntime::start(plan, env)?;
    Ok(run(&runtime))
}

fn is_codex_via_claude(plan: &LaunchPlan) -> bool {
    plan.model_provider() == ModelProvider::Codex && plan.effective_harness() == Backend::Claude
}

/// True when the plan routes a descriptor-backed Anthropic-compatible
/// provider (DeepSeek today, Kimi in #937 Phase 3) directly through the
/// Claude harness -- as opposed to Claude native, the Codex translation
/// bridge, or unified-gateway routing.
fn is_anthropic_compat_via_claude(plan: &LaunchPlan) -> bool {
    plan.effective_harness() == Backend::Claude
        && crate::provider_registry::descriptor_for(plan.model_provider()).is_some()
}

fn is_unified(plan: &LaunchPlan) -> bool {
    plan.routing_mode == RoutingMode::Unified && plan.effective_harness() == Backend::Claude
}

fn unified_startup_notices(codex_available: bool, deepseek_available: bool) -> Vec<&'static str> {
    let mut notices = Vec::new();
    if !codex_available {
        notices.push(
            "[clud] unified gateway: Codex models unavailable; set OPENAI_API_KEY or run `clud auth login codex`",
        );
    }
    if !deepseek_available {
        notices.push(
            "[clud] unified gateway: DeepSeek models unavailable; run `clud auth login deepseek`",
        );
    }
    notices
}

/// Shared union scrub const used by every Anthropic-compat provider's overlay
/// (issue #937 Phase 2, #936 "Generalization" -> 1d). This is the union of:
///
/// - the DeepSeek connector's original list, plus
/// - `ANTHROPIC_SMALL_FAST_MODEL` and `ANTHROPIC_DEFAULT_FABLE_MODEL`, plus
/// - the legacy `*_NAME` variants of every default-model slot.
///
/// The additions are an intended hardening delta, not a no-op refactor: a
/// review finding on #936 noted the original list let an ambient value in any
/// of these slots survive into the DeepSeek child and misroute model
/// selection. `ANTHROPIC_CUSTOM_MODEL_OPTION*` stays a separate prefix scrub
/// below, not a literal entry here, since it has no fixed suffix.
///
/// `CLAUDE_CODE_EFFORT_LEVEL` is deliberately NOT on this list (DD-059): an
/// ambient user value is preserved so the harness's own `/effort` control
/// stays authoritative, and clud no longer injects its own pin -- the catalog
/// default effort travels on the harness's `--effort` session flag instead.
const ANTHROPIC_COMPAT_CONFLICTING: &[&str] = &[
    "ANTHROPIC_BASE_URL",
    "ANTHROPIC_AUTH_TOKEN",
    "ANTHROPIC_API_KEY",
    "ANTHROPIC_MODEL",
    "ANTHROPIC_SMALL_FAST_MODEL",
    "ANTHROPIC_DEFAULT_OPUS_MODEL",
    "ANTHROPIC_DEFAULT_SONNET_MODEL",
    "ANTHROPIC_DEFAULT_HAIKU_MODEL",
    "ANTHROPIC_DEFAULT_FABLE_MODEL",
    "ANTHROPIC_MODEL_NAME",
    "ANTHROPIC_DEFAULT_OPUS_MODEL_NAME",
    "ANTHROPIC_DEFAULT_SONNET_MODEL_NAME",
    "ANTHROPIC_DEFAULT_HAIKU_MODEL_NAME",
    "ANTHROPIC_DEFAULT_FABLE_MODEL_NAME",
    "CLAUDE_CODE_SUBAGENT_MODEL",
    "CLAUDE_CODE_AUTO_COMPACT_WINDOW",
    "CLAUDE_CODE_ENABLE_GATEWAY_MODEL_DISCOVERY",
];

/// Provider-neutral child-env overlay for any Anthropic-compatible API-key
/// provider (#936/#937 Phase 2, replacing the DeepSeek-only
/// `apply_deepseek_overlay`). `descriptor` supplies the base URL and the
/// subagent/haiku wire model; the default wire model when `selection` is
/// `None` comes from the catalog's reviewed default for the descriptor's
/// provider, not a hardcoded literal.
fn apply_anthropic_compat_overlay(
    env: &mut Vec<(String, String)>,
    secret: &str,
    descriptor: &'static crate::provider_registry::AnthropicCompatProvider,
    selection: Option<&crate::provider_catalog::ResolvedModelSelection>,
) {
    // Unconditionally case-insensitive (unlike `env_key_eq`, which mirrors
    // real per-OS env-var uniqueness semantics for the Codex overlay above):
    // this is a security guarantee against leaking an ambient Anthropic key
    // into the child, not an OS-semantics match, and it must hold the same
    // way on every platform.
    env.retain(|(key, _)| {
        !ANTHROPIC_COMPAT_CONFLICTING
            .iter()
            .any(|candidate| key.eq_ignore_ascii_case(candidate))
            && !key
                .to_ascii_uppercase()
                .starts_with("ANTHROPIC_CUSTOM_MODEL_OPTION")
    });
    if descriptor.provider == ModelProvider::OpenRouter {
        env.retain(|(key, _)| !key.eq_ignore_ascii_case("OPENROUTER_API_KEY"));
    }
    let default_wire_model = crate::provider_catalog::reviewed_default_model(descriptor.provider)
        .expect(
            "every Anthropic-compat descriptor's provider must have a reviewed catalog default \
             -- add a `provider_default: true` row in provider_catalog.rs",
        )
        .wire_id;
    let model = selection
        .and_then(|selection| selection.wire_model.as_deref())
        .unwrap_or(default_wire_model);
    let role_models = descriptor.role_models;
    let opus_model = role_models.map_or(model, |roles| roles.opus);
    let sonnet_model = role_models.map_or(model, |roles| roles.sonnet);
    let haiku_model = role_models.map_or(descriptor.subagent_wire_id, |roles| roles.haiku);
    let subagent_model = role_models.map_or(descriptor.subagent_wire_id, |roles| roles.subagent);
    env.extend([
        (
            "ANTHROPIC_BASE_URL".to_string(),
            descriptor.anthropic_base_url.to_string(),
        ),
        ("ANTHROPIC_AUTH_TOKEN".to_string(), secret.to_string()),
        ("ANTHROPIC_MODEL".to_string(), model.to_string()),
        (
            "ANTHROPIC_DEFAULT_OPUS_MODEL".to_string(),
            opus_model.to_string(),
        ),
        (
            "ANTHROPIC_DEFAULT_SONNET_MODEL".to_string(),
            sonnet_model.to_string(),
        ),
        (
            "ANTHROPIC_DEFAULT_HAIKU_MODEL".to_string(),
            haiku_model.to_string(),
        ),
        (
            "CLAUDE_CODE_SUBAGENT_MODEL".to_string(),
            subagent_model.to_string(),
        ),
    ]);
    match role_models.and_then(|roles| roles.fable) {
        Some(fable) => env.push((
            "ANTHROPIC_DEFAULT_FABLE_MODEL".to_string(),
            fable.to_string(),
        )),
        None if role_models.is_none() => env.push((
            "ANTHROPIC_DEFAULT_FABLE_MODEL".to_string(),
            model.to_string(),
        )),
        None => {}
    }
    if descriptor.explicitly_empty_anthropic_api_key {
        env.push(("ANTHROPIC_API_KEY".to_string(), String::new()));
    }
    if descriptor.enable_gateway_model_discovery {
        env.push((
            "CLAUDE_CODE_ENABLE_GATEWAY_MODEL_DISCOVERY".to_string(),
            "1".to_string(),
        ));
    }
    // Catalog data, not a hardcoded `model.ends_with("[1m]")` check (#937
    // Phase 2, #936 "Generalization" -> 1e): exact wire-ID lookup so an
    // auto-context wire model never falls back to a same-family row's window
    // via `cli_id`/alias matching.
    if let Some(window) = crate::provider_catalog::model_by_wire_id(model)
        .and_then(|entry| entry.claude_compact_window)
    {
        env.push((
            "CLAUDE_CODE_AUTO_COMPACT_WINDOW".to_string(),
            window.to_string(),
        ));
    }
}

fn codex_selection_from_plan(plan: &LaunchPlan) -> Result<Option<ModelSpec>, BridgeError> {
    if let Some(selection) = plan
        .model_selection
        .as_ref()
        .filter(|selection| selection.provider == ModelProvider::Codex)
    {
        if let Some(model) = selection.wire_model.clone() {
            return Ok(Some(ModelSpec {
                model,
                effort: selection.effort,
            }));
        }
    }
    plan.codex_model
        .as_deref()
        .map(ModelSpec::parse)
        .transpose()
        .map_err(|error| BridgeError::Model(error.to_string()))
}

fn apply_cross_route_overlay(
    env: &mut Vec<(String, String)>,
    bridge: &BridgeHandle,
) -> Result<(), BridgeError> {
    if env.iter().any(|(key, value)| {
        env_key_eq(key, "CLAUDE_CODE_DISABLE_NONESSENTIAL_TRAFFIC")
            && (value.trim() == "1" || value.trim().eq_ignore_ascii_case("true"))
    }) {
        return Err(BridgeError::DiscoveryDisabled);
    }
    env.retain(|(key, _)| {
        ![
            "ANTHROPIC_BASE_URL",
            "ANTHROPIC_AUTH_TOKEN",
            "ANTHROPIC_API_KEY",
            "CLAUDE_CODE_DISABLE_NONESSENTIAL_TRAFFIC",
        ]
        .iter()
        .any(|sensitive| env_key_eq(key, sensitive))
            && !key
                .to_ascii_uppercase()
                .starts_with("ANTHROPIC_CUSTOM_MODEL_OPTION")
    });
    env.push((
        "ANTHROPIC_BASE_URL".to_string(),
        bridge.base_url().to_string(),
    ));
    env.push((
        "ANTHROPIC_AUTH_TOKEN".to_string(),
        bridge.bearer_token().to_string(),
    ));
    // Claude Code 2.1.223+ discovers every provider-scoped row from the
    // bridge. Its context override is process-wide, so the catalog must prove
    // that every switchable Codex row has one common real ceiling.
    let context_tokens = crate::provider_catalog::common_claude_context_tokens(
        ModelProvider::Codex,
    )
    .ok_or_else(|| {
        BridgeError::Model(
            "Codex Claude-discovery models need one explicit common context-token ceiling"
                .to_string(),
        )
    })?;
    set_env(env, "CLAUDE_CODE_ENABLE_GATEWAY_MODEL_DISCOVERY", "1");
    set_env(
        env,
        "CLAUDE_CODE_MAX_CONTEXT_TOKENS",
        &context_tokens.to_string(),
    );
    push_default(env, "API_TIMEOUT_MS", DEFAULT_API_TIMEOUT_MS);
    Ok(())
}

fn apply_unified_overlay(
    env: &mut Vec<(String, String)>,
    bridge: &BridgeHandle,
) -> Result<(), BridgeError> {
    if env.iter().any(|(key, value)| {
        env_key_eq(key, "CLAUDE_CODE_DISABLE_NONESSENTIAL_TRAFFIC")
            && (value.trim() == "1" || value.trim().eq_ignore_ascii_case("true"))
    }) {
        return Err(BridgeError::DiscoveryDisabled);
    }
    // Replace only the base URL. Unlike direct DeepSeek and Codex routes, the
    // Claude credential stays untouched so saved claude.ai OAuth/API-key auth
    // reaches the native Claude upstream through this gateway.
    env.retain(|(key, _)| !env_key_eq(key, "ANTHROPIC_BASE_URL"));
    env.push((
        "ANTHROPIC_BASE_URL".to_string(),
        bridge.base_url().to_string(),
    ));
    let custom = env
        .iter()
        .find(|(key, _)| env_key_eq(key, "ANTHROPIC_CUSTOM_HEADERS"))
        .map(|(_, value)| value.trim().to_string())
        .filter(|value| !value.is_empty());
    env.retain(|(key, _)| !env_key_eq(key, "ANTHROPIC_CUSTOM_HEADERS"));
    let gateway_header = format!("{UNIFIED_GATEWAY_TOKEN_HEADER}: {}", bridge.bearer_token());
    let custom_headers = custom
        .map(|headers| format!("{gateway_header}\n{headers}"))
        .unwrap_or(gateway_header);
    env.push(("ANTHROPIC_CUSTOM_HEADERS".to_string(), custom_headers));
    set_env(env, "CLAUDE_CODE_ENABLE_GATEWAY_MODEL_DISCOVERY", "1");
    set_env(env, "CLUD_GATEWAY_TOKEN", bridge.bearer_token());
    push_default(env, "API_TIMEOUT_MS", DEFAULT_API_TIMEOUT_MS);
    Ok(())
}

fn set_env(env: &mut Vec<(String, String)>, key: &str, value: &str) {
    env.retain(|(candidate, _)| !env_key_eq(candidate, key));
    env.push((key.to_string(), value.to_string()));
}

fn context_lifecycle_settings(
    bridge: &BridgeHandle,
    header_name: &str,
    header_value: &str,
    allowed_env: &str,
) -> serde_json::Value {
    let compact_url = format!("{}/_clud/context/compact", bridge.base_url());
    let compact_finished_url = format!("{}/_clud/context/compact-finished", bridge.base_url());
    let clear_url = format!("{}/_clud/context/clear", bridge.base_url());
    let hook = |url: String| {
        let mut headers = serde_json::Map::new();
        headers.insert(
            header_name.to_string(),
            serde_json::Value::String(header_value.to_string()),
        );
        serde_json::json!({
            "type": "http",
            "url": url,
            "headers": headers,
            "allowedEnvVars": [allowed_env]
        })
    };
    serde_json::json!({
        "hooks": {
            "PreCompact": [{
                "matcher": "manual|auto",
                "hooks": [hook(compact_url)]
            }],
            "SessionStart": [
                {
                    "matcher": "clear",
                    "hooks": [hook(clear_url)]
                },
                {
                    "matcher": "compact",
                    "hooks": [hook(compact_finished_url)]
                }
            ]
        }
    })
}

fn merged_context_lifecycle_settings(
    plan: &LaunchPlan,
    bridge: &BridgeHandle,
) -> Result<ClaudeSettings, BridgeError> {
    merged_context_lifecycle_settings_with(
        plan,
        bridge,
        "Authorization",
        "Bearer $ANTHROPIC_AUTH_TOKEN",
        "ANTHROPIC_AUTH_TOKEN",
    )
}

fn merged_unified_context_lifecycle_settings(
    plan: &LaunchPlan,
    bridge: &BridgeHandle,
) -> Result<ClaudeSettings, BridgeError> {
    merged_context_lifecycle_settings_with(
        plan,
        bridge,
        UNIFIED_GATEWAY_TOKEN_HEADER,
        "$CLUD_GATEWAY_TOKEN",
        "CLUD_GATEWAY_TOKEN",
    )
}

fn merged_context_lifecycle_settings_with(
    plan: &LaunchPlan,
    bridge: &BridgeHandle,
    header_name: &str,
    header_value: &str,
    allowed_env: &str,
) -> Result<ClaudeSettings, BridgeError> {
    let mut settings = context_lifecycle_settings(bridge, header_name, header_value, allowed_env);
    if let Some(fragment) = declared_hook_fragment(plan) {
        crate::clud_hooks_compile::merge_hook_settings(&mut settings, &fragment)
            .map_err(BridgeError::Settings)?;
    }
    compose_launch_settings(plan, settings)
}

/// Directories the user granted this session that no hook payload mentions.
///
/// `--add-dir` reaches the harness as passthrough argv, and
/// `permissions.additionalDirectories` lives in a settings file the hook has
/// no reason to read. Both widen what the session may touch, so containment
/// has to know about them — and the only place that knows is the launch.
fn harvested_roots(plan: &LaunchPlan, repo_root: &Path) -> Vec<PathBuf> {
    let mut found: Vec<PathBuf> = add_dir_arguments(&plan.command)
        .into_iter()
        .map(|raw| resolve_against_cwd(&raw, plan.cwd.as_deref()))
        .collect();
    found.extend(additional_directories(repo_root));
    found
}

/// Every `--add-dir` value in `command`, in both spellings.
///
/// The flag takes one or more directories, so values are consumed until the
/// next flag. Scanning stops at `--`, past which the tokens belong to whatever
/// the user is invoking rather than to the harness.
fn add_dir_arguments(command: &[String]) -> Vec<String> {
    let mut found = Vec::new();
    let mut index = 1;
    while index < command.len() {
        let argument = &command[index];
        if argument == "--" {
            break;
        }
        if let Some(value) = argument.strip_prefix("--add-dir=") {
            if !value.is_empty() {
                found.push(value.to_string());
            }
            index += 1;
            continue;
        }
        if argument == "--add-dir" {
            index += 1;
            while index < command.len() {
                let value = &command[index];
                if value.starts_with('-') || value == "--" {
                    break;
                }
                found.push(value.clone());
                index += 1;
            }
            continue;
        }
        index += 1;
    }
    found
}

/// `permissions.additionalDirectories` from the repo's own Claude settings.
///
/// Read directly rather than through `hook_health`, which parses only hook
/// entries. Both the shared and the gitignored local file count, since either
/// can widen the session.
fn additional_directories(repo_root: &Path) -> Vec<PathBuf> {
    let mut found = Vec::new();
    for name in ["settings.json", "settings.local.json"] {
        let path = repo_root.join(".claude").join(name);
        let Ok(text) = std::fs::read_to_string(&path) else {
            continue;
        };
        let Ok(document) = serde_json::from_str::<serde_json::Value>(&text) else {
            continue;
        };
        let entries = document
            .get("permissions")
            .and_then(|permissions| permissions.get("additionalDirectories"))
            .and_then(serde_json::Value::as_array);
        for entry in entries.into_iter().flatten() {
            if let Some(raw) = entry.as_str().map(str::trim).filter(|raw| !raw.is_empty()) {
                found.push(resolve_against(repo_root, raw));
            }
        }
    }
    found
}

fn resolve_against_cwd(raw: &str, cwd: Option<&str>) -> PathBuf {
    let base = cwd.map_or_else(
        || std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")),
        PathBuf::from,
    );
    resolve_against(&base, raw)
}

fn resolve_against(base: &Path, raw: &str) -> PathBuf {
    let candidate = Path::new(raw);
    if candidate.is_absolute() {
        candidate.to_path_buf()
    } else {
        base.join(candidate)
    }
}

/// The registry this session's hooks should see, encoded for the child.
///
/// Harvested directories are registered as `extern`: the parent's own
/// project guards have no more business running against a granted sibling
/// directory than against a checkout clud cloned, and misfiring there is the
/// #841 ENOENT class. They differ from `.extern-repos/` clones in *trust* —
/// the user named these at launch — which Phase 4 has to distinguish when it
/// gates running a foreign repo's own hooks.
fn hook_roots_env_value(plan: &LaunchPlan) -> Option<(String, String)> {
    let cwd = plan
        .cwd
        .as_deref()
        .map_or_else(|| PathBuf::from("."), PathBuf::from);
    let repo_root = crate::block_bad_cmd::nearest_repo_root_public(&cwd)?;
    let harvested = harvested_roots(plan, &repo_root);
    if harvested.is_empty() {
        // Nothing the hook cannot work out for itself; leave the env alone.
        return None;
    }
    let encoded: Vec<serde_json::Value> = harvested
        .iter()
        .map(|path| {
            serde_json::json!({
                "kind": crate::clud_hook_roots::RootKind::Extern.as_str(),
                "path": path.to_string_lossy(),
            })
        })
        .collect();
    Some((
        crate::clud_hook_roots::HOOK_ROOTS_ENV.to_string(),
        serde_json::Value::Array(encoded).to_string(),
    ))
}

/// Compose `--settings` for a launch with no bridge, when the repo declares
/// hooks and the frontend can accept them.
///
/// Claude only: codex has no argument surface for hooks — `-c` overrides
/// `config.toml` values and hooks live in a separate `hooks.json` with no
/// flag pointing at an alternate one. Codex keeps the PreToolUse coverage its
/// already-installed `clud-cmd-scan` line gives it.
fn declared_hooks_settings(plan: &LaunchPlan) -> Result<Option<ClaudeSettings>, BridgeError> {
    if plan.effective_harness() != Backend::Claude {
        return Ok(None);
    }
    let Some(fragment) = declared_hook_fragment(plan) else {
        return Ok(None);
    };
    compose_launch_settings(plan, fragment).map(Some)
}

/// The registration for whatever the repo declares in `.clud/hooks.json`, or
/// `None` when it declares nothing (#967 Phase 2b).
///
/// `None` is the signal to leave the launch alone entirely: a repo that has
/// not opted in should see the argv it saw before this feature existed.
fn declared_hook_fragment(plan: &LaunchPlan) -> Option<serde_json::Value> {
    let cwd = plan
        .cwd
        .as_deref()
        .map_or_else(|| std::path::PathBuf::from("."), std::path::PathBuf::from);
    let repo_root = crate::block_bad_cmd::nearest_repo_root_public(&cwd)?;
    let hooks = crate::clud_hooks::discover(&repo_root)?;
    // Phase 5: clud's own `CwdChanged` backstop line rides on frontend
    // support, probed once per launch against the resolved backend binary.
    // Every consumer of the fragment is a Claude launch (the bridge wraps
    // Claude; codex has no argument surface for hooks), so probe only there —
    // a failed probe degrades silently to no line (DD-064).
    let cwd_changed_supported = plan.effective_harness() == Backend::Claude
        && plan
            .command
            .first()
            .map(|binary| {
                crate::backend_bootstrap::probe_claude_cwd_changed_support(binary.as_ref())
            })
            .unwrap_or(false);
    crate::clud_hooks_compile::claude_settings_fragment(&hooks, cwd_changed_supported)
}

/// Hand `generated` to Claude as a launch-scoped `--settings` source.
///
/// When the user supplied their own `--settings`, clud merges into *their*
/// document and replaces the argument, because Claude accepts only one such
/// source and whichever came second would otherwise shadow the other.
fn compose_launch_settings(
    plan: &LaunchPlan,
    settings: serde_json::Value,
) -> Result<ClaudeSettings, BridgeError> {
    let Some(user_argument) = user_settings_argument(&plan.command)? else {
        return write_launch_scoped_settings(settings, false);
    };
    let mut user_settings = read_user_settings(user_argument, plan.cwd.as_deref())?;
    let user_root = user_settings.as_object_mut().ok_or_else(|| {
        BridgeError::Settings("Claude --settings must contain a JSON object".to_string())
    })?;
    let user_hooks = user_root
        .entry("hooks")
        .or_insert_with(|| serde_json::json!({}))
        .as_object_mut()
        .ok_or_else(|| {
            BridgeError::Settings("Claude --settings hooks must be a JSON object".to_string())
        })?;
    let mut settings = settings;
    let generated_hooks = settings["hooks"]
        .as_object_mut()
        .expect("generated hooks are an object");
    for (event, generated_entries) in generated_hooks {
        let user_entries = user_hooks
            .entry(event)
            .or_insert_with(|| serde_json::json!([]))
            .as_array_mut()
            .ok_or_else(|| {
                BridgeError::Settings(format!(
                    "Claude --settings hook event {event} must be a JSON array"
                ))
            })?;
        user_entries.extend(
            generated_entries
                .as_array()
                .expect("generated hook event is an array")
                .iter()
                .cloned(),
        );
    }
    write_launch_scoped_settings(user_settings, true)
}

fn write_launch_scoped_settings(
    settings: serde_json::Value,
    replaces_user_argument: bool,
) -> Result<ClaudeSettings, BridgeError> {
    let mut file = tempfile::Builder::new()
        .prefix("clud-claude-settings-")
        .suffix(".json")
        .tempfile()
        .map_err(|error| {
            BridgeError::Settings(format!(
                "failed to create launch-scoped Claude settings: {error}"
            ))
        })?;
    file.write_all(settings.to_string().as_bytes())
        .map_err(|error| {
            BridgeError::Settings(format!(
                "failed to write launch-scoped Claude settings: {error}"
            ))
        })?;
    let value = file.path().to_string_lossy().into_owned();
    Ok(ClaudeSettings {
        value,
        replaces_user_argument,
        _temp_file: file,
    })
}

fn user_settings_argument(command: &[String]) -> Result<Option<&str>, BridgeError> {
    let mut found = None;
    let mut index = 1;
    while index < command.len() {
        let argument = &command[index];
        if argument == "--" {
            break;
        }
        let value = if argument == "--settings" {
            index += 1;
            Some(
                command
                    .get(index)
                    .ok_or_else(|| {
                        BridgeError::Settings("Claude --settings is missing its value".to_string())
                    })?
                    .as_str(),
            )
        } else {
            argument.strip_prefix("--settings=")
        };
        if let Some(value) = value {
            if found.replace(value).is_some() {
                return Err(BridgeError::Settings(
                    "Claude --settings may only be supplied once".to_string(),
                ));
            }
        }
        index += 1;
    }
    Ok(found)
}

fn read_user_settings(argument: &str, cwd: Option<&str>) -> Result<serde_json::Value, BridgeError> {
    let contents = if argument.trim_start().starts_with('{') {
        argument.to_string()
    } else {
        let supplied = PathBuf::from(argument);
        let path = if supplied.is_absolute() {
            supplied
        } else {
            PathBuf::from(cwd.unwrap_or(".")).join(supplied)
        };
        std::fs::read_to_string(&path).map_err(|error| {
            BridgeError::Settings(format!(
                "failed to read Claude --settings file {}: {error}",
                path.display()
            ))
        })?
    };
    serde_json::from_str(&contents).map_err(|error| {
        BridgeError::Settings(format!("failed to parse Claude --settings JSON: {error}"))
    })
}

fn remove_user_settings_argument(command: &mut Vec<String>) {
    let mut index = 1;
    while index < command.len() {
        if command[index] == "--" {
            return;
        }
        if command[index] == "--settings" {
            command.remove(index);
            if index < command.len() {
                command.remove(index);
            }
            return;
        }
        if command[index].starts_with("--settings=") {
            command.remove(index);
            return;
        }
        index += 1;
    }
}

fn push_default(env: &mut Vec<(String, String)>, key: &str, value: &str) {
    if !env.iter().any(|(candidate, _)| env_key_eq(candidate, key)) {
        env.push((key.to_string(), value.to_string()));
    }
}

fn env_key_eq(left: &str, right: &str) -> bool {
    if cfg!(windows) {
        left.eq_ignore_ascii_case(right)
    } else {
        left == right
    }
}

struct NativeSubprocessAdapter {
    capture_stdout: bool,
    creation_flags: Option<u32>,
}

impl SpawnAdapter<ManagedSubprocess> for NativeSubprocessAdapter {
    type Error = String;

    fn spawn(
        &self,
        mode: SpawnMode,
        command: Vec<String>,
        cwd: Option<String>,
        env: Vec<(String, String)>,
    ) -> Result<ManagedSubprocess, Self::Error> {
        debug_assert_eq!(mode, SpawnMode::Subprocess);
        ManagedSubprocess::start(
            command,
            cwd.map(PathBuf::from),
            env,
            self.capture_stdout,
            self.creation_flags,
        )
    }
}

struct NativePtyAdapter {
    rows: u16,
    cols: u16,
}

impl SpawnAdapter<NativePtyProcess> for NativePtyAdapter {
    type Error = running_process::pty::PtyError;

    fn spawn(
        &self,
        mode: SpawnMode,
        command: Vec<String>,
        cwd: Option<String>,
        env: Vec<(String, String)>,
    ) -> Result<NativePtyProcess, Self::Error> {
        debug_assert_eq!(mode, SpawnMode::Pty);
        NativePtyProcess::new(command, cwd, Some(env), self.rows, self.cols, None)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::backend::{Backend, HarnessSelection, LaunchMode, ModelProvider, PreferenceSource};
    use crate::command::LaunchPlan;
    use crate::graphics::GraphicsConfig;
    use std::cell::RefCell;
    use std::net::TcpListener;

    /// Injectable fake so routing tests never touch the host's real native
    /// credential vault.
    struct FakeSecretStore(Option<String>);

    impl crate::provider_auth::SecretStore for FakeSecretStore {
        fn get(&self) -> Result<Option<String>, crate::provider_auth::SecretStoreError> {
            Ok(self.0.clone())
        }
        fn set(&self, _secret: &str) -> Result<(), crate::provider_auth::SecretStoreError> {
            unreachable!("routing tests never write to the store")
        }
        fn delete(&self) -> Result<(), crate::provider_auth::SecretStoreError> {
            unreachable!("routing tests never write to the store")
        }
    }

    fn plan(provider: ModelProvider, harness: Backend) -> LaunchPlan {
        LaunchPlan {
            command: vec![harness.executable_name().to_string()],
            iterations: 1,
            backend: harness,
            routing_mode: crate::backend::RoutingMode::Direct,
            model_provider: Some(provider),
            requested_harness: Some(match harness {
                Backend::Claude => HarnessSelection::Claude,
                Backend::Codex => HarnessSelection::Codex,
                Backend::DeepSeek => HarnessSelection::DeepSeek,
            }),
            effective_harness: Some(harness),
            provider_source: Some(PreferenceSource::Cli),
            harness_source: Some(PreferenceSource::Cli),
            launch_mode: LaunchMode::Subprocess,
            cwd: None,
            graphics: GraphicsConfig::default(),
            repeat_schedule: None,
            task_summary: None,
            loop_markers: None,
            stream_json_progress: false,
            codex_model: None,
            model_selection: None,
            failover: None,
            failover_allow_metered: false,
        }
    }

    #[test]
    fn native_deepseek_harness_needs_no_clud_bridge_or_vault() {
        let runtime = ForegroundRuntime::start(
            &plan(ModelProvider::DeepSeek, Backend::DeepSeek),
            vec![("UNCHANGED".to_string(), "yes".to_string())],
        )
        .unwrap();
        assert!(runtime.bridge.is_none());
        assert_eq!(
            runtime.env(),
            &[("UNCHANGED".to_string(), "yes".to_string())]
        );
    }

    fn lookup<'a>(env: &'a [(String, String)], key: &str) -> Option<&'a str> {
        env.iter()
            .find(|(candidate, _)| candidate == key)
            .map(|(_, value)| value.as_str())
    }

    #[test]
    fn unified_overlay_preserves_claude_credentials_and_enables_discovery() {
        let mut route = plan(ModelProvider::Claude, Backend::Claude);
        route.routing_mode = RoutingMode::Unified;
        let base = vec![
            (
                "ANTHROPIC_AUTH_TOKEN".to_string(),
                "claude-oauth".to_string(),
            ),
            ("CLAUDE_CODE_EFFORT_LEVEL".to_string(), "xhigh".to_string()),
            (
                "ANTHROPIC_CUSTOM_HEADERS".to_string(),
                "X-Existing: retained".to_string(),
            ),
        ];
        let runtime = ForegroundRuntime::start_with_secret_store(
            &route,
            base.clone(),
            &FakeSecretStore(Some("deepseek-secret".to_string())),
        )
        .unwrap();
        let env = runtime.env();
        assert!(runtime.has_bridge());
        assert_eq!(lookup(env, "ANTHROPIC_AUTH_TOKEN"), Some("claude-oauth"));
        assert_eq!(lookup(env, "ANTHROPIC_BASE_URL"), runtime.base_url());
        assert_eq!(
            lookup(env, "CLAUDE_CODE_ENABLE_GATEWAY_MODEL_DISCOVERY"),
            Some("1")
        );
        let headers = lookup(env, "ANTHROPIC_CUSTOM_HEADERS").unwrap();
        assert!(headers.contains("X-Existing: retained"));
        assert!(headers.contains(UNIFIED_GATEWAY_TOKEN_HEADER));
        assert!(headers.contains(runtime.bearer_token().unwrap()));
        assert!(headers.starts_with(UNIFIED_GATEWAY_TOKEN_HEADER));
        assert_eq!(lookup(env, "CLUD_GATEWAY_TOKEN"), runtime.bearer_token());
        assert_eq!(lookup(env, "CLAUDE_CODE_EFFORT_LEVEL"), Some("xhigh"));
        assert_eq!(lookup(&base, "ANTHROPIC_AUTH_TOKEN"), Some("claude-oauth"));
    }

    #[test]
    fn unified_overlay_does_not_inject_a_global_effort_default() {
        let mut route = plan(ModelProvider::Claude, Backend::Claude);
        route.routing_mode = RoutingMode::Unified;
        let runtime = ForegroundRuntime::start_with_secret_store(
            &route,
            Vec::new(),
            &FakeSecretStore(Some("deepseek-secret".to_string())),
        )
        .unwrap();
        assert_eq!(lookup(runtime.env(), "CLAUDE_CODE_EFFORT_LEVEL"), None);
    }

    /// DD-059: the direct Anthropic-compat overlay neither scrubs an ambient
    /// `CLAUDE_CODE_EFFORT_LEVEL` nor injects its own pin, so `/effort` stays
    /// the live session control -- mirroring the unified overlay's rule. The
    /// old code overwrote the ambient value with the selection's effort (or
    /// a `max` fallback when none was selected).
    #[test]
    fn anthropic_compat_overlay_preserves_ambient_effort_and_injects_no_default() {
        let mut route = plan(ModelProvider::DeepSeek, Backend::Claude);
        // A legacy `@high` selection must not re-pin the user's ambient value.
        route.model_selection = crate::provider_catalog::resolve(
            Some(ModelProvider::DeepSeek),
            Some("deepseek-v4-pro@high"),
            None,
            None,
        )
        .unwrap();
        let runtime = ForegroundRuntime::start_with_secret_store(
            &route,
            vec![("CLAUDE_CODE_EFFORT_LEVEL".to_string(), "xhigh".to_string())],
            &FakeSecretStore(Some("deepseek-secret".to_string())),
        )
        .unwrap();
        assert_eq!(
            lookup(runtime.env(), "CLAUDE_CODE_EFFORT_LEVEL"),
            Some("xhigh")
        );
        assert!(!runtime.has_bridge());

        let empty = ForegroundRuntime::start_with_secret_store(
            &route,
            Vec::new(),
            &FakeSecretStore(Some("deepseek-secret".to_string())),
        )
        .unwrap();
        assert_eq!(lookup(empty.env(), "CLAUDE_CODE_EFFORT_LEVEL"), None);
    }

    #[test]
    fn unified_missing_provider_notices_are_sanitized_and_actionable() {
        let notices = unified_startup_notices(false, false);
        assert_eq!(notices.len(), 2);
        assert!(notices[0].contains("clud auth login codex"));
        assert!(notices[1].contains("clud auth login deepseek"));
        assert!(!notices.join(" ").to_ascii_lowercase().contains("secret"));
        assert!(unified_startup_notices(true, true).is_empty());
    }

    #[test]
    fn unified_mode_refuses_disabled_model_discovery() {
        let mut route = plan(ModelProvider::Claude, Backend::Claude);
        route.routing_mode = RoutingMode::Unified;
        let error = ForegroundRuntime::start_with_secret_store(
            &route,
            vec![(
                "CLAUDE_CODE_DISABLE_NONESSENTIAL_TRAFFIC".to_string(),
                "1".to_string(),
            )],
            &FakeSecretStore(None),
        )
        .unwrap_err();
        assert!(matches!(error, BridgeError::DiscoveryDisabled));
    }

    #[test]
    fn cross_route_overlay_is_child_local_secret_safe_and_honors_defaults() {
        let base = vec![
            ("UNCHANGED".to_string(), "yes".to_string()),
            ("ANTHROPIC_API_KEY".to_string(), "ambient-key".to_string()),
            ("API_TIMEOUT_MS".to_string(), "custom-timeout".to_string()),
        ];
        let runtime =
            ForegroundRuntime::start(&plan(ModelProvider::Codex, Backend::Claude), base.clone())
                .unwrap();
        let env = runtime.env();
        assert_eq!(lookup(env, "UNCHANGED"), Some("yes"));
        assert_eq!(lookup(env, "ANTHROPIC_API_KEY"), None);
        assert_eq!(
            lookup(env, "ANTHROPIC_BASE_URL"),
            Some(runtime.base_url().unwrap())
        );
        assert_eq!(
            lookup(env, "ANTHROPIC_AUTH_TOKEN"),
            Some(runtime.bearer_token().unwrap())
        );
        assert_eq!(lookup(env, "API_TIMEOUT_MS"), Some("custom-timeout"));
        assert_eq!(
            lookup(env, "CLAUDE_CODE_DISABLE_NONESSENTIAL_TRAFFIC"),
            None
        );
        assert_eq!(
            lookup(env, "CLAUDE_CODE_ENABLE_GATEWAY_MODEL_DISCOVERY"),
            Some("1")
        );
        assert_eq!(
            lookup(env, "CLAUDE_CODE_MAX_CONTEXT_TOKENS"),
            Some("1050000")
        );
        assert_eq!(lookup(&base, "ANTHROPIC_API_KEY"), Some("ambient-key"));
    }

    /// The discovery catalog replaces the old scalar custom-row extension.
    #[test]
    fn direct_codex_discovery_scrubs_the_legacy_custom_picker_row() {
        let runtime = ForegroundRuntime::start(
            &plan(ModelProvider::Codex, Backend::Claude),
            vec![(
                "ANTHROPIC_CUSTOM_MODEL_OPTION".to_string(),
                "stale-row".to_string(),
            )],
        )
        .unwrap();
        assert_eq!(lookup(runtime.env(), "ANTHROPIC_CUSTOM_MODEL_OPTION"), None);
    }

    /// Discovery cannot work while the harness's network kill switch is set.
    #[test]
    fn direct_codex_refuses_disabled_model_discovery() {
        let error = ForegroundRuntime::start(
            &plan(ModelProvider::Codex, Backend::Claude),
            vec![(
                "CLAUDE_CODE_DISABLE_NONESSENTIAL_TRAFFIC".to_string(),
                "1".to_string(),
            )],
        )
        .unwrap_err();
        assert!(matches!(error, BridgeError::DiscoveryDisabled));
    }

    /// New typed plans no longer depend on the compatibility selector field.
    #[test]
    fn normalized_selection_supersedes_the_legacy_plan_field() {
        let mut route = plan(ModelProvider::Codex, Backend::Claude);
        route.codex_model = Some("tera".to_string());
        route.model_selection = crate::provider_catalog::resolve(
            Some(ModelProvider::Codex),
            Some("luna"),
            Some("high"),
            None,
        )
        .unwrap();
        ForegroundRuntime::start(&route, Vec::new()).unwrap();
    }

    /// A bad selection fails the launch, not the first turn: by the time a
    /// request is in flight the user has waited, and the message arrives
    /// wrapped in the harness's own API-error framing.
    #[test]
    fn an_unparseable_selection_fails_the_launch_with_the_valid_names() {
        let mut plan = plan(ModelProvider::Codex, Backend::Claude);
        plan.codex_model = Some("tera".to_string());
        let error = ForegroundRuntime::start(&plan, Vec::new()).unwrap_err();
        let rendered = error.to_string();
        assert!(rendered.contains("tera"), "{rendered}");
        assert!(rendered.contains("terra"), "{rendered}");
    }

    fn deepseek_descriptor() -> &'static crate::provider_registry::AnthropicCompatProvider {
        crate::provider_registry::descriptor_for(ModelProvider::DeepSeek)
            .expect("DeepSeek must have an Anthropic-compat descriptor")
    }

    /// GOLDEN (issue #937 Phase 2 Lane 2A): the frozen pre-refactor baseline
    /// for the default (no selection) case, captured against
    /// `apply_deepseek_overlay` before it became
    /// `apply_anthropic_compat_overlay`, updated for exactly one documented
    /// delta -- the new `ANTHROPIC_DEFAULT_FABLE_MODEL` pin (#936
    /// "Generalization" -> 1d). Every other pair is byte-identical to the
    /// pre-refactor baseline that was confirmed green before this function
    /// was touched. Second delta (DD-059): the `CLAUDE_CODE_EFFORT_LEVEL`
    /// pin is gone -- effort travels on the harness's `--effort` flag and the
    /// overlay neither injects nor scrubs it.
    #[test]
    fn golden_anthropic_compat_overlay_default_selection() {
        let mut env = Vec::new();
        apply_anthropic_compat_overlay(&mut env, "ds-golden-secret", deepseek_descriptor(), None);
        assert_eq!(lookup(&env, "CLAUDE_CODE_EFFORT_LEVEL"), None);
        let mut pairs = env.clone();
        pairs.sort();
        assert_eq!(
            pairs,
            vec![
                (
                    "ANTHROPIC_AUTH_TOKEN".to_string(),
                    "ds-golden-secret".to_string()
                ),
                (
                    "ANTHROPIC_BASE_URL".to_string(),
                    "https://api.deepseek.com/anthropic".to_string()
                ),
                (
                    "ANTHROPIC_DEFAULT_FABLE_MODEL".to_string(),
                    "deepseek-v4-pro[1m]".to_string()
                ),
                (
                    "ANTHROPIC_DEFAULT_HAIKU_MODEL".to_string(),
                    "deepseek-v4-flash".to_string()
                ),
                (
                    "ANTHROPIC_DEFAULT_OPUS_MODEL".to_string(),
                    "deepseek-v4-pro[1m]".to_string()
                ),
                (
                    "ANTHROPIC_DEFAULT_SONNET_MODEL".to_string(),
                    "deepseek-v4-pro[1m]".to_string()
                ),
                (
                    "ANTHROPIC_MODEL".to_string(),
                    "deepseek-v4-pro[1m]".to_string()
                ),
                (
                    "CLAUDE_CODE_AUTO_COMPACT_WINDOW".to_string(),
                    "786432".to_string()
                ),
                (
                    "CLAUDE_CODE_SUBAGENT_MODEL".to_string(),
                    "deepseek-v4-flash".to_string()
                ),
            ]
        );
    }

    /// GOLDEN: a selection whose wire model has no `[1m]` suffix, so no
    /// `CLAUDE_CODE_AUTO_COMPACT_WINDOW` is set. Same deltas as above: only
    /// the new `ANTHROPIC_DEFAULT_FABLE_MODEL` pin is added relative to the
    /// confirmed pre-refactor baseline, and no `CLAUDE_CODE_EFFORT_LEVEL`
    /// pin is emitted even for an explicitly selected effort (DD-059).
    #[test]
    fn golden_anthropic_compat_overlay_auto_context_selection_has_no_compact_window() {
        let selection = crate::provider_catalog::resolve(
            Some(ModelProvider::DeepSeek),
            Some("deepseek-v4-pro"),
            Some("high"),
            Some("auto"),
        )
        .unwrap()
        .unwrap();
        assert_eq!(selection.wire_model.as_deref(), Some("deepseek-v4-pro"));
        let mut env = Vec::new();
        apply_anthropic_compat_overlay(
            &mut env,
            "ds-golden-secret",
            deepseek_descriptor(),
            Some(&selection),
        );
        let mut pairs = env.clone();
        pairs.sort();
        assert_eq!(
            pairs,
            vec![
                (
                    "ANTHROPIC_AUTH_TOKEN".to_string(),
                    "ds-golden-secret".to_string()
                ),
                (
                    "ANTHROPIC_BASE_URL".to_string(),
                    "https://api.deepseek.com/anthropic".to_string()
                ),
                (
                    "ANTHROPIC_DEFAULT_FABLE_MODEL".to_string(),
                    "deepseek-v4-pro".to_string()
                ),
                (
                    "ANTHROPIC_DEFAULT_HAIKU_MODEL".to_string(),
                    "deepseek-v4-flash".to_string()
                ),
                (
                    "ANTHROPIC_DEFAULT_OPUS_MODEL".to_string(),
                    "deepseek-v4-pro".to_string()
                ),
                (
                    "ANTHROPIC_DEFAULT_SONNET_MODEL".to_string(),
                    "deepseek-v4-pro".to_string()
                ),
                ("ANTHROPIC_MODEL".to_string(), "deepseek-v4-pro".to_string()),
                (
                    "CLAUDE_CODE_SUBAGENT_MODEL".to_string(),
                    "deepseek-v4-flash".to_string()
                ),
            ]
        );
        assert_eq!(lookup(&env, "CLAUDE_CODE_EFFORT_LEVEL"), None);
    }

    /// GOLDEN: the second documented delta -- the widened scrub const now
    /// removes `ANTHROPIC_SMALL_FAST_MODEL`, `ANTHROPIC_DEFAULT_FABLE_MODEL`,
    /// and the legacy `*_NAME` forms. Before this phase's refactor, an
    /// identically-shaped test (`frozen_baseline_deepseek_overlay_does_not_yet_scrub_the_widened_keys`,
    /// since replaced by this one) proved these keys survived unscrubbed;
    /// that was the review finding #936/#937 document as the intended
    /// hardening delta.
    #[test]
    fn golden_anthropic_compat_overlay_scrubs_the_widened_keys() {
        let base = vec![
            (
                "ANTHROPIC_SMALL_FAST_MODEL".to_string(),
                "ambient-fast".to_string(),
            ),
            (
                "ANTHROPIC_DEFAULT_FABLE_MODEL".to_string(),
                "ambient-fable".to_string(),
            ),
            (
                "ANTHROPIC_MODEL_NAME".to_string(),
                "ambient-model-name".to_string(),
            ),
            (
                "ANTHROPIC_DEFAULT_OPUS_MODEL_NAME".to_string(),
                "ambient-opus-name".to_string(),
            ),
            (
                "ANTHROPIC_DEFAULT_SONNET_MODEL_NAME".to_string(),
                "ambient-sonnet-name".to_string(),
            ),
            (
                "ANTHROPIC_DEFAULT_HAIKU_MODEL_NAME".to_string(),
                "ambient-haiku-name".to_string(),
            ),
            (
                "ANTHROPIC_DEFAULT_FABLE_MODEL_NAME".to_string(),
                "ambient-fable-name".to_string(),
            ),
        ];
        let mut env = base.clone();
        apply_anthropic_compat_overlay(&mut env, "ds-golden-secret", deepseek_descriptor(), None);
        for (key, _) in &base {
            assert_eq!(
                lookup(&env, key),
                if key == "ANTHROPIC_DEFAULT_FABLE_MODEL" {
                    // This slot is scrubbed AND re-set by the overlay itself
                    // (the new FABLE pin), so its post-overlay value is the
                    // overlay's own model, not `None` and not the ambient value.
                    Some("deepseek-v4-pro[1m]")
                } else {
                    None
                },
                "{key} must no longer carry its ambient value after the widened scrub"
            );
        }
    }

    #[test]
    fn deepseek_overlay_replaces_conflicting_profile_values_without_parent_mutation() {
        let base = vec![
            ("anthropic_api_key".to_string(), "ambient-key".to_string()),
            ("ANTHROPIC_MODEL".to_string(), "ambient-model".to_string()),
            (
                "ANTHROPIC_CUSTOM_MODEL_OPTION".to_string(),
                "ambient-picker".to_string(),
            ),
            ("UNCHANGED".to_string(), "yes".to_string()),
        ];
        let mut child = base.clone();
        apply_anthropic_compat_overlay(&mut child, "ds-test-secret", deepseek_descriptor(), None);

        assert_eq!(lookup(&child, "UNCHANGED"), Some("yes"));
        assert_eq!(lookup(&child, "anthropic_api_key"), None);
        assert_eq!(lookup(&child, "ANTHROPIC_CUSTOM_MODEL_OPTION"), None);
        assert_eq!(
            lookup(&child, "ANTHROPIC_BASE_URL"),
            Some("https://api.deepseek.com/anthropic")
        );
        assert_eq!(
            lookup(&child, "ANTHROPIC_AUTH_TOKEN"),
            Some("ds-test-secret")
        );
        assert_eq!(
            lookup(&child, "ANTHROPIC_MODEL"),
            Some("deepseek-v4-pro[1m]")
        );
        assert_eq!(
            lookup(&child, "CLAUDE_CODE_SUBAGENT_MODEL"),
            Some("deepseek-v4-flash")
        );
        assert_eq!(
            lookup(&child, "CLAUDE_CODE_AUTO_COMPACT_WINDOW"),
            Some("786432")
        );
        assert_eq!(lookup(&base, "anthropic_api_key"), Some("ambient-key"));
    }

    #[test]
    fn deepseek_overlay_applies_explicit_model_context_and_effort() {
        let selection = crate::provider_catalog::resolve(
            Some(ModelProvider::DeepSeek),
            Some("deepseek-v4-pro"),
            Some("high"),
            Some("auto"),
        )
        .unwrap()
        .unwrap();
        let mut env = Vec::new();
        apply_anthropic_compat_overlay(
            &mut env,
            "ds-test-secret",
            deepseek_descriptor(),
            Some(&selection),
        );
        assert_eq!(lookup(&env, "ANTHROPIC_MODEL"), Some("deepseek-v4-pro"));
        assert_eq!(lookup(&env, "CLAUDE_CODE_EFFORT_LEVEL"), None);
        assert_eq!(lookup(&env, "CLAUDE_CODE_AUTO_COMPACT_WINDOW"), None);
    }

    #[test]
    fn native_routes_receive_the_original_environment_byte_for_byte() {
        let base = vec![
            ("ANTHROPIC_BASE_URL".to_string(), "user-url".to_string()),
            ("ANTHROPIC_AUTH_TOKEN".to_string(), "user-token".to_string()),
            ("ANTHROPIC_API_KEY".to_string(), "user-key".to_string()),
        ];
        for route in [
            plan(ModelProvider::Claude, Backend::Claude),
            plan(ModelProvider::Codex, Backend::Codex),
        ] {
            let runtime = ForegroundRuntime::start(&route, base.clone()).unwrap();
            assert_eq!(runtime.env(), base);
            assert!(!runtime.has_bridge());
        }
    }

    /// Issue #880: every route `ForegroundRuntime::start` can resolve to,
    /// exercised through the one dispatch point rather than the overlay
    /// helpers in isolation. DeepSeek must never create a `BridgeHandle` --
    /// its route is the direct child-overlay path, not the loopback bridge.
    #[test]
    fn every_provider_harness_route_gets_exactly_the_expected_bridge_state() {
        let base = vec![("UNRELATED".to_string(), "kept".to_string())];
        let store = FakeSecretStore(Some("ds-routing-secret".to_string()));

        let native_claude = ForegroundRuntime::start_with_secret_store(
            &plan(ModelProvider::Claude, Backend::Claude),
            base.clone(),
            &store,
        )
        .unwrap();
        assert!(!native_claude.has_bridge());
        assert_eq!(native_claude.env(), base);

        let native_codex = ForegroundRuntime::start_with_secret_store(
            &plan(ModelProvider::Codex, Backend::Codex),
            base.clone(),
            &store,
        )
        .unwrap();
        assert!(!native_codex.has_bridge());
        assert_eq!(native_codex.env(), base);

        let codex_bridge = ForegroundRuntime::start_with_secret_store(
            &plan(ModelProvider::Codex, Backend::Claude),
            base.clone(),
            &store,
        )
        .unwrap();
        assert!(codex_bridge.has_bridge());

        let deepseek_direct = ForegroundRuntime::start_with_secret_store(
            &plan(ModelProvider::DeepSeek, Backend::Claude),
            base.clone(),
            &store,
        )
        .unwrap();
        assert!(
            !deepseek_direct.has_bridge(),
            "DeepSeek must route directly, never through BridgeHandle"
        );
        assert_eq!(
            lookup(deepseek_direct.env(), "ANTHROPIC_AUTH_TOKEN"),
            Some("ds-routing-secret")
        );
        assert_eq!(lookup(deepseek_direct.env(), "UNRELATED"), Some("kept"));
    }

    #[test]
    fn deepseek_route_without_a_stored_credential_fails_the_launch() {
        let store = FakeSecretStore(None);
        let error = ForegroundRuntime::start_with_secret_store(
            &plan(ModelProvider::DeepSeek, Backend::Claude),
            Vec::new(),
            &store,
        )
        .unwrap_err();
        assert!(matches!(&error, BridgeError::AnthropicCompatCredentials));
    }

    #[cfg(windows)]
    #[test]
    fn windows_overlay_treats_environment_keys_case_insensitively() {
        let base = vec![
            ("anthropic_api_key".to_string(), "ambient-key".to_string()),
            ("Anthropic_Base_Url".to_string(), "old-url".to_string()),
            ("anthropic_auth_token".to_string(), "old-token".to_string()),
            ("api_timeout_ms".to_string(), "custom-timeout".to_string()),
            (
                "claude_code_disable_nonessential_traffic".to_string(),
                "custom-traffic".to_string(),
            ),
        ];
        let runtime =
            ForegroundRuntime::start(&plan(ModelProvider::Codex, Backend::Claude), base).unwrap();
        let env = runtime.env();
        assert_eq!(lookup(env, "ANTHROPIC_API_KEY"), None);
        assert_eq!(lookup(env, "ANTHROPIC_BASE_URL"), runtime.base_url());
        assert_eq!(lookup(env, "ANTHROPIC_AUTH_TOKEN"), runtime.bearer_token());
        assert_eq!(lookup(env, "api_timeout_ms"), Some("custom-timeout"));
        assert_eq!(
            lookup(env, "claude_code_disable_nonessential_traffic"),
            None
        );
        assert_eq!(
            lookup(env, "CLAUDE_CODE_ENABLE_GATEWAY_MODEL_DISCOVERY"),
            Some("1")
        );
        assert_eq!(
            lookup(env, "CLAUDE_CODE_MAX_CONTEXT_TOKENS"),
            Some("1050000")
        );
        assert_eq!(
            env.iter()
                .filter(|(key, _)| env_key_eq(key, "API_TIMEOUT_MS"))
                .count(),
            1
        );
    }

    type RecordedEnvironment = Vec<(String, String)>;
    type RecordedSpawn = (SpawnMode, Vec<String>, RecordedEnvironment);

    #[derive(Default)]
    struct RecordingAdapter {
        calls: RefCell<Vec<RecordedSpawn>>,
    }

    impl SpawnAdapter<()> for RecordingAdapter {
        type Error = std::io::Error;

        fn spawn(
            &self,
            mode: SpawnMode,
            command: Vec<String>,
            _cwd: Option<String>,
            env: Vec<(String, String)>,
        ) -> std::io::Result<()> {
            self.calls.borrow_mut().push((mode, command, env));
            Ok(())
        }
    }

    #[test]
    fn subprocess_and_pty_adapters_receive_the_same_overlay() {
        let runtime =
            ForegroundRuntime::start(&plan(ModelProvider::Codex, Backend::Claude), Vec::new())
                .unwrap();
        let adapter = RecordingAdapter::default();
        runtime
            .spawn_with(&adapter, SpawnMode::Subprocess, vec!["claude".into()], None)
            .unwrap();
        runtime
            .spawn_with(&adapter, SpawnMode::Pty, vec!["claude".into()], None)
            .unwrap();
        let calls = adapter.calls.borrow();
        assert_eq!(calls.len(), 2);
        assert_eq!(calls[0].0, SpawnMode::Subprocess);
        assert_eq!(calls[1].0, SpawnMode::Pty);
        assert_eq!(calls[0].1, calls[1].1);
        assert_eq!(calls[0].2, calls[1].2);
        assert!(lookup(&calls[0].2, "ANTHROPIC_AUTH_TOKEN").is_some());
    }

    #[test]
    fn deepseek_subprocess_and_pty_receive_the_same_secret_child_overlay() {
        let mut env = vec![("ANTHROPIC_API_KEY".to_string(), "ambient-key".to_string())];
        apply_anthropic_compat_overlay(&mut env, "ds-test-secret", deepseek_descriptor(), None);
        let runtime = ForegroundRuntime {
            env,
            bridge: None,
            claude_settings: None,
            startup_notices: Vec::new(),
        };
        let adapter = RecordingAdapter::default();
        runtime
            .spawn_with(&adapter, SpawnMode::Subprocess, vec!["claude".into()], None)
            .unwrap();
        runtime
            .spawn_with(&adapter, SpawnMode::Pty, vec!["claude".into()], None)
            .unwrap();

        let calls = adapter.calls.borrow();
        assert_eq!(calls.len(), 2);
        assert_eq!(calls[0].2, calls[1].2);
        assert_eq!(
            lookup(&calls[0].2, "ANTHROPIC_AUTH_TOKEN"),
            Some("ds-test-secret")
        );
        assert_eq!(lookup(&calls[0].2, "ANTHROPIC_API_KEY"), None);
        assert!(!runtime.has_bridge());
    }

    #[test]
    fn bridge_spawns_register_authenticated_context_lifecycle_hooks() {
        let runtime =
            ForegroundRuntime::start(&plan(ModelProvider::Codex, Backend::Claude), Vec::new())
                .unwrap();
        let adapter = RecordingAdapter::default();
        runtime
            .spawn_with(
                &adapter,
                SpawnMode::Subprocess,
                vec!["claude".into(), "-p".into(), "hello".into()],
                None,
            )
            .unwrap();

        let calls = adapter.calls.borrow();
        let command = &calls[0].1;
        let settings_index = command
            .iter()
            .position(|argument| argument == "--settings")
            .expect("the bridge launch must register session-local lifecycle hooks");
        let settings_path = PathBuf::from(&command[settings_index + 1]);
        assert!(settings_path.is_absolute());
        let settings: serde_json::Value = serde_json::from_str(
            &std::fs::read_to_string(&settings_path).expect("the settings file must remain live"),
        )
        .expect("--settings must point to valid JSON");
        let base_url = runtime.base_url().unwrap();
        assert_eq!(settings["hooks"]["PreCompact"][0]["matcher"], "manual|auto");
        assert_eq!(
            settings["hooks"]["PreCompact"][0]["hooks"][0]["url"],
            format!("{base_url}/_clud/context/compact")
        );
        assert_eq!(settings["hooks"]["SessionStart"][0]["matcher"], "clear");
        assert_eq!(
            settings["hooks"]["SessionStart"][0]["hooks"][0]["url"],
            format!("{base_url}/_clud/context/clear")
        );
        assert_eq!(settings["hooks"]["SessionStart"][1]["matcher"], "compact");
        assert_eq!(
            settings["hooks"]["SessionStart"][1]["hooks"][0]["url"],
            format!("{base_url}/_clud/context/compact-finished")
        );
        for event in ["PreCompact", "SessionStart"] {
            for entry in settings["hooks"][event].as_array().unwrap() {
                let hook = &entry["hooks"][0];
                assert_eq!(hook["type"], "http");
                assert_eq!(
                    hook["headers"]["Authorization"],
                    "Bearer $ANTHROPIC_AUTH_TOKEN"
                );
                assert_eq!(
                    hook["allowedEnvVars"],
                    serde_json::json!(["ANTHROPIC_AUTH_TOKEN"])
                );
            }
        }
        assert!(
            !command.join(" ").contains(runtime.bearer_token().unwrap()),
            "the launch-scoped bearer must stay in the environment, not argv"
        );
        assert!(
            !command.join(" ").contains(base_url),
            "the launch-private bridge URL must stay out of argv"
        );
    }

    #[test]
    fn bridge_merges_inline_user_settings_with_context_lifecycle_hooks() {
        let mut route = plan(ModelProvider::Codex, Backend::Claude);
        route.command = vec![
            "claude".into(),
            "--settings".into(),
            serde_json::json!({
                "permissions": {"defaultMode": "plan"},
                "hooks": {
                    "PreCompact": [{
                        "matcher": "manual",
                        "hooks": [{"type": "command", "command": "echo user-hook"}]
                    }]
                }
            })
            .to_string(),
        ];
        let runtime = ForegroundRuntime::start(&route, Vec::new()).unwrap();
        let adapter = RecordingAdapter::default();
        runtime
            .spawn_with(
                &adapter,
                SpawnMode::Subprocess,
                route.command.clone(),
                route.cwd.clone(),
            )
            .unwrap();

        let calls = adapter.calls.borrow();
        let command = &calls[0].1;
        assert_eq!(
            command
                .iter()
                .filter(|argument| argument.as_str() == "--settings")
                .count(),
            1
        );
        let merged_path = PathBuf::from(&command[2]);
        assert!(merged_path.is_absolute());
        assert!(!command.join(" ").contains("echo user-hook"));
        assert!(!command.join(" ").contains(runtime.base_url().unwrap()));
        let settings: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(merged_path).unwrap()).unwrap();
        assert_eq!(settings["permissions"]["defaultMode"], "plan");
        assert_eq!(
            settings["hooks"]["PreCompact"][0]["hooks"][0]["command"],
            "echo user-hook"
        );
        assert_eq!(settings["hooks"]["PreCompact"].as_array().unwrap().len(), 2);
        assert_eq!(settings["hooks"]["SessionStart"][0]["matcher"], "clear");
    }

    #[test]
    fn bridge_merges_file_user_settings_with_context_lifecycle_hooks() {
        let directory = tempfile::tempdir().unwrap();
        let settings_path = directory.path().join("claude-settings.json");
        std::fs::write(
            &settings_path,
            serde_json::json!({
                "env": {"USER_SETTING": "preserved"},
                "hooks": {
                    "SessionStart": [{
                        "matcher": "startup",
                        "hooks": [{"type": "command", "command": "echo startup"}]
                    }]
                }
            })
            .to_string(),
        )
        .unwrap();
        let mut route = plan(ModelProvider::Codex, Backend::Claude);
        route.cwd = Some(directory.path().to_string_lossy().into_owned());
        route.command = vec!["claude".into(), "--settings=claude-settings.json".into()];
        let runtime = ForegroundRuntime::start(&route, Vec::new()).unwrap();
        let adapter = RecordingAdapter::default();
        runtime
            .spawn_with(
                &adapter,
                SpawnMode::Pty,
                route.command.clone(),
                route.cwd.clone(),
            )
            .unwrap();

        let calls = adapter.calls.borrow();
        let command = &calls[0].1;
        assert_eq!(command[1], "--settings");
        assert_eq!(command.len(), 3);
        let merged_path = PathBuf::from(&command[2]);
        assert!(merged_path.is_absolute());
        assert!(!command.join(" ").contains("preserved"));
        let settings: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&merged_path).unwrap()).unwrap();
        assert_eq!(settings["env"]["USER_SETTING"], "preserved");
        assert_eq!(
            settings["hooks"]["SessionStart"].as_array().unwrap().len(),
            3
        );
        assert_eq!(settings["hooks"]["PreCompact"][0]["matcher"], "manual|auto");
        drop(calls);
        drop(runtime);
        assert!(!merged_path.exists());
    }

    #[test]
    fn bridge_leaves_settings_shaped_positional_arguments_untouched() {
        let mut route = plan(ModelProvider::Codex, Backend::Claude);
        route.command = vec![
            "claude".into(),
            "--".into(),
            "--settings".into(),
            "literal prompt text".into(),
        ];
        let runtime = ForegroundRuntime::start(&route, Vec::new()).unwrap();
        let adapter = RecordingAdapter::default();
        runtime
            .spawn_with(&adapter, SpawnMode::Subprocess, route.command.clone(), None)
            .unwrap();

        let calls = adapter.calls.borrow();
        let command = &calls[0].1;
        assert_eq!(&command[3..], &route.command[1..]);
        assert_eq!(command[1], "--settings");
        let generated: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&command[2]).unwrap()).unwrap();
        assert_eq!(
            generated["hooks"]["PreCompact"][0]["matcher"],
            "manual|auto"
        );
    }

    #[test]
    fn all_scoped_outcomes_drop_the_bridge_and_close_its_port() {
        #[derive(Clone, Copy)]
        enum Outcome {
            Success,
            ChildFailure,
            SpawnFailure,
            Cancelled,
        }

        for outcome in [
            Outcome::Success,
            Outcome::ChildFailure,
            Outcome::SpawnFailure,
            Outcome::Cancelled,
        ] {
            let mut address = None;
            let result = with_foreground_runtime(
                &plan(ModelProvider::Codex, Backend::Claude),
                Vec::new(),
                |runtime| {
                    address = runtime.socket_addr();
                    match outcome {
                        Outcome::Success => 0,
                        Outcome::ChildFailure | Outcome::SpawnFailure => 1,
                        Outcome::Cancelled => 130,
                    }
                },
            )
            .unwrap();
            assert!(matches!(result, 0 | 1 | 130));

            // The property is that the runtime released the bridge port.
            //
            // Probing with `connect` and requiring a refusal is not that
            // property: ephemeral ports are recycled promptly, so a sibling
            // test in the same parallel run can bind this exact address
            // between the drop and the probe, and the connect then succeeds
            // against *its* listener. `BridgeHandle::shutdown` joins the
            // serve thread, so ours is provably gone by the time we get here
            // -- the old assertion was reading someone else's socket and
            // calling it a leak. It failed roughly half of full-suite runs
            // locally while passing in isolation, which is the signature.
            //
            // Binding is the question actually worth asking. If the bind
            // succeeds the port was free, which is the property. If it fails,
            // another listener owns the address now, so nothing about our
            // bridge can be concluded either way -- inconclusive, not a
            // failure.
            let address = address.unwrap();
            if let Ok(reclaimed) = TcpListener::bind(address) {
                drop(reclaimed);
            }
        }
    }

    #[test]
    fn runtime_debug_omits_bridge_url_and_token() {
        let runtime =
            ForegroundRuntime::start(&plan(ModelProvider::Codex, Backend::Claude), Vec::new())
                .unwrap();
        let rendered = format!("{runtime:?}");
        assert!(!rendered.contains(runtime.base_url().unwrap()));
        assert!(!rendered.contains(runtime.bearer_token().unwrap()));
    }

    // -- Kimi (#937 Phase 3, Lane 3B) -----------------------------------
    //
    // `apply_anthropic_compat_overlay` is fully generic (landed in Phase 2),
    // so nothing below requires a production change in this file -- these
    // tests exist to prove the existing generic machinery produces exactly
    // Kimi's official profile (#936) once Lane 3A's descriptor/catalog rows
    // land, and to pin the routing/no-bridge/no-secret-leak guarantees
    // DeepSeek already has.

    fn kimi_descriptor() -> &'static crate::provider_registry::AnthropicCompatProvider {
        crate::provider_registry::descriptor_for(ModelProvider::Kimi)
            .expect("Kimi must have an Anthropic-compat descriptor")
    }

    /// #936's exact documented profile: every default-model slot *and* the
    /// subagent slot pin to `kimi-k3[1m]` (unlike DeepSeek, whose
    /// haiku/subagent slots use a cheaper flash model), compact window
    /// 1048576, effort max.
    #[test]
    fn golden_kimi_overlay_default_selection() {
        let mut env = Vec::new();
        apply_anthropic_compat_overlay(&mut env, "kimi-golden-secret", kimi_descriptor(), None);
        let mut pairs = env.clone();
        pairs.sort();
        assert_eq!(
            pairs,
            vec![
                (
                    "ANTHROPIC_AUTH_TOKEN".to_string(),
                    "kimi-golden-secret".to_string()
                ),
                (
                    "ANTHROPIC_BASE_URL".to_string(),
                    "https://api.moonshot.ai/anthropic".to_string()
                ),
                (
                    "ANTHROPIC_DEFAULT_FABLE_MODEL".to_string(),
                    "kimi-k3[1m]".to_string()
                ),
                (
                    "ANTHROPIC_DEFAULT_HAIKU_MODEL".to_string(),
                    "kimi-k3[1m]".to_string()
                ),
                (
                    "ANTHROPIC_DEFAULT_OPUS_MODEL".to_string(),
                    "kimi-k3[1m]".to_string()
                ),
                (
                    "ANTHROPIC_DEFAULT_SONNET_MODEL".to_string(),
                    "kimi-k3[1m]".to_string()
                ),
                ("ANTHROPIC_MODEL".to_string(), "kimi-k3[1m]".to_string()),
                (
                    "CLAUDE_CODE_AUTO_COMPACT_WINDOW".to_string(),
                    "1048576".to_string()
                ),
                (
                    "CLAUDE_CODE_SUBAGENT_MODEL".to_string(),
                    "kimi-k3[1m]".to_string()
                ),
            ]
        );
    }

    /// Ambient `ANTHROPIC_API_KEY`, `ANTHROPIC_SMALL_FAST_MODEL`, and the
    /// mixed-case `anthropic_api_key` form are actively scrubbed by the
    /// shared union const. `MOONSHOT_API_KEY` is not an `ANTHROPIC_*`
    /// variable and is not scrubbed (there is no ambient-credential fallback
    /// to scrub away -- #936 says "do not fall back to ... ambient
    /// MOONSHOT_API_KEY", a claim about which value seeds
    /// `ANTHROPIC_AUTH_TOKEN`, not about deleting an unrelated key from the
    /// child env). The real proof is that the auth token equals the
    /// injected vault secret and never the ambient Moonshot value, even
    /// when both are present in the base environment. The parent `base`
    /// vector must stay byte-identical throughout.
    #[test]
    fn kimi_overlay_scrubs_ambient_anthropic_values_and_never_consults_moonshot_key() {
        let base = vec![
            ("ANTHROPIC_API_KEY".to_string(), "ambient-key".to_string()),
            (
                "ANTHROPIC_SMALL_FAST_MODEL".to_string(),
                "ambient-fast".to_string(),
            ),
            (
                "anthropic_api_key".to_string(),
                "ambient-mixed-case-key".to_string(),
            ),
            (
                "MOONSHOT_API_KEY".to_string(),
                "ambient-moonshot-key".to_string(),
            ),
            ("UNCHANGED".to_string(), "yes".to_string()),
        ];
        let mut child = base.clone();
        apply_anthropic_compat_overlay(&mut child, "kimi-test-secret", kimi_descriptor(), None);

        assert_eq!(lookup(&child, "UNCHANGED"), Some("yes"));
        assert_eq!(lookup(&child, "ANTHROPIC_API_KEY"), None);
        assert_eq!(lookup(&child, "ANTHROPIC_SMALL_FAST_MODEL"), None);
        assert_eq!(lookup(&child, "anthropic_api_key"), None);
        assert_eq!(
            lookup(&child, "ANTHROPIC_AUTH_TOKEN"),
            Some("kimi-test-secret"),
            "the auth token must come from the injected vault secret, never an ambient key"
        );
        assert_ne!(
            lookup(&child, "ANTHROPIC_AUTH_TOKEN"),
            Some("ambient-moonshot-key"),
            "MOONSHOT_API_KEY must never be consulted as a credential source"
        );
        assert_eq!(lookup(&base, "ANTHROPIC_API_KEY"), Some("ambient-key"));
        assert_eq!(
            lookup(&base, "MOONSHOT_API_KEY"),
            Some("ambient-moonshot-key")
        );
    }

    /// Kimi routes directly through the child-overlay path, exactly like
    /// DeepSeek: no `BridgeHandle`, so no loopback listener either.
    #[test]
    fn kimi_direct_route_creates_no_bridge_and_no_listener() {
        let base = vec![("UNRELATED".to_string(), "kept".to_string())];
        let store = FakeSecretStore(Some("kimi-routing-secret".to_string()));
        let runtime = ForegroundRuntime::start_with_secret_store(
            &plan(ModelProvider::Kimi, Backend::Claude),
            base.clone(),
            &store,
        )
        .unwrap();
        assert!(
            !runtime.has_bridge(),
            "Kimi must route directly, never through BridgeHandle"
        );
        assert_eq!(runtime.socket_addr(), None, "no listener without a bridge");
        assert_eq!(runtime.base_url(), None);
        assert_eq!(runtime.bearer_token(), None);
        assert_eq!(
            lookup(runtime.env(), "ANTHROPIC_AUTH_TOKEN"),
            Some("kimi-routing-secret")
        );
        assert_eq!(lookup(runtime.env(), "UNRELATED"), Some("kept"));
    }

    #[test]
    fn kimi_route_without_a_stored_credential_fails_the_launch() {
        let store = FakeSecretStore(None);
        let error = ForegroundRuntime::start_with_secret_store(
            &plan(ModelProvider::Kimi, Backend::Claude),
            Vec::new(),
            &store,
        )
        .unwrap_err();
        // Every descriptor-backed provider shares one secret-free,
        // provider-neutral credential error.
        assert!(matches!(&error, BridgeError::AnthropicCompatCredentials));
    }

    #[test]
    fn kimi_subprocess_and_pty_receive_the_same_secret_child_overlay() {
        let mut env = vec![("ANTHROPIC_API_KEY".to_string(), "ambient-key".to_string())];
        apply_anthropic_compat_overlay(&mut env, "kimi-test-secret", kimi_descriptor(), None);
        let runtime = ForegroundRuntime {
            env,
            bridge: None,
            claude_settings: None,
            startup_notices: Vec::new(),
        };
        let adapter = RecordingAdapter::default();
        runtime
            .spawn_with(&adapter, SpawnMode::Subprocess, vec!["claude".into()], None)
            .unwrap();
        runtime
            .spawn_with(&adapter, SpawnMode::Pty, vec!["claude".into()], None)
            .unwrap();

        let calls = adapter.calls.borrow();
        assert_eq!(calls.len(), 2);
        assert_eq!(calls[0].2, calls[1].2);
        assert_eq!(
            lookup(&calls[0].2, "ANTHROPIC_AUTH_TOKEN"),
            Some("kimi-test-secret")
        );
        assert_eq!(lookup(&calls[0].2, "ANTHROPIC_API_KEY"), None);
        assert!(!runtime.has_bridge());
    }

    fn openrouter_descriptor() -> &'static crate::provider_registry::AnthropicCompatProvider {
        crate::provider_registry::descriptor_for(ModelProvider::OpenRouter)
            .expect("OpenRouter must have an Anthropic-compat descriptor")
    }

    #[test]
    fn golden_openrouter_overlay_uses_documented_claude_gateway_profile() {
        let base = vec![
            (
                "ANTHROPIC_API_KEY".to_string(),
                "ambient-anthropic".to_string(),
            ),
            (
                "OPENROUTER_API_KEY".to_string(),
                "ambient-openrouter".to_string(),
            ),
            (
                "ANTHROPIC_DEFAULT_FABLE_MODEL".to_string(),
                "ambient-fable".to_string(),
            ),
            (
                "CLAUDE_CODE_ENABLE_GATEWAY_MODEL_DISCOVERY".to_string(),
                "0".to_string(),
            ),
        ];
        let mut child = base.clone();
        apply_anthropic_compat_overlay(
            &mut child,
            "openrouter-vault-secret",
            openrouter_descriptor(),
            None,
        );

        assert_eq!(
            lookup(&child, "ANTHROPIC_BASE_URL"),
            Some("https://openrouter.ai/api")
        );
        assert_eq!(
            lookup(&child, "ANTHROPIC_AUTH_TOKEN"),
            Some("openrouter-vault-secret")
        );
        assert_eq!(lookup(&child, "ANTHROPIC_API_KEY"), Some(""));
        assert_eq!(lookup(&child, "OPENROUTER_API_KEY"), None);
        assert_eq!(
            lookup(&child, "ANTHROPIC_MODEL"),
            Some("~anthropic/claude-sonnet-latest")
        );
        assert_eq!(
            lookup(&child, "ANTHROPIC_DEFAULT_OPUS_MODEL"),
            Some("~anthropic/claude-opus-latest")
        );
        assert_eq!(
            lookup(&child, "ANTHROPIC_DEFAULT_SONNET_MODEL"),
            Some("~anthropic/claude-sonnet-latest")
        );
        assert_eq!(
            lookup(&child, "ANTHROPIC_DEFAULT_HAIKU_MODEL"),
            Some("~anthropic/claude-haiku-latest")
        );
        assert_eq!(
            lookup(&child, "CLAUDE_CODE_SUBAGENT_MODEL"),
            Some("~anthropic/claude-opus-latest")
        );
        assert_eq!(
            lookup(&child, "ANTHROPIC_DEFAULT_FABLE_MODEL"),
            Some("~anthropic/claude-fable-latest")
        );
        assert_eq!(
            lookup(&child, "CLAUDE_CODE_ENABLE_GATEWAY_MODEL_DISCOVERY"),
            Some("1")
        );
        assert_eq!(
            lookup(&base, "ANTHROPIC_API_KEY"),
            Some("ambient-anthropic")
        );
    }

    #[test]
    fn openrouter_direct_route_creates_no_bridge_and_requires_its_vault_secret() {
        let store = FakeSecretStore(Some("openrouter-routing-secret".to_string()));
        let runtime = ForegroundRuntime::start_with_secret_store(
            &plan(ModelProvider::OpenRouter, Backend::Claude),
            Vec::new(),
            &store,
        )
        .unwrap();
        assert!(!runtime.has_bridge());
        assert_eq!(runtime.socket_addr(), None);
        assert_eq!(
            lookup(runtime.env(), "ANTHROPIC_AUTH_TOKEN"),
            Some("openrouter-routing-secret")
        );
    }

    #[test]
    fn openrouter_route_without_a_stored_credential_reports_a_provider_neutral_error() {
        let error = ForegroundRuntime::start_with_secret_store(
            &plan(ModelProvider::OpenRouter, Backend::Claude),
            Vec::new(),
            &FakeSecretStore(None),
        )
        .unwrap_err();
        assert!(matches!(&error, BridgeError::AnthropicCompatCredentials));
        assert!(!error.to_string().contains("DeepSeek"));
    }

    /// `launch_preflight_target` (provider_auth.rs) and `PreflightError`
    /// are already provider-neutral: `launch_preflight_target` is a plain
    /// registry lookup, and `PreflightError::describe` takes the descriptor
    /// as a parameter rather than hardcoding a provider. Both are exercised
    /// against DeepSeek in `provider_auth.rs`'s own test module (which this
    /// lane does not own), so this only pins that the same generic surface
    /// resolves correctly for Kimi's descriptor once it lands: the
    /// dry-run-is-vault-free guarantee, and that the actionable message
    /// names `clud auth login kimi`. The interactive
    /// prompt/store/continue, empty/cancel, and non-interactive
    /// no-stdin-read mechanics themselves live in `preflight_with`, a
    /// private fn in `provider_auth.rs` that takes no `ModelProvider`
    /// parameter at all (only an injected `SecretStore` + closure) -- so
    /// DeepSeek's existing coverage of that fn already proves Kimi's
    /// identical behavior; there is no Kimi-specific mechanism to
    /// duplicate here.
    #[test]
    fn kimi_preflight_target_resolves_to_kimis_descriptor_and_is_vault_free_on_dry_run() {
        assert_eq!(
            crate::provider_auth::launch_preflight_target(ModelProvider::Kimi, false),
            Some(kimi_descriptor())
        );
        assert_eq!(
            crate::provider_auth::launch_preflight_target(ModelProvider::Kimi, true),
            None,
            "dry run must never resolve a descriptor to preflight against, for any provider"
        );
    }

    #[test]
    fn kimi_preflight_error_names_kimi_and_its_login_command() {
        let descriptor = kimi_descriptor();
        assert_eq!(
            crate::provider_auth::PreflightError::Missing.describe(descriptor),
            "Kimi credentials are not configured; run `clud auth login kimi`"
        );
        assert_eq!(
            crate::provider_auth::PreflightError::Cancelled.describe(descriptor),
            "Kimi credential entry was cancelled"
        );
    }

    // -----------------------------------------------------------------
    // #967 Phase 2b: declared hooks compiled into `--settings`.
    // -----------------------------------------------------------------

    fn repo_declaring(hooks: &str) -> tempfile::TempDir {
        let tmp = tempfile::TempDir::new().unwrap();
        std::fs::create_dir_all(tmp.path().join(".git")).unwrap();
        std::fs::create_dir_all(tmp.path().join(".clud")).unwrap();
        std::fs::write(tmp.path().join(".clud").join("hooks.json"), hooks).unwrap();
        tmp
    }

    fn plan_in(provider: ModelProvider, harness: Backend, cwd: &std::path::Path) -> LaunchPlan {
        let mut plan = plan(provider, harness);
        plan.cwd = Some(cwd.to_string_lossy().into_owned());
        plan
    }

    /// Every file under `root`, with its bytes — for proving a launch wrote
    /// nothing (DD-049).
    fn snapshot(root: &std::path::Path) -> std::collections::BTreeMap<std::path::PathBuf, Vec<u8>> {
        let mut out = std::collections::BTreeMap::new();
        let mut stack = vec![root.to_path_buf()];
        while let Some(dir) = stack.pop() {
            let Ok(entries) = std::fs::read_dir(&dir) else {
                continue;
            };
            for entry in entries.flatten() {
                let path = entry.path();
                if path.is_dir() {
                    stack.push(path);
                } else if let Ok(bytes) = std::fs::read(&path) {
                    out.insert(path, bytes);
                }
            }
        }
        out
    }

    /// The argv clud would actually spawn, via the recording adapter.
    fn composed_command(runtime: &ForegroundRuntime, command: Vec<String>) -> Vec<String> {
        let adapter = RecordingAdapter::default();
        runtime
            .spawn_with(&adapter, SpawnMode::Subprocess, command, None)
            .unwrap();
        let calls = adapter.calls.borrow();
        calls[0].1.clone()
    }

    fn settings_argument(runtime: &ForegroundRuntime) -> Option<String> {
        let command = composed_command(runtime, vec!["claude".to_string()]);
        let index = command
            .iter()
            .position(|argument| argument == "--settings")?;
        command.get(index + 1).cloned()
    }

    #[test]
    fn a_plain_claude_launch_carries_declared_hooks_as_settings() {
        // The ungating: before Phase 2b only bridge routes composed settings,
        // so a plain launch registered nothing.
        let repo = repo_declaring(r#"{"hooks":{"Stop":[{"command":"guard"}]}}"#);
        let runtime = ForegroundRuntime::start(
            &plan_in(ModelProvider::Claude, Backend::Claude, repo.path()),
            Vec::new(),
        )
        .unwrap();

        let path = settings_argument(&runtime).expect("--settings injected");
        let document: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(
            document["hooks"]["Stop"][0]["hooks"][0]["command"],
            "clud-cmd-scan --event Stop"
        );
        assert!(lookup(runtime.env(), crate::clud_hooks_compile::DISPATCH_ENV).is_some());
    }

    #[test]
    fn a_repo_that_declares_nothing_gets_an_untouched_launch() {
        let repo = tempfile::TempDir::new().unwrap();
        std::fs::create_dir_all(repo.path().join(".git")).unwrap();

        let runtime = ForegroundRuntime::start(
            &plan_in(ModelProvider::Claude, Backend::Claude, repo.path()),
            Vec::new(),
        )
        .unwrap();

        assert_eq!(settings_argument(&runtime), None);
        assert!(lookup(runtime.env(), crate::clud_hooks_compile::DISPATCH_ENV).is_none());
    }

    #[test]
    fn a_launch_modifies_no_file_in_the_repo() {
        // DD-049: settings reach the harness as arguments, never as writes.
        let repo = repo_declaring(r#"{"hooks":{"Stop":[{"command":"guard"}]}}"#);
        let before = snapshot(repo.path());

        let runtime = ForegroundRuntime::start(
            &plan_in(ModelProvider::Claude, Backend::Claude, repo.path()),
            Vec::new(),
        )
        .unwrap();
        let _ = settings_argument(&runtime);

        assert_eq!(
            snapshot(repo.path()),
            before,
            "the launch wrote to the repo"
        );
    }

    #[test]
    fn a_user_supplied_settings_argument_is_merged_not_shadowed() {
        // Claude accepts one `--settings`; a second would shadow the first.
        let repo = repo_declaring(r#"{"hooks":{"Stop":[{"command":"guard"}]}}"#);
        let user = repo.path().join("mine.json");
        std::fs::write(
            &user,
            r#"{"model":"theirs","hooks":{"Stop":[{"hooks":[{"command":"user-hook"}]}]}}"#,
        )
        .unwrap();

        let mut launch_plan = plan_in(ModelProvider::Claude, Backend::Claude, repo.path());
        launch_plan.command = vec![
            "claude".to_string(),
            "--settings".to_string(),
            user.to_string_lossy().into_owned(),
        ];
        let runtime = ForegroundRuntime::start(&launch_plan, Vec::new()).unwrap();

        let composed = composed_command(&runtime, launch_plan.command.clone());
        let occurrences = composed
            .iter()
            .filter(|argument| *argument == "--settings")
            .count();
        assert_eq!(occurrences, 1, "exactly one source survives: {composed:?}");

        let index = composed
            .iter()
            .position(|argument| argument == "--settings")
            .unwrap();
        let document: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&composed[index + 1]).unwrap()).unwrap();
        assert_eq!(document["model"], "theirs", "user keys survive");
        let commands: Vec<&str> = document["hooks"]["Stop"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|group| group["hooks"][0]["command"].as_str())
            .collect();
        assert!(commands.contains(&"user-hook"), "{commands:?}");
        assert!(
            commands.contains(&"clud-cmd-scan --event Stop"),
            "{commands:?}"
        );
    }

    // -----------------------------------------------------------------
    // #967 Phase 3b: roots harvested at launch.
    // -----------------------------------------------------------------

    fn hook_roots_env(runtime: &ForegroundRuntime) -> Option<String> {
        lookup(runtime.env(), crate::clud_hook_roots::HOOK_ROOTS_ENV).map(ToOwned::to_owned)
    }

    fn roots_in(encoded: &str) -> Vec<String> {
        serde_json::from_str::<serde_json::Value>(encoded)
            .unwrap()
            .as_array()
            .unwrap()
            .iter()
            .map(|entry| entry["path"].as_str().unwrap().to_string())
            .collect()
    }

    #[test]
    fn add_dir_targets_are_harvested_into_the_registry() {
        // `--add-dir` reaches the harness as passthrough argv and appears in
        // no hook payload, so the launch is the only place that can see it.
        let repo = tempfile::TempDir::new().unwrap();
        std::fs::create_dir_all(repo.path().join(".git")).unwrap();
        let extra = tempfile::TempDir::new().unwrap();

        let mut launch = plan_in(ModelProvider::Claude, Backend::Claude, repo.path());
        launch.command = vec![
            "claude".to_string(),
            "--add-dir".to_string(),
            extra.path().to_string_lossy().into_owned(),
        ];
        let runtime = ForegroundRuntime::start(&launch, Vec::new()).unwrap();

        let encoded = hook_roots_env(&runtime).expect("roots carried to the hook");
        assert!(
            roots_in(&encoded)
                .iter()
                .any(|path| path.contains(extra.path().file_name().unwrap().to_str().unwrap())),
            "{encoded}"
        );
    }

    #[test]
    fn add_dir_accepts_both_spellings_and_several_directories() {
        let repo = tempfile::TempDir::new().unwrap();
        std::fs::create_dir_all(repo.path().join(".git")).unwrap();
        let first = tempfile::TempDir::new().unwrap();
        let second = tempfile::TempDir::new().unwrap();

        let mut launch = plan_in(ModelProvider::Claude, Backend::Claude, repo.path());
        launch.command = vec![
            "claude".to_string(),
            "--add-dir".to_string(),
            first.path().to_string_lossy().into_owned(),
            second.path().to_string_lossy().into_owned(),
            "--verbose".to_string(),
        ];
        let runtime = ForegroundRuntime::start(&launch, Vec::new()).unwrap();
        assert_eq!(
            roots_in(&hook_roots_env(&runtime).expect("roots")).len(),
            2,
            "the flag takes directories until the next flag"
        );

        launch.command = vec![
            "claude".to_string(),
            format!("--add-dir={}", first.path().to_string_lossy()),
        ];
        let joined = ForegroundRuntime::start(&launch, Vec::new()).unwrap();
        assert_eq!(roots_in(&hook_roots_env(&joined).expect("roots")).len(), 1);
    }

    #[test]
    fn tokens_after_a_bare_separator_are_not_harness_flags() {
        let repo = tempfile::TempDir::new().unwrap();
        std::fs::create_dir_all(repo.path().join(".git")).unwrap();
        let extra = tempfile::TempDir::new().unwrap();

        let mut launch = plan_in(ModelProvider::Claude, Backend::Claude, repo.path());
        launch.command = vec![
            "claude".to_string(),
            "--".to_string(),
            "--add-dir".to_string(),
            extra.path().to_string_lossy().into_owned(),
        ];
        let runtime = ForegroundRuntime::start(&launch, Vec::new()).unwrap();

        assert_eq!(hook_roots_env(&runtime), None);
    }

    #[test]
    fn additional_directories_from_claude_settings_are_harvested_too() {
        let repo = tempfile::TempDir::new().unwrap();
        std::fs::create_dir_all(repo.path().join(".git")).unwrap();
        std::fs::create_dir_all(repo.path().join(".claude")).unwrap();
        std::fs::write(
            repo.path().join(".claude").join("settings.json"),
            r#"{"permissions":{"additionalDirectories":["../sibling"]}}"#,
        )
        .unwrap();

        let runtime = ForegroundRuntime::start(
            &plan_in(ModelProvider::Claude, Backend::Claude, repo.path()),
            Vec::new(),
        )
        .unwrap();

        let encoded = hook_roots_env(&runtime).expect("roots");
        assert!(
            roots_in(&encoded)
                .iter()
                .any(|path| path.contains("sibling")),
            "{encoded}"
        );
    }

    #[test]
    fn a_launch_that_grants_nothing_leaves_the_env_alone() {
        // The hook can work the rest out for itself; an empty registry would
        // be noise the child has to parse on every tool call.
        let repo = tempfile::TempDir::new().unwrap();
        std::fs::create_dir_all(repo.path().join(".git")).unwrap();

        let runtime = ForegroundRuntime::start(
            &plan_in(ModelProvider::Claude, Backend::Claude, repo.path()),
            Vec::new(),
        )
        .unwrap();

        assert_eq!(hook_roots_env(&runtime), None);
    }

    #[test]
    fn harvested_directories_are_registered_as_extern() {
        // A granted sibling is no more the parent's business than a checkout
        // clud cloned: its project guards would misfire there (#841). The two
        // differ in trust, not in firing, which Phase 4 has to separate.
        let repo = tempfile::TempDir::new().unwrap();
        std::fs::create_dir_all(repo.path().join(".git")).unwrap();
        let extra = tempfile::TempDir::new().unwrap();

        let mut launch = plan_in(ModelProvider::Claude, Backend::Claude, repo.path());
        launch.command = vec![
            "claude".to_string(),
            "--add-dir".to_string(),
            extra.path().to_string_lossy().into_owned(),
        ];
        let runtime = ForegroundRuntime::start(&launch, Vec::new()).unwrap();

        let encoded = hook_roots_env(&runtime).expect("roots");
        let document: serde_json::Value = serde_json::from_str(&encoded).unwrap();
        assert_eq!(document[0]["kind"], "extern");

        let roots = crate::clud_hook_roots::HookRoots::resolve(repo.path(), &[], Some(&encoded));
        assert!(
            !roots.parent_hooks_apply_to(&extra.path().join("file.rs")),
            "the parent's guards must not follow the agent into a granted directory"
        );
    }

    #[test]
    fn a_codex_harness_launch_gets_no_settings_because_codex_cannot_take_them() {
        // `-c` overrides config.toml; codex hooks live in a separate
        // hooks.json with no flag pointing at an alternate one.
        let repo = repo_declaring(r#"{"hooks":{"Stop":[{"command":"guard"}]}}"#);
        let runtime = ForegroundRuntime::start(
            &plan_in(ModelProvider::Codex, Backend::Codex, repo.path()),
            Vec::new(),
        )
        .unwrap();

        assert_eq!(settings_argument(&runtime), None);
        assert!(lookup(runtime.env(), crate::clud_hooks_compile::DISPATCH_ENV).is_none());
    }
}
