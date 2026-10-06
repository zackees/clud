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

/// `clud --<provider>` routed directly through the Claude harness.
fn third_party_claude_route(provider: ModelProvider) -> ResolvedLaunchTarget {
    ResolvedLaunchTarget {
        routing_mode: RoutingMode::Direct,
        model_provider: provider,
        requested_harness: HarnessSelection::Claude,
        effective_harness: provider.native_harness(),
        provider_source: PreferenceSource::Cli,
        harness_source: PreferenceSource::Cli,
    }
}

/// Every non-Anthropic provider the CLI routes through the Claude harness,
/// paired with its CLI shortcut flag.
fn non_anthropic_claude_routes() -> Vec<(ModelProvider, &'static str)> {
    let mut routes: Vec<(ModelProvider, &'static str)> =
        crate::provider_registry::ANTHROPIC_COMPAT_PROVIDERS
            .iter()
            .map(|descriptor| (descriptor.provider, descriptor.cli_flag))
            .collect();
    if !routes
        .iter()
        .any(|(provider, _)| *provider == ModelProvider::OpenRouter)
    {
        routes.push((ModelProvider::OpenRouter, "--openrouter"));
    }
    routes
}

/// True when the command suppresses the Workflow tool, either as the value
/// of a `--disallowedTools` flag or inline as `--disallowedTools=...`.
fn plan_disallows_workflow(command: &[String]) -> bool {
    command.iter().enumerate().any(|(index, arg)| {
        let after_disallowed = index > 0 && command[index - 1] == "--disallowedTools";
        (after_disallowed || arg.starts_with("--disallowedTools")) && arg.contains("Workflow")
    })
}

/// #1813: `/grind` on a gateway route must still get the Workflow tool. The
/// router starts the workflow and ends its turn; if the Workflow tool never
/// fires there is no task notification to wake it, so the run stalls with no
/// error at all. Pin, for every non-Anthropic provider, that the native
/// harness is Claude, that the grind gate admits the route, and that the
/// interactive plan neither goes headless nor disallows `Workflow`.
#[test]
fn grind_launches_the_workflow_session_on_every_non_anthropic_claude_route() {
    let url = "https://github.com/zackees/clud/issues";
    for (provider, flag) in non_anthropic_claude_routes() {
        assert_eq!(
            provider.native_harness(),
            Backend::Claude,
            "{provider}: /grind needs the Workflow tool, which only the Claude harness has"
        );
        let args = parse(&["clud", flag, "grind", url]);
        let target = third_party_claude_route(provider);
        assert_eq!(
            grind_launch_error(&args, target),
            None,
            "{provider}: grind must launch on a Claude-harness gateway route"
        );
        let plan = build_launch_plan_for_target(&args, target, "claude");
        assert!(
            !plan.command.iter().any(|arg| arg == "-p"),
            "{provider}: grind must not go headless; command={:?}",
            plan.command
        );
        assert_eq!(plan.launch_mode, console_launch_mode(), "{provider}");
        assert_eq!(plan.iterations, 1, "{provider}");
        assert!(plan.loop_markers.is_none(), "{provider}");
        assert!(
            plan.command
                .last()
                .is_some_and(|prompt| prompt.starts_with("/grind ") && prompt.contains(url)),
            "{provider}: last arg must seed /grind with the URL; command={:?}",
            plan.command
        );
        assert!(
            !plan_disallows_workflow(&plan.command),
            "{provider}: the Workflow tool must not be disallowed (#1813 silent stall); \
             command={:?}",
            plan.command
        );
    }
}

/// #1808: the OpenRouter route clears the Claude-harness gate, but the
/// session-shape refusals still apply to it: `/grind` needs one foreground
/// interactive PTY whatever provider the session talks to.
#[test]
fn grind_on_openrouter_still_refuses_detached_and_subprocess_sessions() {
    let url = "https://github.com/zackees/clud/issues";
    let target = third_party_claude_route(ModelProvider::OpenRouter);
    let args = parse(&["clud", "--openrouter", "grind", url]);
    assert_eq!(grind_launch_error(&args, target), None);
    for (flag, needle) in [
        ("--detach", "foreground interactive PTY"),
        ("--subprocess", "interactive PTY"),
    ] {
        let args = parse(&["clud", "--openrouter", flag, "grind", url]);
        let error = grind_launch_error(&args, target)
            .unwrap_or_else(|| panic!("{flag}: OpenRouter grind must be refused"));
        assert!(error.contains(needle), "{flag}: {error}");
    }
}

/// #1809: the OpenRouter `/grind` session is a plain interactive harness
/// session: no headless flag, no clud-side repetition, and no stream-json
/// progress rendering.
#[test]
fn grind_on_openrouter_boots_one_plain_interactive_session() {
    let url = "https://github.com/zackees/clud/issues/1807";
    let args = parse(&["clud", "--openrouter", "grind", url]);
    let plan = build_launch_plan_for_target(
        &args,
        third_party_claude_route(ModelProvider::OpenRouter),
        "claude",
    );
    let seed = format!("/grind {url}");
    assert_eq!(plan.command.last(), Some(&seed));
    assert!(!plan.command.contains(&"-p".to_string()));
    assert!(!plan.command.contains(&"exec".to_string()));
    assert!(!plan.stream_json_progress);
    assert!(plan.repeat_schedule.is_none());
    assert!(plan.loop_markers.is_none());
    assert_eq!(plan.iterations, 1);
}
