//! Launch policy contains no credentials or reversible mapping.

use crate::backend::ModelProvider;

pub const PATTERNS: &[&str] = &[
    "openrouter",
    "anthropic",
    "openai",
    "github",
    "aws",
    "slack",
    "google",
    "stripe",
    "jwt",
    "pem",
    "vault",
];

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Policy {
    pub enabled: bool,
    pub reason: String,
    pub patterns: Vec<String>,
}

impl Default for Policy {
    fn default() -> Self {
        Self::resolve(ModelProvider::Claude, None, None, false)
    }
}

impl Policy {
    pub fn resolve(
        provider: ModelProvider,
        model: Option<&str>,
        explicit: Option<bool>,
        training_allowed: bool,
    ) -> Self {
        let free = provider == ModelProvider::OpenRouter
            && model.is_some_and(|model| model.ends_with(":free"));
        let (enabled, reason) = match explicit {
            Some(true) => (true, "explicit --redact-secrets"),
            Some(false) => (false, "explicit --no-redact-secrets"),
            None if free => (true, "free OpenRouter model"),
            None if training_allowed => (true, "route explicitly allows training/data collection"),
            None => (false, "no free or explicitly training-allowed route"),
        };
        Self {
            enabled,
            reason: reason.to_string(),
            patterns: PATTERNS.iter().map(|kind| (*kind).to_string()).collect(),
        }
    }
}

/// A native harness cannot hand its direct HTTP path to the unified gateway.
pub const GATEWAY_REQUIRED: &str =
    "secret redaction requires clud's gateway; use --unified with the Claude harness";

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn explicit_training_policy_enables_redaction_and_flags_override_it() {
        for provider in [
            ModelProvider::Claude,
            ModelProvider::OpenRouter,
            ModelProvider::Codex,
        ] {
            let automatic = Policy::resolve(provider, Some("paid-model"), None, true);
            assert!(automatic.enabled);
            assert!(automatic.reason.contains("training"));
            assert!(!Policy::resolve(provider, Some("paid-model"), Some(false), true).enabled);
            assert!(Policy::resolve(provider, Some("paid-model"), Some(true), false).enabled);
            assert!(!Policy::resolve(provider, Some("paid-model"), None, false).enabled);
        }
    }
}
