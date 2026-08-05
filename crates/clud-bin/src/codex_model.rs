//! Codex model + reasoning-effort selection for the bridge (issue #752).
//!
//! # Why selection lives in the model string
//!
//! The Claude harness talks to the bridge over the Anthropic Messages API,
//! which has no field for "which Codex model" or "at what effort". Two
//! channels could carry that intent, and they are not equally reliable:
//!
//! - **`output_config.effort`** — what `/effort` and the `/model` slider
//!   populate. It only appears when the harness decides the model *supports*
//!   effort, which it determines by pattern-matching the model id against
//!   known families. A raw `gpt-5.6-*` id matches nothing, so the control may
//!   never be offered and the field may never be sent.
//! - **The model id itself** — never validated, never rewritten, and never
//!   dropped behind a custom `ANTHROPIC_BASE_URL`, because the gateway is
//!   declared to own the namespace. Whatever the user types in `/model`
//!   arrives here verbatim.
//!
//! So the model string is the only channel that cannot silently fail, which
//! is why `<alias>@<effort>` is the primary spelling and `output_config` is
//! the secondary one. `/model terra@max` works regardless of how the
//! harness's capability matching resolves; `/effort max` works only if it
//! resolves favourably. Supporting both costs one parser.
//!
//! # Precedence
//!
//! `@effort` suffix → `output_config.effort` → the `thinking` budget ladder →
//! the model's own catalog default. Most specific wins, and the two explicit
//! channels beat the two inferred ones.

use std::fmt;

/// Reasoning effort, restricted to what the gpt-5.6 family actually accepts.
///
/// Deliberately **not** the full Responses enum. `minimal` is a valid API
/// value that no gpt-5.6 model supports — the family starts at `low` — and
/// the old budget ladder emitted it for small budgets, so every such request
/// was rejected upstream for a reason the user could not see. `ultra` is a
/// Codex *product* orchestration mode and was never a `reasoning.effort`
/// value at all.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Effort {
    /// Reasoning off. Reachable only through `thinking: {"type":"disabled"}`
    /// or an explicit `@none`.
    None,
    Low,
    Medium,
    High,
    XHigh,
    Max,
}

impl Effort {
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

    /// Every spelling a user can type, in ladder order. Used for parsing and
    /// for the "valid values are ..." half of an error message, so the two can
    /// never drift.
    pub const ALL: [Self; 6] = [
        Self::None,
        Self::Low,
        Self::Medium,
        Self::High,
        Self::XHigh,
        Self::Max,
    ];

    pub fn parse(value: &str) -> Option<Self> {
        let value = value.trim().to_ascii_lowercase();
        Self::ALL
            .into_iter()
            .find(|effort| effort.as_str() == value)
    }

    fn catalog() -> String {
        Self::ALL
            .iter()
            .map(|effort| effort.as_str())
            .collect::<Vec<_>>()
            .join(", ")
    }
}

impl fmt::Display for Effort {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

/// One row of the gpt-5.6 catalog: the id that goes on the wire, the short
/// name a user types, and the effort the model itself defaults to.
///
/// The default effort is per-model and is **not** uniform: `sol` defaults to
/// `low` while `terra` and `luna` default to `medium`. Hardcoding one global
/// default (as the bridge did) silently over- or under-spends depending on
/// which model is selected.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CodexModel {
    pub id: &'static str,
    pub alias: &'static str,
    pub default_effort: Effort,
}

/// The known family. Unknown ids are still forwarded verbatim (see
/// [`ModelSpec::parse`]) — this table exists to give short names, correct
/// per-model defaults, and a typo-proof error message, not to gatekeep.
pub const CODEX_MODELS: [CodexModel; 3] = [
    CodexModel {
        id: "gpt-5.6-sol",
        alias: "sol",
        default_effort: Effort::Low,
    },
    CodexModel {
        id: "gpt-5.6-terra",
        alias: "terra",
        default_effort: Effort::Medium,
    },
    CodexModel {
        id: "gpt-5.6-luna",
        alias: "luna",
        default_effort: Effort::Medium,
    },
];

/// Look up a catalog row by wire id.
pub fn model_by_id(id: &str) -> Option<CodexModel> {
    CODEX_MODELS.into_iter().find(|model| model.id == id)
}

fn model_by_alias(alias: &str) -> Option<CodexModel> {
    let alias = alias.trim().to_ascii_lowercase();
    CODEX_MODELS
        .into_iter()
        .find(|model| model.alias == alias || model.id == alias)
}

fn alias_catalog() -> String {
    CODEX_MODELS
        .iter()
        .map(|model| model.alias)
        .collect::<Vec<_>>()
        .join(", ")
}

