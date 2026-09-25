//! #1067 step 3: let clud turn the command gate on for a session, opt-in.
//!
//! The gate itself (`block_bad_cmd_gate.rs`) enforces only where
//! `CLUD_CMD_GATE` is set in the session environment. Until now the only way
//! to set it was exporting it by hand. This layer sets it for a clud-launched
//! session when the user opts in with [`OPT_IN_KEY`] **and** the wrapper is
//! actually resolvable on the session's `PATH`: a gate that demands `tap`
//! when there is no `tap` would refuse every command.
//!
//! It is a child-env layer, next to nounset (#1066), rather than a
//! `LaunchPlan` field: `apply_child_env_policy` is the one builder the
//! foreground runner and the daemon both use, so a detached session is gated
//! exactly like a foreground one without a new daemon wire field.
//!
//! Step 4 (default-on) is deliberately not taken. The replay measurement in
//! DD-056 found 15.7% of real agent commands refused even when every
//! statement is wrapped.

/// Opt in. `1`/`true`/`yes`/`on` in clud's own environment.
pub const OPT_IN_KEY: &str = "CLUD_CMD_GATE_AUTO";
/// The variable the gate reads (`block_bad_cmd_gate::GATE_MODE_ENV`).
pub const GATE_KEY: &str = "CLUD_CMD_GATE";
/// The wrapper name override the gate reads.
const PREFIX_KEY: &str = "CLUD_CMD_GATE_PREFIX";

fn is_truthy(raw: &str) -> bool {
    matches!(
        raw.trim().to_ascii_lowercase().as_str(),
        "1" | "true" | "yes" | "on"
    )
}

fn lookup<'a>(env: &'a [(String, String)], key: &str) -> Option<&'a str> {
    env.iter()
        .rev()
        .find(|(candidate, _)| candidate == key)
        .map(|(_, value)| value.as_str())
}

/// The overrides for a child env built from `env`. Empty unless the user
/// opted in, has not already chosen a gate mode themselves, and the wrapper
/// resolves on `env`'s `PATH`.
pub fn env_overrides(env: &[(String, String)]) -> Vec<(String, String)> {
    env_overrides_with(env, |name, path| {
        crate::shim_resolve::which(name, path).is_some()
    })
}

/// Test seam: `resolves(name, path)` stands in for a `PATH` lookup.
pub fn env_overrides_with(
    env: &[(String, String)],
    resolves: impl Fn(&str, &str) -> bool,
) -> Vec<(String, String)> {
    if !lookup(env, OPT_IN_KEY).is_some_and(is_truthy) {
        return Vec::new();
    }
    // An explicit `CLUD_CMD_GATE` (on or off) is the user's own choice and
    // the documented revert; never override it.
    if lookup(env, GATE_KEY).is_some() {
        return Vec::new();
    }
    let prefix = lookup(env, PREFIX_KEY)
        .map(str::trim)
        .filter(|prefix| !prefix.is_empty())
        .unwrap_or("tap");
    let path = lookup(env, "PATH").unwrap_or_default();
    if !resolves(prefix, path) {
        return Vec::new();
    }
    vec![(GATE_KEY.to_string(), "enforce".to_string())]
}

#[cfg(test)]
mod tests {
    use super::*;

    fn env(pairs: &[(&str, &str)]) -> Vec<(String, String)> {
        pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect()
    }

    fn tap_on(dir: &'static str) -> impl Fn(&str, &str) -> bool {
        move |name, path| name == "tap" && path.split(':').any(|p| p == dir)
    }

    #[test]
    fn gates_only_when_opted_in_and_the_wrapper_resolves() {
        let enforce = vec![("CLUD_CMD_GATE".to_string(), "enforce".to_string())];
        let opted = env(&[("CLUD_CMD_GATE_AUTO", "1"), ("PATH", "/usr/bin:/opt/tap")]);
        assert_eq!(env_overrides_with(&opted, tap_on("/opt/tap")), enforce);
        // Not on PATH: no gate, because every command would be refused.
        assert!(env_overrides_with(&opted, tap_on("/elsewhere")).is_empty());
        // Not opted in (absent or falsy): exactly the pre-#1067 behavior.
        for value in [None, Some("0"), Some("no"), Some("")] {
            let mut base = env(&[("PATH", "/opt/tap")]);
            if let Some(value) = value {
                base.push(("CLUD_CMD_GATE_AUTO".to_string(), value.to_string()));
            }
            assert!(env_overrides_with(&base, tap_on("/opt/tap")).is_empty());
        }
    }

    #[test]
    fn an_explicit_gate_mode_is_never_overridden() {
        for mode in ["enforce", "off", ""] {
            let base = env(&[
                ("CLUD_CMD_GATE_AUTO", "on"),
                ("CLUD_CMD_GATE", mode),
                ("PATH", "/opt/tap"),
            ]);
            assert!(env_overrides_with(&base, tap_on("/opt/tap")).is_empty());
        }
    }

    #[test]
    fn a_custom_prefix_is_what_must_resolve() {
        let base = env(&[
            ("CLUD_CMD_GATE_AUTO", "yes"),
            ("CLUD_CMD_GATE_PREFIX", "guard"),
            ("PATH", "/opt/bin"),
        ]);
        assert!(env_overrides_with(&base, tap_on("/opt/bin")).is_empty());
        assert_eq!(
            env_overrides_with(&base, |name, _| name == "guard").len(),
            1
        );
    }
}
