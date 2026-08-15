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

/// The registry: DeepSeek's row from Phase 1, plus Kimi's row landed in
/// #937 Phase 3.
pub const ANTHROPIC_COMPAT_PROVIDERS: &[AnthropicCompatProvider] = &[
    AnthropicCompatProvider {
        provider: ModelProvider::DeepSeek,
        display_name: "DeepSeek",
        settings_id: "deepseek",
        cli_flag: "--deepseek",
        // Bound to the vault module's own constants rather than re-typed as
        // literals: two independent copies of a credential identifier can
        // drift silently, and drift here orphans stored keys.
        vault_service: crate::provider_auth::DEEPSEEK_VAULT_SERVICE,
        vault_account: crate::provider_auth::DEEPSEEK_VAULT_ACCOUNT,
        anthropic_base_url: "https://api.deepseek.com/anthropic",
        login_command: "clud auth login deepseek",
        subagent_wire_id: "deepseek-v4-flash",
    },
    AnthropicCompatProvider {
        provider: ModelProvider::Kimi,
        display_name: "Kimi",
        settings_id: "kimi",
        cli_flag: "--kimi",
        vault_service: crate::provider_auth::KIMI_VAULT_SERVICE,
        vault_account: crate::provider_auth::KIMI_VAULT_ACCOUNT,
        anthropic_base_url: "https://api.moonshot.ai/anthropic",
        login_command: "clud auth login kimi",
        // Unlike DeepSeek (which points haiku/subagent at a cheaper flash
        // model), Kimi's official Claude Code profile points the haiku AND
        // subagent slots at the same main model: kimi-k3[1m].
        subagent_wire_id: "kimi-k3[1m]",
    },
];

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

    #[test]
    fn kimi_vault_identifiers_are_frozen_for_credential_continuity() {
        let descriptor = descriptor_for(ModelProvider::Kimi).unwrap();
        assert_eq!(descriptor.vault_service, "clud.kimi");
        assert_eq!(descriptor.vault_account, "api-key-v1");
    }

    /// Cross-provider vault isolation (#937 Phase 3 DoD): Kimi's vault
    /// service must differ from DeepSeek's even though both currently use
    /// the same account name, so a launch never reads or writes the wrong
    /// provider's credential.
    #[test]
    fn kimi_and_deepseek_vault_services_are_isolated() {
        let deepseek = descriptor_for(ModelProvider::DeepSeek).unwrap();
        let kimi = descriptor_for(ModelProvider::Kimi).unwrap();
        assert_ne!(deepseek.vault_service, kimi.vault_service);
        assert_eq!(kimi.vault_service, "clud.kimi");
    }

    #[test]
    fn kimi_subagent_wire_id_points_at_the_same_model_unlike_deepseek() {
        // Kimi's official Claude Code profile is deliberately different from
        // DeepSeek's: haiku/subagent slots point at the SAME model as the
        // main model, not a cheaper flash variant.
        let deepseek = descriptor_for(ModelProvider::DeepSeek).unwrap();
        let kimi = descriptor_for(ModelProvider::Kimi).unwrap();
        assert_eq!(kimi.subagent_wire_id, "kimi-k3[1m]");
        // DeepSeek's subagent slot is a cheaper flash model, not its main
        // model -- Kimi's is the same model, which is the documented
        // asymmetry between the two profiles.
        assert_ne!(deepseek.subagent_wire_id, "deepseek-v4-pro[1m]");
        assert_ne!(kimi.subagent_wire_id, deepseek.subagent_wire_id);
    }
}