/// A parsed `<model>[@<effort>]` selection.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModelSpec {
    /// The id that goes on the wire — an alias is already expanded here.
    pub model: String,
    /// Effort the user asked for explicitly. `None` means "not specified",
    /// which is not the same as "medium": it defers to the lower-precedence
    /// channels and ultimately to the model's own default.
    pub effort: Option<Effort>,
}

impl ModelSpec {
    /// Parse `sol`, `terra@max`, `gpt-5.6-luna@low`, or a bare unknown id.
    ///
    /// An **unknown alias fails**; an unknown *full* id does not. The
    /// distinction is what keeps a typo from being billed: `/model tera` is
    /// almost certainly a slip for `terra`, and silently forwarding it earns a
    /// confusing upstream 400 (or worse, silently falls back to a default and
    /// bills the wrong model). But `gpt-5.7-whatever` is how a user reaches a
    /// model released after this table was written, and refusing it would make
    /// the table a gate we would have to keep updating.
    ///
    /// The heuristic: anything containing a `-` or `.` is a full id and passes
    /// through; a bare word must be in the alias table.
    pub fn parse(raw: &str) -> Result<Self, SelectionError> {
        let raw = raw.trim();
        if raw.is_empty() {
            return Err(SelectionError::EmptyModel);
        }
        let (model_part, effort_part) = match raw.rsplit_once('@') {
            Some((model, effort)) => (model.trim(), Some(effort.trim())),
            None => (raw, None),
        };
        if model_part.is_empty() {
            return Err(SelectionError::EmptyModel);
        }

        let effort = match effort_part {
            None => None,
            Some(value) => {
                Some(
                    Effort::parse(value).ok_or_else(|| SelectionError::UnknownEffort {
                        given: value.to_string(),
                    })?,
                )
            }
        };

        let model = match model_by_alias(model_part) {
            Some(model) => model.id.to_string(),
            None if looks_like_full_id(model_part) => model_part.to_string(),
            None => {
                return Err(SelectionError::UnknownAlias {
                    given: model_part.to_string(),
                })
            }
        };

        Ok(Self { model, effort })
    }

    /// The effort to send when no higher-precedence channel supplied one:
    /// this spec's explicit `@effort`, else the model's catalog default, else
    /// `medium` for an id we do not know.
    pub fn effective_effort(&self) -> Effort {
        self.effort.unwrap_or_else(|| {
            model_by_id(&self.model)
                .map(|model| model.default_effort)
                .unwrap_or(Effort::Medium)
        })
    }

    /// How the selection is spelled back to the user — in `--dry-run`, in the
    /// picker entry, and in error messages. Round-trips through [`parse`].
    pub fn display(&self) -> String {
        match self.effort {
            Some(effort) => format!("{}@{}", self.model, effort),
            None => self.model.clone(),
        }
    }
}

/// A bare word must be a known alias; anything with id-shaped punctuation is
/// forwarded as a full model id.
fn looks_like_full_id(value: &str) -> bool {
    value.contains('-') || value.contains('.')
}

// ---------------------------------------------------------------------------
// Model picker
// ---------------------------------------------------------------------------

/// The one row clud can add to Claude Code's `/model` picker.
///
/// # Why one row and not three (issue #820)
///
/// Read against Claude Code 2.1.212's own picker builder, there are exactly
/// six sources of rows, and none of them can be made to yield three honest
/// Codex entries:
///
/// 1. The built-in Anthropic lineup, optionally *renamed* by
///    `ANTHROPIC_DEFAULT_{OPUS,SONNET,HAIKU,FABLE}_MODEL`. Repointing those at
///    Codex ids is the tier hijacking DD-035 already rejected: it burns the
///    Anthropic names and lies about what is running.
/// 2. `ANTHROPIC_CUSTOM_MODEL_OPTION` — read once, as a scalar, and pushed as
///    a single `{value, label, description}`. There is no indexed, repeated,
///    or delimited form: the binary contains exactly four names in this
///    family (`…_OPTION`, `…_OPTION_NAME`, `…_OPTION_DESCRIPTION`,
///    `…_OPTION_SUPPORTED_CAPABILITIES`), all scalars.
/// 3. Gateway discovery (`GET /v1/models`), which needs an opt-in
///    `CLAUDE_CODE_ENABLE_GATEWAY_MODEL_DISCOVERY`, does not run while
///    non-essential traffic is disabled (clud forces that off), and filters
///    the response with `/^(claude|anthropic)/i` — dropping every id the
///    bridge serves. #820 rules it out explicitly, as does DD-035.
/// 4. `additionalModelOptionsCache` in the user's global config: a cache of
///    Anthropic's *own* server response, refreshed behind our back. Not an
///    extension point.
/// 5. The `availableModels` settings allowlist, which only ever *adds* ids
///    matching `anthropic.…` or `claude-…`; `gpt-5.6-*` is skipped.
/// 6. Whatever model is currently selected, which is a row because it is
///    selected — not a way to advertise one that is not.
///
/// So the honest ceiling is one row, and the row's description is the only
/// place the other two models can be named. `/model <id>` still accepts any
/// string (a custom `ANTHROPIC_BASE_URL` owns the namespace), so naming them
/// is enough to make them reachable.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PickerEntry {
    /// `ANTHROPIC_CUSTOM_MODEL_OPTION` — sent back verbatim as the request's
    /// model when the row is chosen, so it must round-trip [`ModelSpec::parse`].
    pub value: String,
    /// `ANTHROPIC_CUSTOM_MODEL_OPTION_NAME` — the row's label.
    pub name: String,
    /// `ANTHROPIC_CUSTOM_MODEL_OPTION_DESCRIPTION` — the catalog, because
    /// there is no second row to put it in.
    pub description: String,
}

