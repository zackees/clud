//! One resolved route per launch (#1852 step 1, issue #1855).
//!
//! clud reaches non-Anthropic models through four backends, each of which used
//! to re-derive the same launch decisions from `LaunchPlan` and the provider
//! descriptor: the direct overlay, the unified gateway, the Codex-via-Claude
//! bridge, and the DeepSeek-native harness. This module makes that decision
//! **once**, as a pure function of the plan (plus the one piece of process
//! state the precedence rule genuinely needs, [`Ambient`]).
//!
//! Step 1 only *computes* the route and carries it on `LaunchPlan`; no backend
//! reads it yet. Steps 2-4 turn each backend into a renderer of this value and
//! then delete the copies it replaces, so the snapshots in
//! `route_snapshots_tests.rs` must stay byte-identical through those steps.
//!
//! Two deliberate deviations from the sketch in #1852:
//!
//! - `scrub`, `base_url` and the credential identifiers are owned `String`s
//!   rather than `&'static`, because the route rides on `LaunchPlan` and must
//!   survive a lossless serde round trip to a daemon worker.
//! - `CredentialSource::Vault(ModelProvider)` names the provider rather than a
//!   `{service, account}` pair. The identifiers are read from the provider's
//!   registry descriptor at spawn time, which is what removes the hard-coded
//!   vault constants the unified overlay carries today (divergence #5).
//!
//! A fifth [`RouteBackend::Native`] covers a plain Claude launch, which has no
//! provider overlay at all and therefore re-derives nothing.

use serde::{Deserialize, Serialize};

use crate::backend::{Backend, ModelProvider, RoutingMode};
use crate::codex_history::ConversationRoute;
use crate::command::LaunchPlan;

/// The direct route's client timeout (#1263): the harness's own, since clud is
/// not in the request path and cannot time the stream out itself.
pub const DIRECT_TIMEOUT_MS: &str = "600000";
/// Every bridge route's request timeout (DD-028). Deliberately separate from
/// the direct value: a bridge has clud in the path and can watch the stream.
pub const BRIDGE_TIMEOUT_MS: &str = "3000000";

/// Env keys the direct Anthropic-compatible overlay owns. Removing them all is
/// a security guarantee, not an OS-semantics match: an ambient Anthropic key
/// must never leak into a child that is billed to another provider's key.
///
/// `CLAUDE_CODE_EFFORT_LEVEL` is deliberately absent (DD-059): an ambient user
/// value is preserved so the harness's own `/effort` control stays
/// authoritative, and the catalog default travels on the harness's `--effort`
/// flag instead. `ANTHROPIC_CUSTOM_MODEL_OPTION*` is a prefix scrub, applied
/// separately by every overlay that rewrites the model slots.
pub const ANTHROPIC_COMPAT_SCRUB: &[&str] = &[
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

/// Env keys the Codex-via-Claude bridge overlay owns.
pub const CODEX_VIA_CLAUDE_SCRUB: &[&str] = &[
    "ANTHROPIC_BASE_URL",
    "ANTHROPIC_AUTH_TOKEN",
    "ANTHROPIC_API_KEY",
    "ANTHROPIC_DEFAULT_OPUS_MODEL",
    "ANTHROPIC_DEFAULT_SONNET_MODEL",
    "ANTHROPIC_DEFAULT_OPUS_MODEL_NAME",
    "ANTHROPIC_DEFAULT_SONNET_MODEL_NAME",
];

/// Prefix, not a key: every `ANTHROPIC_CUSTOM_MODEL_OPTION*` spelling is
/// dropped by the overlays that rewrite the model slots.
pub const CUSTOM_MODEL_OPTION_PREFIX: &str = "ANTHROPIC_CUSTOM_MODEL_OPTION";

/// The gateway's current slot bindings on the Codex bridge. The bridge cannot
/// serve Anthropic model IDs, so both roles bind to honest Codex rows.
pub const CODEX_BRIDGE_OPUS_MODEL: &str = "clud-claude-codex-sol";
pub const CODEX_BRIDGE_SONNET_MODEL: &str = "clud-claude-codex-terra";

/// The environment the precedence rules consult, kept explicit so resolving a
/// route stays a pure function of its inputs.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Ambient {
    /// An inherited `CLAUDE_CODE_SUBAGENT_MODEL`. It wins the subagent slot
    /// only on a constrained launch and only when the allowlist admits it
    /// (#1257): clud owns the boundary, the user chooses inside it.
    pub subagent_model: Option<String>,
}

