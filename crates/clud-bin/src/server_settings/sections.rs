//! The registry of server settings sections, and the sections themselves.
//!
//! To add a setting: define a `Deserialize` struct, implement [`Section`] with
//! its key and semantic checks, list it in [`SECTIONS`], and add its built-in
//! value under `sections` in `assets/server-settings.json`.

use std::sync::OnceLock;

use serde::Deserialize;

use super::{Section, SectionSpec};
use crate::backend::ModelProvider;
use crate::provider_catalog;

/// Every section this build validates. A section missing here is treated as
/// unknown: kept verbatim in the cache, never decoded.
pub(crate) const SECTIONS: &[SectionSpec] = &[SectionSpec::of::<DeepSeekSettings>()];

/// DeepSeek model names. DeepSeek renames API slugs in place
/// (`deepseek-v4-flash` became `deepseek-flash` with V4.1-Flash), so these
/// names must be changeable without a clud release. The values can only name
/// `deepseek-*` models: never another provider, a URL, or a credential.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct DeepSeekSettings {
    /// Direct-launch default when neither `--model` nor a saved profile names
    /// one. Resolved through the catalog exactly like a CLI spelling.
    pub default_model: String,
    /// Wire ID for Claude Code's haiku and subagent slots.
    pub subagent_model: String,
}

const MAX_MODEL_ID_LEN: usize = 64;

impl Section for DeepSeekSettings {
    const KEY: &'static str = "deepseek";

    fn validate(&self) -> Result<(), String> {
        for (field, value) in [
            ("default_model", &self.default_model),
            ("subagent_model", &self.subagent_model),
        ] {
            if !is_deepseek_model_id(value) {
                return Err(format!("{field} is not a DeepSeek model ID"));
            }
        }
        Ok(())
    }
}

fn is_deepseek_model_id(value: &str) -> bool {
    let base = value.strip_suffix("[1m]").unwrap_or(value);
    !base.is_empty()
        && base.len() <= MAX_MODEL_ID_LEN
        && base
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-'))
        && provider_catalog::infer_provider(base) == Some(ModelProvider::DeepSeek)
}

/// DeepSeek's settings for this process.
pub fn deepseek() -> &'static DeepSeekSettings {
    static SETTINGS: OnceLock<DeepSeekSettings> = OnceLock::new();
    SETTINGS.get_or_init(|| super::snapshot().section())
}

/// Served default model for `provider`, if its model names are served.
pub fn provider_default_model(provider: ModelProvider) -> Option<&'static str> {
    (provider == ModelProvider::DeepSeek).then(|| deepseek().default_model.as_str())
}

/// Served haiku/subagent wire ID for `provider`, if its model names are served.
pub fn provider_subagent_model(provider: ModelProvider) -> Option<&'static str> {
    (provider == ModelProvider::DeepSeek).then(|| deepseek().subagent_model.as_str())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::server_settings::built_in;

    fn deepseek_section(default_model: &str, subagent_model: &str) -> Result<(), String> {
        DeepSeekSettings {
            default_model: default_model.to_string(),
            subagent_model: subagent_model.to_string(),
        }
        .validate()
    }

    /// The built-in copy is what ships offline, so it must agree with the
    /// catalog's reviewed default and the registry's compiled-in subagent.
    #[test]
    fn built_in_deepseek_names_match_the_catalog_default_and_registry_fallback() {
        let settings = built_in::<DeepSeekSettings>();
        let selection = provider_catalog::resolve(
            Some(ModelProvider::DeepSeek),
            Some(&settings.default_model),
            None,
            None,
        )
        .unwrap()
        .unwrap();
        let catalog_default =
            provider_catalog::reviewed_default_model(ModelProvider::DeepSeek).unwrap();
        assert_eq!(selection.model.as_deref(), Some(catalog_default.cli_id));
        assert_eq!(
            selection.wire_model.as_deref(),
            Some(catalog_default.wire_id)
        );
        assert_eq!(
            settings.subagent_model,
            crate::provider_registry::descriptor_for(ModelProvider::DeepSeek)
                .unwrap()
                .subagent_wire_id
        );
    }

    #[test]
    fn served_model_names_are_deepseek_flash_and_apply_only_to_deepseek() {
        assert_eq!(
            provider_default_model(ModelProvider::DeepSeek),
            Some("deepseek-flash")
        );
        assert_eq!(
            provider_subagent_model(ModelProvider::DeepSeek),
            Some("deepseek-flash")
        );
        for provider in ModelProvider::ALL
            .iter()
            .copied()
            .filter(|provider| *provider != ModelProvider::DeepSeek)
        {
            assert_eq!(provider_default_model(provider), None, "{provider}");
            assert_eq!(provider_subagent_model(provider), None, "{provider}");
        }
    }

    #[test]
    fn deepseek_section_accepts_future_deepseek_names() {
        assert_eq!(
            deepseek_section("deepseek-flash-2[1m]", "deepseek-v5.0_lite"),
            Ok(())
        );
    }

    #[test]
    fn deepseek_section_rejects_anything_but_a_deepseek_model_name() {
        let too_long = format!("deepseek-{}", "a".repeat(MAX_MODEL_ID_LEN));
        for value in [
            "gpt-5.6-terra",
            "kimi-k3[1m]",
            "openrouter-claude-sonnet",
            "claude-opus",
            "https://example.invalid/deepseek-flash",
            "deepseek-flash@high",
            "deepseek flash",
            "deepseek-flash\n",
            "",
            "[1m]",
            too_long.as_str(),
        ] {
            assert!(
                deepseek_section(value, "deepseek-flash").is_err(),
                "default {value:?}"
            );
            assert!(
                deepseek_section("deepseek-flash", value).is_err(),
                "subagent {value:?}"
            );
        }
    }
}
