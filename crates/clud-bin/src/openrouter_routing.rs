//! `--provider-only`: pin an OpenRouter launch to named upstream providers.
//!
//! OpenRouter routes each request across every provider serving the model
//! unless the request body carries a `provider` object. On the direct route
//! clud is not in the request path, so it hands that object to Claude Code
//! through `CLAUDE_CODE_EXTRA_BODY`, a JSON object Claude Code merges into
//! every request. `allow_fallbacks` is always `false`: a pinned launch fails
//! a request rather than silently rerouting to another provider or price.

use serde_json::{json, Map, Value};

/// The variable Claude Code merges into every request body.
pub const EXTRA_BODY_ENV: &str = "CLAUDE_CODE_EXTRA_BODY";

/// Whether `slug` looks like an OpenRouter provider slug (`parasail`,
/// `parasail/fp8`, `deepinfra/bf16`): lowercase ASCII letters, digits and
/// `-`, `_`, `.`, `/`, not empty, not starting or ending with `/`.
pub fn valid_slug(slug: &str) -> bool {
    !slug.is_empty()
        && slug.len() <= 64
        && !slug.starts_with('/')
        && !slug.ends_with('/')
        && slug.bytes().all(|byte| {
            byte.is_ascii_lowercase() || byte.is_ascii_digit() || b"-_./".contains(&byte)
        })
}

/// Refusal for a `--provider-only` value, or `None` when every slug is valid.
pub fn invalid_slug(slugs: &[String]) -> Option<String> {
    slugs.iter().find(|slug| !valid_slug(slug)).map(|slug| {
        format!(
            "--provider-only {slug:?} is not an OpenRouter provider slug (lowercase, e.g. \
             `parasail/fp8`)"
        )
    })
}

/// The `CLAUDE_CODE_EXTRA_BODY` value for `slugs`, merged into an `existing`
/// value: other keys the user set are kept, and only `provider` is replaced.
/// An existing value that is not a JSON object is an error, not overwritten.
pub fn extra_body(existing: Option<&str>, slugs: &[String]) -> Result<String, String> {
    let mut body = match existing.map(str::trim).filter(|text| !text.is_empty()) {
        None => Map::new(),
        Some(text) => match serde_json::from_str::<Value>(text) {
            Ok(Value::Object(map)) => map,
            _ => {
                return Err(format!(
                    "--provider-only needs {EXTRA_BODY_ENV} to be a JSON object, but it is set \
                     to something else; unset it or make it an object"
                ))
            }
        },
    };
    body.insert(
        "provider".to_string(),
        json!({"only": slugs, "allow_fallbacks": false}),
    );
    Ok(Value::Object(body).to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn slugs(values: &[&str]) -> Vec<String> {
        values.iter().map(|value| value.to_string()).collect()
    }

    #[test]
    fn provider_slugs_are_validated() {
        for good in ["parasail", "parasail/fp8", "deepinfra/bf16", "z-ai", "a.b_c"] {
            assert!(valid_slug(good), "{good}");
        }
        for bad in ["", "/fp8", "parasail/", "Parasail", "para sail", "x;y"] {
            assert!(!valid_slug(bad), "{bad:?}");
        }
        assert_eq!(invalid_slug(&slugs(&["parasail/fp8"])), None);
        assert!(invalid_slug(&slugs(&["parasail/fp8", "Bad"]))
            .is_some_and(|message| message.contains("\"Bad\"")));
    }

    #[test]
    fn extra_body_pins_the_providers_with_fallbacks_off() {
        let body = extra_body(None, &slugs(&["parasail/fp8"])).unwrap();
        let value: Value = serde_json::from_str(&body).unwrap();
        assert_eq!(
            value,
            json!({"provider": {"only": ["parasail/fp8"], "allow_fallbacks": false}})
        );
    }

    #[test]
    fn extra_body_keeps_the_users_other_keys_and_replaces_provider() {
        let existing = r#"{"top_k": 5, "provider": {"sort": "price"}}"#;
        let body = extra_body(Some(existing), &slugs(&["a", "b/fp8"])).unwrap();
        let value: Value = serde_json::from_str(&body).unwrap();
        assert_eq!(value["top_k"], 5);
        assert_eq!(
            value["provider"],
            json!({"only": ["a", "b/fp8"], "allow_fallbacks": false})
        );
        assert!(extra_body(Some("[1, 2]"), &slugs(&["a"])).is_err());
        assert!(extra_body(Some("not json"), &slugs(&["a"])).is_err());
        assert!(extra_body(Some("  "), &slugs(&["a"])).is_ok());
    }
}