impl Ambient {
    /// Read the ambient values from this process.
    pub fn from_process() -> Self {
        Self {
            subagent_model: std::env::var("CLAUDE_CODE_SUBAGENT_MODEL")
                .ok()
                .map(|value| value.trim().to_string())
                .filter(|value| !value.is_empty()),
        }
    }
}

/// Which of clud's backends serves this launch.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RouteBackend {
    /// Plain Claude: no provider overlay, nothing re-derived.
    Native,
    /// A descriptor-backed provider straight from the Claude harness
    /// (`--openrouter`, `--deepseek`, `--kimi`).
    Direct,
    /// clud's launch-scoped gateway in front of the Claude harness.
    Unified,
    /// The Codex translation bridge behind the Claude harness.
    CodexBridge,
    /// The `dsh` harness, which owns its own provider configuration.
    DeepSeekNative,
}

impl RouteBackend {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Native => "native",
            Self::Direct => "direct",
            Self::Unified => "unified",
            Self::CodexBridge => "codex_bridge",
            Self::DeepSeekNative => "deepseek_native",
        }
    }
}

/// Where an upstream's credential comes from. Never the credential itself: the
/// secret is read once, at spawn time, from this description.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "source", content = "value")]
pub enum CredentialSource {
    /// The Claude harness's own auth. clud neither reads nor injects it, which
    /// is what lets saved claude.ai OAuth reach the native upstream through
    /// the unified gateway.
    Harness,
    /// Codex's subscription credentials, resolved by `codex_upstream`.
    CodexAuth,
    /// clud's native credential vault. The identifiers come from the
    /// provider's registry descriptor, so there is one copy of them.
    Vault(ModelProvider),
    /// A key the child reads from its own environment. The DeepSeek-native
    /// harness keeps its keys in the process environment, and naming that
    /// explicitly is what makes the difference visible and testable.
    Env(String),
}

impl CredentialSource {
    /// The vault identifiers this source names, when it is a vault source.
    pub fn vault_identifiers(&self) -> Option<(&'static str, &'static str)> {
        match self {
            Self::Vault(provider) => crate::provider_registry::descriptor_for(*provider)
                .map(|descriptor| (descriptor.vault_service, descriptor.vault_account)),
            _ => None,
        }
    }
}

/// One upstream this launch can reach.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UpstreamRoute {
    pub provider: ModelProvider,
    pub base_url: String,
    pub credential: CredentialSource,
    pub conversation_route: ConversationRoute,
}

/// One slot's chosen model. The resolver decides *which row*; a renderer asks
/// for the id its backend needs -- `wire_id` for the direct route, which
/// writes the id verbatim, and `discovery_id` for the gateway, which must name
/// a row Claude Code can classify.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SlotModel {
    pub wire_id: String,
    pub discovery_id: Option<String>,
}

impl SlotModel {
    /// The slot a named id resolves to, or `None` when nothing names it.
    pub fn of(id: &str) -> Option<Self> {
        let id = id.trim();
        if id.is_empty() {
            return None;
        }
        Some(match crate::provider_catalog::model_by_any_id(id) {
            Some(row) => Self {
                wire_id: id.to_string(),
                discovery_id: row.discovery_id.map(str::to_string),
            },
            None => Self {
                wire_id: id.to_string(),
                discovery_id: None,
            },
        })
    }
}

