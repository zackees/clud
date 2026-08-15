//! Registry of "Anthropic-compatible API-key providers": providers that speak
//! the Anthropic Messages API directly rather than needing a translation
//! bridge (as Codex does). Today this is DeepSeek only; Kimi lands in Phase 3
//! of #937 as a second row, per the design in #936's "Generalization"
//! section.
//!
//! This is a pure data hoist -- see #936 for why a `&'static` descriptor
//! table plus guardrail tests was chosen over a `dyn Provider` trait object:
//! every provider is known at compile time, and it matches this repo's
//! established pattern (`CatalogModel`, `BUNDLED_SKILLS`, `BUNDLED_TOOLS`).

use crate::backend::ModelProvider;

/// Descriptor for one Anthropic-compatible API-key provider. Fields capture
/// everything that varies between such providers; the shared logic (vault
/// access, preflight, child-env overlay, gateway proxying) stays provider-
/// neutral and is parameterized by these values.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AnthropicCompatProvider {
    pub provider: ModelProvider,
    /// Human-readable name, e.g. "DeepSeek".
    pub display_name: &'static str,
    /// `settings.json` / `--provider` value, e.g. "deepseek".
    pub settings_id: &'static str,
    /// CLI shortcut flag, e.g. "--deepseek".
    pub cli_flag: &'static str,
    /// Native-vault service identifier: the `keyring` service on non-Windows
    /// and the first half of the Windows Credential Manager target built by
    /// `provider_auth::vault_target`. Changing an existing provider's value
    /// orphans every already-stored credential in the OS credential vault.
    pub vault_service: &'static str,
    /// Native-vault account identifier: the `keyring` account on non-Windows
    /// and the second half of the Windows target. Same continuity guarantee
    /// as `vault_service`.
    pub vault_account: &'static str,
    /// Anthropic-compatible base URL the child talks to, e.g.
    /// `https://api.deepseek.com/anthropic`.
    pub anthropic_base_url: &'static str,
    /// `clud auth login <settings_id>` -- surfaced in preflight failure
    /// messages.
    pub login_command: &'static str,
    /// Wire model ID placed in the haiku/subagent env slots by the child-env
    /// overlay (e.g. `CLAUDE_CODE_SUBAGENT_MODEL`, `ANTHROPIC_DEFAULT_HAIKU_MODEL`).
    pub subagent_wire_id: &'static str,
}

/// The registry. Only the DeepSeek row exists in Phase 1; Kimi's row lands in
/// Phase 3.
pub const ANTHROPIC_COMPAT_PROVIDERS: &[AnthropicCompatProvider] = &[AnthropicCompatProvider {
    provider: ModelProvider::DeepSeek,
    display_name: "DeepSeek",
    settings_id: "deepseek",
    cli_flag: "--deepseek",
    // Bound to the vault module's own constants rather than re-typed as
    // literals: two independent copies of a credential identifier can drift
    // silently, and drift here orphans stored keys.
    vault_service: crate::provider_auth::DEEPSEEK_VAULT_SERVICE,
    vault_account: crate::provider_auth::DEEPSEEK_VAULT_ACCOUNT,
    anthropic_base_url: "https://api.deepseek.com/anthropic",
    login_command: "clud auth login deepseek",
    subagent_wire_id: "deepseek-v4-flash",
}];

/// Look up the Anthropic-compat descriptor for a provider, if it has one.
/// `Claude` and `Codex` never resolve here -- Claude is native, Codex is a
/// translation bridge, not a direct Anthropic-compatible route.
pub fn descriptor_for(provider: ModelProvider) -> Option<&'static AnthropicCompatProvider> {
    ANTHROPIC_COMPAT_PROVIDERS
        .iter()
        .find(|entry| entry.provider == provider)
}

/// A resolved unified-gateway route for one Anthropic-compatible provider:
/// its base URL and the API key retrieved from the vault at launch time.
/// Defined alongside the descriptor (rather than in `codex_bridge.rs`) so
/// Phase 4's gateway-refactor lane and out-of-file-consumer lane can each
/// depend on this type without depending on each other's landing order.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AnthropicCompatRoute {
    pub provider: ModelProvider,
    pub base_url: String,
    pub api_key: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_descriptor_resolves_to_itself_and_appears_once() {
        for (index, entry) in ANTHROPIC_COMPAT_PROVIDERS.iter().enumerate() {
            assert_eq!(descriptor_for(entry.provider), Some(entry));
            for other in &ANTHROPIC_COMPAT_PROVIDERS[index + 1..] {
                assert_ne!(
                    entry.provider, other.provider,
                    "provider appears twice in ANTHROPIC_COMPAT_PROVIDERS"
                );
            }
        }
    }

    #[test]
    fn claude_and_codex_have_no_anthropic_compat_descriptor() {
        assert_eq!(descriptor_for(ModelProvider::Claude), None);
        assert_eq!(descriptor_for(ModelProvider::Codex), None);
    }

    #[test]
    fn deepseek_vault_identifiers_are_frozen_for_credential_continuity() {
        // Changing either literal orphans every already-stored DeepSeek key
        // in the OS credential vault: the keyring::Entry path keys on
        // (service, account), and the Windows CredReadW/CredWriteW path
        // builds its TARGET as "{service}/{account}" (provider_auth.rs).
        // Do not "clean up" these strings without a migration.
        let descriptor = descriptor_for(ModelProvider::DeepSeek).unwrap();
        assert_eq!(descriptor.vault_service, "clud.deepseek");
        assert_eq!(descriptor.vault_account, "api-key-v1");
    }
}
