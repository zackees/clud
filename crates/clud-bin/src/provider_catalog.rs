//! Provider-neutral model registry and launch-selection normalization.
//!
//! Clud CLI/settings IDs, Claude gateway discovery IDs, and provider wire IDs
//! are distinct namespaces. This module is the only mapping authority among
//! them; direct launches and the unified gateway consume the same rows.

use serde::{Deserialize, Serialize};

use crate::backend::ModelProvider;

/// Provider-neutral launch effort. Individual providers decide which values
/// they accept and how they translate them at their wire boundary.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum EffortLevel {
    None,
    Low,
    Medium,
    High,
    XHigh,
    Max,
}

impl EffortLevel {
    pub const ALL: [Self; 6] = [
        Self::None,
        Self::Low,
        Self::Medium,
        Self::High,
        Self::XHigh,
        Self::Max,
    ];

    pub const fn as_str(self) -> &'static str {
        match self {
            Self::None => "none",
            Self::Low => "low",
            Self::Medium => "medium",
            Self::High => "high",
            Self::XHigh => "xhigh",
            Self::Max => "max",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        let value = value.trim().to_ascii_lowercase();
        Self::ALL
            .into_iter()
            .find(|effort| effort.as_str() == value)
    }

    pub(crate) fn catalog() -> String {
        Self::ALL
            .iter()
            .map(|effort| effort.as_str())
            .collect::<Vec<_>>()
            .join(", ")
    }
}

impl std::fmt::Display for EffortLevel {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(self.as_str())
    }
}

const ALL_CODEX_EFFORTS: &[EffortLevel] = &[
    EffortLevel::None,
    EffortLevel::Low,
    EffortLevel::Medium,
    EffortLevel::High,
    EffortLevel::XHigh,
    EffortLevel::Max,
];
const ANTHROPIC_EFFORTS: &[EffortLevel] = &[
    EffortLevel::Low,
    EffortLevel::Medium,
    EffortLevel::High,
    EffortLevel::XHigh,
    EffortLevel::Max,
];
/// Kimi K3 documents exactly three reasoning-effort levels -- low, high, and
/// max (default max). Deliberately NOT `ANTHROPIC_EFFORTS`: medium and xhigh
/// are not part of K3's contract and must be rejected before an upstream
/// request. See https://platform.kimi.ai/docs/guide/kimi-k3-quickstart.
const KIMI_EFFORTS: &[EffortLevel] = &[EffortLevel::Low, EffortLevel::High, EffortLevel::Max];
const AUTO_CONTEXT: &[&str] = &["auto"];
const AUTO_OR_1M_CONTEXT: &[&str] = &["auto", "1m"];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CatalogModel {
    pub cli_id: &'static str,
    pub provider: ModelProvider,
    pub wire_id: &'static str,
    pub discovery_id: Option<&'static str>,
    pub display_name: &'static str,
    pub legacy_aliases: &'static [&'static str],
    pub supported_efforts: &'static [EffortLevel],
    pub supported_context_windows: &'static [&'static str],
    pub default_effort: Option<EffortLevel>,
    pub default_context_window: Option<&'static str>,
    /// Reviewed direct-launch default for this provider. Claude intentionally
    /// has no row marked: its harness-owned default remains authoritative.
    pub provider_default: bool,
    /// The model's real input context ceiling when Claude Code reaches it
    /// through a discovery ID that the harness does not natively recognize.
    /// Provider-scoped discovery sessions require one common non-`None` value
    /// across every advertised row; [`common_claude_context_tokens`] enforces
    /// that contract instead of letting an adapter restate the number.
    pub claude_max_context_tokens: Option<u32>,
    /// `CLAUDE_CODE_AUTO_COMPACT_WINDOW` value to set in the child-env overlay
    /// when this row's wire ID is selected, or `None` if the overlay should
    /// leave the variable unset. Replaces a hardcoded `model.ends_with("[1m]")`
    /// check with catalog data (#937 Phase 2).
    pub claude_compact_window: Option<u32>,
    /// Whether the provider's endpoint accepts Anthropic `image` content
    /// blocks for this model (#1200).
    ///
    /// A silently-dropped image is the failure this guards against: the
    /// endpoint can answer `200` with the image replaced by a text
    /// placeholder, so no status code and no stream event reveals it. `false`
    /// therefore means *verified to drop* — it is evidence-backed, and it is
    /// what raises the launch notice. `true` means no such drop has been
    /// observed for the row; it is not a promise about a model nobody has
    /// probed. Do not flip a row to `false` to silence a warning you have not
    /// reproduced against the live endpoint.
    pub supports_images: bool,
}

pub const MODELS: &[CatalogModel] = &[
    CatalogModel {
        cli_id: "codex-sol",
        provider: ModelProvider::Codex,
        wire_id: "gpt-5.6-sol",
        discovery_id: Some("clud-claude-codex-sol"),
        display_name: "Codex Sol (OpenAI)",
        legacy_aliases: &["sol"],
        supported_efforts: ALL_CODEX_EFFORTS,
        supported_context_windows: AUTO_CONTEXT,
        default_effort: Some(EffortLevel::Low),
        default_context_window: None,
        provider_default: true,
        claude_max_context_tokens: Some(1_050_000),
        claude_compact_window: None,
        supports_images: true,
    },
    CatalogModel {
        cli_id: "codex-terra",
        provider: ModelProvider::Codex,
        wire_id: "gpt-5.6-terra",
        discovery_id: Some("clud-claude-codex-terra"),
        display_name: "Codex Terra (OpenAI)",
        legacy_aliases: &["terra"],
        supported_efforts: ALL_CODEX_EFFORTS,
        supported_context_windows: AUTO_CONTEXT,
        default_effort: Some(EffortLevel::Medium),
        default_context_window: None,
        provider_default: false,
        claude_max_context_tokens: Some(1_050_000),
        claude_compact_window: None,
        supports_images: true,
    },
    CatalogModel {
        cli_id: "codex-luna",
        provider: ModelProvider::Codex,
        wire_id: "gpt-5.6-luna",
        discovery_id: Some("clud-claude-codex-luna"),
        display_name: "Codex Luna (OpenAI)",
        legacy_aliases: &["luna"],
        supported_efforts: ALL_CODEX_EFFORTS,
        supported_context_windows: AUTO_CONTEXT,
        default_effort: Some(EffortLevel::Medium),
        default_context_window: None,
        provider_default: false,
        claude_max_context_tokens: Some(1_050_000),
        claude_compact_window: None,
        supports_images: true,
    },
    CatalogModel {
        cli_id: "deepseek-v4-pro",
        provider: ModelProvider::DeepSeek,
        wire_id: "deepseek-v4-pro[1m]",
        discovery_id: Some("clud-claude-deepseek-v4-pro-0813"),
        // DeepSeek keeps the stable API slug while upgrading the served
        // checkpoint. The 2026-08-12 pricing page identifies this alias as
        // DeepSeek-V4-Pro-0813.
        display_name: "DeepSeek V4 Pro 0813",
        legacy_aliases: &["deepseek-v4-pro[1m]", "clud-claude-deepseek-v4-pro"],
        supported_efforts: ANTHROPIC_EFFORTS,
        supported_context_windows: AUTO_OR_1M_CONTEXT,
        default_effort: Some(EffortLevel::Low),
        default_context_window: Some("1m"),
        // Still served, but no longer the default: DeepSeek's documented
        // Claude Code profile moved to V4.1 Flash (#1192).
        provider_default: false,
        claude_max_context_tokens: None,
        claude_compact_window: Some(786_432),
        // Verified against the live endpoint (#1200): DeepSeek's anthropic
        // surface replaces an image block with a literal `[Unsupported Image]`
        // placeholder for this model and still answers 200, so the drop is
        // invisible to every caller. Do not flip this without a live probe.
        supports_images: false,
    },
    CatalogModel {
        cli_id: "deepseek-flash",
        provider: ModelProvider::DeepSeek,
        wire_id: "deepseek-flash[1m]",
        discovery_id: Some("clud-claude-deepseek-flash"),
        // DeepSeek-V4.1-Flash. DeepSeek renamed the slug from
        // `deepseek-v4-flash`, which it still routes for compatibility, so the
        // old spelling and discovery ID stay aliases of this row.
        display_name: "DeepSeek V4.1 Flash",
        legacy_aliases: &[
            "deepseek-flash[1m]",
            "deepseek-v4-flash",
            "clud-claude-deepseek-v4-flash",
        ],
        supported_efforts: ANTHROPIC_EFFORTS,
        supported_context_windows: AUTO_OR_1M_CONTEXT,
        default_effort: Some(EffortLevel::Low),
        default_context_window: Some("1m"),
        // The compiled-in default. A direct launch normally takes the served
        // name from `server_settings` first, which the guard test there keeps
        // equal to this row.
        provider_default: true,
        claude_max_context_tokens: None,
        claude_compact_window: Some(786_432),
        supports_images: true,
    },
    CatalogModel {
        cli_id: "kimi-k3",
        provider: ModelProvider::Kimi,
        wire_id: "kimi-k3[1m]",
        discovery_id: Some("clud-claude-kimi-k3"),
        display_name: "Kimi K3",
        legacy_aliases: &["kimi-k3[1m]"],
        supported_efforts: KIMI_EFFORTS,
        supported_context_windows: AUTO_OR_1M_CONTEXT,
        default_effort: Some(EffortLevel::Low),
        default_context_window: Some("1m"),
        provider_default: true,
        claude_max_context_tokens: None,
        claude_compact_window: Some(1_048_576),
        supports_images: true,
    },
    CatalogModel {
        cli_id: "openrouter-claude-sonnet",
        provider: ModelProvider::OpenRouter,
        wire_id: "~anthropic/claude-sonnet-latest",
        // One reviewed row, advertised only in unified mode so the route can be
        // selected and failed over (#968). OpenRouter's live gateway discovery
        // still owns its changing inventory in a direct `--openrouter` launch;
        // that inventory is deliberately not mirrored here.
        discovery_id: Some("clud-claude-openrouter-sonnet"),
        display_name: "Claude Sonnet via OpenRouter",
        legacy_aliases: &[],
        supported_efforts: ANTHROPIC_EFFORTS,
        supported_context_windows: AUTO_CONTEXT,
        default_effort: Some(EffortLevel::Low),
        default_context_window: None,
        provider_default: true,
        claude_max_context_tokens: None,
        claude_compact_window: None,
        supports_images: true,
    },
    // Claude tier aliases are compatibility rows. Versioned Claude inventory
    // can be added without changing the stable provider-qualified grammar.
    CatalogModel {
        cli_id: "claude-opus",
        provider: ModelProvider::Claude,
        wire_id: "opus",
        discovery_id: None,
        display_name: "Claude Opus",
        legacy_aliases: &["opus"],
        supported_efforts: ANTHROPIC_EFFORTS,
        supported_context_windows: AUTO_CONTEXT,
        default_effort: None,
        default_context_window: None,
        provider_default: false,
        claude_max_context_tokens: None,
        claude_compact_window: None,
        supports_images: true,
    },
    CatalogModel {
        cli_id: "claude-sonnet",
        provider: ModelProvider::Claude,
        wire_id: "sonnet",
        discovery_id: None,
        display_name: "Claude Sonnet",
        legacy_aliases: &["sonnet"],
        supported_efforts: ANTHROPIC_EFFORTS,
        supported_context_windows: AUTO_CONTEXT,
        default_effort: None,
        default_context_window: None,
        provider_default: false,
        claude_max_context_tokens: None,
        claude_compact_window: None,
        supports_images: true,
    },
    CatalogModel {
        cli_id: "claude-haiku",
        provider: ModelProvider::Claude,
        wire_id: "haiku",
        discovery_id: None,
        display_name: "Claude Haiku",
        legacy_aliases: &["haiku"],
        supported_efforts: ANTHROPIC_EFFORTS,
        supported_context_windows: AUTO_CONTEXT,
        default_effort: None,
        default_context_window: None,
        provider_default: false,
        claude_max_context_tokens: None,
        claude_compact_window: None,
        supports_images: true,
    },
];

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SelectionSource {
    Cli,
    LegacyModelSuffix,
    ProviderSetting,
    /// A served model name (`server_settings`, #1192): the live, cached, or
    /// embedded copy of `assets/server-settings.json`.
    ServerDefault,
    CatalogDefault,
}