/// The six model slots Claude Code resolves a launch through. `None` means
/// this backend leaves that slot alone, which is a decision in its own right:
/// the unified gateway sets none of them when the launch is unpinned.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SlotModels {
    pub main: Option<SlotModel>,
    pub opus: Option<SlotModel>,
    pub sonnet: Option<SlotModel>,
    pub haiku: Option<SlotModel>,
    pub fable: Option<SlotModel>,
    pub subagent: Option<SlotModel>,
}

/// What the gateway may advertise through `/v1/models` discovery.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "policy", content = "allowlist")]
pub enum DiscoveryPolicy {
    /// Discovery is not requested; the model set is clud's own.
    Off,
    /// Discovery is on and may add any row the route can serve.
    On,
    /// Discovery is on but clud filters it to exactly these ids.
    Filtered(Vec<String>),
}

/// The context window a launch tells the harness about.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ContextPolicy {
    /// `CLAUDE_CODE_MAX_CONTEXT_TOKENS`: the real served window, when known.
    pub max_tokens: Option<u32>,
    /// `CLAUDE_CODE_AUTO_COMPACT_WINDOW`: the compact threshold, when the
    /// catalog reviewed one for this wire id.
    pub auto_compact_window: Option<u32>,
}

/// The per-turn effort capability (#1528).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EffortPolicy {
    /// The `CLAUDE_CODE_MODEL_CAPABILITIES` token, e.g.
    /// `vendor/model=per_turn_effort`. `None` means this launch advertises no
    /// capability, which is the safe default: the harness then keeps its own.
    pub per_turn_capability: Option<String>,
}

/// OpenRouter's upstream routing object (`--provider-only`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProviderRouting {
    pub only: Vec<String>,
    pub allow_fallbacks: bool,
}

/// Whether a launch check is meaningful for a route, and why not when it is
/// not. `Unsupported` is a hard refusal that names the backend.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "state", content = "reason")]
pub enum Applicability {
    Applies,
    NotApplicable(String),
    Unsupported(String),
}

/// One launch check's placement and verdict, recorded so `--dry-run` can show
/// what ran. Step 5c (#1862) turns these into enforcement before the backend
/// branch; step 1 only classifies them.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LaunchCheck {
    pub name: String,
    pub applicability: Applicability,
    pub verdict: String,
}

/// Everything one launch decided, decided once.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ResolvedRoute {
    pub backend: RouteBackend,
    pub provider: ModelProvider,
    /// Every upstream this launch may reach, in preference order. One for the
    /// direct route; Claude first for the unified gateway.
    pub upstreams: Vec<UpstreamRoute>,
    pub slots: SlotModels,
    /// The boundary every slot, discovery row and bridge request is held to
    /// (#1257). Empty means unconstrained.
    pub allowlist: Vec<String>,
    pub discovery: DiscoveryPolicy,
    pub context: ContextPolicy,
    pub effort: EffortPolicy,
    pub upstream_routing: Option<ProviderRouting>,
    /// The effective per-request timeout, in milliseconds. `None` means this
    /// backend does not set one.
    pub timeout_ms: Option<u64>,
    /// Env keys every renderer of this route removes.
    pub scrub: Vec<String>,
    pub checks: Vec<LaunchCheck>,
}

impl ResolvedRoute {
    /// The environment variable a non-vault credential reads, when this route
    /// uses one. The native harnesses keep their key in the process
    /// environment, which is why they are the one `CredentialSource::Env`.
    pub fn credential_env(&self) -> Option<&str> {
        self.upstreams
            .iter()
            .find_map(|upstream| match &upstream.credential {
                CredentialSource::Env(var) => Some(var.as_str()),
                _ => None,
            })
    }
}

