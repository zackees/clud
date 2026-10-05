//! Unit tests for the `grind` launch contract: the one-interactive-session
//! shape of `/grind`, the Claude-harness requirement, and the exemption the
//! `grind reconcile` pass earns by never launching a session (#1803).

use super::*;

/// #1803: `grind reconcile` never launches a session -- it reads GitHub state
/// through `gh` and exits with its own status -- so the interactive harness
/// gate cannot apply to it. It parses as `Command::Grind`, so the gate used
/// to fire on the resolved harness alone and refused the pass with "requires
/// the Claude harness" in a session that really was Claude. The false
/// negative only showed on `reconcile`: its siblings (`grind-facts`,
/// `grind-scripts`, `clud tool run`) are dispatched from
/// `dispatch_fast_path_command` before any backend is resolved, so they never
/// reach this gate at all.
#[test]
fn grind_reconcile_is_exempt_from_the_interactive_harness_gate() {
    let args = parse(&["clud", "grind", "reconcile"]);
    // A non-Claude harness resolved from a saved global preference: the exact
    // shape that produced the false negative.
    let saved_codex = ResolvedLaunchTarget {
        routing_mode: RoutingMode::Direct,
        model_provider: ModelProvider::Codex,
        requested_harness: HarnessSelection::Codex,
        effective_harness: Backend::Codex,
        provider_source: PreferenceSource::GlobalSetting,
        harness_source: PreferenceSource::GlobalSetting,
    };
    for target in [saved_codex, deepseek_harness_target()] {
        assert_eq!(
            grind_launch_error(&args, target),
            None,
            "grind reconcile launches no session and must not require a harness \
             (resolved harness: {})",
            target.effective_harness
        );
    }
}

/// The exemption is scoped to the reconcile keyword: an ordinary `/grind`
/// launch on a harness without native `/loop` is still refused.
#[test]
fn grind_reconcile_exemption_does_not_open_the_interactive_launch() {
    let args = parse(&[
        "clud",
        "--harness",
        "deepseek",
        "grind",
        "https://github.com/zackees/clud/issues",
    ]);
    assert_eq!(
        grind_launch_error(&args, deepseek_harness_target()),
        Some(
            "`clud grind` requires the Claude harness, whose Workflow tool and `/loop` the `/grind` skill drives; use `--harness claude`"
        )
    );
}

#[test]
fn grind_rejects_harnesses_without_native_loop() {
    let args = parse(&[
        "clud",
        "--harness",
        "deepseek",
        "grind",
        "https://github.com/zackees/clud/issues",
    ]);
    assert!(grind_launch_error(&args, deepseek_harness_target()).is_some());
}

#[test]
fn grind_uses_one_interactive_harness_session_without_external_loop_state() {
    let p = plan(&["clud", "grind", "https://github.com/zackees/clud/issues"]);
    let prompt = last_arg(&p);
    assert!(
        prompt.starts_with("/grind "),
        "grind prompt must start with /grind; got: {prompt:?}"
    );
    assert!(
        prompt.contains("https://github.com/zackees/clud/issues"),
        "grind prompt must contain the URL; got: {prompt:?}"
    );
    assert!(!p.command.iter().any(|arg| arg == "-p"));
    assert_eq!(p.launch_mode, console_launch_mode());
    assert_eq!(p.iterations, 1);
    assert!(p.loop_markers.is_none());
    assert!(p.repeat_schedule.is_none());
    assert!(!p.stream_json_progress);
}

#[test]
fn grind_rejects_subprocess_and_detached_modes() {
    for argv in [
        vec![
            "clud",
            "--subprocess",
            "grind",
            "https://github.com/zackees/clud/issues",
        ],
        vec![
            "clud",
            "--detach",
            "grind",
            "https://github.com/zackees/clud/issues",
        ],
    ] {
        let args = parse(&argv);
        assert!(
            grind_launch_error(&args, bridge_target()).is_some(),
            "argv={argv:?}"
        );
    }
}

#[test]
fn grind_through_claude_harness_seeds_native_loop_interactively() {
    let args = parse(&[
        "clud",
        "--codex",
        "--harness",
        "claude",
        "grind",
        "https://github.com/zackees/clud/issues",
    ]);
    let plan = build_launch_plan_for_target(&args, bridge_target(), "claude");
    assert!(!plan.command.iter().any(|arg| arg == "-p"));
    assert_eq!(plan.launch_mode, console_launch_mode());
    assert_eq!(plan.iterations, 1);
    assert!(plan.loop_markers.is_none());
    assert!(plan.command.last().is_some_and(|prompt| {
        prompt.starts_with("/grind ") && prompt.contains("zackees/clud/issues")
    }));
}