/// Provider-specific saved values supplied to direct launch resolution.
/// Every value is already typed except the catalog model ID and context token.
#[derive(Debug, Clone, Copy, Default)]
pub struct ProviderSelectionDefaults<'a> {
    pub model: Option<&'a str>,
    pub effort: Option<EffortLevel>,
    pub context_window: Option<&'a str>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ResolvedModelSelection {
    pub provider: ModelProvider,
    /// Stable clud CLI/settings identifier, e.g. `codex-terra`.
    pub model: Option<String>,
    /// Provider-native request identifier. Never a credential.
    pub wire_model: Option<String>,
    #[serde(default)]
    pub effort: Option<EffortLevel>,
    #[serde(default)]
    pub context_window: Option<String>,
    #[serde(default)]
    pub model_source: Option<SelectionSource>,
    #[serde(default)]
    pub effort_source: Option<SelectionSource>,
    #[serde(default)]
    pub context_window_source: Option<SelectionSource>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SelectionError {
    UnknownModel(String),
    ProviderConflict {
        provider: ModelProvider,
        model: String,
        model_provider: ModelProvider,
    },
    InvalidEffort(String),
    InvalidContextWindow(String),
    UnsupportedEffort {
        model: String,
        effort: String,
    },
    UnsupportedContextWindow {
        model: String,
        context_window: String,
    },
    ContextRequiresModel {
        provider: ModelProvider,
        context_window: String,
    },
    ConflictingEffort {
        legacy: String,
        explicit: String,
    },
    ConflictingContextWindow {
        legacy: String,
        explicit: String,
    },
}

impl std::fmt::Display for SelectionError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::UnknownModel(model) => write!(f, "unknown model '{model}'"),
            Self::ProviderConflict {
                provider,
                model,
                model_provider,
            } => write!(
                f,
                "model '{model}' belongs to provider '{model_provider}', but the explicit provider is '{provider}'"
            ),
            Self::InvalidEffort(effort) => write!(f, "invalid reasoning effort '{effort}'"),
            Self::InvalidContextWindow(value) => {
                write!(f, "invalid context window '{value}'; expected auto or 1m")
            }
            Self::UnsupportedEffort { model, effort } => {
                write!(f, "model '{model}' does not support effort '{effort}'")
            }
            Self::UnsupportedContextWindow {
                model,
                context_window,
            } => write!(
                f,
                "model '{model}' does not support context window '{context_window}'"
            ),
            Self::ContextRequiresModel {
                provider,
                context_window,
            } => write!(
                f,
                "provider '{provider}' requires --model when --context-window is '{context_window}'"
            ),
            Self::ConflictingEffort { legacy, explicit } => write!(
                f,
                "model suffix selected effort '{legacy}', but --effort selected '{explicit}'"
            ),
            Self::ConflictingContextWindow { legacy, explicit } => write!(
                f,
                "model suffix selected context window '{legacy}', but --context-window selected '{explicit}'"
            ),
        }
    }
}

impl std::error::Error for SelectionError {}

fn catalog_match(value: &str) -> Option<CatalogModel> {
    MODELS.iter().copied().find(|entry| {
        entry.cli_id.eq_ignore_ascii_case(value)
            || entry.wire_id.eq_ignore_ascii_case(value)
            || entry
                .discovery_id
                .is_some_and(|id| id.eq_ignore_ascii_case(value))
            || entry
                .legacy_aliases
                .iter()
                .any(|alias| alias.eq_ignore_ascii_case(value))
    })
}

pub fn model_by_cli_id(value: &str) -> Option<CatalogModel> {
    MODELS.iter().copied().find(|entry| entry.cli_id == value)
}

/// Canonical CLI IDs retired by a provider rename, mapped to the successor
/// row's CLI ID. Settings written before the rename store the old canonical ID,
/// so settings validation accepts these while still rejecting wire IDs and
/// shorthand aliases (#1192).
const RETIRED_CLI_IDS: &[(&str, &str)] = &[("deepseek-v4-flash", "deepseek-flash")];

pub fn model_by_retired_cli_id(value: &str) -> Option<CatalogModel> {
    RETIRED_CLI_IDS
        .iter()
        .find(|(retired, _)| *retired == value)
        .and_then(|(_, current)| model_by_cli_id(current))
}

/// Exact wire-ID lookup, deliberately narrower than [`catalog_match`] (which
/// also matches `cli_id` and aliases). The child-env overlay uses this to
/// look up `claude_compact_window`: an auto-context wire model like
/// `"deepseek-v4-pro"` must NOT fall back to matching the `[1m]` row via its
/// `cli_id` and pick up that row's compaction window.
pub fn model_by_wire_id(value: &str) -> Option<CatalogModel> {
    MODELS.iter().copied().find(|entry| entry.wire_id == value)
}

/// Resolve a model identifier emitted by Claude's gateway model picker.
/// Versioned discovery IDs make the served checkpoint visible in the UI;
/// retired IDs remain accepted so an already-selected or cached row does not
/// silently fall through to native Claude routing after a catalog refresh.
pub fn model_by_discovery_id(value: &str) -> Option<CatalogModel> {
    MODELS.iter().copied().find(|entry| {
        entry
            .discovery_id
            .is_some_and(|id| id.eq_ignore_ascii_case(value))
            || entry
                .legacy_aliases
                .iter()
                .any(|alias| alias.starts_with("clud-claude-") && alias.eq_ignore_ascii_case(value))
    })
}

/// Resolve a model string a persisted or continued session may still carry:
/// a provider wire ID, CLI ID, or legacy alias. Discovery IDs are resolved by
/// [`model_by_discovery_id`]; this covers the remaining namespaces so a
/// gateway request naming `gpt-*` or `deepseek-*` routes to its own provider
/// instead of being proxied to Anthropic as an "ordinary Claude" model.
/// Claude-owned rows are excluded — those remain caller-owned native IDs.
/// Any catalog row matching `value` by CLI id, wire id, discovery id, or a
/// legacy alias -- including Claude and OpenRouter rows.
///
/// This answers "does clud know this ID at all", which is a different question
/// from "can this route serve it" ([`non_claude_model_by_any_id`], which
/// excludes Claude and OpenRouter). Conflating the two made the bridge tell a
/// user that `opus` or `openrouter-claude-sonnet` does not exist, when the
/// accurate answer is that clud knows it and this gateway is not serving it
/// (#1010).
pub fn model_by_any_id(value: &str) -> Option<CatalogModel> {
    catalog_match(value)
}

