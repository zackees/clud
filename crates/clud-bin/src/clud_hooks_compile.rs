//! Compiling clud's hook set into a frontend's native configuration
//! (zackees/clud#977, #967 Phase 2b).
//!
//! clud does not write hook lines into anyone's settings file. It composes the
//! registration a frontend needs and hands it over as a **launch argument**,
//! which removes a whole class of hazards a file-writing design carries:
//! idempotence, read-modify-write lost updates, two writers fighting over one
//! file (#847), per-repo gitignore assumptions, and stale state left behind by
//! a killed session. See DD-049.
//!
//! ## What gets registered
//!
//! One dispatcher line per declared event, matching every tool:
//!
//! ```json
//! { "hooks": { "Stop": [ { "matcher": "*",
//!     "hooks": [ { "type": "command", "command": "clud-cmd-scan --event Stop" } ] } ] } }
//! ```
//!
//! Matching every tool is deliberate. The per-hook `matcher` a repo declares is
//! applied by clud when it dispatches, so narrowing here would silently drop
//! declarations: an installed line scoped to `Bash` can never deliver an
//! `Edit`-matched hook.
//!
//! ## Frontend support is not uniform
//!
//! **Claude** takes `--settings <file-or-json>`, an *additional* source that
//! merges with the settings files rather than replacing them, with hook entries
//! concatenating across levels. That is the whole mechanism.
//!
//! **Codex has no argument surface for hooks at all.** Its `-c key=value`
//! overrides values that would otherwise come from `config.toml`, and codex
//! hooks live in a separate `hooks.json`; no flag points at an alternate one.
//! So codex keeps the coverage the already-installed `clud-cmd-scan` PreToolUse
//! line gives it — which since #980 runs declared hooks too — and gets nothing
//! for other events, matching codex's own apparent single-event support. No
//! second codex writer is introduced.

use serde_json::{json, Value};

use crate::clud_hooks::CludHooks;

/// The helper the compiled lines invoke. Same binary as the installed
/// PreToolUse guard; the event argument is what distinguishes the roles.
pub const DISPATCHER_BINARY: &str = "clud-cmd-scan";

/// Marks a session whose frontend has clud's dispatcher lines registered.
///
/// Without it the bare `clud-cmd-scan` line has to run declared hooks itself,
/// because before Phase 2b that line was the only thing that could. With it,
/// the bare line leaves declared hooks to the compiled lines and each hook
/// runs exactly once. Sessions clud did not launch never see the marker and
/// keep the old behavior.
pub const DISPATCH_ENV: &str = "CLUD_HOOK_DISPATCH";

/// The event a bare invocation serves; it needs no compiled line to exist,
/// but gets one anyway so its matcher is not whatever the user happened to
/// install.
pub const PRE_TOOL_USE: &str = "PreToolUse";

/// The command string a compiled line runs for `event`.
#[must_use]
pub fn dispatcher_command(event: &str) -> String {
    format!("{DISPATCHER_BINARY} --event {event}")
}

/// Build the Claude settings fragment registering clud's dispatcher for every
/// event `hooks` declares.
///
/// `None` when the repo declares nothing, which is the signal not to pass
/// `--settings` at all — a repo that has not opted in should see a launch
/// identical to the one it saw before this feature existed.
#[must_use]
pub fn claude_settings_fragment(hooks: &CludHooks) -> Option<Value> {
    let mut events = serde_json::Map::new();
    for event in hooks.events() {
        events.insert(
            event.to_string(),
            json!([{
                "matcher": "*",
                "hooks": [{
                    "type": "command",
                    "command": dispatcher_command(event),
                }],
            }]),
        );
    }
    if events.is_empty() {
        return None;
    }
    Some(json!({ "hooks": Value::Object(events) }))
}

/// Merge `overlay`'s hook entries into `base`, concatenating per event.
///
/// Mirrors how the harness itself layers hooks — entries add rather than
/// replace — so clud's registration cannot silently displace a document it is
/// merging into, whether that is the bridge's lifecycle hooks or a settings
/// file the user passed on the command line.
pub fn merge_hook_settings(base: &mut Value, overlay: &Value) -> Result<(), String> {
    let Some(overlay_events) = overlay.get("hooks").and_then(Value::as_object) else {
        return Ok(());
    };
    let base_root = base
        .as_object_mut()
        .ok_or_else(|| "settings must be a JSON object".to_string())?;
    let base_events = base_root
        .entry("hooks")
        .or_insert_with(|| json!({}))
        .as_object_mut()
        .ok_or_else(|| "settings `hooks` must be a JSON object".to_string())?;

    for (event, entries) in overlay_events {
        let entries = entries
            .as_array()
            .ok_or_else(|| format!("settings hook event {event} must be a JSON array"))?;
        let existing = base_events
            .entry(event.clone())
            .or_insert_with(|| json!([]))
            .as_array_mut()
            .ok_or_else(|| format!("settings hook event {event} must be a JSON array"))?;
        existing.extend(entries.iter().cloned());
    }
    Ok(())
}

#[cfg(test)]
#[path = "clud_hooks_compile_tests.rs"]
mod clud_hooks_compile_tests;
