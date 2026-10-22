//! Launch-local reversible request redaction (#1868).

pub use super::session::{Counts, Session};

#[cfg(test)]
mod tests {
    use std::borrow::Cow;

    use super::*;

    fn session() -> Session {
        Session::with_key([7; 32], Vec::new())
    }

    fn openrouter() -> String {
        format!("sk-or-v1-{}", "a".repeat(64))
    }

    #[test]
    fn detects_precise_patterns_and_classifies_prefixes_before_openai() {
        let fixtures = [
            ("openrouter", openrouter()),
            ("anthropic", format!("sk-ant-{}", "a".repeat(24))),
            ("openai", format!("sk-proj-{}", "a".repeat(24))),
            ("github", format!("ghp_{}", "a".repeat(36))),
            ("github", format!("github_pat_{}", "a".repeat(60))),
            ("aws", format!("AKIA{}", "A".repeat(16))),
            ("slack", format!("xoxb-{}", "a".repeat(20))),
            ("google", format!("AIza{}", "a".repeat(35))),
            ("stripe", format!("sk_live_{}", "a".repeat(24))),
            (
                "jwt",
                format!(
                    "eyJ{}.{}.{}",
                    "a".repeat(20),
                    "b".repeat(20),
                    "c".repeat(20)
                ),
            ),
            (
                "pem",
                "-----BEGIN PRIVATE KEY-----\nFAKE_KEY\n-----END PRIVATE KEY-----".to_string(),
            ),
        ];
        for (kind, secret) in fixtures {
            let mut state = session();
            let token = state.redact_text(&secret).into_owned();
            assert!(
                token.starts_with("clud_secret_"),
                "missing {kind} detection"
            );
            assert_eq!(token.len(), 24);
            assert_eq!(state.restore_text(&token), secret);
            assert_eq!(state.counts().kinds.get(kind), Some(&1));
            assert_eq!(state.counts().kinds.len(), 1);
        }
    }

    #[test]
    fn preserves_code_hashes_names_addresses_and_non_secret_boundaries() {
        let corpus = [
            "0123456789abcdef0123456789abcdef01234567",
            "zackees/ci.yml@0123456789abcdef0123456789abcdef01234567",
            "sha256:0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef",
            "12345678-abcd-4321-abcd-123456789abc",
            "while compiler_start <= time { let task = builder.args(); }",
            "jane.doe@example.com 0.0.0.0 127.0.0.1 10.0.0.12",
            "data:image/png;base64,iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJ",
            "const x=()=>({name:'Jane Doe',address:'10 Main Street'});",
            "task-abcdefghijklmnopqrstuvwxyz",
            "prefixAKIAAAAAAAAAAAAAAAAAsuffix",
            "-----BEGIN PUBLIC KEY-----\nFAKE_PUBLIC\n-----END PUBLIC KEY-----",
            "-----BEGIN CERTIFICATE-----\nFAKE_CERT\n-----END CERTIFICATE-----",
        ];
        let mut state = session();
        for text in corpus {
            assert!(
                matches!(state.redact_text(text), Cow::Borrowed(_)),
                "{text}"
            );
        }
        assert!(state.counts().kinds.is_empty());
    }

    #[test]
    fn tokens_are_stable_within_a_session_and_unlinkable_across_sessions() {
        let secret = openrouter();
        let mut first = session();
        let token = first.redact_text(&secret).into_owned();
        assert_eq!(first.redact_text(&secret), token);
        assert_ne!(token, secret);
        assert_ne!(
            Session::with_key([8; 32], Vec::new()).redact_text(&secret),
            token
        );
        let second = format!("sk-or-v1-{}", "b".repeat(64));
        assert_ne!(first.redact_text(&second), token);
    }

    #[test]
    fn truncated_hmac_collision_never_overwrites_another_secret() {
        let secret = openrouter();
        let mut state = session();
        let occupied = state.reserve_collision(&secret);
        let token = state.redact_text(&secret).into_owned();
        assert_ne!(token, occupied);
        assert_eq!(state.restore_text(&occupied), "DIFFERENT_FAKE_SECRET");
        assert_eq!(state.restore_text(&token), secret);
        assert_eq!(state.redact_text(&secret), token);
    }

    #[test]
    fn nonstream_json_restoration_keeps_tool_input_escaping_valid() {
        let secret = "FAKE_\"quote\\slash\nnewline_\u{1f600}";
        let mut state = Session::with_key([7; 32], vec![secret.to_string()]);
        let input = serde_json::to_vec(&serde_json::json!({"content":[
            {"type":"text", "text":secret},
            {"type":"tool_use", "input":{"private_key":secret}}
        ]}))
        .unwrap();
        let encoded = state.redact_body(&input).unwrap().into_owned();
        assert!(!String::from_utf8_lossy(&encoded).contains("FAKE_"));
        let restored = state.restore_body(&encoded).unwrap();
        assert_eq!(
            serde_json::from_slice::<serde_json::Value>(&restored).unwrap(),
            serde_json::from_slice::<serde_json::Value>(&input).unwrap()
        );
    }