pub fn non_claude_model_by_any_id(value: &str) -> Option<CatalogModel> {
    catalog_match(value).filter(|entry| {
        entry.provider != ModelProvider::Claude && entry.provider != ModelProvider::OpenRouter
    })
}

pub fn models_for_provider(provider: ModelProvider) -> impl Iterator<Item = CatalogModel> {
    MODELS
        .iter()
        .copied()
        .filter(move |entry| entry.provider == provider)
}

pub fn reviewed_default_model(provider: ModelProvider) -> Option<CatalogModel> {
    models_for_provider(provider).find(|entry| entry.provider_default)
}

/// Return the common real context ceiling for a provider-scoped Claude
/// discovery catalog.
///
/// Claude Code's `CLAUDE_CODE_MAX_CONTEXT_TOKENS` is process-wide, while a
/// provider-scoped picker can switch among every registered row. Returning
/// `None` for missing or inconsistent metadata forces a future model addition
/// to make that policy explicit instead of silently inheriting a stale value.
pub fn common_claude_context_tokens(provider: ModelProvider) -> Option<u32> {
    let mut models = models_for_provider(provider);
    let context_tokens = models.next()?.claude_max_context_tokens?;
    models
        .all(|entry| entry.claude_max_context_tokens == Some(context_tokens))
        .then_some(context_tokens)
}

pub fn supported_efforts(provider: ModelProvider) -> &'static [EffortLevel] {
    provider_efforts(provider)
}

pub fn supported_context_windows(provider: ModelProvider) -> &'static [&'static str] {
    provider_context_windows(provider)
}

fn split_effort_suffix(raw: &str) -> (&str, Option<&str>) {
    raw.rsplit_once('@').map_or((raw, None), |(model, effort)| {
        (model.trim(), Some(effort.trim()))
    })
}

fn split_context_suffix(raw: &str) -> (&str, Option<&str>) {
    raw.strip_suffix("[1m]")
        .map_or((raw, None), |model| (model, Some("1m")))
}

pub(crate) fn inferred_provider_from_wire(value: &str) -> Option<ModelProvider> {
    let lower = value.to_ascii_lowercase();
    ModelProvider::ALL.iter().copied().find(|provider| {
        provider
            .wire_prefixes()
            .iter()
            .any(|prefix| lower.starts_with(prefix))
    })
}

fn provider_efforts(provider: ModelProvider) -> &'static [EffortLevel] {
    match provider {
        ModelProvider::Codex => ALL_CODEX_EFFORTS,
        ModelProvider::Claude | ModelProvider::DeepSeek | ModelProvider::OpenRouter => {
            ANTHROPIC_EFFORTS
        }
        // K3 documents only low/high/max; the narrower per-model slice on the
        // catalog row is what actually rejects an unsupported value.
        ModelProvider::Kimi => KIMI_EFFORTS,
    }
}

fn provider_context_windows(provider: ModelProvider) -> &'static [&'static str] {
    match provider {
        ModelProvider::Claude | ModelProvider::Codex | ModelProvider::OpenRouter => AUTO_CONTEXT,
        ModelProvider::DeepSeek | ModelProvider::Kimi => AUTO_OR_1M_CONTEXT,
    }
}

fn validate_capabilities(
    label: &str,
    efforts: &[EffortLevel],
    contexts: &[&str],
    effort: Option<EffortLevel>,
    context: Option<&str>,
) -> Result<(), SelectionError> {
    if let Some(value) = effort {
        if !efforts.contains(&value) {
            return Err(SelectionError::UnsupportedEffort {
                model: label.to_string(),
                effort: value.as_str().to_string(),
            });
        }
    }
    if let Some(value) = context {
        if !contexts.contains(&value) {
            return Err(SelectionError::UnsupportedContextWindow {
                model: label.to_string(),
                context_window: value.to_string(),
            });
        }
    }
    Ok(())
}

/// Infer a provider from a registered CLI/alias/wire ID or an unambiguous
/// provider wire prefix. Compound compatibility suffixes are ignored.
pub fn infer_provider(model: &str) -> Option<ModelProvider> {
    let (without_effort, _) = split_effort_suffix(model.trim());
    let (base, _) = split_context_suffix(without_effort);
    catalog_match(base)
        .and_then(|entry| provider_inferred_from_catalog(entry, base))
        .or_else(|| inferred_provider_from_wire(base))
}

/// OpenRouter reuses provider-owned `anthropic/*` wire IDs, so an exact wire
/// match is not enough to infer the gateway. Its clud-qualified ID remains an
/// unambiguous inference source.
fn provider_inferred_from_catalog(entry: CatalogModel, input: &str) -> Option<ModelProvider> {
    if entry.provider == ModelProvider::OpenRouter
        && entry.wire_id.eq_ignore_ascii_case(input)
        && !entry.cli_id.eq_ignore_ascii_case(input)
    {
        None
    } else {
        Some(entry.provider)
    }
}

/// Normalize compatibility spellings at the CLI boundary. `None` preserves a
/// provider's existing reviewed default/profile.
pub fn resolve(
    requested_provider: Option<ModelProvider>,
    model: Option<&str>,
    effort: Option<&str>,
    context_window: Option<&str>,
) -> Result<Option<ResolvedModelSelection>, SelectionError> {
    let explicit_effort = effort
        .map(|value| {
            EffortLevel::parse(value)
                .ok_or_else(|| SelectionError::InvalidEffort(value.to_string()))
        })
        .transpose()?;
    let explicit_context = context_window
        .map(|value| value.trim().to_ascii_lowercase())
        .map(|value| match value.as_str() {
            "auto" | "1m" => Ok(value),
            _ => Err(SelectionError::InvalidContextWindow(value)),
        })
        .transpose()?;

    let Some(raw_model) = model.map(str::trim) else {
        let Some(provider) = requested_provider else {
            return Ok(None);
        };
        if explicit_effort.is_none() && explicit_context.is_none() {
            return Ok(None);
        }
        validate_capabilities(
            provider.as_str(),
            provider_efforts(provider),
            provider_context_windows(provider),
            explicit_effort,
            explicit_context.as_deref(),
        )?;
        // "This provider has a 1m-tier model" rather than `provider ==
        // ModelProvider::DeepSeek`: a model-less --context-window auto only
        // needs disambiguation (which of the provider's models?) when the
        // provider has more than one context tier to choose from. Kimi's
        // future row (#937 Phase 3) has a 1m tier too and picks this up with
        // zero edits here.
        if explicit_context.as_deref() == Some("auto")
            && models_for_provider(provider)
                .any(|entry| entry.supported_context_windows.contains(&"1m"))
        {
            return Err(SelectionError::ContextRequiresModel {
                provider,
                context_window: "auto".to_string(),
            });
        }
        return Ok(Some(ResolvedModelSelection {
            provider,
            model: None,
            wire_model: None,
            effort: explicit_effort,
            context_window: explicit_context,
            model_source: None,
            effort_source: explicit_effort.map(|_| SelectionSource::Cli),
            context_window_source: context_window.map(|_| SelectionSource::Cli),
        }));
    };

    let (without_effort, legacy_effort_raw) = split_effort_suffix(raw_model);
    let legacy_effort = legacy_effort_raw
        .map(|value| {
            EffortLevel::parse(value)
                .ok_or_else(|| SelectionError::InvalidEffort(value.to_string()))
        })
        .transpose()?;
    let (base_model, legacy_context_raw) = split_context_suffix(without_effort);
    let legacy_context = legacy_context_raw.map(str::to_string);

    let catalog = catalog_match(base_model).or_else(|| catalog_match(without_effort));
    let inferred_provider = catalog
        .and_then(|entry| provider_inferred_from_catalog(entry, base_model))
        .or_else(|| inferred_provider_from_wire(base_model));
    if let (Some(provider), Some(model_provider)) = (requested_provider, inferred_provider) {
        if provider != model_provider {
            return Err(SelectionError::ProviderConflict {
                provider,
                model: raw_model.to_string(),
                model_provider,
            });
        }
    }
    if catalog.is_none()
        && inferred_provider.is_none()
        && requested_provider == Some(ModelProvider::Codex)
        && !base_model.contains('-')
        && !base_model.contains('.')
    {
        return Err(SelectionError::UnknownModel(base_model.to_string()));
    }
    let model_provider = inferred_provider
        .or(requested_provider)
        .ok_or_else(|| SelectionError::UnknownModel(base_model.to_string()))?;

    if let (Some(legacy), Some(explicit)) = (legacy_effort, explicit_effort) {
        if legacy != explicit {
            return Err(SelectionError::ConflictingEffort {
                legacy: legacy.as_str().to_string(),
                explicit: explicit.as_str().to_string(),
            });
        }
    }
    if let (Some(legacy), Some(explicit)) = (legacy_context.as_deref(), explicit_context.as_deref())
    {
        if legacy != explicit {
            return Err(SelectionError::ConflictingContextWindow {
                legacy: legacy.to_string(),
                explicit: explicit.to_string(),
            });
        }
    }

    let effective_effort = explicit_effort.or(legacy_effort);
    let effective_context = explicit_context.or(legacy_context);
    let (label, efforts, contexts) = catalog.map_or_else(
        || {
            (
                base_model,
                provider_efforts(model_provider),
                provider_context_windows(model_provider),
            )
        },
        |entry| {
            (
                entry.cli_id,
                entry.supported_efforts,
                entry.supported_context_windows,
            )
        },
    );
    validate_capabilities(
        label,
        efforts,
        contexts,
        effective_effort,
        effective_context.as_deref(),
    )?;

    let mut wire_model =
        catalog.map_or_else(|| base_model.to_string(), |entry| entry.wire_id.to_string());
    // "This model has a 1m context tier" rather than `model_provider ==
    // ModelProvider::DeepSeek`: `contexts` is already the resolved model's
    // (or, for an uncataloged wire ID, the provider's fallback) supported
    // context-window slice, so this is behavior-identical for DeepSeek/Codex
    // and picks up Kimi's future 1m row (#937 Phase 3) unchanged.
    if contexts.contains(&"1m") {
        match effective_context.as_deref() {
            Some("auto") => {
                wire_model = wire_model
                    .strip_suffix("[1m]")
                    .unwrap_or(&wire_model)
                    .to_string();
            }
            Some("1m") if !wire_model.ends_with("[1m]") => wire_model.push_str("[1m]"),
            _ => {}
        }
    }

    Ok(Some(ResolvedModelSelection {
        provider: model_provider,
        model: Some(
            catalog.map_or_else(|| base_model.to_string(), |entry| entry.cli_id.to_string()),
        ),
        wire_model: Some(wire_model),
        effort: effective_effort,
        context_window: effective_context,
        model_source: Some(SelectionSource::Cli),
        effort_source: explicit_effort
            .map(|_| SelectionSource::Cli)
            .or_else(|| legacy_effort.map(|_| SelectionSource::LegacyModelSuffix)),
        context_window_source: context_window
            .map(|_| SelectionSource::Cli)
            .or_else(|| legacy_context_raw.map(|_| SelectionSource::LegacyModelSuffix)),
    }))
}