/// The one provider-to-route mapping. `failover::route_for` and
/// `codex_bridge::conversation_route_for` were identical copies of this table;
/// step 6 (#1864) deletes them and points their callers here.
pub const fn conversation_route(provider: ModelProvider) -> ConversationRoute {
    match provider {
        ModelProvider::Claude => ConversationRoute::Claude,
        ModelProvider::Codex => ConversationRoute::Codex,
        ModelProvider::DeepSeek => ConversationRoute::DeepSeek,
        ModelProvider::OpenRouter => ConversationRoute::OpenRouter,
        ModelProvider::Kimi => ConversationRoute::Kimi,
    }
}

fn is_codex_via_claude(plan: &LaunchPlan) -> bool {
    plan.model_provider() == ModelProvider::Codex && plan.effective_harness() == Backend::Claude
}

fn is_unified(plan: &LaunchPlan) -> bool {
    plan.routing_mode == RoutingMode::Unified && plan.effective_harness() == Backend::Claude
}

fn is_direct(plan: &LaunchPlan) -> bool {
    plan.effective_harness() == Backend::Claude
        && crate::provider_registry::descriptor_for(plan.model_provider()).is_some()
}

/// The active backend for a plan. Order matters and mirrors
/// `ForegroundRuntime::start_with_secret_store`'s branch, so the route and the
/// runtime can never disagree about which backend is running.
pub fn backend_for(plan: &LaunchPlan) -> RouteBackend {
    if is_unified(plan) {
        RouteBackend::Unified
    } else if is_codex_via_claude(plan) {
        RouteBackend::CodexBridge
    } else if is_direct(plan) {
        RouteBackend::Direct
    } else if plan.effective_harness() == Backend::DeepSeek {
        RouteBackend::DeepSeekNative
    } else {
        RouteBackend::Native
    }
}

/// The wire model a direct launch starts on: the selection's, else the
/// provider's reviewed catalog default.
fn direct_wire_model(
    provider: ModelProvider,
    selection: Option<&crate::provider_catalog::ResolvedModelSelection>,
) -> Option<&str> {
    selection
        .and_then(|selection| selection.wire_model.as_deref())
        .or_else(|| {
            crate::provider_catalog::reviewed_default_model(provider).map(|row| row.wire_id)
        })
}

/// The boundary a pin creates, with the Codex bridge's own injected role rows
/// folded in (DD-038's substitutions are inside the boundary by construction).
pub fn codex_bridge_allowlist(plan: &LaunchPlan) -> Vec<String> {
    let mut allowed = plan.allowed_models.clone();
    if !allowed.is_empty() {
        for role in [CODEX_BRIDGE_OPUS_MODEL, CODEX_BRIDGE_SONNET_MODEL] {
            if !allowed.iter().any(|entry| entry.eq_ignore_ascii_case(role)) {
                allowed.push(role.to_string());
            }
        }
    }
    allowed
}

