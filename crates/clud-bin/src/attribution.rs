//! Claude Code commit and PR attribution is opt-in under clud (#1317).
//!
//! Claude Code appends a `Co-Authored-By: Claude …` trailer to commits and a
//! "Generated with Claude Code" line to PR bodies. Its settings expose both as
//! `attribution.commit` and `attribution.pr`; an empty string hides each one.
//!
//! clud hides both by default. `--coauthor` (or `CLUD_COAUTHOR=1`) leaves
//! Claude Code's own attribution in place, and `--coauthor=TAG` (or
//! `CLUD_COAUTHOR=TAG`) replaces both strings with `TAG`.
//!
//! The choice travels on [`crate::command::LaunchPlan::coauthor`], so
//! `--dry-run` shows it and old daemon payloads default to hidden. The
//! runtime merges [`merge_into`]'s keys into the launch's single `--settings`
//! document (`foreground_runtime::ForegroundRuntime::apply_attribution`),
//! adding only keys the user's own `--settings` did not already set.

use serde::{Deserialize, Serialize};

/// Environment variable equivalent of `--coauthor[=TAG]`.
pub const COAUTHOR_ENV: &str = "CLUD_COAUTHOR";

/// What a Claude launch says about who wrote a commit or PR.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "mode", content = "tag")]
pub enum Coauthor {
    /// No trailer and no PR line: both attribution strings are empty.
    #[default]
    Hidden,
    /// Claude Code's own attribution, untouched.
    Harness,
    /// This text in place of both attribution strings.
    Tag(String),
}

/// Resolve the flag and the environment variable. The flag wins.
///
/// `flag` is `None` without `--coauthor`, `Some("")` for a bare
/// `--coauthor`, and `Some(tag)` for `--coauthor=TAG`. In the environment,
/// empty/`0`/`false`/`off` mean unset, `1`/`true`/`on`/`yes` mean
/// [`Coauthor::Harness`], and anything else is a tag.
pub fn resolve(flag: Option<&str>, env_value: Option<&str>) -> Coauthor {
    if let Some(flag) = flag {
        return from_text(flag, Coauthor::Harness);
    }
    let Some(value) = env_value.map(str::trim) else {
        return Coauthor::Hidden;
    };
    if value.is_empty()
        || value == "0"
        || value.eq_ignore_ascii_case("false")
        || value.eq_ignore_ascii_case("off")
    {
        return Coauthor::Hidden;
    }
    if value == "1"
        || value.eq_ignore_ascii_case("true")
        || value.eq_ignore_ascii_case("on")
        || value.eq_ignore_ascii_case("yes")
    {
        return Coauthor::Harness;
    }
    Coauthor::Tag(value.to_string())
}

fn from_text(text: &str, when_empty: Coauthor) -> Coauthor {
    let text = text.trim();
    if text.is_empty() {
        when_empty
    } else {
        Coauthor::Tag(text.to_string())
    }
}

/// [`resolve`] against the process environment.
pub fn resolve_from_env(flag: Option<&str>) -> Coauthor {
    resolve(flag, std::env::var(COAUTHOR_ENV).ok().as_deref())
}

/// Add this choice's `attribution` keys to a Claude settings document.
///
/// Returns whether the document changed. [`Coauthor::Harness`] never changes
/// it, and a key the user already set is kept: an explicit `--settings`
/// outranks clud's default.
pub fn merge_into(coauthor: &Coauthor, document: &mut serde_json::Value) -> bool {
    let text = match coauthor {
        Coauthor::Harness => return false,
        Coauthor::Hidden => String::new(),
        Coauthor::Tag(tag) => tag.clone(),
    };
    let Some(root) = document.as_object_mut() else {
        return false;
    };
    let Some(attribution) = root
        .entry("attribution")
        .or_insert_with(|| serde_json::json!({}))
        .as_object_mut()
    else {
        return false;
    };
    let mut changed = false;
    for key in ["commit", "pr"] {
        if !attribution.contains_key(key) {
            attribution.insert(key.to_string(), serde_json::Value::String(text.clone()));
            changed = true;
        }
    }
    changed
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn default_is_hidden() {
        assert_eq!(resolve(None, None), Coauthor::Hidden);
        assert_eq!(Coauthor::default(), Coauthor::Hidden);
    }

    #[test]
    fn flag_opts_in_and_carries_an_optional_tag() {
        assert_eq!(resolve(Some(""), None), Coauthor::Harness);
        assert_eq!(
            resolve(Some("Co-Authored-By: Bot <b@x>"), None),
            Coauthor::Tag("Co-Authored-By: Bot <b@x>".into())
        );
        // The flag outranks the environment.
        assert_eq!(resolve(Some(""), Some("0")), Coauthor::Harness);
    }

    #[test]
    fn env_value_is_off_on_or_a_tag() {
        for off in ["", " ", "0", "false", "OFF"] {
            assert_eq!(resolve(None, Some(off)), Coauthor::Hidden, "{off:?}");
        }
        for on in ["1", "true", "on", "YES"] {
            assert_eq!(resolve(None, Some(on)), Coauthor::Harness, "{on:?}");
        }
        assert_eq!(
            resolve(None, Some("Made by X")),
            Coauthor::Tag("Made by X".into())
        );
    }

    #[test]
    fn hidden_blanks_both_strings() {
        let mut document = json!({});
        assert!(merge_into(&Coauthor::Hidden, &mut document));
        assert_eq!(document, json!({"attribution": {"commit": "", "pr": ""}}));
    }

    #[test]
    fn tag_replaces_both_strings() {
        let mut document = json!({"theme": "dark"});
        assert!(merge_into(&Coauthor::Tag("T".into()), &mut document));
        assert_eq!(
            document,
            json!({"theme": "dark", "attribution": {"commit": "T", "pr": "T"}})
        );
    }

    #[test]
    fn harness_leaves_the_document_alone() {
        let mut document = json!({"theme": "dark"});
        assert!(!merge_into(&Coauthor::Harness, &mut document));
        assert_eq!(document, json!({"theme": "dark"}));
    }

    #[test]
    fn user_values_are_kept() {
        let mut document = json!({"attribution": {"commit": "mine"}});
        assert!(merge_into(&Coauthor::Hidden, &mut document));
        assert_eq!(
            document,
            json!({"attribution": {"commit": "mine", "pr": ""}})
        );
        assert!(!merge_into(&Coauthor::Hidden, &mut document));
    }

    #[test]
    fn serializes_with_a_mode_tag() {
        assert_eq!(
            serde_json::to_value(Coauthor::Hidden).unwrap(),
            json!({"mode": "hidden"})
        );
        assert_eq!(
            serde_json::to_value(Coauthor::Tag("T".into())).unwrap(),
            json!({"mode": "tag", "tag": "T"})
        );
    }
}