/// Resolve direct-launch CLI input over a saved provider profile and the
/// reviewed catalog default. Unified launches pass `use_catalog_default =
/// false` and no profile so an initial model never imports direct-provider
/// policy.
pub fn resolve_for_launch(
    provider: ModelProvider,
    cli_model: Option<&str>,
    cli_effort: Option<&str>,
    cli_context_window: Option<&str>,
    saved: Option<ProviderSelectionDefaults<'_>>,
    use_catalog_default: bool,
) -> Result<Option<ResolvedModelSelection>, SelectionError> {
    resolve_for_launch_with_server_default(
        provider,
        cli_model,
        cli_effort,
        cli_context_window,
        saved,
        use_catalog_default,
        None,
    )
}

/// As [`resolve_for_launch`], with a served default model (`server_settings`,
/// #1192) ranked below a saved profile model and above the catalog default.
/// It applies only to a direct launch (`use_catalog_default`). A served model
/// the catalog does not know keeps its own wire ID and `[1m]` suffix rather
/// than inheriting the catalog default row's effort and context.
pub fn resolve_for_launch_with_server_default(
    provider: ModelProvider,
    cli_model: Option<&str>,
    cli_effort: Option<&str>,
    cli_context_window: Option<&str>,
    saved: Option<ProviderSelectionDefaults<'_>>,
    use_catalog_default: bool,
    server_default: Option<&str>,
) -> Result<Option<ResolvedModelSelection>, SelectionError> {
    let catalog_default = use_catalog_default
        .then(|| reviewed_default_model(provider))
        .flatten();
    let server_default = server_default.filter(|_| use_catalog_default);
    let saved = saved.unwrap_or_default();
    let (model, model_source) = if let Some(model) = cli_model {
        (Some(model), Some(SelectionSource::Cli))
    } else if let Some(model) = saved.model {
        (Some(model), Some(SelectionSource::ProviderSetting))
    } else if let Some(model) = server_default {
        (Some(model), Some(SelectionSource::ServerDefault))
    } else if let Some(model) = catalog_default {
        (Some(model.cli_id), Some(SelectionSource::CatalogDefault))
    } else {
        (None, None)
    };

    let cli_has_legacy_effort = cli_model
        .and_then(|model| split_effort_suffix(model).1)
        .is_some();
    let cli_has_legacy_context = cli_model
        .map(|model| split_effort_suffix(model).0)
        .and_then(|model| split_context_suffix(model).1)
        .is_some();
    let saved_effort = (cli_effort.is_none() && !cli_has_legacy_effort)
        .then_some(saved.effort)
        .flatten();
    let saved_context = (cli_context_window.is_none() && !cli_has_legacy_context)
        .then_some(saved.context_window)
        .flatten();
    let effort_text = cli_effort.or_else(|| saved_effort.map(EffortLevel::as_str));
    let context_text = cli_context_window.or(saved_context);

    let mut selection = resolve(Some(provider), model, effort_text, context_text)?;
    let Some(selection) = selection.as_mut() else {
        return Ok(None);
    };
    selection.model_source = model_source;
    let served = model_source == Some(SelectionSource::ServerDefault);
    // A served `name[1m]` is the served file's context choice, not a legacy
    // spelling the user typed.
    if served && selection.context_window_source == Some(SelectionSource::LegacyModelSuffix) {
        selection.context_window_source = Some(SelectionSource::ServerDefault);
    }
    if saved_effort.is_some() {
        selection.effort_source = Some(SelectionSource::ProviderSetting);
    }
    if saved_context.is_some() {
        selection.context_window_source = Some(SelectionSource::ProviderSetting);
    }

    if use_catalog_default {
        let selected_catalog = selection
            .model
            .as_deref()
            .and_then(model_by_cli_id)
            .or(catalog_default.filter(|_| !served));
        if selection.effort.is_none() {
            if let Some(effort) = selected_catalog.and_then(|entry| entry.default_effort) {
                selection.effort = Some(effort);
                selection.effort_source = Some(SelectionSource::CatalogDefault);
            }
        }
        if selection.context_window.is_none() {
            if let Some(context) = selected_catalog.and_then(|entry| entry.default_context_window) {
                selection.context_window = Some(context.to_string());
                selection.context_window_source = Some(SelectionSource::CatalogDefault);
            }
        }
    }
    Ok(Some(selection.clone()))
}

// ---------------------------------------------------------------------------
// Launch-time model allowlist (#1257)
//
// A `--model` pin is a cost boundary, not merely a main-conversation default:
// an OpenRouter key is account-wide, so every model id sent with it bills at
// that model's rate -- including the auxiliary slots Claude Code fills on its
// own. These helpers are the one place that boundary is expressed, so the
// child-env overlay, gateway discovery, and the bridge's refusal all agree on
// membership.
// ---------------------------------------------------------------------------

/// Build the launch-time model allowlist (#1257).
///
/// A `--model` pin alone is the implicit allowlist `{<id>}`. An explicit
/// `--allow-model` *replaces* that implicit single-entry set rather than
/// extending it, which is how a launch names a small set instead of exactly
/// one; a pin is then validated as a member rather than silently widening the
/// boundary. Neither present yields an empty list, meaning no constraint, so a
/// default launch keeps byte-for-byte today's behavior.
pub fn model_allowlist(pin: Option<&str>, explicit: &[String]) -> Vec<String> {
    let entries: Vec<&str> = if explicit.is_empty() {
        pin.into_iter().collect()
    } else {
        explicit.iter().map(String::as_str).collect()
    };
    let mut allowed: Vec<String> = Vec::new();
    for entry in entries {
        let entry = entry.trim();
        if entry.is_empty() {
            continue;
        }
        if !allowed.iter().any(|kept| kept.eq_ignore_ascii_case(entry)) {
            allowed.push(entry.to_string());
        }
    }
    allowed
}

/// Fail fast, at launch, when the model this launch will bill sits outside an
/// explicit `--allow-model`. Refusing before the child starts is the point:
/// the alternative is a session that runs and then cannot serve its own first
/// turn. An unconstrained launch has nothing to check.
pub fn validate_model_allowlist(allowed: &[String], effective: Option<&str>) -> Result<(), String> {
    if allowed.is_empty() {
        return Ok(());
    }
    let Some(effective) = effective else {
        return Ok(());
    };
    if model_allowlist_allows(allowed, effective) {
        return Ok(());
    }
    Err(format!(
        "model '{effective}' is not allowed by --allow-model; allowed models: {}",
        allowed.join(", ")
    ))
}

/// Whether `requested` -- spelled in any of clud's model namespaces -- is a
/// member of `allowed`. An empty allowlist constrains nothing and accepts
/// everything, which is what keeps an unconstrained launch byte-for-byte
/// unchanged.
pub fn model_allowlist_allows(allowed: &[String], requested: &str) -> bool {
    allowed.is_empty()
        || allowed
            .iter()
            .any(|entry| model_ids_identify_the_same_row(entry, requested))
}

