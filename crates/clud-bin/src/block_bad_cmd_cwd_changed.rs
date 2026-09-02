//! The `CwdChanged` reactive backstop (zackees/clud#967 Phase 5, #966 D12).
//!
//! The PreToolUse scanner sees only `cd`s written in a tool call. A session
//! cwd moved by an alias or a script (`goto-docs`, `activate`, …) is invisible
//! to it — yet it breaks exactly what `bash.block_cd` protects: every later
//! tool call resolves relative paths against the new cwd, and repo-relative
//! hooks stop resolving. `CwdChanged` is the harness's reactive signal that
//! the session cwd *did* move, and this module turns it into a visible drift
//! warning and a chance to re-root.
//!
//! ## Hygiene only — never correctness
//!
//! The upstream cwd contract is unstable (anthropics/claude-code#83636,
//! #76708, #84685), so nothing here may be load-bearing:
//!
//! - the handler always exits 0. The harness gives `CwdChanged` hooks no
//!   decision control — an exit 2 only surfaces stderr to the user, and the
//!   change is not reverted — so a denial here is a lie clud cannot enforce.
//! - a declared hook's exit-2 is downgraded to a warning for the same reason.
//! - the line is registered only where a capability probe says the frontend
//!   fires the event, and a probe that cannot run degrades to no line.
//!
//! See DD-064.
//!
//! ## What runs here
//!
//! 1. **The drift warning.** Resolve `bash.block_cd` against the session
//!    parent root; when the new cwd violates the policy — escaping the
//!    registered roots in relaxed, escaping them in strict too — warn that a
//!    chdir the scanner could not see moved the session. Warn, never block.
//! 2. **Tier B.** The repo-declared `CwdChanged` hooks, rooted at the repo
//!    the session now stands in, via the same firing matrix as every other
//!    event — with the deny downgraded to a warning.

use super::*;

/// Handle `clud-cmd-scan --event CwdChanged`.
///
/// Always exits 0: the harness gives `CwdChanged` no decision control, so
/// anything else would be a denial that cannot be enforced (see module docs).
/// `parent` is the session root the registered-root set pins against —
/// `None` when nothing in the environment or the cwd walk can name one, in
/// which case the drift warning is skipped and only Tier B runs (Tier B
/// re-resolves its own root from `CLUD_PROJECT_DIR` or the payload cwd).
pub fn handle_cwd_changed(raw_payload: &str, parent: Option<&Path>) -> i32 {
    let value: Value = match serde_json::from_str(if raw_payload.trim().is_empty() {
        "{}"
    } else {
        raw_payload
    }) {
        Ok(value) => value,
        Err(error) => {
            append_log(&format!("cwd_changed_json_decode_error: {error}"));
            append_log("allowed");
            return 0;
        }
    };

    let process_cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    let new_cwd = cwd_from_payload(&value, &process_cwd);
    append_log(&format!(
        "cwd_changed_event cwd={:?}",
        new_cwd.to_string_lossy()
    ));

    if let Some(parent) = parent {
        let config =
            crate::repo_clud_config::discover_effective_clud_config(parent).unwrap_or_default();
        let env_roots = std::env::var(crate::clud_hook_roots::HOOK_ROOTS_ENV).ok();
        let roots = crate::clud_hook_roots::HookRoots::resolve(
            parent,
            &config.hook_roots.children,
            env_roots.as_deref(),
        )
        .paths();
        let scan = scan_hook_cwd_sensitivity(parent, home_dir().as_deref());
        let in_repo = super::block_bad_cmd_cd::nearest_repo_root(parent).is_some();
        let policy = resolve_policy(config.bash.block_cd, in_repo, &scan);
        append_log(&format!("cwd_changed_drift_check policy={policy:?}"));
        if let Some(warning) = super::block_bad_cmd_cd::drift_warning(&new_cwd, policy, &roots) {
            eprintln!("{warning}");
            append_log("cwd_changed_drift_warning_emitted");
        }
    }

    // Tier B: the repo-declared `CwdChanged` hooks. The payload carries no
    // command text — nothing was *called*, the session *moved* — so the view
    // resolves against the new cwd, exactly the containment the session is
    // now subject to. A block cannot be enforced (the cwd has already
    // changed), so it surfaces as a warning instead.
    if let Some(payload) = parse_payload_value(&value, &new_cwd) {
        if let Some(denial) = declared_hook_denial(
            crate::clud_hooks_compile::CWD_CHANGED_EVENT,
            &payload,
            raw_payload,
        ) {
            for message in &denial.log_messages {
                append_log(message);
            }
            append_log(&format!(
                "cwd_changed_denial_not_enforceable: {}",
                denial.reason
            ));
            eprintln!(
                "[clud] a declared CwdChanged hook refused this move ({}) — but the cwd has \
                 already changed and CwdChanged refusals cannot be enforced, so nothing was \
                 blocked.",
                denial.reason
            );
        }
    }

    append_log("allowed");
    0
}

/// The session root the registered-root set pins against.
///
/// The harness exports `CLAUDE_PROJECT_DIR` on every spawned hook and it
/// stays put while the session cwd drifts, so it is the reliable answer.
/// Without it, walk up from the process cwd — best effort: a `CwdChanged`
/// hook runs with the *new* cwd, so the walk answers "which repo did the
/// session land in" rather than "where did it start".
#[must_use]
pub(super) fn session_parent_root() -> Option<PathBuf> {
    if let Some(root) = std::env::var_os("CLAUDE_PROJECT_DIR") {
        let root = PathBuf::from(root);
        if root.is_dir() {
            return Some(root);
        }
    }
    let cwd = std::env::current_dir().ok()?;
    nearest_repo_root_public(&cwd)
}

/// The directory the session moved to.
///
/// `new_cwd` is the event's own field; `cwd` is documented to equal it and
/// is the fallback older builds named. Missing or empty falls back to the
/// process cwd — the hook itself runs in the new directory, so that is the
/// same answer.
#[must_use]
pub(super) fn cwd_from_payload(value: &Value, process_cwd: &Path) -> PathBuf {
    value
        .get("new_cwd")
        .or_else(|| value.get("cwd"))
        .and_then(Value::as_str)
        .filter(|raw| !raw.trim().is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(|| process_cwd.to_path_buf())
}

#[cfg(test)]
#[path = "block_bad_cmd_cwd_changed_tests.rs"]
mod block_bad_cmd_cwd_changed_tests;