/// Build the picker row for a selection.
pub fn picker_entry(selection: &ModelSpec) -> PickerEntry {
    let value = selection.display();
    PickerEntry {
        name: format!("Codex {value}"),
        description: picker_description(),
        value,
    }
}

/// Rendered from the catalog and the effort ladder so the row can never
/// advertise a model or an effort the parser would then reject.
fn picker_description() -> String {
    let models = CODEX_MODELS
        .iter()
        .map(|model| format!("{} = {} ({})", model.alias, model.id, model.default_effort))
        .collect::<Vec<_>>()
        .join(", ");
    format!(
        "Codex through clud. Claude Code shows one custom row, so switch with \
         /model <name>: {models}. Append @<effort> to override: {}.",
        Effort::catalog()
    )
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SelectionError {
    EmptyModel,
    UnknownAlias { given: String },
    UnknownEffort { given: String },
}

impl fmt::Display for SelectionError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::EmptyModel => write!(formatter, "model selection is empty"),
            // Naming the valid values is the whole point: the failure mode
            // being prevented is a typo that would otherwise be billed
            // against the wrong model or rejected upstream with no hint.
            Self::UnknownAlias { given } => write!(
                formatter,
                "unknown Codex model '{given}' — valid short names are {} \
                 (or pass a full model id such as gpt-5.6-terra)",
                alias_catalog()
            ),
            Self::UnknownEffort { given } => write!(
                formatter,
                "unknown reasoning effort '{given}' — valid efforts are {}",
                Effort::catalog()
            ),
        }
    }
}

impl std::error::Error for SelectionError {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn short_names_expand_to_wire_ids() {
        assert_eq!(ModelSpec::parse("sol").unwrap().model, "gpt-5.6-sol");
        assert_eq!(ModelSpec::parse("terra").unwrap().model, "gpt-5.6-terra");
        assert_eq!(ModelSpec::parse("luna").unwrap().model, "gpt-5.6-luna");
        assert_eq!(ModelSpec::parse(" TERRA ").unwrap().model, "gpt-5.6-terra");
    }

    #[test]
    fn the_effort_suffix_is_parsed_off_the_model_id() {
        let spec = ModelSpec::parse("terra@max").unwrap();
        assert_eq!(spec.model, "gpt-5.6-terra");
        assert_eq!(spec.effort, Some(Effort::Max));

        let spec = ModelSpec::parse("gpt-5.6-luna@low").unwrap();
        assert_eq!(spec.model, "gpt-5.6-luna");
        assert_eq!(spec.effort, Some(Effort::Low));
    }

    #[test]
    fn no_suffix_means_unspecified_not_medium() {
        // The distinction matters: `sol` must fall back to sol's own `low`,
        // not to a global `medium` that would overspend on every turn.
        let spec = ModelSpec::parse("sol").unwrap();
        assert_eq!(spec.effort, None);
        assert_eq!(spec.effective_effort(), Effort::Low);
        assert_eq!(
            ModelSpec::parse("terra").unwrap().effective_effort(),
            Effort::Medium
        );
        assert_eq!(
            ModelSpec::parse("luna").unwrap().effective_effort(),
            Effort::Medium
        );
    }

    #[test]
    fn an_unknown_alias_names_the_valid_ones() {
        let error = ModelSpec::parse("tera").unwrap_err();
        let rendered = error.to_string();
        assert!(rendered.contains("'tera'"), "{rendered}");
        for alias in ["sol", "terra", "luna"] {
            assert!(rendered.contains(alias), "{rendered} must name {alias}");
        }
    }