    #[test]
    fn scans_system_messages_tool_results_historical_inputs_and_text_documents() {
        let secret = "FAKE_VAULT_SENTINEL_WITH_NO_PROVIDER_PREFIX";
        let mut state = Session::with_key([7; 32], vec![secret.to_string()]);
        let body = serde_json::json!({
            "system": [{"type":"text", "text":secret}],
            "messages": [{"role":"user", "content":[
                {"type":"text", "text":secret},
                {"type":"tool_result", "tool_use_id":"x", "content":secret},
                {"type":"tool_result", "tool_use_id":"y", "content":[{"type":"text","text":secret}]},
                {"type":"tool_use", "id":"z", "name":"write", "input":{"nested":[secret]}},
                {"type":"document", "source":{"type":"text","media_type":"text/plain","data":secret}},
            ]}]
        });
        let input = serde_json::to_vec(&body).unwrap();
        let output = state.redact_body(&input).unwrap().into_owned();
        assert!(!String::from_utf8_lossy(&output).contains(secret));
        let restored = state.restore_text(std::str::from_utf8(&output).unwrap());
        assert_eq!(
            serde_json::from_str::<serde_json::Value>(&restored).unwrap(),
            body
        );
        assert_eq!(state.counts().kinds.get("vault"), Some(&6));
    }

    #[test]
    fn leaves_images_and_binary_documents_untouched() {
        let secret = openrouter();
        let input = serde_json::to_vec(&serde_json::json!({"messages":[{"content":[
            {"type":"image","source":{"type":"base64","data":secret}},
            {"type":"document","source":{"type":"base64","data":secret}}
        ]}]}))
        .unwrap();
        assert!(matches!(
            session().redact_body(&input).unwrap(),
            Cow::Borrowed(_)
        ));
    }

    #[test]
    fn clean_requests_preserve_every_original_byte() {
        let mut state = session();
        let mut seed = 17_u64;
        for _ in 0..100 {
            seed = seed.wrapping_mul(6364136223846793005).wrapping_add(1);
            let body = format!(
                " {{\n  \"messages\" : [{{\"role\":\"user\", \"content\":\"code {seed}\"}}] }}\n"
            );
            assert!(matches!(
                state.redact_body(body.as_bytes()).unwrap(),
                Cow::Borrowed(_)
            ));
        }
    }

    #[test]
    fn split_tokens_restore_once_at_every_two_and_three_fragment_boundary() {
        let mut state = session();
        let secret = openrouter();
        let token = state.redact_text(&secret).into_owned();
        assert!(token.starts_with("clud_secret_"));
        assert!(!token.contains(&secret));
        for first in 0..=token.len() {
            for second in first..=token.len() {
                let mut pending = String::new();
                let mut restored =
                    state.restore_fragment(&mut pending, &token[..first], false, false);
                restored.push_str(&state.restore_fragment(
                    &mut pending,
                    &token[first..second],
                    false,
                    false,
                ));
                restored.push_str(&state.restore_fragment(
                    &mut pending,
                    &token[second..],
                    false,
                    true,
                ));
                assert_eq!(restored, secret, "offsets {first},{second}");
                assert!(pending.is_empty());
            }
        }
    }

    #[test]
    fn restored_json_fragments_escape_quotes_slashes_and_newlines() {
        let secret = "FAKE_PRIVATE_\"quote\\slash\nnext";
        let mut state = Session::with_key([7; 32], vec![secret.to_string()]);
        let token = state.redact_text(secret).into_owned();
        let mut pending = String::new();
        let mut result = state.restore_fragment(
            &mut pending,
            &format!("{{\"value\":\"{}", &token[..8]),
            true,
            false,
        );
        result.push_str(&state.restore_fragment(
            &mut pending,
            &format!("{}\"}}", &token[8..]),
            true,
            true,
        ));
        assert_eq!(
            serde_json::from_str::<serde_json::Value>(&result).unwrap()["value"],
            secret
        );
    }

    #[test]
    fn unknown_tokens_pass_through_and_are_counted_without_values() {
        let mut state = session();
        let text = "model-invented clud_secret_deadbeef0000";
        assert_eq!(state.restore_text(text), text);
        assert_eq!(state.counts().unknown_tokens, 1);
    }

    #[test]
    fn incomplete_tokens_flush_verbatim_at_block_end() {
        let mut state = session();
        let mut pending = String::new();
        assert_eq!(
            state.restore_fragment(&mut pending, "hello clud_sec", false, false),
            "hello "
        );
        assert!(pending.len() <= 24);
        assert_eq!(
            state.restore_fragment(&mut pending, "", false, true),
            "clud_sec"
        );
        assert!(pending.is_empty());
    }

    #[test]
    fn round_trip_echo_preserves_code_and_repeated_credentials() {
        let mut state = session();
        for index in 0..100 {
            let input = format!(
                "while compiler_start <= time {{}} // {index} {} {}",
                openrouter(),
                openrouter()
            );
            let encoded = state.redact_text(&input).into_owned();
            assert!(!encoded.contains("sk-or-v1-"));
            assert_eq!(state.restore_text(&encoded), input);
        }
    }

    #[test]
    fn status_contains_counts_only_and_never_secret_or_mapping() {
        let secret = openrouter();
        let mut state = session();
        let token = state.redact_text(&secret).into_owned();
        let status = serde_json::to_string(&state.counts()).unwrap();
        let debug = format!("{:?}", state.counts());
        assert!(!status.contains(&secret));
        assert!(!status.contains(&token));
        assert!(!debug.contains(&secret));
        assert!(!debug.contains(&token));
        assert_eq!(state.counts().kinds.get("openrouter"), Some(&1));
    }
}
