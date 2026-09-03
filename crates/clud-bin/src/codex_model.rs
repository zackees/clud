//! Codex model + reasoning-effort selection for the bridge (issue #752).
//!
//! # Compatibility selector
//!
//! Current Claude Code clients discover registered `clud-claude-codex-*` IDs
//! and send effort independently. The `<model>@<effort>` parser remains the
//! bridge's compatibility boundary for old plans, continued sessions,
//! provider-native `none`, and explicit future wire IDs.
//!
//! `@effort` suffix → `output_config.effort` → the `thinking` budget ladder →
//! the model's own catalog default. Most specific wins, and the two explicit
//! channels beat the two inferred ones.

use std::fmt;

pub use crate::provider_catalog::EffortLevel as Effort;
use crate::provider_catalog::{CatalogModel, MODELS};

/// The Codex view of the provider-neutral model registry.
pub fn codex_models() -> impl Iterator<Item = CatalogModel> {
    MODELS
        .iter()
        .copied()
        .filter(|model| model.provider == crate::backend::ModelProvider::Codex)
}

/// Look up a catalog row by wire id.
pub fn model_by_id(id: &str) -> Option<CatalogModel> {
    codex_models().find(|model| model.wire_id == id)
}

fn model_by_alias(alias: &str) -> Option<CatalogModel> {
    let alias = alias.trim().to_ascii_lowercase();
    codex_models().find(|model| {
        model.cli_id == alias
            || model.wire_id == alias
            || model
                .legacy_aliases
                .iter()
                .any(|candidate| *candidate == alias)
    })
}

fn alias_catalog() -> String {
    codex_models()
        .filter_map(|model| model.legacy_aliases.first().copied())
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
    ///
    /// On the Codex bridge's request path this forward compatibility is
    /// qualified: #1005 refuses an uncataloged id there because the harness
    /// merges its own catalog into the picker and invents ids the user never
    /// chose. #1022 restores it for the one case where provenance is known --
    /// the id pinned at launch. See `codex_bridge::serve_codex_discovery_messages`.
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
            Some(model) => model.wire_id.to_string(),
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
                .and_then(|model| model.default_effort)
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
