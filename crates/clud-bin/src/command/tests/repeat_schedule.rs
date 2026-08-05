use super::*;

#[test]
fn test_warning_fires_when_repeat_alone() {
    // --repeat 1h with neither --no-done nor --done: warning + contract OFF.
    let msg = repeat_implies_no_done_warning(Some("1h"), false, None);
    assert!(msg.is_some(), "expected warning when --repeat is alone");
    let text = msg.unwrap();
    assert!(text.contains("--repeat"));
    assert!(text.contains("--no-done"));
    assert!(text.contains("DONE marker"));
}

#[test]
fn test_warning_suppressed_when_no_done_explicit() {
    // User already opted out — no need to warn them.
    let msg = repeat_implies_no_done_warning(Some("1h"), true, None);
    assert!(
        msg.is_none(),
        "explicit --no-done must suppress the warning, got: {msg:?}"
    );
}

#[test]
fn test_warning_suppressed_when_done_path_provided() {
    // --done <path> overrides --repeat's implicit --no-done; no warning.
    let msg = repeat_implies_no_done_warning(Some("1h"), false, Some("DONE.md"));
    assert!(
        msg.is_none(),
        "--done <path> must suppress the warning, got: {msg:?}"
    );
}

#[test]
fn test_warning_silent_without_repeat() {
    // Plain `clud loop "task"` without --repeat: helper never warns.
    let msg = repeat_implies_no_done_warning(None, false, None);
    assert!(msg.is_none());
    let msg = repeat_implies_no_done_warning(None, true, None);
    assert!(msg.is_none());
    let msg = repeat_implies_no_done_warning(None, false, Some("DONE.md"));
    assert!(msg.is_none());
}

// ---- Flag-precedence: contract / loop_markers behavior in plan ----

#[test]
fn test_loop_explicit_no_done_honored_without_repeat() {
    // Without --repeat, --no-done still suppresses the contract — this is
    // the original #2 behavior preserved. Already covered by
    // test_loop_no_done_omits_contract above; we add an explicit assert
    // that loop_markers is None to make the contract crystal clear.
    let p = plan(&["clud", "loop", "--no-done", "task"]);
    assert!(p.loop_markers.is_none());
    assert!(p.repeat_schedule.is_none());
    let prompt = prompt_from_plan(&p);
    assert!(!prompt.contains("DONE"));
    assert!(!prompt.contains("BLOCKED"));
}

#[test]
fn test_loop_repeat_with_explicit_no_done_still_omits_contract() {
    // Belt-and-suspenders: passing both --repeat and --no-done is
    // idempotent — no contract injection, no markers.
    let p = plan(&["clud", "loop", "--repeat", "30m", "--no-done", "task"]);
    assert!(p.loop_markers.is_none());
    assert_eq!(
        p.repeat_schedule.as_ref().map(|s| s.interval_secs),
        Some(30 * 60)
    );
    let prompt = prompt_from_plan(&p);
    assert_eq!(prompt, "task");
}

#[test]
fn test_loop_done_path_uses_supplied_path_in_prompt() {
    // --done <path> must thread the *supplied* path into the prompt
    // contract, not the default `.clud/loop/DONE`. Issue #95: the
    // contract now uses absolute paths, but the user-supplied filename
    // is still visible in the absolute form.
    let p = plan(&["clud", "loop", "--done", "custom/DONE.txt", "task"]);
    let prompt = prompt_from_plan(&p);
    // The DONE filename must appear; the directory segment may use
    // either separator depending on platform.
    assert!(
        prompt.contains("DONE.txt"),
        "prompt missing custom DONE filename: {prompt}"
    );
    // BLOCKED is derived from the DONE *filename's extension* via
    // `blocked_path_from_done`, which uses platform-native path joining.
    // On unix that's `custom/BLOCKED.txt`; on Windows `custom\BLOCKED.txt`.
    // The load-bearing invariant is that the BLOCKED filename mirrors the
    // DONE extension — assert on the filename to stay platform-agnostic.
    assert!(
        prompt.contains("BLOCKED.txt"),
        "prompt missing derived BLOCKED filename: {prompt}"
    );
    assert!(p.loop_markers.is_some());
    let markers = p.loop_markers.unwrap();
    assert!(markers.done_path.ends_with("DONE.txt"));
    assert!(markers.blocked_path.ends_with("BLOCKED.txt"));
}

#[test]
fn test_loop_repeat_30s_parses() {
    let p = plan(&["clud", "loop", "--repeat", "30s", "task"]);
    assert_eq!(
        p.repeat_schedule.as_ref().map(|s| s.interval_secs),
        Some(30)
    );
}

#[test]
fn test_loop_repeat_5m_parses() {
    let p = plan(&["clud", "loop", "--repeat", "5m", "task"]);
    assert_eq!(
        p.repeat_schedule.as_ref().map(|s| s.interval_secs),
        Some(5 * 60)
    );
}