/// Two model spellings name the same thing when their strings agree (either
/// namespace, either case) or when both resolve to one catalog row.
///
/// The row check is what lets `--allow-model codex-terra` admit the
/// `gpt-5.6-terra` wire ID and the `clud-claude-codex-terra` discovery ID the
/// gateway actually receives. The string check is what lets an uncataloged ID
/// such as `xiaomi/mimo-v2.6-flash` be named at all: clud deliberately does
/// not own OpenRouter's namespace, so there is no row to resolve for it.
fn model_ids_identify_the_same_row(entry: &str, requested: &str) -> bool {
    let entry = entry.trim();
    let requested = requested.trim();
    if entry.eq_ignore_ascii_case(requested) {
        return true;
    }
    // `wire@effort` is one model with a session-time modifier, so the effort
    // suffix is never a different model.
    let entry_base = split_effort_suffix(entry).0;
    let requested_base = split_effort_suffix(requested).0;
    if entry_base.eq_ignore_ascii_case(requested_base) {
        return true;
    }
    match (catalog_match(entry_base), catalog_match(requested_base)) {
        (Some(entry_row), Some(requested_row)) => entry_row.cli_id == requested_row.cli_id,
        _ => false,
    }
}

/// The model an auxiliary slot resolves to on a constrained launch, or `None`
/// when the launch is unconstrained (the caller then keeps the descriptor's
/// own role mapping).
///
/// The launch's own selection wins when the allowlist admits it, so `--model`
/// still decides *which* member of a multi-entry allowlist fills the slots.
/// An uncataloged pin reaches the caller verbatim: that is the whole point of
/// `--openrouter --model xiaomi/mimo-v2.6-flash`.
pub fn model_allowlist_slot(
    allowed: &[String],
    selection: Option<&ResolvedModelSelection>,
) -> Option<String> {
    if allowed.is_empty() {
        return None;
    }
    if let Some(selection) = selection {
        for candidate in [selection.wire_model.as_deref(), selection.model.as_deref()]
            .into_iter()
            .flatten()
        {
            if model_allowlist_allows(allowed, candidate) {
                return Some(candidate.to_string());
            }
        }
    }
    allowed.first().cloned()
}

/// The refusal a bridge sends for a request naming a model outside the
/// allowlist (#1257). Naming the set is the whole value of the message: a
/// mid-session `/model` switch must tell the user what it was blocked from
/// billing, not fail anonymously.
pub fn model_allowlist_refusal(allowed: &[String], requested: &str) -> String {
    format!(
        "model '{}' is not allowed for this launch; allowed models: {}",
        requested.trim(),
        allowed.join(", ")
    )
}

#[cfg(test)]
mod allowlist_tests {
    use super::*;

    fn ids(values: &[&str]) -> Vec<String> {
        values.iter().map(|value| value.to_string()).collect()
    }

    #[test]
    fn a_lone_model_pin_is_the_allowlist() {
        assert_eq!(
            model_allowlist(Some("xiaomi/mimo-v2.6-flash"), &[]),
            ids(&["xiaomi/mimo-v2.6-flash"])
        );
    }

    #[test]
    fn repeated_allow_model_builds_the_allowlist_and_drops_repeats() {
        assert_eq!(
            model_allowlist(
                Some("deepseek-flash"),
                &ids(&["deepseek-flash", "deepseek-v4-pro", "DEEPSEEK-FLASH", " "])
            ),
            ids(&["deepseek-flash", "deepseek-v4-pro"])
        );
    }

    #[test]
    fn no_pin_and_no_allowlist_is_no_constraint() {
        let allowed = model_allowlist(None, &[]);
        assert!(allowed.is_empty());
        assert!(model_allowlist_allows(&allowed, "claude-opus-5"));
    }

    #[test]
    fn an_explicit_allow_list_replaces_the_pin_rather_than_extending_it() {
        assert_eq!(
            model_allowlist(Some("ignored"), &ids(&["a", "b"])),
            ids(&["a", "b"])
        );
    }

    #[test]
    fn a_pin_outside_an_explicit_allow_list_fails_launch_validation() {
        let allowed = model_allowlist(Some("xiaomi/mimo-v2.6-flash"), &ids(&["a", "b"]));
        let error = validate_model_allowlist(&allowed, Some("xiaomi/mimo-v2.6-flash"))
            .expect_err("the pin is not a member of {a, b}");
        assert!(error.contains("xiaomi/mimo-v2.6-flash"), "{error}");
        assert!(error.contains("--allow-model"), "{error}");
        assert!(error.contains("a, b"), "{error}");
    }

    #[test]
    fn a_pin_inside_an_explicit_allow_list_passes_launch_validation() {
        let allowed = model_allowlist(Some("deepseek-flash"), &ids(&["deepseek-flash", "b"]));
        assert!(validate_model_allowlist(&allowed, Some("deepseek-flash")).is_ok());
    }

    #[test]
    fn membership_spans_cli_wire_and_discovery_namespaces_of_one_row() {
        let allowed = ids(&["codex-terra"]);
        for spelling in ["gpt-5.6-terra", "clud-claude-codex-terra", "CODEx-terra"] {
            assert!(
                model_allowlist_allows(&allowed, spelling),
                "{spelling} is the same catalog row as codex-terra"
            );
        }
        assert!(!model_allowlist_allows(&allowed, "gpt-5.6-sol"));
        assert!(!model_allowlist_allows(&allowed, "claude-opus-5"));
    }

    #[test]
    fn an_uncataloged_openrouter_id_is_matched_by_its_own_string() {
        let allowed = ids(&["xiaomi/mimo-v2.6-flash"]);
        assert!(model_allowlist_allows(&allowed, "xiaomi/mimo-v2.6-flash"));
        assert!(!model_allowlist_allows(
            &allowed,
            "~anthropic/claude-opus-latest"
        ));
    }

    #[test]
    fn an_effort_suffix_never_changes_which_model_was_asked_for() {
        let allowed = ids(&["codex-terra"]);
        assert!(model_allowlist_allows(&allowed, "gpt-5.6-terra@high"));
    }

    #[test]
    fn the_slot_follows_the_launch_selection_when_the_allow_list_admits_it() {
        let allowed = ids(&["deepseek-flash", "deepseek-v4-pro"]);
        let selection = resolve(
            Some(ModelProvider::DeepSeek),
            Some("deepseek-v4-pro"),
            None,
            None,
        )
        .unwrap()
        .unwrap();
        // The wire id wins over the CLI id when both are admitted: the wire
        // id is what the slot actually puts on the wire, and the allowlist
        // admits it through the same catalog row as its CLI spelling.
        assert_eq!(
            model_allowlist_slot(&allowed, Some(&selection)).as_deref(),
            Some("deepseek-v4-pro[1m]")
        );
    }

    #[test]
    fn the_slot_falls_back_to_the_first_allowed_entry_without_a_selection() {
        let allowed = ids(&["xiaomi/mimo-v2.6-flash"]);
        assert_eq!(
            model_allowlist_slot(&allowed, None).as_deref(),
            Some("xiaomi/mimo-v2.6-flash")
        );
        assert_eq!(model_allowlist_slot(&[], None), None);
    }

    #[test]
    fn a_context_suffix_never_changes_which_model_was_asked_for() {
        // `deepseek-flash[1m]` is the wire id of the same catalog row as the
        // CLI id `deepseek-flash`, so either spelling admits the other.
        let allowed = ids(&["deepseek-flash"]);
        assert!(model_allowlist_allows(&allowed, "deepseek-flash[1m]"));
        assert!(model_allowlist_allows(
            &ids(&["deepseek-flash[1m]"]),
            "deepseek-flash"
        ));
        assert!(!model_allowlist_allows(&allowed, "deepseek-v4-pro[1m]"));
    }
}

#[cfg(test)]
mod reachability_tests {
    use super::*;