    #[test]
    fn an_unknown_effort_names_the_valid_ones() {
        // `ultra` is the trap: it is real in the Codex catalog as a product
        // mode, so a user can reasonably try it, but it is not a
        // `reasoning.effort` value and upstream rejects it.
        let rendered = ModelSpec::parse("terra@ultra").unwrap_err().to_string();
        assert!(rendered.contains("'ultra'"), "{rendered}");
        for effort in ["low", "medium", "high", "xhigh", "max"] {
            assert!(rendered.contains(effort), "{rendered} must name {effort}");
        }
    }

    #[test]
    fn minimal_is_not_an_accepted_effort() {
        // No gpt-5.6 model supports it; accepting it here would resurrect the
        // silent upstream rejection the ladder used to cause.
        assert!(Effort::parse("minimal").is_none());
        assert!(ModelSpec::parse("terra@minimal").is_err());
    }

    #[test]
    fn an_unknown_full_id_passes_through_but_a_bare_word_does_not() {
        // Forward-compatibility: a model released after this table was
        // written must remain reachable.
        let spec = ModelSpec::parse("gpt-5.7-nova@high").unwrap();
        assert_eq!(spec.model, "gpt-5.7-nova");
        assert_eq!(spec.effort, Some(Effort::High));
        // ... but a bare word cannot be forwarded, because it is far more
        // likely a typo than a real id, and a typo must not reach billing.
        assert!(ModelSpec::parse("nova").is_err());
    }

    #[test]
    fn an_unknown_id_falls_back_to_medium() {
        assert_eq!(
            ModelSpec::parse("gpt-5.7-nova").unwrap().effective_effort(),
            Effort::Medium
        );
    }

    #[test]
    fn display_round_trips_through_parse() {
        for raw in ["terra", "sol@max", "gpt-5.6-luna@none", "gpt-5.7-nova"] {
            let spec = ModelSpec::parse(raw).unwrap();
            assert_eq!(
                ModelSpec::parse(&spec.display()).unwrap(),
                spec,
                "{raw} must round-trip"
            );
        }
    }

    /// Issue #820: the one row the harness will render has to carry the whole
    /// catalog, because there is no second row to put the rest in.
    #[test]
    fn the_single_picker_row_names_every_selectable_model() {
        let entry = picker_entry(&ModelSpec::parse("sol").unwrap());

        // The row's value is what `/model` will send, so it must survive the
        // same parser the bridge resolves requests with.
        assert_eq!(entry.value, "gpt-5.6-sol");
        assert_eq!(ModelSpec::parse(&entry.value).unwrap().model, "gpt-5.6-sol");
        assert!(entry.name.contains("gpt-5.6-sol"), "{}", entry.name);

        // Terra and luna are unreachable from a second row, so the
        // description is the only place a user can discover them.
        for model in CODEX_MODELS {
            assert!(
                entry.description.contains(model.id),
                "{} must name {}",
                entry.description,
                model.id
            );
            assert!(
                entry.description.contains(model.alias),
                "{} must name {}",
                entry.description,
                model.alias
            );
        }
        for effort in Effort::ALL {
            assert!(
                entry.description.contains(effort.as_str()),
                "{} must name {effort}",
                entry.description
            );
        }
    }

    /// An `@effort` selection stays on the row verbatim: the value is what the
    /// harness sends back, so dropping the suffix would silently reset effort
    /// the moment a user picked their own row out of the picker.
    #[test]
    fn the_picker_row_keeps_the_effort_suffix_it_was_launched_with() {
        let entry = picker_entry(&ModelSpec::parse("luna@xhigh").unwrap());
        assert_eq!(entry.value, "gpt-5.6-luna@xhigh");
        assert_eq!(
            ModelSpec::parse(&entry.value).unwrap(),
            ModelSpec::parse("luna@xhigh").unwrap()
        );
    }

    /// A forward-compatible id has no catalog row, so the label falls back to
    /// the id rather than inventing a short name for it.
    #[test]
    fn an_unknown_full_id_still_produces_a_usable_row() {
        let entry = picker_entry(&ModelSpec::parse("gpt-5.7-nova@high").unwrap());
        assert_eq!(entry.value, "gpt-5.7-nova@high");
        assert!(entry.name.contains("gpt-5.7-nova@high"), "{}", entry.name);
    }

    #[test]
    fn empty_and_malformed_selections_are_rejected() {
        assert_eq!(
            ModelSpec::parse("").unwrap_err(),
            SelectionError::EmptyModel
        );
        assert_eq!(
            ModelSpec::parse("   ").unwrap_err(),
            SelectionError::EmptyModel
        );
        assert_eq!(
            ModelSpec::parse("@max").unwrap_err(),
            SelectionError::EmptyModel
        );
    }
}
