//! Foreground child runtime for provider/harness cross-routes (issue #626).

use crate::backend::{Backend, ModelProvider, RoutingMode};
use crate::codex_bridge::{
    BridgeConfig, BridgeError, BridgeHandle, UnifiedGatewayConfig, UNIFIED_GATEWAY_TOKEN_HEADER,
};
use crate::codex_model::{picker_entry, ModelSpec};
use crate::codex_translate::default_model_spec;
use crate::command::LaunchPlan;
use crate::subprocess::ManagedSubprocess;
use running_process::pty::NativePtyProcess;
use std::fmt;
use std::io::Write;
#[cfg(test)]
use std::net::SocketAddr;
use std::path::PathBuf;

const DEFAULT_API_TIMEOUT_MS: &str = "3000000";
const DEFAULT_DISABLE_NONESSENTIAL_TRAFFIC: &str = "1";

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
    pub fn start(plan: &LaunchPlan, env: Vec<(String, String)>) -> Result<Self, BridgeError> {
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
        .map_err(|_| BridgeError::DeepSeekCredentials)?;
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
            let codex_available =
                crate::codex_upstream::ResolvedCredentials::resolve_default().is_ok();
            let startup_notices = unified_startup_notices(codex_available, deepseek_key.is_some());
            let bridge = BridgeHandle::start(
                BridgeConfig::default()
                    .with_unified_gateway(UnifiedGatewayConfig::new(deepseek_key, codex_available)),
            )?;
            apply_unified_overlay(&mut env, &bridge)?;
            let settings = merged_unified_context_lifecycle_settings(plan, &bridge)?;
            (Some(bridge), Some(settings), startup_notices)
        } else if is_codex_via_claude(plan) {
            // A selection that does not parse fails the launch rather than
            // the first turn: by the time a request is in flight the user has
            // already waited, and the message would arrive wrapped in the
            // harness's own API-error framing.
            let selection = match plan.codex_model.as_deref() {
                Some(raw) => Some(
                    ModelSpec::parse(raw).map_err(|error| BridgeError::Model(error.to_string()))?,
                ),
                None => None,
            };
            let bridge =
                BridgeHandle::start(BridgeConfig::default().with_default_model(selection.clone()))?;
            apply_cross_route_overlay(&mut env, &bridge, selection.as_ref());
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
                .map_err(|_| BridgeError::DeepSeekCredentials)?
                .ok_or(BridgeError::DeepSeekCredentials)?;
            apply_anthropic_compat_overlay(
                &mut env,
                &secret,
                descriptor,
                plan.model_selection.as_ref(),
            );
            (None, None, Vec::new())
        } else {
            (None, None, Vec::new())
        };
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
    "CLAUDE_CODE_EFFORT_LEVEL",
    "CLAUDE_CODE_AUTO_COMPACT_WINDOW",
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
    let default_wire_model = crate::provider_catalog::reviewed_default_model(descriptor.provider)
        .expect(
            "every Anthropic-compat descriptor's provider must have a reviewed catalog default \
             -- add a `provider_default: true` row in provider_catalog.rs",
        )
        .wire_id;
    let model = selection
        .and_then(|selection| selection.wire_model.as_deref())
        .unwrap_or(default_wire_model);
    let effort = selection
        .and_then(|selection| selection.effort)
        .unwrap_or(crate::provider_catalog::EffortLevel::Max)
        .as_str();
    env.extend([
        (
            "ANTHROPIC_BASE_URL".to_string(),
            descriptor.anthropic_base_url.to_string(),
        ),
        ("ANTHROPIC_AUTH_TOKEN".to_string(), secret.to_string()),
        ("ANTHROPIC_MODEL".to_string(), model.to_string()),
        (
            "ANTHROPIC_DEFAULT_OPUS_MODEL".to_string(),
            model.to_string(),
        ),
        (
            "ANTHROPIC_DEFAULT_SONNET_MODEL".to_string(),
            model.to_string(),
        ),
        (
            "ANTHROPIC_DEFAULT_HAIKU_MODEL".to_string(),
            descriptor.subagent_wire_id.to_string(),
        ),
        (
            "ANTHROPIC_DEFAULT_FABLE_MODEL".to_string(),
            model.to_string(),
        ),
        (
            "CLAUDE_CODE_SUBAGENT_MODEL".to_string(),
            descriptor.subagent_wire_id.to_string(),
        ),
        ("CLAUDE_CODE_EFFORT_LEVEL".to_string(), effort.to_string()),
    ]);
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