    /// Guards the claim the unified refusal in `codex_bridge` relies on
    /// (#1010 item 2): reaching that branch means clud has genuinely never
    /// heard of the model, so #1009's "clud knows it, this gateway is not
    /// serving it" case cannot fire there and adding it would be dead code.
    ///
    /// The condition is narrower than "no `clud-claude-*` alias exists" —
    /// `clud-claude-deepseek-v4-pro` is exactly such an alias, and it is
    /// harmless because `non_claude_model_by_any_id` resolves DeepSeek rows,
    /// so `catalog` is `Some` and the refusal never fires.
    ///
    /// What would make the second case reachable is such an alias on a
    /// **Claude or OpenRouter** row — the two providers that lookup excludes.
    /// Then `exact_catalog` misses it (not a discovery id), the fallback
    /// excludes it (wrong provider), `catalog` is `None`, the refusal fires,
    /// and `model_by_any_id` finds it: a model clud knows, called unknown.
    /// That is item 1's bug, on the other route.
    #[test]
    fn only_a_claude_or_openrouter_alias_would_make_the_unified_second_case_reachable() {
        for entry in MODELS {
            if entry.provider != ModelProvider::Claude
                && entry.provider != ModelProvider::OpenRouter
            {
                continue;
            }
            let mut candidates = vec![("cli_id", entry.cli_id), ("wire_id", entry.wire_id)];
            candidates.extend(entry.legacy_aliases.iter().map(|a| ("legacy_alias", *a)));
            for (kind, value) in candidates {
                if !value.starts_with("clud-claude-") {
                    continue;
                }
                assert_eq!(
                    entry.discovery_id,
                    Some(value),
                    "{kind} `{value}` is a clud-claude-* identifier on a {:?} row and is not \
                     that row's discovery_id, so the unified refusal in codex_bridge can now be \
                     reached by a model clud knows. Restore #1009's second case there — see #1010.",
                    entry.provider,
                );
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Issue #955: an adapter must never have to choose between duplicate
    /// defaults or restate a provider's context ceiling outside this table.
    #[test]
    fn provider_defaults_are_unique_and_codex_discovery_has_one_context_policy() {
        for provider in ModelProvider::ALL.iter().copied() {
            assert!(
                models_for_provider(provider)
                    .filter(|entry| entry.provider_default)
                    .count()
                    <= 1,
                "{provider} has more than one reviewed default"
            );
        }

        assert_eq!(
            models_for_provider(ModelProvider::Codex)
                .filter(|entry| entry.provider_default)
                .count(),
            1,
            "Codex must have exactly one reviewed default"
        );

        assert_eq!(
            common_claude_context_tokens(ModelProvider::Codex),
            Some(1_050_000)
        );
        assert!(models_for_provider(ModelProvider::Codex)
            .all(|entry| entry.claude_max_context_tokens == Some(1_050_000)));
    }

    /// #1200: the only row whose endpoint silently drops images, pinned so a
    /// later catalog edit cannot lose the capability signal and the launch
    /// notice's input cannot drift from the evidence. `deepseek-v4-pro`
    /// answers `200` with the image replaced by an `[Unsupported Image]`
    /// placeholder; `deepseek-flash` ingests the same body correctly.
    #[test]
    fn deepseek_pro_is_the_only_row_verified_to_drop_images() {
        assert!(!model_by_cli_id("deepseek-v4-pro").unwrap().supports_images);
        assert!(model_by_cli_id("deepseek-flash").unwrap().supports_images);

        let droppers: Vec<&str> = MODELS
            .iter()
            .filter(|entry| !entry.supports_images)
            .map(|entry| entry.cli_id)
            .collect();
        assert_eq!(
            droppers,
            vec!["deepseek-v4-pro"],
            "a new dropper needs its own notice decision, and its own live evidence"
        );

        // `image_capability_notice` names the provider's reviewed default as
        // the alternative and `.expect()`s one exists, so a dropper whose
        // provider offers nothing image-capable would either panic at launch
        // or recommend a model that drops images too. Pin both halves here
        // rather than discovering them in the field.
        for entry in MODELS.iter().filter(|entry| !entry.supports_images) {
            let alternative = reviewed_default_model(entry.provider);
            assert!(
                alternative.is_some(),
                "{} drops images, but its provider has no reviewed default to name",
                entry.cli_id
            );
            assert!(
                alternative.is_some_and(|default| default.supports_images),
                "{} drops images, and its provider's reviewed default does too -- \
                 the notice would recommend a model that also drops them",
                entry.cli_id
            );
        }
    }

    /// The native provider must never carry the warning: a `false` here would
    /// tell a Claude session that its own endpoint cannot accept images.
    #[test]
    fn every_claude_row_accepts_images() {
        assert!(models_for_provider(ModelProvider::Claude).all(|entry| entry.supports_images));
    }

    /// Guardrail: `apply_anthropic_compat_overlay` (foreground_runtime.rs)
    /// expects `reviewed_default_model` to resolve for every
    /// Anthropic-compat descriptor's provider, and panics via `.expect()` at
    /// launch if it doesn't. Pin the invariant here so a future descriptor
    /// row (e.g. Kimi in #937 Phase 3) without a matching
    /// `provider_default: true` catalog row fails a test instead of a
    /// runtime launch.
    #[test]
    fn every_anthropic_compat_provider_has_a_reviewed_catalog_default() {
        for descriptor in crate::provider_registry::ANTHROPIC_COMPAT_PROVIDERS {
            assert!(
                reviewed_default_model(descriptor.provider).is_some(),
                "{} has no provider_default: true catalog row",
                descriptor.display_name
            );
        }
    }

    #[test]
    fn inferred_provider_from_wire_matches_representative_ids() {
        // Table-driven rewrite over ModelProvider::ALL / wire_prefixes must
        // return the exact same answers as the original hardcoded ladder.
        assert_eq!(
            inferred_provider_from_wire("deepseek-v4-pro[1m]"),
            Some(ModelProvider::DeepSeek)
        );
        assert_eq!(
            inferred_provider_from_wire("gpt-5.6-terra"),
            Some(ModelProvider::Codex)
        );
        assert_eq!(
            inferred_provider_from_wire("codex-sol"),
            Some(ModelProvider::Codex)
        );
        assert_eq!(
            inferred_provider_from_wire("claude-opus-x"),
            Some(ModelProvider::Claude)
        );
        assert_eq!(inferred_provider_from_wire("unknown-wire-id"), None);
    }

    #[test]
    fn qualified_codex_model_infers_provider_and_keeps_effort_independent() {
        let selection = resolve(None, Some("codex-terra"), Some("high"), None)
            .unwrap()
            .unwrap();
        assert_eq!(selection.provider, ModelProvider::Codex);
        assert_eq!(selection.wire_model.as_deref(), Some("gpt-5.6-terra"));
        assert_eq!(selection.effort, Some(EffortLevel::High));
        assert_eq!(selection.effort_source, Some(SelectionSource::Cli));
    }

    #[test]
    fn conflicting_provider_fails_before_launch() {
        assert!(matches!(
            resolve(Some(ModelProvider::DeepSeek), Some("codex-sol"), None, None),
            Err(SelectionError::ProviderConflict { .. })
        ));
    }

    #[test]
    fn claude_prefixed_compatibility_ids_are_not_claimed_by_codex() {
        let selection = resolve(None, Some("claude-opus"), None, None)
            .unwrap()
            .unwrap();
        assert_eq!(selection.provider, ModelProvider::Claude);
        assert_eq!(selection.model.as_deref(), Some("claude-opus"));
        assert_eq!(selection.wire_model.as_deref(), Some("opus"));
    }

    #[test]
    fn equal_legacy_and_explicit_effort_values_coalesce() {
        let selection = resolve(
            Some(ModelProvider::Codex),
            Some("terra@high"),
            Some("high"),
            None,
        )
        .unwrap()
        .unwrap();
        assert_eq!(selection.effort, Some(EffortLevel::High));
        assert_eq!(selection.effort_source, Some(SelectionSource::Cli));
    }

    #[test]
    fn conflicting_legacy_and_explicit_effort_values_fail() {
        assert!(matches!(
            resolve(
                Some(ModelProvider::Codex),
                Some("terra@low"),
                Some("high"),
                None,
            ),
            Err(SelectionError::ConflictingEffort { .. })
        ));
    }

    #[test]
    fn deepseek_wire_context_suffix_normalizes_to_an_independent_field() {
        let selection = resolve(
            Some(ModelProvider::DeepSeek),
            Some("deepseek-v4-pro[1m]"),
            Some("max"),
            Some("1m"),
        )
        .unwrap()
        .unwrap();
        assert_eq!(selection.model.as_deref(), Some("deepseek-v4-pro"));
        assert_eq!(selection.wire_model.as_deref(), Some("deepseek-v4-pro[1m]"));
        assert_eq!(selection.context_window.as_deref(), Some("1m"));
    }

    #[test]
    fn effort_without_an_explicit_model_is_still_normalized() {
        let selection = resolve(Some(ModelProvider::Codex), None, Some("high"), None)
            .unwrap()
            .unwrap();
        assert_eq!(selection.provider, ModelProvider::Codex);
        assert_eq!(selection.effort, Some(EffortLevel::High));
    }

    #[test]
    fn unknown_future_codex_wire_id_remains_reachable_without_rewriting() {
        let selection = resolve(Some(ModelProvider::Codex), Some("gpt-5.7-nova"), None, None)
            .unwrap()
            .unwrap();
        assert_eq!(selection.model.as_deref(), Some("gpt-5.7-nova"));
        assert_eq!(selection.wire_model.as_deref(), Some("gpt-5.7-nova"));

        let repeated = resolve(
            Some(ModelProvider::Codex),
            selection.wire_model.as_deref(),
            None,
            None,
        )
        .unwrap()
        .unwrap();
        assert_eq!(repeated.model, selection.model);
        assert_eq!(repeated.wire_model, selection.wire_model);
    }

    #[test]
    fn explicit_provider_keeps_unknown_custom_wire_ids_lossless() {
        let selection = resolve(
            Some(ModelProvider::Claude),
            Some("My-Gateway-Model"),
            None,
            None,
        )
        .unwrap()
        .unwrap();
        assert_eq!(selection.model.as_deref(), Some("My-Gateway-Model"));
        assert_eq!(selection.wire_model.as_deref(), Some("My-Gateway-Model"));

        let repeated = resolve(
            Some(ModelProvider::Claude),
            selection.wire_model.as_deref(),
            None,
            None,
        )
        .unwrap()
        .unwrap();
        assert_eq!(repeated.wire_model, selection.wire_model);
    }

    #[test]
    fn marker_shaped_custom_wire_id_is_not_decoded() {
        let selection = resolve(
            Some(ModelProvider::Claude),
            Some("claude-wire-special"),
            None,
            None,
        )
        .unwrap()
        .unwrap();
        assert_eq!(selection.model.as_deref(), Some("claude-wire-special"));
        assert_eq!(selection.wire_model.as_deref(), Some("claude-wire-special"));
    }

    #[test]
    fn bare_unknown_codex_alias_is_rejected_but_full_ids_remain_reachable() {
        assert!(matches!(
            resolve(Some(ModelProvider::Codex), Some("tera"), None, None),
            Err(SelectionError::UnknownModel(model)) if model == "tera"
        ));
        assert!(resolve(
            Some(ModelProvider::Codex),
            Some("my-gateway-model"),
            None,
            None
        )
        .is_ok());
    }

    #[test]
    fn discovery_ids_round_trip_through_the_same_catalog_rows() {
        for entry in MODELS.iter().filter(|entry| entry.discovery_id.is_some()) {
            let selection = resolve(None, entry.discovery_id, None, None)
                .unwrap()
                .unwrap();
            assert_eq!(selection.provider, entry.provider);
            assert_eq!(selection.model.as_deref(), Some(entry.cli_id));
            assert_eq!(selection.wire_model.as_deref(), Some(entry.wire_id));
        }
    }

    #[test]
    fn deepseek_checkpoint_discovery_id_is_versioned_with_legacy_routing() {
        let current = model_by_discovery_id("clud-claude-deepseek-v4-pro-0813").unwrap();
        let legacy = model_by_discovery_id("clud-claude-deepseek-v4-pro").unwrap();
        assert_eq!(current, legacy);
        assert_eq!(current.cli_id, "deepseek-v4-pro");
        assert_eq!(current.wire_id, "deepseek-v4-pro[1m]");
        assert_eq!(current.display_name, "DeepSeek V4 Pro 0813");
    }

    #[test]
    fn model_less_modifiers_are_validated_against_the_provider_profile() {
        assert!(matches!(
            resolve(Some(ModelProvider::DeepSeek), None, Some("none"), None),
            Err(SelectionError::UnsupportedEffort { .. })
        ));
        assert!(matches!(
            resolve(Some(ModelProvider::Claude), None, None, Some("1m")),
            Err(SelectionError::UnsupportedContextWindow { .. })
        ));
        assert!(matches!(
            resolve(Some(ModelProvider::DeepSeek), None, None, Some("auto")),
            Err(SelectionError::ContextRequiresModel { .. })
        ));
    }

    #[test]
    fn explicit_deepseek_auto_context_removes_the_legacy_wire_suffix() {
        let selection = resolve(
            Some(ModelProvider::DeepSeek),
            Some("deepseek-v4-pro"),
            None,
            Some("auto"),
        )
        .unwrap()
        .unwrap();
        assert_eq!(selection.wire_model.as_deref(), Some("deepseek-v4-pro"));
    }

    #[test]
    fn discovery_ids_are_unique_and_reserved() {
        let ids: Vec<&str> = MODELS
            .iter()
            .filter_map(|entry| entry.discovery_id)
            .collect();
        assert!(ids.iter().all(|id| id.starts_with("clud-claude-")));
        let mut deduped = ids.clone();
        deduped.sort_unstable();
        deduped.dedup();
        assert_eq!(deduped.len(), ids.len());
    }

    #[test]
    fn launch_cli_model_overrides_saved_and_catalog_default() {
        let saved = ProviderSelectionDefaults {
            model: Some("codex-terra"),
            effort: Some(EffortLevel::Low),
            context_window: None,
        };
        let selection = resolve_for_launch(
            ModelProvider::Codex,
            Some("codex-luna"),
            Some("high"),
            None,
            Some(saved),
            true,
        )
        .unwrap()
        .unwrap();
        assert_eq!(selection.model.as_deref(), Some("codex-luna"));
        assert_eq!(selection.model_source, Some(SelectionSource::Cli));
        assert_eq!(selection.effort, Some(EffortLevel::High));
        assert_eq!(selection.effort_source, Some(SelectionSource::Cli));
    }

    #[test]
    fn launch_falls_back_to_saved_provider_profile() {
        let saved = ProviderSelectionDefaults {
            model: Some("codex-luna"),
            effort: Some(EffortLevel::High),
            context_window: None,
        };
        let selection =
            resolve_for_launch(ModelProvider::Codex, None, None, None, Some(saved), true)
                .unwrap()
                .unwrap();
        assert_eq!(selection.model.as_deref(), Some("codex-luna"));
        assert_eq!(
            selection.model_source,
            Some(SelectionSource::ProviderSetting)
        );
        assert_eq!(selection.effort, Some(EffortLevel::High));
        assert_eq!(
            selection.effort_source,
            Some(SelectionSource::ProviderSetting)
        );
    }

    #[test]
    fn launch_falls_back_to_reviewed_catalog_default() {
        let selection = resolve_for_launch(ModelProvider::Codex, None, None, None, None, true)
            .unwrap()
            .unwrap();
        assert_eq!(selection.provider, ModelProvider::Codex);
        assert_eq!(selection.model.as_deref(), Some("codex-sol"));
        assert_eq!(selection.wire_model.as_deref(), Some("gpt-5.6-sol"));
        assert_eq!(
            selection.model_source,
            Some(SelectionSource::CatalogDefault)
        );
        assert_eq!(selection.effort, Some(EffortLevel::Low));
        assert_eq!(
            selection.effort_source,
            Some(SelectionSource::CatalogDefault)
        );
    }

    #[test]
    fn codex_launch_default_precedence_is_explicit_and_source_tracked() {
        let terra = resolve_for_launch(
            ModelProvider::Codex,
            Some("codex-terra"),
            None,
            None,
            None,
            true,
        )
        .unwrap()
        .unwrap();
        assert_eq!(terra.model.as_deref(), Some("codex-terra"));
        assert_eq!(terra.effort, Some(EffortLevel::Medium));
        assert_eq!(terra.model_source, Some(SelectionSource::Cli));
        assert_eq!(terra.effort_source, Some(SelectionSource::CatalogDefault));

        let high = resolve_for_launch(ModelProvider::Codex, None, Some("high"), None, None, true)
            .unwrap()
            .unwrap();
        assert_eq!(high.model.as_deref(), Some("codex-sol"));
        assert_eq!(high.effort, Some(EffortLevel::High));
        assert_eq!(high.model_source, Some(SelectionSource::CatalogDefault));
        assert_eq!(high.effort_source, Some(SelectionSource::Cli));

        let saved = ProviderSelectionDefaults {
            model: Some("codex-terra"),
            effort: Some(EffortLevel::High),
            context_window: None,
        };
        let saved = resolve_for_launch(ModelProvider::Codex, None, None, None, Some(saved), true)
            .unwrap()
            .unwrap();
        assert_eq!(saved.model.as_deref(), Some("codex-terra"));
        assert_eq!(saved.effort, Some(EffortLevel::High));
        assert_eq!(saved.model_source, Some(SelectionSource::ProviderSetting));
        assert_eq!(saved.effort_source, Some(SelectionSource::ProviderSetting));
    }

    #[test]
    fn launch_deepseek_catalog_default_carries_context_window() {
        let selection = resolve_for_launch(ModelProvider::DeepSeek, None, None, None, None, true)
            .unwrap()
            .unwrap();
        let default = reviewed_default_model(ModelProvider::DeepSeek).unwrap();
        assert_eq!(default.display_name, "DeepSeek V4.1 Flash");
        assert_eq!(selection.model.as_deref(), Some("deepseek-flash"));
        assert_eq!(selection.wire_model.as_deref(), Some("deepseek-flash[1m]"));
        assert_eq!(selection.effort, Some(EffortLevel::Low));
        assert_eq!(
            selection.effort_source,
            Some(SelectionSource::CatalogDefault)
        );
        assert_eq!(selection.context_window.as_deref(), Some("1m"));
        assert_eq!(
            selection.context_window_source,
            Some(SelectionSource::CatalogDefault)
        );
    }

    /// #1192: DeepSeek renamed `deepseek-v4-flash` to `deepseek-flash`; every
    /// older spelling a user or picker may still hold keeps resolving.
    #[test]
    fn retired_deepseek_flash_spellings_resolve_to_v4_1_flash() {
        for spelling in [
            "deepseek-flash",
            "deepseek-flash[1m]",
            "deepseek-v4-flash",
            "clud-claude-deepseek-flash",
            "clud-claude-deepseek-v4-flash",
        ] {
            let selection = resolve(None, Some(spelling), None, None).unwrap().unwrap();
            assert_eq!(selection.provider, ModelProvider::DeepSeek, "{spelling}");
            assert_eq!(
                selection.model.as_deref(),
                Some("deepseek-flash"),
                "{spelling}"
            );
            assert_eq!(
                selection.wire_model.as_deref(),
                Some("deepseek-flash[1m]"),
                "{spelling}"
            );
        }
        assert_eq!(
            model_by_discovery_id("clud-claude-deepseek-v4-flash").map(|model| model.cli_id),
            Some("deepseek-flash")
        );
        let auto = resolve(None, Some("deepseek-v4-flash"), None, Some("auto"))
            .unwrap()
            .unwrap();
        assert_eq!(auto.wire_model.as_deref(), Some("deepseek-flash"));
        let pro = resolve(None, Some("deepseek-v4-pro[1m]"), None, None)
            .unwrap()
            .unwrap();
        assert_eq!(pro.model.as_deref(), Some("deepseek-v4-pro"));
    }

    #[test]
    fn retired_cli_ids_name_a_successor_that_still_aliases_them() {
        for (retired, current) in RETIRED_CLI_IDS {
            assert!(
                model_by_cli_id(retired).is_none(),
                "{retired} is not retired"
            );
            let successor = model_by_retired_cli_id(retired).unwrap();
            assert_eq!(successor.cli_id, *current);
            assert!(successor.legacy_aliases.contains(retired), "{retired}");
        }
        assert!(model_by_retired_cli_id("gpt-5.6-terra").is_none());
    }

    #[test]
    fn launch_server_default_ranks_below_saved_and_cli_models_and_above_the_catalog() {
        let served = resolve_for_launch_with_server_default(
            ModelProvider::DeepSeek,
            None,
            None,
            None,
            None,
            true,
            Some("deepseek-flash"),
        )
        .unwrap()
        .unwrap();
        assert_eq!(served.model.as_deref(), Some("deepseek-flash"));
        assert_eq!(served.wire_model.as_deref(), Some("deepseek-flash[1m]"));
        assert_eq!(served.model_source, Some(SelectionSource::ServerDefault));
        assert_eq!(served.effort, Some(EffortLevel::Low));
        assert_eq!(served.context_window.as_deref(), Some("1m"));

        let saved = ProviderSelectionDefaults {
            model: Some("deepseek-v4-pro"),
            ..ProviderSelectionDefaults::default()
        };
        let saved_wins = resolve_for_launch_with_server_default(
            ModelProvider::DeepSeek,
            None,
            None,
            None,
            Some(saved),
            true,
            Some("deepseek-flash"),
        )
        .unwrap()
        .unwrap();
        assert_eq!(saved_wins.model.as_deref(), Some("deepseek-v4-pro"));
        assert_eq!(
            saved_wins.model_source,
            Some(SelectionSource::ProviderSetting)
        );

        let cli_wins = resolve_for_launch_with_server_default(
            ModelProvider::DeepSeek,
            Some("deepseek-v4-pro"),
            None,
            None,
            Some(saved),
            true,
            Some("deepseek-flash"),
        )
        .unwrap()
        .unwrap();
        assert_eq!(cli_wins.model_source, Some(SelectionSource::Cli));

        // Unified launches never import direct-provider policy, served or not.
        assert_eq!(
            resolve_for_launch_with_server_default(
                ModelProvider::DeepSeek,
                None,
                None,
                None,
                None,
                false,
                Some("deepseek-flash"),
            )
            .unwrap(),
            None
        );
    }

    #[test]
    fn an_uncataloged_server_default_keeps_its_own_wire_and_context() {
        let with_context = resolve_for_launch_with_server_default(
            ModelProvider::DeepSeek,
            None,
            None,
            None,
            None,
            true,
            Some("deepseek-flash-2[1m]"),
        )
        .unwrap()
        .unwrap();
        assert_eq!(with_context.model.as_deref(), Some("deepseek-flash-2"));
        assert_eq!(
            with_context.wire_model.as_deref(),
            Some("deepseek-flash-2[1m]")
        );
        assert_eq!(with_context.context_window.as_deref(), Some("1m"));
        assert_eq!(
            with_context.context_window_source,
            Some(SelectionSource::ServerDefault)
        );

        let auto = resolve_for_launch_with_server_default(
            ModelProvider::DeepSeek,
            None,
            None,
            None,
            None,
            true,
            Some("deepseek-flash-2"),
        )
        .unwrap()
        .unwrap();
        assert_eq!(auto.wire_model.as_deref(), Some("deepseek-flash-2"));
        assert_eq!(
            auto.context_window, None,
            "must not inherit the catalog default row's 1m context"
        );
    }

    #[test]
    fn launch_without_catalog_default_never_imports_provider_policy() {
        // Unified routing passes use_catalog_default = false and no profile, so
        // an initial model is never seeded from direct-provider defaults.
        let selection =
            resolve_for_launch(ModelProvider::Codex, None, None, None, None, false).unwrap();
        assert!(selection.is_none());
    }

    #[test]
    fn kimi_catalog_row_resolves_by_cli_id_wire_id_and_discovery_id() {
        let by_cli = resolve(None, Some("kimi-k3"), None, None).unwrap().unwrap();
        assert_eq!(by_cli.provider, ModelProvider::Kimi);
        assert_eq!(by_cli.model.as_deref(), Some("kimi-k3"));
        assert_eq!(by_cli.wire_model.as_deref(), Some("kimi-k3[1m]"));

        let by_wire = resolve(None, Some("kimi-k3[1m]"), None, None)
            .unwrap()
            .unwrap();
        assert_eq!(by_wire.provider, ModelProvider::Kimi);
        assert_eq!(by_wire.model.as_deref(), Some("kimi-k3"));
        assert_eq!(by_wire.wire_model.as_deref(), Some("kimi-k3[1m]"));

        let by_discovery = resolve(None, Some("clud-claude-kimi-k3"), None, None)
            .unwrap()
            .unwrap();
        assert_eq!(by_discovery.provider, ModelProvider::Kimi);
        assert_eq!(by_discovery.model.as_deref(), Some("kimi-k3"));
        assert_eq!(by_discovery.wire_model.as_deref(), Some("kimi-k3[1m]"));
    }

    /// K3 documents only low/high/max -- medium and xhigh must be rejected
    /// before any upstream request (#937 Phase 3), unlike DeepSeek which
    /// reuses the full `ANTHROPIC_EFFORTS` slice.
    #[test]
    fn kimi_rejects_medium_and_xhigh_but_accepts_low_high_max() {
        for rejected in ["medium", "xhigh"] {
            assert!(
                matches!(
                    resolve(
                        Some(ModelProvider::Kimi),
                        Some("kimi-k3"),
                        Some(rejected),
                        None
                    ),
                    Err(SelectionError::UnsupportedEffort { .. })
                ),
                "expected effort '{rejected}' to be rejected for Kimi"
            );
        }
        for accepted in ["low", "high", "max"] {
            assert!(
                resolve(
                    Some(ModelProvider::Kimi),
                    Some("kimi-k3"),
                    Some(accepted),
                    None
                )
                .is_ok(),
                "expected effort '{accepted}' to be accepted for Kimi"
            );
        }
    }

    #[test]
    fn kimi_launch_catalog_default_carries_1m_context_and_low_effort() {
        let selection = resolve_for_launch(ModelProvider::Kimi, None, None, None, None, true)
            .unwrap()
            .unwrap();
        let default = reviewed_default_model(ModelProvider::Kimi).unwrap();
        assert_eq!(default.display_name, "Kimi K3");
        assert_eq!(selection.model.as_deref(), Some("kimi-k3"));
        assert_eq!(selection.wire_model.as_deref(), Some("kimi-k3[1m]"));
        assert_eq!(selection.effort, Some(EffortLevel::Low));
        assert_eq!(selection.context_window.as_deref(), Some("1m"));
        assert_eq!(default.claude_compact_window, Some(1_048_576));
    }

    #[test]
    fn openrouter_default_is_qualified_and_does_not_claim_anthropic_wire_ids() {
        let selection = resolve_for_launch(ModelProvider::OpenRouter, None, None, None, None, true)
            .unwrap()
            .unwrap();
        assert_eq!(selection.model.as_deref(), Some("openrouter-claude-sonnet"));
        assert_eq!(
            selection.wire_model.as_deref(),
            Some("~anthropic/claude-sonnet-latest")
        );
        assert_eq!(ModelProvider::OpenRouter.wire_prefixes(), &[] as &[&str]);
        assert_eq!(
            infer_provider("~anthropic/claude-sonnet-latest"),
            None,
            "an Anthropic-owned wire alias must not imply the OpenRouter gateway"
        );
        assert!(matches!(
            resolve(None, Some("~anthropic/claude-sonnet-latest"), None, None),
            Err(SelectionError::UnknownModel(_))
        ));
        assert_eq!(
            resolve(None, Some("openrouter-claude-sonnet"), None, None)
                .unwrap()
                .unwrap()
                .provider,
            ModelProvider::OpenRouter
        );
        assert_eq!(
            serde_json::to_value(&selection).unwrap()["provider"],
            "openrouter"
        );
    }

    #[test]
    fn launch_legacy_effort_suffix_suppresses_saved_effort() {
        let saved = ProviderSelectionDefaults {
            model: None,
            effort: Some(EffortLevel::High),
            context_window: None,
        };
        let selection = resolve_for_launch(
            ModelProvider::Codex,
            Some("terra@low"),
            None,
            None,
            Some(saved),
            true,
        )
        .unwrap()
        .unwrap();
        assert_eq!(selection.effort, Some(EffortLevel::Low));
        assert_eq!(
            selection.effort_source,
            Some(SelectionSource::LegacyModelSuffix)
        );
    }
}