/// The slot decision for every descriptor-driven backend: which row each slot
/// names, with the ambient subagent precedence applied exactly once.
///
/// This is the single copy of a rule that today exists twice, and whose two
/// copies already differ (divergence #1, DD-077 / #1257).
fn descriptor_slots(
    descriptor: &'static crate::provider_registry::AnthropicCompatProvider,
    selection: Option<&crate::provider_catalog::ResolvedModelSelection>,
    allowlist: &[String],
    ambient: &Ambient,
) -> SlotModels {
    let model = direct_wire_model(descriptor.provider, selection).unwrap_or_default();
    let role_models = descriptor.role_models;
    let served_subagent = crate::server_settings::provider_subagent_model(descriptor.provider)
        .unwrap_or(descriptor.subagent_wire_id);
    let constrained = crate::provider_catalog::model_allowlist_slot(allowlist, selection);
    let ambient_subagent = ambient
        .subagent_model
        .as_deref()
        .filter(|_| constrained.is_some())
        .filter(|value| crate::provider_catalog::model_allowlist_allows(allowlist, value));
    let (opus, sonnet, haiku, subagent, fable) = match constrained.as_deref() {
        // A pin covers every slot, and fable with it: an OpenRouter key bills
        // whatever id the harness sends, aliases included.
        Some(constrained) => (
            constrained,
            constrained,
            constrained,
            ambient_subagent.unwrap_or(constrained),
            Some(constrained),
        ),
        // Unconstrained: the provider's own role table is authoritative. A
        // provider without one is single-profile, so every slot is the main
        // model -- except fable, which the overlay pins to main only in that
        // case. A provider *with* a table pins fable only if the table names
        // one; `None` there means "leave the slot alone".
        None => (
            role_models.map_or(model, |roles| roles.opus),
            role_models.map_or(model, |roles| roles.sonnet),
            role_models.map_or(served_subagent, |roles| roles.haiku),
            ambient_subagent
                .unwrap_or_else(|| role_models.map_or(served_subagent, |roles| roles.subagent)),
            role_models.map_or(Some(model), |roles| roles.fable),
        ),
    };
    let main = constrained.as_deref().unwrap_or(model);
    SlotModels {
        main: SlotModel::of(main),
        opus: SlotModel::of(opus),
        sonnet: SlotModel::of(sonnet),
        haiku: SlotModel::of(haiku),
        subagent: SlotModel::of(subagent),
        fable: fable.and_then(SlotModel::of),
    }
}

/// The context window a wire id is served with: the catalog's reviewed compact
/// window and the served datasheet's exact window, keyed on the same id on
/// every backend (divergence #3).
fn context_policy(wire_id: Option<&str>) -> ContextPolicy {
    let Some(wire_id) = wire_id else {
        return ContextPolicy::default();
    };
    ContextPolicy {
        max_tokens: crate::server_settings::effective_context_window(wire_id),
        auto_compact_window: crate::provider_catalog::model_by_wire_id(wire_id)
            .and_then(|row| row.claude_compact_window),
    }
}

fn upstream_for(
    provider: ModelProvider,
    base_url: &str,
    credential: CredentialSource,
) -> UpstreamRoute {
    UpstreamRoute {
        provider,
        base_url: base_url.to_string(),
        credential,
        conversation_route: conversation_route(provider),
    }
}

fn scrub_list(backend: RouteBackend, provider: ModelProvider) -> Vec<String> {
    let mut keys: Vec<String> = match backend {
        RouteBackend::Direct => ANTHROPIC_COMPAT_SCRUB
            .iter()
            .map(|k| k.to_string())
            .collect(),
        RouteBackend::CodexBridge => CODEX_VIA_CLAUDE_SCRUB
            .iter()
            .map(|k| k.to_string())
            .collect(),
        // The gateway replaces only its own two keys; it deliberately leaves
        // the Claude credential in place so native auth reaches the upstream.
        RouteBackend::Unified => vec![
            "ANTHROPIC_BASE_URL".to_string(),
            "ANTHROPIC_CUSTOM_HEADERS".to_string(),
        ],
        RouteBackend::Native | RouteBackend::DeepSeekNative => Vec::new(),
    };
    // The direct route additionally drops this provider's own ambient key:
    // an OpenRouter child must never inherit one from the parent shell.
    if backend == RouteBackend::Direct && provider == ModelProvider::OpenRouter {
        keys.push("OPENROUTER_API_KEY".to_string());
    }
    // Only the overlays that rewrite the model slots drop the user's own
    // custom model options; the gateway leaves them alone entirely.
    if matches!(backend, RouteBackend::Direct | RouteBackend::CodexBridge) {
        keys.push(CUSTOM_MODEL_OPTION_PREFIX.to_string());
    }
    keys
}