fn apply_cross_route_overlay(
    env: &mut Vec<(String, String)>,
    bridge: &BridgeHandle,
    selection: Option<&ModelSpec>,
) {
    env.retain(|(key, _)| {
        ![
            "ANTHROPIC_BASE_URL",
            "ANTHROPIC_AUTH_TOKEN",
            "ANTHROPIC_API_KEY",
        ]
        .iter()
        .any(|sensitive| env_key_eq(key, sensitive))
    });
    env.push((
        "ANTHROPIC_BASE_URL".to_string(),
        bridge.base_url().to_string(),
    ));
    env.push((
        "ANTHROPIC_AUTH_TOKEN".to_string(),
        bridge.bearer_token().to_string(),
    ));
    // Put the selection in the harness's model picker. The harness renders
    // exactly one custom row (see `codex_model::PickerEntry` for the six row
    // sources and why none of them yields three), so the row is
    // unconditional — an unpinned launch would otherwise show only Anthropic
    // names that quietly run on the bridge's default — and its description
    // carries the models the row itself cannot name.
    let entry = picker_entry(&selection.cloned().unwrap_or_else(default_model_spec));
    push_default(env, "ANTHROPIC_CUSTOM_MODEL_OPTION", &entry.value);
    push_default(env, "ANTHROPIC_CUSTOM_MODEL_OPTION_NAME", &entry.name);
    push_default(
        env,
        "ANTHROPIC_CUSTOM_MODEL_OPTION_DESCRIPTION",
        &entry.description,
    );
    push_default(env, "API_TIMEOUT_MS", DEFAULT_API_TIMEOUT_MS);
    push_default(
        env,
        "CLAUDE_CODE_DISABLE_NONESSENTIAL_TRAFFIC",
        DEFAULT_DISABLE_NONESSENTIAL_TRAFFIC,
    );
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
    let generated_hooks = settings["hooks"]
        .as_object_mut()
        .expect("generated lifecycle hooks are an object");
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
                .expect("generated lifecycle hook event is an array")
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
    use std::net::TcpStream;

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
        }
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
            Some("1")
        );
        assert_eq!(lookup(&base, "ANTHROPIC_API_KEY"), Some("ambient-key"));
    }

    /// The selection reaches the child as a picker entry. Gateway model
    /// discovery cannot carry it (it drops every id not prefixed `claude`),
    /// so this variable is the only route to a visible, honest row.
    #[test]
    fn a_selection_becomes_a_picker_entry_in_the_child_environment() {
        let mut plan = plan(ModelProvider::Codex, Backend::Claude);
        plan.codex_model = Some("gpt-5.6-luna@high".to_string());
        let runtime = ForegroundRuntime::start(&plan, Vec::new()).unwrap();
        let env = runtime.env();
        assert_eq!(
            lookup(env, "ANTHROPIC_CUSTOM_MODEL_OPTION"),
            Some("gpt-5.6-luna@high")
        );
        assert_eq!(
            lookup(env, "ANTHROPIC_CUSTOM_MODEL_OPTION_NAME"),
            Some("Codex gpt-5.6-luna@high")
        );
    }

    /// Issue #820: all three gpt-5.6 models must be reachable from the picker.
    ///
    /// Claude Code renders **one** custom row — `ANTHROPIC_CUSTOM_MODEL_OPTION`
    /// is a scalar and has no indexed or list form — so the row's description
    /// is the only place the models the user did *not* launch with can appear.
    #[test]
    fn the_picker_row_makes_every_codex_model_discoverable() {
        let mut plan = plan(ModelProvider::Codex, Backend::Claude);
        plan.codex_model = Some("sol".to_string());
        let runtime = ForegroundRuntime::start(&plan, Vec::new()).unwrap();
        let env = runtime.env();
        assert_eq!(
            lookup(env, "ANTHROPIC_CUSTOM_MODEL_OPTION"),
            Some("gpt-5.6-sol")
        );
        let description = lookup(env, "ANTHROPIC_CUSTOM_MODEL_OPTION_DESCRIPTION")
            .expect("the single custom row must carry the rest of the catalog");
        for id in ["gpt-5.6-sol", "gpt-5.6-terra", "gpt-5.6-luna"] {
            assert!(description.contains(id), "{description} must name {id}");
        }
    }

    /// Without `--model` the picker used to show no Codex row at all, so every
    /// visible row was an Anthropic name that quietly ran on the bridge's
    /// default. The honest row is now unconditional and spells that default.
    #[test]
    fn an_unpinned_launch_still_gets_an_honest_default_row() {
        let runtime =
            ForegroundRuntime::start(&plan(ModelProvider::Codex, Backend::Claude), Vec::new())
                .unwrap();
        let env = runtime.env();
        assert_eq!(
            lookup(env, "ANTHROPIC_CUSTOM_MODEL_OPTION"),
            Some(crate::codex_translate::DEFAULT_CODEX_MODEL)
        );
        assert!(lookup(env, "ANTHROPIC_CUSTOM_MODEL_OPTION_DESCRIPTION").is_some());
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
    /// was touched.
    #[test]
    fn golden_anthropic_compat_overlay_default_selection() {
        let mut env = Vec::new();
        apply_anthropic_compat_overlay(&mut env, "ds-golden-secret", deepseek_descriptor(), None);
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
                ("CLAUDE_CODE_EFFORT_LEVEL".to_string(), "max".to_string()),
                (
                    "CLAUDE_CODE_SUBAGENT_MODEL".to_string(),
                    "deepseek-v4-flash".to_string()
                ),
            ]
        );
    }

    /// GOLDEN: a selection whose wire model has no `[1m]` suffix, so no
    /// `CLAUDE_CODE_AUTO_COMPACT_WINDOW` is set. Same delta as above: only
    /// the new `ANTHROPIC_DEFAULT_FABLE_MODEL` pin is added relative to the
    /// confirmed pre-refactor baseline.
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
                ("CLAUDE_CODE_EFFORT_LEVEL".to_string(), "high".to_string()),
                (
                    "CLAUDE_CODE_SUBAGENT_MODEL".to_string(),
                    "deepseek-v4-flash".to_string()
                ),
            ]
        );
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
        assert_eq!(lookup(&env, "CLAUDE_CODE_EFFORT_LEVEL"), Some("high"));
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
        assert!(matches!(error, BridgeError::DeepSeekCredentials));
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
            Some("custom-traffic")
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
            assert!(TcpStream::connect(address.unwrap()).is_err());
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
}
