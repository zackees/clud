//! Compiling declarations into a frontend's native registration (#967 Phase 2b).

use super::*;
use crate::clud_hooks::parse;

fn hooks(text: &str) -> CludHooks {
    parse(text).expect("parses")
}

fn commands_for(fragment: &Value, event: &str) -> Vec<String> {
    fragment["hooks"][event]
        .as_array()
        .unwrap_or(&Vec::new())
        .iter()
        .flat_map(|group| {
            group["hooks"]
                .as_array()
                .cloned()
                .unwrap_or_default()
                .into_iter()
        })
        .filter_map(|handler| handler["command"].as_str().map(ToOwned::to_owned))
        .collect()
}

#[test]
fn every_declared_event_gets_a_dispatcher_line() {
    let fragment = claude_settings_fragment(&hooks(
        r#"{"hooks":{"PreToolUse":[{"command":"a"}],"Stop":[{"command":"b"}]}}"#,
    ))
    .expect("declared");

    assert_eq!(
        commands_for(&fragment, "PreToolUse"),
        vec!["clud-cmd-scan --event PreToolUse"]
    );
    assert_eq!(
        commands_for(&fragment, "Stop"),
        vec!["clud-cmd-scan --event Stop"]
    );
}

#[test]
fn several_hooks_on_one_event_still_register_a_single_line() {
    // clud dispatches the whole event; one registration is all the frontend
    // needs, and more would run the set repeatedly.
    let fragment = claude_settings_fragment(&hooks(
        r#"{"hooks":{"Stop":[{"command":"a"},{"command":"b"},{"command":"c"}]}}"#,
    ))
    .expect("declared");

    assert_eq!(commands_for(&fragment, "Stop").len(), 1);
}

#[test]
fn the_registered_matcher_is_catch_all_whatever_the_declaration_scopes_to() {
    // The per-hook matcher is applied by clud at dispatch time. Narrowing the
    // registration would silently drop declarations — a line scoped to `Bash`
    // can never deliver an `Edit`-matched hook.
    let fragment = claude_settings_fragment(&hooks(
        r#"{"hooks":{"PreToolUse":[{"matcher":"Edit","command":"a"}]}}"#,
    ))
    .expect("declared");

    assert_eq!(fragment["hooks"]["PreToolUse"][0]["matcher"], "*");
}

#[test]
fn a_repo_that_declares_nothing_compiles_to_nothing() {
    // The signal not to pass `--settings` at all: a repo that has not opted
    // in should see the launch it saw before this feature existed.
    assert!(claude_settings_fragment(&hooks("{}")).is_none());
    assert!(claude_settings_fragment(&hooks(r#"{"hooks":{"Stop":[]}}"#)).is_none());
}

#[test]
fn the_dispatcher_command_names_the_event() {
    assert_eq!(dispatcher_command("Stop"), "clud-cmd-scan --event Stop");
}

// -----------------------------------------------------------------
// Merging into an existing settings document.
// -----------------------------------------------------------------

#[test]
fn merging_concatenates_per_event_rather_than_replacing() {
    // Mirrors how the harness layers hooks. Replacing would let clud's
    // registration silently displace the bridge's lifecycle hooks, or a
    // settings document the user passed on the command line.
    let mut base = json!({"hooks":{"Stop":[{"matcher":"*","hooks":[{"command":"theirs"}]}]}});
    let overlay = claude_settings_fragment(&hooks(r#"{"hooks":{"Stop":[{"command":"a"}]}}"#))
        .expect("declared");

    merge_hook_settings(&mut base, &overlay).expect("merges");

    assert_eq!(
        commands_for(&base, "Stop"),
        vec!["theirs", "clud-cmd-scan --event Stop"]
    );
}

#[test]
fn merging_preserves_unrelated_keys_and_events() {
    let mut base = json!({
        "model": "some-model",
        "permissions": {"deny": ["Bash(rm *)"]},
        "hooks": {"SessionStart": [{"hooks":[{"command":"theirs"}]}]}
    });
    let overlay = claude_settings_fragment(&hooks(r#"{"hooks":{"Stop":[{"command":"a"}]}}"#))
        .expect("declared");

    merge_hook_settings(&mut base, &overlay).expect("merges");

    assert_eq!(base["model"], "some-model");
    assert_eq!(base["permissions"]["deny"][0], "Bash(rm *)");
    assert_eq!(commands_for(&base, "SessionStart"), vec!["theirs"]);
    assert_eq!(
        commands_for(&base, "Stop"),
        vec!["clud-cmd-scan --event Stop"]
    );
}

#[test]
fn merging_into_a_document_without_hooks_creates_the_section() {
    let mut base = json!({"model": "some-model"});
    let overlay = claude_settings_fragment(&hooks(r#"{"hooks":{"Stop":[{"command":"a"}]}}"#))
        .expect("declared");

    merge_hook_settings(&mut base, &overlay).expect("merges");

    assert_eq!(
        commands_for(&base, "Stop"),
        vec!["clud-cmd-scan --event Stop"]
    );
}

#[test]
fn a_malformed_target_is_an_error_rather_than_a_silent_drop() {
    // Losing a registration quietly would mean the hooks never fire and
    // nothing says why.
    let overlay = claude_settings_fragment(&hooks(r#"{"hooks":{"Stop":[{"command":"a"}]}}"#))
        .expect("declared");

    let mut not_an_object = json!([]);
    assert!(merge_hook_settings(&mut not_an_object, &overlay).is_err());

    let mut hooks_not_an_object = json!({"hooks": "nope"});
    assert!(merge_hook_settings(&mut hooks_not_an_object, &overlay).is_err());

    let mut event_not_an_array = json!({"hooks": {"Stop": "nope"}});
    assert!(merge_hook_settings(&mut event_not_an_array, &overlay).is_err());
}

#[test]
fn an_overlay_without_hooks_is_a_no_op() {
    let mut base = json!({"hooks":{"Stop":[{"hooks":[{"command":"theirs"}]}]}});
    let before = base.clone();

    merge_hook_settings(&mut base, &json!({"model": "x"})).expect("merges");

    assert_eq!(base, before);
}