/// The launch checks this route is subject to, with today's placement recorded
/// in the verdict. Enforcement moves in front of the backend branch in step 5c
/// (#1862); step 1 classifies without changing what runs.
fn launch_checks(
    route: &ResolvedRoute,
    free: Option<&str>,
    provider_only: &[String],
) -> Vec<LaunchCheck> {
    let backend = route.backend;
    let openrouter = route.provider == ModelProvider::OpenRouter;
    let free_applicability =
        if openrouter && matches!(backend, RouteBackend::Direct | RouteBackend::Unified) {
            Applicability::Applies
        } else if openrouter {
            Applicability::NotApplicable(
                "this harness does not route an OpenRouter `:free` id through clud".to_string(),
            )
        } else {
            Applicability::NotApplicable("the launch does not name an OpenRouter model".to_string())
        };
    let free_verdict = match free {
        Some(verdict) if backend == RouteBackend::Direct => {
            format!("checked against the offline catalog: {verdict}")
        }
        Some(_) => "not run on this backend today; the catalog verdict is the same everywhere \
                   (#1862)"
            .to_string(),
        None => "no `:free` wire id on this launch".to_string(),
    };
    let provider_only_applicability = if provider_only.is_empty() {
        Applicability::NotApplicable("no --provider-only slugs on this launch".to_string())
    } else if matches!(
        backend,
        RouteBackend::CodexBridge | RouteBackend::DeepSeekNative
    ) {
        Applicability::Unsupported(
            "--provider-only applies only to an OpenRouter launch clud routes".to_string(),
        )
    } else if openrouter {
        Applicability::Applies
    } else {
        Applicability::NotApplicable(
            "only OpenRouter publishes an upstream routing object".to_string(),
        )
    };
    let provider_only_verdict = if provider_only.is_empty() {
        "nothing to pin".to_string()
    } else {
        match backend {
            RouteBackend::Direct => "rendered into CLAUDE_CODE_EXTRA_BODY".to_string(),
            RouteBackend::Unified => {
                "the gateway injects provider.only per request (#1863)".to_string()
            }
            _ => "refused before launch".to_string(),
        }
    };
    vec![
        LaunchCheck {
            name: "openrouter_free_cost".to_string(),
            applicability: free_applicability,
            verdict: free_verdict,
        },
        LaunchCheck {
            name: "provider_only".to_string(),
            applicability: provider_only_applicability,
            verdict: provider_only_verdict,
        },
    ]
}

/// Resolve the route against the process's cached catalog.
pub fn resolve(plan: &LaunchPlan, ambient: &Ambient) -> ResolvedRoute {
    resolve_with_catalog(
        plan,
        ambient,
        &crate::openrouter_catalog::catalog_cached_or_embedded(),
        crate::server_settings::per_turn_effort_enabled(),
    )
}

/// What one backend's branch contributes before the shared policies are keyed
/// on it.
struct BackendPlan {
    upstreams: Vec<UpstreamRoute>,
    slots: SlotModels,
    allowlist: Vec<String>,
    discovery: DiscoveryPolicy,
}

/// The direct route: one descriptor, one key, clud not in the request path.
fn direct_plan(
    provider: ModelProvider,
    selection: Option<&crate::provider_catalog::ResolvedModelSelection>,
    allowlist: Vec<String>,
    ambient: &Ambient,
) -> BackendPlan {
    let descriptor = crate::provider_registry::descriptor_for(provider)
        .expect("a direct route always has a descriptor");
    let slots = descriptor_slots(descriptor, selection, &allowlist, ambient);
    // #1257: discovery only adds rows and cannot subtract them (DD-054), so a
    // constrained launch does not ask for it at all rather than advertising a
    // set the allowlist would then have to be enforced against afterwards.
    let discovery = if descriptor.enable_gateway_model_discovery && allowlist.is_empty() {
        DiscoveryPolicy::On
    } else {
        DiscoveryPolicy::Off
    };
    BackendPlan {
        upstreams: vec![upstream_for(
            provider,
            descriptor.anthropic_base_url,
            CredentialSource::Vault(provider),
        )],
        slots,
        allowlist,
        discovery,
    }
}

