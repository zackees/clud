//! Windows-only tests of `crate::foreground_runtime`: Windows environment
//! keys are case-insensitive.

#[cfg(test)]
mod tests {
    use crate::backend::{Backend, ModelProvider};
    use crate::foreground_runtime::tests::{lookup, plan};
    use crate::foreground_runtime::{env_key_eq, ForegroundRuntime};

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
}