/// The unified gateway: one local gateway in front of the harness, holding a
/// route per Anthropic-compatible provider plus the native Claude and Codex
/// upstreams.
fn unified_plan(
    provider: ModelProvider,
    selection: Option<&crate::provider_catalog::ResolvedModelSelection>,
    allowlist: Vec<String>,
    ambient: &Ambient,
) -> BackendPlan {
    // Which of these can serve right now is a launch-time credential probe,
    // deliberately not a resolution input.
    let mut upstreams = vec![upstream_for(
        ModelProvider::Claude,
        crate::codex_bridge::ANTHROPIC_MESSAGES_BASE_URL,
        CredentialSource::Harness,
    )];
    for descriptor in crate::provider_registry::ANTHROPIC_COMPAT_PROVIDERS {
        upstreams.push(upstream_for(
            descriptor.provider,
            descriptor.anthropic_base_url,
            CredentialSource::Vault(descriptor.provider),
        ));
    }
    upstreams.push(upstream_for(
        ModelProvider::Codex,
        crate::codex_bridge::ANTHROPIC_MESSAGES_BASE_URL,
        CredentialSource::CodexAuth,
    ));
    // The slot decision is the direct route's, made by the same function:
    // divergence #1 is that today's gateway copy does not make it at all.
    let slots = match crate::provider_registry::descriptor_for(provider) {
        Some(descriptor) => descriptor_slots(descriptor, selection, &allowlist, ambient),
        None => SlotModels::default(),
    };
    let discovery = if allowlist.is_empty() {
        DiscoveryPolicy::On
    } else {
        DiscoveryPolicy::Filtered(allowlist.clone())
    };
    BackendPlan {
        upstreams,
        slots,
        allowlist,
        discovery,
    }
}

/// The Codex translation bridge behind the Claude harness.
fn codex_bridge_plan(plan: &LaunchPlan) -> BackendPlan {
    let selection = plan.model_selection.as_ref();
    let main = crate::codex_model::ModelSpec::parse(
        selection
            .and_then(|selection| selection.wire_model.as_deref())
            .or(plan.codex_model.as_deref())
            .unwrap_or_default(),
    )
    .ok()
    .map(|spec| spec.model);
    let slots = SlotModels {
        main: main.as_deref().and_then(SlotModel::of),
        opus: SlotModel::of(CODEX_BRIDGE_OPUS_MODEL),
        sonnet: SlotModel::of(CODEX_BRIDGE_SONNET_MODEL),
        ..SlotModels::default()
    };
    BackendPlan {
        upstreams: vec![upstream_for(
            ModelProvider::Codex,
            crate::codex_bridge::ANTHROPIC_MESSAGES_BASE_URL,
            CredentialSource::CodexAuth,
        )],
        slots,
        allowlist: codex_bridge_allowlist(plan),
        discovery: DiscoveryPolicy::On,
    }
}

/// The DeepSeek-native harness: `dsh` owns the request path, and clud hands it
/// a key in the environment plus, for OpenRouter, a `--patch` overlay.
fn deepseek_native_plan(
    provider: ModelProvider,
    selection: Option<&crate::provider_catalog::ResolvedModelSelection>,
) -> BackendPlan {
    let key_provider = match provider {
        ModelProvider::OpenRouter => ModelProvider::OpenRouter,
        _ => ModelProvider::DeepSeek,
    };
    let var = crate::dsh_harness::key_env_for(key_provider)
        .unwrap_or("DEEPSEEK_API_KEY")
        .to_string();
    // dsh resolves its OpenRouter route from its own bundled catalog, so the
    // model clud names in the `--patch` overlay is `openrouter_model`'s. On
    // its DeepSeek route it reads the selected model directly.
    let main = match provider {
        ModelProvider::OpenRouter => crate::dsh_harness::openrouter_model(selection),
        _ => selection.and_then(|selection| selection.wire_model.as_deref()),
    }
    .map(str::to_string);
    BackendPlan {
        upstreams: vec![upstream_for(
            key_provider,
            crate::provider_registry::descriptor_for(key_provider)
                .map(|descriptor| descriptor.anthropic_base_url)
                .unwrap_or_default(),
            CredentialSource::Env(var),
        )],
        slots: SlotModels {
            main: main.as_deref().and_then(SlotModel::of),
            ..SlotModels::default()
        },
        allowlist: Vec::new(),
        discovery: DiscoveryPolicy::Off,
    }
}

/// Resolve the route from explicit inputs, so a test never needs the daemon's
/// catalog cache or the served server settings.
pub fn resolve_with_catalog(
    plan: &LaunchPlan,
    ambient: &Ambient,
    catalog: &crate::openrouter_catalog::Catalog,
    per_turn_effort_enabled: bool,
) -> ResolvedRoute {
    let backend = backend_for(plan);
    let provider = plan.model_provider();
    let selection = plan.model_selection.as_ref();
    let planned = match backend {
        RouteBackend::Direct => {
            direct_plan(provider, selection, plan.allowed_models.clone(), ambient)
        }
        RouteBackend::Unified => {
            unified_plan(provider, selection, plan.allowed_models.clone(), ambient)
        }
        RouteBackend::CodexBridge => codex_bridge_plan(plan),
        RouteBackend::DeepSeekNative => deepseek_native_plan(provider, selection),
        RouteBackend::Native => BackendPlan {
            upstreams: Vec::new(),
            slots: SlotModels::default(),
            allowlist: Vec::new(),
            discovery: DiscoveryPolicy::Off,
        },
    };
    let BackendPlan {
        upstreams,
        slots,
        allowlist,
        discovery,
    } = planned;
    // The main wire id the context and effort policies are keyed on: the
    // slot's own spelling, which is what every renderer actually writes.
    let main_wire = slots.main.as_ref().map(|slot| slot.wire_id.clone());
    let mut route = ResolvedRoute {
        backend,
        provider,
        upstreams,
        slots,
        allowlist,
        discovery,
        context: context_policy(main_wire.as_deref()),
        effort: EffortPolicy {
            per_turn_capability: main_wire.as_deref().and_then(|wire| {
                crate::foreground_runtime::per_turn_effort_capability(
                    catalog,
                    wire,
                    per_turn_effort_enabled,
                )
            }),
        },
        upstream_routing: provider_routing(provider, &plan.provider_only),
        timeout_ms: match backend {
            RouteBackend::Direct => DIRECT_TIMEOUT_MS.parse().ok(),
            RouteBackend::Unified | RouteBackend::CodexBridge => BRIDGE_TIMEOUT_MS.parse().ok(),
            RouteBackend::Native | RouteBackend::DeepSeekNative => None,
        },
        scrub: scrub_list(backend, provider),
        checks: Vec::new(),
    };
    let free = plan
        .model_selection
        .as_ref()
        .and_then(|selection| selection.wire_model.as_deref())
        .filter(|_| provider == ModelProvider::OpenRouter)
        .map(|wire| {
            crate::openrouter_free::check(wire, catalog)
                .as_str()
                .to_string()
        });
    route.checks = launch_checks(&route, free.as_deref(), &plan.provider_only);
    route
}

/// OpenRouter's upstream routing object, when the launch asked for one.
/// Validation of the slugs themselves stays where the refusal is printed.
fn provider_routing(provider: ModelProvider, slugs: &[String]) -> Option<ProviderRouting> {
    // Only OpenRouter publishes this object; another provider's `--provider-only`
    // is a launch error the CLI already refuses.
    if slugs.is_empty() || provider != ModelProvider::OpenRouter {
        return None;
    }
    Some(ProviderRouting {
        only: slugs.to_vec(),
        allow_fallbacks: false,
    })
}

#[cfg(test)]
#[path = "route_plan_tests.rs"]
mod tests;
