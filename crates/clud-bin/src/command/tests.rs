use super::builder::{
    build_launch_plan, build_launch_plan_at, build_launch_plan_for_target, grind_launch_error,
    interactive_builtin_resume_error, next_run_at_millis, parse_repeat_interval,
    plan_mode_suppression_notice, repeat_implies_no_done_warning,
};
use super::prompts::{
    build_do_prompt, build_fix_prompt, build_grind_prompt, build_up_prompt, is_github_url,
    FIX_PROMPT,
};
use super::types::LaunchPlan;
use crate::args::Args;
use crate::backend::{
    Backend, HarnessSelection, LaunchMode, ModelProvider, PreferenceSource, ResolvedLaunchTarget,
    RoutingMode,
};
use crate::clud_settings::DEFAULT_CODEX_GITHUB_PLUGIN_CONFIG_OVERRIDE;

fn parse(raw: &[&str]) -> Args {
    let raw: Vec<String> = raw.iter().map(|s| s.to_string()).collect();
    Args::parse_from_raw(raw)
}

fn plan(raw: &[&str]) -> LaunchPlan {
    let mut args = parse(raw);
    let backend = crate::backend::resolve_backend(args.claude, args.codex);
    if matches!(backend, Backend::Codex) {
        args.codex_config_overrides = vec![DEFAULT_CODEX_GITHUB_PLUGIN_CONFIG_OVERRIDE.to_string()];
    }
    build_launch_plan(&args, backend, backend.executable_name())
}

fn plan_at(raw: &[&str], cwd: &std::path::Path) -> LaunchPlan {
    let mut args = parse(raw);
    let backend = crate::backend::resolve_backend(args.claude, args.codex);
    if matches!(backend, Backend::Codex) {
        args.codex_config_overrides = vec![DEFAULT_CODEX_GITHUB_PLUGIN_CONFIG_OVERRIDE.to_string()];
    }
    build_launch_plan_at(&args, backend, backend.executable_name(), cwd)
}

fn prompt_from_plan(p: &LaunchPlan) -> &str {
    let idx = p.command.iter().position(|a| a == "-p").unwrap();
    &p.command[idx + 1]
}

/// Find the last positional (non-flag, non-subcommand) argument of the plan.
/// For codex we emit the prompt positionally, so this picks it up.
fn last_arg(p: &LaunchPlan) -> &str {
    p.command.last().map(String::as_str).unwrap_or("")
}

fn codex_prefix() -> Vec<String> {
    vec![
        "codex".to_string(),
        "-c".to_string(),
        DEFAULT_CODEX_GITHUB_PLUGIN_CONFIG_OVERRIDE.to_string(),
    ]
}

fn codex_exec_index(p: &LaunchPlan) -> usize {
    p.command.iter().position(|arg| arg == "exec").unwrap()
}

fn codex_config_values(p: &LaunchPlan) -> Vec<&str> {
    p.command
        .windows(2)
        .filter_map(|pair| (pair[0] == "-c").then_some(pair[1].as_str()))
        .collect()
}

#[test]
fn test_prompt_with_yolo() {
    let p = plan(&["clud", "-p", "hello"]);
    assert_eq!(
        p.command,
        vec!["claude", "--dangerously-skip-permissions", "-p", "hello"]
    );
    assert_eq!(p.iterations, 1);
    assert_eq!(p.launch_mode, LaunchMode::Subprocess);
}

#[test]
fn explicit_run_is_the_same_launch_as_bare_clud() {
    for (bare_argv, run_argv) in [
        (vec!["clud"], vec!["clud", "run"]),
        (
            vec!["clud", "--prompt", "hello"],
            vec!["clud", "--prompt", "hello", "run"],
        ),
        (
            vec!["clud", "--message", "hello"],
            vec!["clud", "--message", "hello", "run"],
        ),
        (
            vec!["clud", "--continue"],
            vec!["clud", "--continue", "run"],
        ),
        (
            vec!["clud", "--resume=session-id"],
            vec!["clud", "--resume=session-id", "run"],
        ),
    ] {
        let bare = plan(&bare_argv);
        let explicit = plan(&run_argv);
        assert_eq!(explicit.command, bare.command, "{run_argv:?}");
        assert_eq!(explicit.iterations, bare.iterations, "{run_argv:?}");
        assert_eq!(explicit.routing_mode, bare.routing_mode, "{run_argv:?}");
    }
}

#[test]
fn test_loop_automatically_disallows_interactive_tools() {
    let p = plan(&["clud", "loop", "fix the build"]);
    assert!(p
        .command
        .iter()
        .any(|a| a == "--disallowedTools=EnterPlanMode,AskUserQuestion"));
}

#[test]
fn test_repeat_loop_automatically_disallows_interactive_tools() {
    let p = plan(&["clud", "loop", "fix the build", "--repeat", "1m"]);
    assert!(p
        .command
        .iter()
        .any(|a| a == "--disallowedTools=EnterPlanMode,AskUserQuestion"));
    assert_eq!(
        p.repeat_schedule.as_ref().map(|s| s.interval_secs),
        Some(60)
    );
}

#[test]
fn test_loop_under_codex_provider_claude_harness_disallows_interactive_tools() {
    let args = parse(&["clud", "loop", "fix the build"]);
    let target = ResolvedLaunchTarget {
        routing_mode: RoutingMode::Direct,
        model_provider: ModelProvider::Codex,
        requested_harness: HarnessSelection::Claude,
        effective_harness: Backend::Claude,
        provider_source: PreferenceSource::Cli,
        harness_source: PreferenceSource::Cli,
    };
    let p = build_launch_plan_for_target(&args, target, "claude");
    assert!(p
        .command
        .iter()
        .any(|a| a == "--disallowedTools=EnterPlanMode,Task,AskUserQuestion"));
}

/// Issue #955 acceptance path: Sol stays selected when `loop` turns the
/// Claude harness launch into a subprocess/repeat plan.
#[test]
fn sol_through_claude_loop_keeps_the_discovery_model_and_wire_selection() {
    let args = parse(&[
        "clud",
        "--model",
        "codex-sol",
        "--effort",
        "low",
        "loop",
        "--loop-count",
        "1",
        "--no-done",
        "reply once",
    ]);
    let plan = build_launch_plan_for_target(&args, bridge_target(), "claude");
    assert_eq!(plan.iterations, 1);
    assert!(plan
        .command
        .windows(2)
        .any(|pair| pair == ["--model", "clud-claude-codex-sol"]));
    assert!(plan
        .command
        .windows(2)
        .any(|pair| pair == ["--effort", "low"]));
    assert_eq!(
        plan.model_selection
            .as_ref()
            .and_then(|selection| selection.wire_model.as_deref()),
        Some("gpt-5.6-sol")
    );
}

/// `clud --codex --harness claude`, interactive, no `--unattended`.
fn bridge_target() -> ResolvedLaunchTarget {
    ResolvedLaunchTarget {
        routing_mode: RoutingMode::Direct,
        model_provider: ModelProvider::Codex,
        requested_harness: HarnessSelection::Claude,
        effective_harness: Backend::Claude,
        provider_source: PreferenceSource::Cli,
        harness_source: PreferenceSource::Cli,
    }
}

fn deepseek_bridge_target() -> ResolvedLaunchTarget {
    ResolvedLaunchTarget {
        routing_mode: RoutingMode::Direct,
        model_provider: ModelProvider::DeepSeek,
        requested_harness: HarnessSelection::Claude,
        effective_harness: Backend::Claude,
        provider_source: PreferenceSource::Cli,
        harness_source: PreferenceSource::Cli,
    }
}

fn deepseek_harness_target() -> ResolvedLaunchTarget {
    ResolvedLaunchTarget {
        routing_mode: RoutingMode::Direct,
        model_provider: ModelProvider::DeepSeek,
        requested_harness: HarnessSelection::DeepSeek,
        effective_harness: Backend::DeepSeek,
        provider_source: PreferenceSource::Cli,
        harness_source: PreferenceSource::Cli,
    }
}

#[test]
fn deepseek_harness_bare_launch_uses_web_profile() {
    let args = parse(&["clud", "--harness", "deepseek"]);
    let plan = build_launch_plan_for_target(&args, deepseek_harness_target(), "dsh");
    assert_eq!(plan.command, vec!["dsh", "web"]);
    assert_eq!(plan.effective_harness(), Backend::DeepSeek);
}

#[test]
fn deepseek_harness_prompt_uses_headless_profile_without_yolo_flags() {
    let args = parse(&["clud", "--harness", "deepseek", "-p", "hello"]);
    let plan = build_launch_plan_for_target(&args, deepseek_harness_target(), "dsh");
    assert_eq!(plan.command, vec!["dsh", "--profile", "headless", "hello"]);
}

#[test]
fn deepseek_harness_one_shot_builtins_keep_headless_profile() {
    for argv in [
        vec!["clud", "--harness", "deepseek", "up"],
        vec!["clud", "--harness", "deepseek", "rebase"],
        vec!["clud", "--harness", "deepseek", "fix"],
        vec![
            "clud",
            "--harness",
            "deepseek",
            "do",
            "https://github.com/zackees/clud/issues/1036",
        ],
    ] {
        let args = parse(&argv);
        let plan = build_launch_plan_for_target(&args, deepseek_harness_target(), "dsh");
        assert_eq!(
            &plan.command[..3],
            ["dsh", "--profile", "headless"],
            "argv={argv:?}, cmd={:?}",
            plan.command
        );
        assert_eq!(plan.launch_mode, LaunchMode::Subprocess);
    }
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
    assert_eq!(
        grind_launch_error(&args, deepseek_harness_target()),
        Some(
            "`clud grind` requires the Claude harness, whose interactive prompt supports `/loop`; use `--harness claude`"
        )
    );
}

#[test]
fn deepseek_harness_native_passthrough_does_not_inject_web() {
    let args = parse(&[
        "clud",
        "--harness",
        "deepseek",
        "--",
        "--profile",
        "headless",
        "hello",
    ]);
    let plan = build_launch_plan_for_target(&args, deepseek_harness_target(), "dsh");
    assert_eq!(plan.command, ["dsh", "--profile", "headless", "hello"]);
}

fn kimi_bridge_target() -> ResolvedLaunchTarget {
    ResolvedLaunchTarget {
        routing_mode: RoutingMode::Direct,
        model_provider: ModelProvider::Kimi,
        requested_harness: HarnessSelection::Claude,
        effective_harness: Backend::Claude,
        provider_source: PreferenceSource::Cli,
        harness_source: PreferenceSource::Cli,
    }
}

fn unified_target(provider: ModelProvider) -> ResolvedLaunchTarget {
    ResolvedLaunchTarget {
        routing_mode: RoutingMode::Unified,
        model_provider: provider,
        requested_harness: HarnessSelection::Claude,
        effective_harness: Backend::Claude,
        provider_source: PreferenceSource::Cli,
        harness_source: PreferenceSource::Cli,
    }
}

#[test]
fn test_bridge_disallows_plan_mode_even_when_interactive() {
    // The reported bug: an ordinary interactive question on this bridge turned
    // into an unprompted planning session. Suppression must not be gated on
    // `--unattended`.
    let args = parse(&["clud"]);
    let p = build_launch_plan_for_target(&args, bridge_target(), "claude");
    assert!(p
        .command
        .iter()
        .any(|a| a == "--disallowedTools=EnterPlanMode,Task"));
}

#[test]
fn test_bridge_permanently_disallows_task_subagents() {
    let args = parse(&["clud", "--allow-plan-mode"]);
    let p = build_launch_plan_for_target(&args, bridge_target(), "claude");
    assert!(p.command.iter().any(|a| a == "--disallowedTools=Task"));
}

#[test]
fn test_bridge_leaves_ask_user_question_alone() {
    // Only plan mode is the complaint; multiple-choice questions stay usable
    // in an interactive session.
    let args = parse(&["clud"]);
    let p = build_launch_plan_for_target(&args, bridge_target(), "claude");
    assert!(!p.command.iter().any(|a| a.contains("AskUserQuestion")));
}

#[test]
fn test_allow_plan_mode_restores_plan_mode_on_the_bridge() {
    let args = parse(&["clud", "--allow-plan-mode"]);
    let p = build_launch_plan_for_target(&args, bridge_target(), "claude");
    assert!(p.command.iter().any(|a| a == "--disallowedTools=Task"));
    // And the flag itself must not leak to the backend as passthrough.
    assert!(!p.command.iter().any(|a| a == "--allow-plan-mode"));
}

#[test]
fn test_allow_plan_mode_does_not_re_enable_it_for_unattended_runs() {
    // `--unattended` has its own, older reason to strip plan mode (a loop that
    // parks on a human never finishes). Opting into plan mode must not defeat
    // that; the bridge rule is the only thing `--allow-plan-mode` turns off.
    let args = parse(&["clud", "--allow-plan-mode", "--unattended", "-p", "hi"]);
    let p = build_launch_plan_for_target(&args, bridge_target(), "claude");
    assert!(p
        .command
        .iter()
        .any(|a| a == "--disallowedTools=EnterPlanMode,Task,AskUserQuestion"));
}

#[test]
fn test_plain_claude_keeps_plan_mode_interactively() {
    // Narrow rule: only non-Claude providers routing through the Claude harness
    // (Codex, DeepSeek) are affected.
    let p = plan(&["clud"]);
    assert!(!p.command.iter().any(|a| a.starts_with("--disallowedTools")));
}

#[test]
fn test_deepseek_bridge_disallows_plan_mode() {
    // DeepSeek models driving the Claude harness self-invoke EnterPlanMode
    // the same way Codex models do (zackees/clud#841 follow-up).
    // Unlike Codex, DeepSeek keeps `Task` — only `EnterPlanMode` is stripped.
    let args = parse(&["clud"]);
    let p = build_launch_plan_for_target(&args, deepseek_bridge_target(), "claude");
    let disallowed = p
        .command
        .iter()
        .find(|a| a.starts_with("--disallowedTools"))
        .map(|a| a.as_str())
        .unwrap_or("");
    assert!(
        disallowed.contains("EnterPlanMode"),
        "expected EnterPlanMode disallowed, got: {disallowed:?}"
    );
    assert!(
        !disallowed.contains("Task"),
        "Task should not be disallowed for DeepSeek, got: {disallowed:?}"
    );
}

#[test]
fn test_allow_plan_mode_restores_plan_mode_on_deepseek_bridge() {
    let args = parse(&["clud", "--allow-plan-mode"]);
    let p = build_launch_plan_for_target(&args, deepseek_bridge_target(), "claude");
    assert!(!p
        .command
        .iter()
        .any(|a| a.contains("EnterPlanMode") && a.starts_with("--disallowedTools")));
}

/// #937 Phase 3 / #936 "Decisions": "Do not copy DeepSeek-specific plan-mode
/// suppression without Kimi-specific failing evidence." `is_non_claude_
/// claude_harness_bridge`'s `matches!` is not compiler-exhaustive, so the
/// absence of a `ModelProvider::Kimi` arm there is a silent, deliberate
/// choice rather than something the compiler forces a reviewer to notice.
/// This test makes that choice visible: an interactive Kimi-via-Claude
/// launch (exercised through a real interactive `parse`, not `--unattended`
/// or `loop`, which strip plan mode for an unrelated reason) must keep
/// `EnterPlanMode` available.
#[test]
fn test_kimi_bridge_does_not_suppress_plan_mode() {
    let args = parse(&["clud"]);
    let p = build_launch_plan_for_target(&args, kimi_bridge_target(), "claude");
    assert!(
        !p.command
            .iter()
            .any(|a| a.starts_with("--disallowedTools") && a.contains("EnterPlanMode")),
        "Kimi must not suppress EnterPlanMode without Kimi-specific evidence (#936): {:?}",
        p.command
    );
}

#[test]
fn test_plain_codex_harness_keeps_getting_no_claude_only_flag() {
    // Codex harness has no EnterPlanMode surface and rejects the flag.
    let p = plan(&["clud", "--codex"]);
    assert!(!p.command.iter().any(|a| a.starts_with("--disallowedTools")));
}

/// The Claude harness receives the registered discovery ID while the plan
/// retains the provider wire selection for bridge/default compatibility.
#[test]
fn test_bridge_uses_a_discovery_id_and_separate_effort() {
    let args = parse(&["clud", "--model", "terra@high"]);
    let p = build_launch_plan_for_target(&args, bridge_target(), "claude");
    let model_index = p.command.iter().position(|a| a == "--model").unwrap();
    assert_eq!(p.command[model_index + 1], "clud-claude-codex-terra");
    assert_eq!(p.codex_model.as_deref(), Some("gpt-5.6-terra@high"));
    assert!(p
        .command
        .windows(2)
        .any(|pair| pair == ["--effort", "high"]));
}

/// DD-059: a plain `clud --deepseek` launch carries the catalog default
/// effort on the harness's own `--effort` session flag (an initial value, so
/// `/effort` stays the live session control) instead of a pinned
/// `CLAUDE_CODE_EFFORT_LEVEL` env var. `main` populates
/// `resolved_model_selection` from `resolve_for_launch` with the catalog
/// default enabled for direct launches; the test mirrors that seam.
#[test]
fn test_deepseek_bridge_catalog_default_effort_rides_the_session_flag() {
    let mut args = parse(&["clud", "--deepseek"]);
    args.resolved_model_selection = crate::provider_catalog::resolve_for_launch(
        ModelProvider::DeepSeek,
        None,
        None,
        None,
        None,
        true,
    )
    .unwrap();
    let p = build_launch_plan_for_target(&args, deepseek_bridge_target(), "claude");
    assert!(
        p.command.windows(2).any(|pair| pair == ["--effort", "low"]),
        "expected --effort low from the catalog default, got: {}",
        p.command.join(" ")
    );
}

/// Issue #955: the shared catalog is the extension seam for both the native
/// Codex harness and Claude's discovery namespace. This deliberately iterates
/// rows instead of restating Sol/Terra/Luna in the adapter test.
#[test]
fn every_registered_codex_model_has_native_and_claude_harness_addresses() {
    for model in crate::provider_catalog::models_for_provider(ModelProvider::Codex) {
        let args = parse(&["clud", "--model", model.cli_id, "--effort", "high"]);
        let claude = build_launch_plan_for_target(&args, bridge_target(), "claude");
        let discovery_id = model.discovery_id.expect("Codex discovery id");
        assert!(
            claude
                .command
                .windows(2)
                .any(|pair| pair == ["--model", discovery_id]),
            "{}",
            claude.command.join(" ")
        );
        assert!(claude
            .command
            .windows(2)
            .any(|pair| pair == ["--effort", "high"]));

        let native_target =
            crate::backend::resolve_launch_target(false, true, false, None, None, None).unwrap();
        let native = build_launch_plan_for_target(&args, native_target, "codex");
        assert!(
            native
                .command
                .windows(2)
                .any(|pair| pair == ["-m", model.wire_id]),
            "{}",
            native.command.join(" ")
        );
    }
}

#[test]
fn unified_initial_routes_use_discovery_ids_and_keep_plan_mode() {
    for (provider, model, discovery_id) in [
        (ModelProvider::Codex, "codex-luna", "clud-claude-codex-luna"),
        (
            ModelProvider::DeepSeek,
            "deepseek-v4-pro",
            "clud-claude-deepseek-v4-pro-0813",
        ),
    ] {
        let args = parse(&["clud", "--model", model, "--effort", "high"]);
        let plan = build_launch_plan_for_target(&args, unified_target(provider), "claude");
        assert!(
            plan.command
                .windows(2)
                .any(|pair| pair == ["--model", discovery_id]),
            "{}",
            plan.command.join(" ")
        );
        assert!(
            plan.command
                .windows(2)
                .any(|pair| pair == ["--effort", "high"]),
            "{}",
            plan.command.join(" ")
        );
        assert!(!plan
            .command
            .iter()
            .any(|arg| arg == "--disallowedTools=EnterPlanMode"));
        assert_eq!(plan.codex_model, None);
    }
}

#[test]
fn model_less_bridge_effort_pins_the_reviewed_default_model() {
    let args = parse(&["clud", "--effort", "high"]);
    let plan = build_launch_plan_for_target(&args, bridge_target(), "claude");
    assert!(plan
        .command
        .windows(2)
        .any(|pair| pair == ["--model", "clud-claude-codex-terra"]));
    assert!(plan
        .command
        .windows(2)
        .any(|pair| pair == ["--effort", "high"]));
    assert_eq!(plan.codex_model.as_deref(), Some("gpt-5.6-terra@high"));
}

#[test]
fn bridge_keeps_none_effort_on_the_discovery_id() {
    let args = parse(&["clud", "--model", "luna", "--effort", "none"]);
    let plan = build_launch_plan_for_target(&args, bridge_target(), "claude");
    assert!(plan
        .command
        .windows(2)
        .any(|pair| pair == ["--model", "clud-claude-codex-luna@none"]));
    assert!(!plan.command.iter().any(|arg| arg == "--effort"));
}

#[test]
fn native_codex_receives_normalized_model_and_effort_as_separate_settings() {
    let args = parse(&[
        "clud",
        "--codex",
        "--model",
        "terra@high",
        "--effort",
        "high",
    ]);
    let target =
        crate::backend::resolve_launch_target(false, true, false, None, None, None).unwrap();
    let plan = build_launch_plan_for_target(&args, target, "codex");
    assert!(plan
        .command
        .windows(2)
        .any(|pair| pair == ["-m", "gpt-5.6-terra"]));
    assert!(plan
        .command
        .windows(2)
        .any(|pair| pair == ["-c", "model_reasoning_effort=\"high\""]));
}

#[test]
fn native_claude_receives_effort_as_a_session_flag() {
    let args = parse(&["clud", "--claude", "--model", "opus", "--effort", "high"]);
    let target =
        crate::backend::resolve_launch_target(true, false, false, None, None, None).unwrap();
    let plan = build_launch_plan_for_target(&args, target, "claude");
    assert!(plan
        .command
        .windows(2)
        .any(|pair| pair == ["--model", "opus"]));
    assert!(plan
        .command
        .windows(2)
        .any(|pair| pair == ["--effort", "high"]));
}

/// An id we do not know is forwarded untouched — the alias table gives short
/// names, it does not gate which models are reachable.
#[test]
fn test_bridge_forwards_an_unknown_full_model_id_untouched() {
    let args = parse(&["clud", "--model", "gpt-5.7-nova"]);
    let p = build_launch_plan_for_target(&args, bridge_target(), "claude");
    let model_index = p.command.iter().position(|a| a == "--model").unwrap();
    assert_eq!(p.command[model_index + 1], "gpt-5.7-nova");
    assert_eq!(p.codex_model.as_deref(), Some("gpt-5.7-nova"));
}

/// A typo is left alone here and rejected by the bridge, which owns the
/// message. What must not happen is a silent substitution of the default.
#[test]
fn test_bridge_does_not_substitute_a_default_for_an_unknown_alias() {
    let args = parse(&["clud", "--model", "tera"]);
    let p = build_launch_plan_for_target(&args, bridge_target(), "claude");
    let model_index = p.command.iter().position(|a| a == "--model").unwrap();
    assert_eq!(p.command[model_index + 1], "tera");
    assert_eq!(p.codex_model, None);
}

/// Off the bridge, `--model` is the harness's own flag and must not be
/// rewritten: `clud --model sonnet` selects a Claude model, not a Codex one.
#[test]
fn test_native_routes_never_rewrite_the_model_flag() {
    let p = plan(&["clud", "--model", "sonnet"]);
    let model_index = p.command.iter().position(|a| a == "--model").unwrap();
    assert_eq!(p.command[model_index + 1], "sonnet");
    assert_eq!(p.codex_model, None);
}

#[test]
fn test_plan_mode_suppression_notice_is_green_and_tty_only() {
    let args = parse(&["clud"]);
    let notice = plan_mode_suppression_notice(&args, bridge_target(), true, false).unwrap();
    assert!(notice.starts_with("\x1b[32m"));
    assert!(notice.ends_with("\x1b[0m"));
    assert!(notice.contains("--allow-plan-mode"));

    // Not a terminal, or structured output wanted: stay silent.
    assert_eq!(
        plan_mode_suppression_notice(&args, bridge_target(), false, false),
        None
    );
    assert_eq!(
        plan_mode_suppression_notice(&args, bridge_target(), true, true),
        None
    );

    // Nothing suppressed => nothing announced.
    let allowed = parse(&["clud", "--allow-plan-mode"]);
    assert_eq!(
        plan_mode_suppression_notice(&allowed, bridge_target(), true, false),
        None
    );
}

#[test]
fn test_unattended_disallows_interactive_tools() {
    let p = plan(&["clud", "--unattended", "-p", "hello"]);
    assert_eq!(
        p.command,
        vec![
            "claude",
            "--dangerously-skip-permissions",
            "--disallowedTools=EnterPlanMode,AskUserQuestion",
            "-p",
            "hello"
        ]
    );
}

#[test]
fn test_unattended_emits_a_single_argv_token() {
    // `claude` declares `--disallowedTools <tools...>` as variadic: the
    // space-separated spelling swallows the following token, so a later
    // `-p <prompt>` silently vanishes and claude exits 0 producing nothing.
    // Keeping this one `=`-bound token is what makes the flag order-safe.
    let p = plan(&["clud", "--unattended", "-p", "hello"]);
    assert!(!p.command.iter().any(|a| a == "--disallowedTools"));
    assert!(p
        .command
        .iter()
        .any(|a| a == "--disallowedTools=EnterPlanMode,AskUserQuestion"));
    // The prompt must survive intact directly after its own flag.
    let idx = p.command.iter().position(|a| a == "-p").unwrap();
    assert_eq!(p.command[idx + 1], "hello");
}

#[test]
fn test_unattended_is_a_noop_for_codex_harness() {
    // Codex has no EnterPlanMode/AskUserQuestion surface, and it would reject
    // a claude-only flag.
    let p = plan(&["clud", "--codex", "--unattended", "-p", "hello"]);
    assert!(!p.command.iter().any(|a| a.starts_with("--disallowedTools")));
}

#[test]
fn test_unattended_is_not_forwarded_as_passthrough() {
    // `--unattended` must be in `bool_flags` in `split_known_unknown`, or the
    // splitter routes it to the backend, which errors on an unknown flag.
    let p = plan(&["clud", "--unattended", "-p", "hello"]);
    assert!(!p.command.iter().any(|a| a == "--unattended"));
}

#[test]
fn test_safe_mode_no_yolo() {
    let p = plan(&["clud", "--safe", "-p", "hello"]);
    assert_eq!(p.command, vec!["claude", "-p", "hello"]);
}

#[test]
fn test_codex_prompt_goes_through_exec_subcommand() {
    // Codex's `-p` is `--profile`, not a prompt flag. Non-interactive
    // runs must use `codex exec <prompt>` with the prompt as positional.
    let p = plan(&["clud", "--codex", "-p", "hello"]);
    assert_eq!(
        p.command,
        [
            codex_prefix(),
            vec![
                "exec".to_string(),
                "--dangerously-bypass-approvals-and-sandbox".to_string(),
                "hello".to_string(),
            ],
        ]
        .concat()
    );
    // `codex exec` is non-interactive; subprocess mode is fine.
    assert_eq!(p.launch_mode, LaunchMode::Subprocess);
}

#[test]
fn test_codex_interactive_defaults_to_pty_without_tty() {
    // Under `cargo test` there is no controlling terminal, so
    // `parent_has_tty` is false and the interactive TUI gets a PTY.
    // With a real TTY (normal user invocation), codex runs as a
    // subprocess that inherits the terminal directly — see
    // `backend::test_codex_interactive_with_tty_uses_subprocess`.
    let p = plan(&["clud", "--codex"]);
    assert_eq!(
        p.command,
        [
            codex_prefix(),
            vec!["--dangerously-bypass-approvals-and-sandbox".to_string()],
        ]
        .concat()
    );
    assert_eq!(p.launch_mode, LaunchMode::Pty);
}

#[test]
fn test_codex_keeps_native_agents_when_agents_md_exists() {
    let repo = tempfile::tempdir().unwrap();
    std::fs::write(repo.path().join("AGENTS.md"), "native agents").unwrap();
    std::fs::write(repo.path().join("CODEX.md"), "codex fallback").unwrap();
    std::fs::write(repo.path().join("CLAUDE.md"), "claude fallback").unwrap();

    let p = plan_at(&["clud", "--codex"], repo.path());

    assert!(!codex_config_values(&p)
        .iter()
        .any(|value| value.starts_with("project_doc_fallback_filenames=")));
}

#[test]
fn test_codex_uses_codex_md_as_project_doc_fallback_before_claude_md() {
    let repo = tempfile::tempdir().unwrap();
    std::fs::write(repo.path().join("CODEX.md"), "codex fallback").unwrap();
    std::fs::write(repo.path().join("CLAUDE.md"), "claude fallback").unwrap();

    let p = plan_at(&["clud", "--codex"], repo.path());

    assert!(codex_config_values(&p).contains(&r#"project_doc_fallback_filenames=["CODEX.md"]"#));
    assert!(!codex_config_values(&p)
        .iter()
        .any(|value| value.contains("CLAUDE.md")));
}

#[test]
fn test_codex_uses_claude_md_when_agents_and_codex_are_absent() {
    let repo = tempfile::tempdir().unwrap();
    std::fs::write(repo.path().join("CLAUDE.md"), "claude fallback").unwrap();

    let p = plan_at(&["clud", "--codex"], repo.path());

    assert!(codex_config_values(&p).contains(&r#"project_doc_fallback_filenames=["CLAUDE.md"]"#));
}

#[test]
fn test_codex_project_doc_fallback_noops_when_no_instruction_file_exists() {
    let repo = tempfile::tempdir().unwrap();

    let p = plan_at(&["clud", "--codex"], repo.path());

    assert!(!codex_config_values(&p)
        .iter()
        .any(|value| value.starts_with("project_doc_fallback_filenames=")));
}

#[test]
fn test_codex_continue_uses_resume_last() {
    // `-c` on codex maps to `codex resume --last`, not `--continue`.
    let p = plan(&["clud", "--codex", "-c"]);
    assert_eq!(
        p.command,
        [
            codex_prefix(),
            vec![
                "resume".to_string(),
                "--dangerously-bypass-approvals-and-sandbox".to_string(),
                "--last".to_string(),
            ],
        ]
        .concat()
    );
    assert_eq!(p.launch_mode, LaunchMode::Pty);
}

#[test]
fn test_codex_resume_with_session_id() {
    let p = plan(&["clud", "--codex", "-r", "sess-123"]);
    assert_eq!(
        p.command,
        [
            codex_prefix(),
            vec![
                "resume".to_string(),
                "--dangerously-bypass-approvals-and-sandbox".to_string(),
                "sess-123".to_string(),
            ],
        ]
        .concat()
    );
}

#[test]
fn test_codex_model_uses_short_m() {
    // Codex's model flag is `-m/--model`; Claude's is `--model`.
    let p = plan(&["clud", "--codex", "--model", "gpt-5"]);
    assert_eq!(
        p.command,
        [
            codex_prefix(),
            vec![
                "--dangerously-bypass-approvals-and-sandbox".to_string(),
                "-m".to_string(),
                "gpt-5".to_string(),
            ],
        ]
        .concat()
    );
}

#[test]
fn test_codex_builtin_verbs_seed_interactive_sessions() {
    for argv in [
        vec!["clud", "--codex", "up"],
        vec!["clud", "--codex", "rebase"],
        vec!["clud", "--codex", "fix"],
        vec![
            "clud",
            "--codex",
            "do",
            "https://github.com/zackees/clud/issues/1036",
        ],
        vec![
            "clud",
            "--codex",
            "grind",
            "https://github.com/zackees/clud/issues",
        ],
    ] {
        let p = plan(&argv);
        assert_eq!(p.command[0], "codex", "argv={argv:?}");
        assert!(
            !p.command.iter().any(|arg| arg == "exec"),
            "one-shot built-in must not use codex exec; argv={argv:?}, cmd={:?}",
            p.command
        );
        assert!(
            p.command.iter().all(|arg| arg != "-p"),
            "Codex prompt must stay positional; argv={argv:?}, cmd={:?}",
            p.command
        );
        assert_eq!(
            p.launch_mode,
            LaunchMode::Pty,
            "headless tests have no parent TTY, so an interactive Codex TUI needs a PTY; argv={argv:?}"
        );
    }
}

#[test]
fn codex_resumed_builtins_put_the_session_selector_before_the_prompt() {
    for mut argv in [
        vec!["clud", "--codex", "--resume=sess-123", "up"],
        vec!["clud", "--codex", "--resume=sess-123", "rebase"],
        vec!["clud", "--codex", "--resume=sess-123", "fix"],
        vec![
            "clud",
            "--codex",
            "--resume=sess-123",
            "do",
            "https://github.com/zackees/clud/issues/1036",
        ],
        vec![
            "clud",
            "--codex",
            "--resume=sess-123",
            "grind",
            "https://github.com/zackees/clud/issues",
        ],
    ] {
        let resumed_plan = plan(&argv);
        let resume = resumed_plan
            .command
            .iter()
            .position(|arg| arg == "resume")
            .unwrap();
        let session = resumed_plan
            .command
            .iter()
            .position(|arg| arg == "sess-123")
            .unwrap();
        assert!(resume < session && session + 1 == resumed_plan.command.len() - 1);
        assert!(!resumed_plan.command.iter().any(|arg| arg == "exec"));

        argv.retain(|arg| *arg != "--resume=sess-123");
        argv.insert(2, "--continue");
        let continued_plan = plan(&argv);
        let resume = continued_plan
            .command
            .iter()
            .position(|arg| arg == "resume")
            .unwrap();
        let last = continued_plan
            .command
            .iter()
            .position(|arg| arg == "--last")
            .unwrap();
        assert!(resume < last && last + 1 == continued_plan.command.len() - 1);
        assert!(!continued_plan.command.iter().any(|arg| arg == "exec"));
    }
}

#[test]
fn bare_resume_with_interactive_codex_builtin_is_rejected_and_plan_safe() {
    let args = parse(&[
        "clud",
        "--codex",
        "--resume",
        "do",
        "https://github.com/zackees/clud/issues/1036",
    ]);
    assert!(interactive_builtin_resume_error(&args, Backend::Codex)
        .unwrap()
        .contains("--resume=<session>"));
    assert!(interactive_builtin_resume_error(&args, Backend::Claude).is_none());

    let plan = build_launch_plan(&args, Backend::Codex, "codex");
    assert!(plan.command.iter().any(|arg| arg == "resume"));
    assert!(!plan.command.iter().any(|arg| arg.starts_with("/goal")));
}

/// #1173: `grind` is one interactive Claude-harness session. On the Codex
/// harness it is refused outright by `grind_launch_error` -- not by the
/// headless-builtin `--resume` check, which no longer applies to it -- and
/// the plan carries none of clud's own loop machinery.
#[test]
fn bare_resume_with_codex_grind_is_refused_by_the_harness_check() {
    let args = parse(&[
        "clud",
        "--codex",
        "--resume",
        "grind",
        "https://github.com/zackees/clud/issues",
    ]);
    assert!(interactive_builtin_resume_error(&args, Backend::Codex).is_none());
    let codex_harness = ResolvedLaunchTarget {
        routing_mode: RoutingMode::Direct,
        model_provider: ModelProvider::Codex,
        requested_harness: HarnessSelection::Codex,
        effective_harness: Backend::Codex,
        provider_source: PreferenceSource::Cli,
        harness_source: PreferenceSource::Cli,
    };
    let error = grind_launch_error(&args, codex_harness).expect("grind needs the Claude harness");
    assert!(error.contains("requires the Claude harness"), "{error}");
    let plan = build_launch_plan(&args, Backend::Codex, "codex");
    assert_eq!(plan.iterations, 1);
    assert!(plan.loop_markers.is_none());
    assert!(plan.repeat_schedule.is_none());
}

#[test]
fn unresolved_do_target_has_a_safe_interactive_plan_fallback() {
    for (argv, expected_backend_token) in [
        (vec!["clud", "--codex", "do"], "codex"),
        (vec!["clud", "do"], "claude"),
        (vec!["clud", "--harness", "deepseek", "do"], "dsh"),
    ] {
        let plan = if expected_backend_token == "dsh" {
            let args = parse(&argv);
            build_launch_plan_for_target(&args, deepseek_harness_target(), "dsh")
        } else {
            plan(&argv)
        };
        assert_eq!(plan.command[0], expected_backend_token, "argv={argv:?}");
        assert!(
            !plan.command.iter().any(|arg| arg == "exec" || arg == "headless"),
            "an unresolved target must fall back to the harness's interactive entry; argv={argv:?}, cmd={:?}",
            plan.command
        );
    }
}

#[test]
fn test_model_flag() {
    let p = plan(&["clud", "--model", "opus", "-p", "hello"]);
    assert_eq!(
        p.command,
        vec![
            "claude",
            "--dangerously-skip-permissions",
            "--model",
            "opus",
            "-p",
            "hello"
        ]
    );
}

#[test]
fn test_continue_session() {
    let p = plan(&["clud", "-c"]);
    assert_eq!(
        p.command,
        vec!["claude", "--dangerously-skip-permissions", "--continue"]
    );
}

#[test]
fn test_message_flag() {
    let p = plan(&["clud", "-m", "fix bug"]);
    assert_eq!(
        p.command,
        vec!["claude", "--dangerously-skip-permissions", "-m", "fix bug"]
    );
}

#[test]
fn test_up_default() {
    let p = plan(&["clud", "up"]);
    let prompt = prompt_from_plan(&p);
    assert!(prompt.contains("lint"));
    assert!(prompt.contains("codeup"));
    assert!(prompt.contains("<your one-line summary>"));
    assert!(!prompt.contains("-p\n"));
}

#[test]
fn test_up_with_message() {
    let p = plan(&["clud", "up", "-m", "bump version"]);
    let prompt = prompt_from_plan(&p);
    assert!(prompt.contains("codeup -m \"bump version\""));
    assert!(!prompt.contains("<your one-line summary>"));
}

#[test]
fn test_up_with_publish() {
    let p = plan(&["clud", "up", "--publish"]);
    let prompt = prompt_from_plan(&p);
    assert!(prompt.contains("codeup -m \"<your one-line summary>\" -p"));
}

#[test]
fn test_up_with_message_and_publish() {
    let p = plan(&["clud", "up", "-m", "release v2", "--publish"]);
    let prompt = prompt_from_plan(&p);
    assert!(prompt.contains("codeup -m \"release v2\" -p"));
}

#[test]
fn test_rebase_command() {
    let p = plan(&["clud", "rebase"]);
    let prompt = prompt_from_plan(&p);
    assert!(prompt.contains("git fetch"));
    assert!(prompt.contains("rebase"));
}

#[test]
fn test_fix_default() {
    let p = plan(&["clud", "fix"]);
    let prompt = prompt_from_plan(&p);
    assert!(prompt.contains("linting"));
    assert!(prompt.contains("unit tests"));
}

#[test]
fn test_fix_with_github_url() {
    let p = plan(&[
        "clud",
        "fix",
        "https://github.com/user/repo/actions/runs/123",
    ]);
    let prompt = prompt_from_plan(&p);
    assert!(prompt.contains("https://github.com/user/repo/actions/runs/123"));
    assert!(prompt.contains("gh run view"));
    assert!(prompt.contains("lint-test"));
}

#[test]
fn test_fix_with_non_github_url() {
    let p = plan(&["clud", "fix", "https://example.com/logs"]);
    let prompt = prompt_from_plan(&p);
    assert!(prompt.contains("linting"));
    assert!(!prompt.contains("example.com"));
}

#[test]
fn test_do_command_resolves_goal_prompt() {
    let p = plan(&["clud", "do", "https://github.com/zackees/clud/issues/866"]);
    // `do` seeds an interactive session, so the prompt is the trailing
    // positional (no `-p`), same shape as codex.
    let prompt = last_arg(&p);
    assert!(prompt.starts_with("/goal "));
    assert!(prompt.contains("https://github.com/zackees/clud/issues/866"));
    assert!(!prompt.contains("{url}"));
}

#[test]
fn test_build_do_prompt_substitutes_url() {
    let prompt = build_do_prompt("https://example.com/thing");
    assert!(prompt.starts_with("/goal read https://example.com/thing"));
    assert!(!prompt.contains("{url}"));
}

#[test]
fn test_build_do_prompt_accepts_bare_domain_url() {
    let prompt = build_do_prompt("github.com/zackees/clud/issues/1036");
    assert!(prompt.starts_with("/goal read github.com/zackees/clud/issues/1036"));
}

#[test]
fn test_build_do_prompt_treats_free_form_input_as_a_goal_not_a_url() {
    for input in [
        "refactor the launch mode classifier",
        "README.md",
        "v2.8.0",
        "foo.rs",
    ] {
        let prompt = build_do_prompt(input);
        assert!(
            prompt.starts_with(&format!("/goal {input}")),
            "prompt={prompt}"
        );
        assert!(!prompt.starts_with("/goal read "), "prompt={prompt}");
        assert!(prompt.contains("validated, tested, pushed and merged"));
    }
}

#[test]
fn test_build_grind_prompt_substitutes_url() {
    let prompt = build_grind_prompt("https://example.com/repo/issues");
    assert!(prompt.starts_with("/loop "));
    assert!(prompt.contains("https://example.com/repo/issues"));
    assert!(!prompt.contains("{url}"));
}

#[test]
fn grind_uses_one_interactive_harness_session_without_external_loop_state() {
    let p = plan(&["clud", "grind", "https://github.com/zackees/clud/issues"]);
    let prompt = last_arg(&p);
    assert!(
        prompt.starts_with("/loop "),
        "grind prompt must start with /loop; got: {prompt:?}"
    );
    assert!(
        prompt.contains("https://github.com/zackees/clud/issues"),
        "grind prompt must contain the URL; got: {prompt:?}"
    );
    assert!(!p.command.iter().any(|arg| arg == "-p"));
    assert_eq!(p.launch_mode, LaunchMode::Pty);
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
    assert_eq!(plan.launch_mode, LaunchMode::Pty);
    assert_eq!(plan.iterations, 1);
    assert!(plan.loop_markers.is_none());
    assert!(plan.command.last().is_some_and(|prompt| {
        prompt.starts_with("/loop ") && prompt.contains("zackees/clud/issues")
    }));
}

#[test]
fn test_loop_command() {
    let p = plan(&["clud", "loop", "--loop-count", "5", "do stuff"]);
    assert_eq!(p.iterations, 5);
    assert!(p.command.contains(&"-p".to_string()));
    let prompt = prompt_from_plan(&p);
    assert!(prompt.starts_with("do stuff"));
    // Issue #95: contract now embeds absolute paths; the relative
    // suffix is still present, but the separator is platform-native.
    assert!(
        prompt.contains(".clud/loop/DONE") || prompt.contains(".clud\\loop\\DONE"),
        "prompt missing DONE marker path: {prompt}"
    );
    assert!(
        prompt.contains(".clud/loop/BLOCKED") || prompt.contains(".clud\\loop\\BLOCKED"),
        "prompt missing BLOCKED marker path: {prompt}"
    );
    assert!(p.loop_markers.is_some());
}

#[test]
fn test_loop_default_count() {
    let p = plan(&["clud", "loop", "task"]);
    assert_eq!(p.iterations, 50);
}

#[test]
fn test_loop_no_done_omits_contract() {
    let p = plan(&["clud", "loop", "--no-done", "task"]);
    let prompt = prompt_from_plan(&p);
    assert_eq!(prompt, "task");
    assert!(p.loop_markers.is_none());
}

#[test]
fn test_loop_repeat_implies_no_done_contract() {
    let p = plan(&["clud", "loop", "--repeat", "1h", "task"]);
    let prompt = prompt_from_plan(&p);
    assert_eq!(prompt, "task");
    assert!(p.loop_markers.is_none());
    assert_eq!(
        p.repeat_schedule.as_ref().map(|s| s.interval_secs),
        Some(3600)
    );
}

#[test]
fn test_loop_repeat_with_done_override_restores_contract() {
    let p = plan(&[
        "clud", "loop", "--repeat", "1h", "--done", "DONE.md", "task",
    ]);
    let prompt = prompt_from_plan(&p);
    assert!(prompt.contains("DONE.md"));
    assert!(prompt.contains("BLOCKED.md"));
    assert!(p.loop_markers.is_some());
    let markers = p.loop_markers.unwrap();
    assert!(markers.done_path.ends_with("DONE.md"));
    assert!(markers.blocked_path.ends_with("BLOCKED.md"));
}

// ---- Issue #48: `clud --codex loop "..."` must drive codex the same ----
// way `clud loop` drives claude: exec subcommand, positional prompt,
// DONE/BLOCKED contract appended, loop_markers populated, and the
// non-interactive launch mode (subprocess) selected.

#[test]
fn test_codex_loop_routes_through_exec() {
    let p = plan(&["clud", "--codex", "loop", "--loop-count", "5", "do stuff"]);
    assert_eq!(p.command[0], "codex");
    assert!(codex_exec_index(&p) > 0);
    assert!(p
        .command
        .contains(&"--dangerously-bypass-approvals-and-sandbox".to_string()));
    assert_eq!(p.iterations, 5);
    assert_eq!(p.backend, Backend::Codex);
}

#[test]
fn test_codex_loop_prompt_is_positional_not_dash_p() {
    // Codex's `-p` is `--profile`; the prompt must be the final positional.
    let p = plan(&["clud", "--codex", "loop", "do stuff"]);
    assert!(
        p.command.iter().all(|a| a != "-p"),
        "codex must not emit -p for the prompt; cmd={:?}",
        p.command
    );
    let last = last_arg(&p);
    assert!(
        last.starts_with("do stuff"),
        "codex prompt must be the last positional arg; got: {last:?}"
    );
}

#[test]
fn test_codex_loop_appends_done_marker_contract() {
    let p = plan(&["clud", "--codex", "loop", "do stuff"]);
    let prompt = last_arg(&p);
    // Issue #95: absolute paths in contract; assert on the relative
    // suffix using platform-native separators.
    assert!(
        prompt.contains(".clud/loop/DONE") || prompt.contains(".clud\\loop\\DONE"),
        "prompt missing DONE marker path: {prompt}"
    );
    assert!(
        prompt.contains(".clud/loop/BLOCKED") || prompt.contains(".clud\\loop\\BLOCKED"),
        "prompt missing BLOCKED marker path: {prompt}"
    );
    assert!(p.loop_markers.is_some());
}

#[test]
fn test_codex_loop_default_count() {
    let p = plan(&["clud", "--codex", "loop", "task"]);
    assert_eq!(p.iterations, 50);
}

#[test]
fn test_codex_loop_no_done_omits_contract() {
    let p = plan(&["clud", "--codex", "loop", "--no-done", "task"]);
    let prompt = last_arg(&p);
    assert_eq!(prompt, "task");
    assert!(p.loop_markers.is_none());
}

#[test]
fn test_codex_loop_uses_subprocess_launch_mode() {
    // `codex exec` is non-interactive → subprocess (pipe-friendly),
    // just like `clud --codex -p "..."`.
    let p = plan(&["clud", "--codex", "loop", "task"]);
    assert_eq!(p.launch_mode, LaunchMode::Subprocess);
}

#[test]
fn test_codex_loop_safe_mode_omits_bypass_flag() {
    let p = plan(&["clud", "--codex", "--safe", "loop", "task"]);
    assert!(!p
        .command
        .contains(&"--dangerously-bypass-approvals-and-sandbox".to_string()));
    assert_eq!(p.command[0], "codex");
    assert!(codex_exec_index(&p) > 0);
}

#[test]
fn test_codex_loop_forwards_passthrough_flags() {
    // `clud --codex loop "task" -- --verbose` must keep the passthrough
    // flag so the test harness can inject mock-agent flags the same way
    // it does for the claude path.
    let p = plan(&["clud", "--codex", "loop", "task", "--", "--verbose"]);
    assert!(p.command.contains(&"--verbose".to_string()));
}

// ---- Stream-JSON progress injection ----
//
// `clud loop` against claude in *subprocess* launch mode (Windows default,
// or anywhere `--subprocess` is forced) used to go silent for the whole
// iteration because `claude -p` buffers its final response. The fix is to
// append `--output-format stream-json --verbose` so claude streams its
// turn events live, and let the runtime render each event as a one-line
// progress update. PTY-mode loops already show the live TUI, so no
// injection is needed there.

/// Helper: locate the index of `needle` in `cmd`, panicking with a
/// readable message if missing.
fn expect_arg(cmd: &[String], needle: &str) -> usize {
    cmd.iter().position(|a| a == needle).unwrap_or_else(|| {
        panic!("expected `{needle}` in command; got {cmd:?}");
    })
}

#[test]
fn test_claude_loop_subprocess_injects_stream_json() {
    let p = plan(&["clud", "--subprocess", "loop", "task"]);
    assert_eq!(p.launch_mode, LaunchMode::Subprocess);
    let idx = expect_arg(&p.command, "stream-json");
    assert_eq!(
        p.command[idx - 1],
        "--output-format",
        "stream-json must follow --output-format; cmd={:?}",
        p.command
    );
    assert!(
        p.command.iter().any(|a| a == "--verbose"),
        "stream-json requires --verbose per claude's CLI contract; cmd={:?}",
        p.command
    );
    assert!(
        p.stream_json_progress,
        "LaunchPlan must signal the runtime to parse stream-json"
    );
}

#[test]
fn grind_never_enables_external_loop_stream_json() {
    let p = plan(&[
        "clud",
        "--subprocess",
        "grind",
        "https://github.com/zackees/clud/issues",
    ]);
    assert_eq!(p.launch_mode, LaunchMode::Pty);
    assert!(!p.command.iter().any(|arg| arg == "stream-json"));
    assert!(!p.command.iter().any(|arg| arg == "--verbose"));
    assert!(!p.stream_json_progress);
    assert!(p.command.last().is_some_and(|arg| arg.starts_with("/loop")));
}

#[test]
fn test_claude_loop_stream_json_flags_emitted_before_prompt() {
    // Regression guard for PR #91 / commit 8c0818a: the stream-json flags
    // must be inserted BEFORE `-p <prompt>` so that `command[-1]` is the
    // prompt body. Dry-run consumers, the Python integration tests in
    // tests/test_hello.py, and downstream tooling all rely on the
    // "prompt is the last arg" contract.
    let p = plan(&["clud", "--subprocess", "loop", "do stuff"]);
    assert!(p.stream_json_progress);

    // Prompt body must still be the last positional.
    let last = p.command.last().expect("cmd is non-empty");
    assert!(
        last.starts_with("do stuff"),
        "command[-1] must be the prompt body, got: {last:?} (full cmd: {:?})",
        p.command
    );

    // Each stream-json flag must appear strictly before `-p`.
    let p_idx = expect_arg(&p.command, "-p");
    for flag in ["--output-format", "stream-json", "--verbose"] {
        let flag_idx = expect_arg(&p.command, flag);
        assert!(
            flag_idx < p_idx,
            "{flag} (idx {flag_idx}) must come before -p (idx {p_idx}); cmd={:?}",
            p.command
        );
    }
}

#[test]
fn test_claude_loop_pty_does_not_inject_stream_json() {
    // PTY mode runs the live claude TUI; switching it into the
    // non-interactive stream-json wire format would *remove* the
    // streaming UX we already have.
    let p = plan(&["clud", "--pty", "loop", "task"]);
    assert_eq!(p.launch_mode, LaunchMode::Pty);
    assert!(
        !p.command.iter().any(|a| a == "stream-json"),
        "pty-mode loop must NOT inject stream-json; cmd={:?}",
        p.command
    );
    assert!(
        !p.stream_json_progress,
        "pty mode does not need the stream-json renderer"
    );
}

#[test]
fn test_codex_loop_does_not_inject_stream_json() {
    // codex does not accept `--output-format stream-json` — the flag is
    // claude-only. Forcing subprocess to be explicit so the test is
    // platform-independent.
    let p = plan(&["clud", "--codex", "--subprocess", "loop", "task"]);
    assert!(
        !p.command.iter().any(|a| a == "stream-json"),
        "codex must NOT receive --output-format stream-json; cmd={:?}",
        p.command
    );
    assert!(!p.stream_json_progress);
}

#[test]
fn test_claude_plain_prompt_does_not_inject_stream_json() {
    // Single-shot `clud -p` is short-lived and not a loop, so we keep
    // the existing UX untouched. Stream-json injection is loop-only.
    let p = plan(&["clud", "--subprocess", "-p", "hello"]);
    assert!(
        !p.command.iter().any(|a| a == "stream-json"),
        "plain -p must NOT receive stream-json injection; cmd={:?}",
        p.command
    );
    assert!(!p.stream_json_progress);
}

#[test]
fn test_claude_do_launches_interactively_without_print_flag() {
    // Regression for the `clud do` "nothing happened / halts" report: `do`
    // runs the `/goal` contract, which drives a long interactive Stop-hook
    // loop. Headless `-p` mode buffers its single final response and shows
    // nothing until the whole goal completes, so it must seed an *interactive*
    // session (bare positional prompt, no `-p`) instead.
    let p = plan(&["clud", "do", "https://github.com/o/r/issues/1"]);
    assert!(
        !p.command.iter().any(|a| a == "-p"),
        "clud do must NOT run headless `-p`; cmd={:?}",
        p.command
    );
    assert!(
        !p.stream_json_progress,
        "interactive do renders the live TUI; it must not use the stream-json renderer"
    );
    // The `/goal` prompt body is still the trailing positional and is intact.
    let last = p.command.last().expect("cmd is non-empty");
    assert!(
        last.starts_with("/goal ") && last.contains("https://github.com/o/r/issues/1"),
        "command[-1] must be the /goal prompt seed; got {last:?} (cmd={:?})",
        p.command
    );
}

#[test]
fn test_claude_loop_safe_mode_still_injects_stream_json() {
    // `--safe` only drops the YOLO permissions flag; it must not also
    // suppress progress streaming, which is orthogonal.
    let p = plan(&["clud", "--subprocess", "--safe", "loop", "task"]);
    assert!(p.command.iter().any(|a| a == "stream-json"));
    assert!(p.stream_json_progress);
    // Sanity: --safe removed the permissions bypass.
    assert!(!p
        .command
        .iter()
        .any(|a| a == "--dangerously-skip-permissions"));
}

#[test]
fn test_pty_override() {
    let p = plan(&["clud", "--pty", "-p", "hello"]);
    assert_eq!(p.launch_mode, LaunchMode::Pty);
}

#[test]
fn test_graphics_config_threads_into_launch_plan() {
    let p = plan(&[
        "clud",
        "--graphics=sixel",
        "--graphics-image",
        "banner.png",
        "--pty",
        "-p",
        "hello",
    ]);
    assert_eq!(p.graphics.mode, crate::graphics::GraphicsMode::Sixel);
    assert_eq!(
        p.graphics.image_path.as_ref().map(|path| path.as_os_str()),
        Some(std::ffi::OsStr::new("banner.png"))
    );
    assert!(!p.command.iter().any(|arg| arg.starts_with("--graphics")));
}

#[test]
fn test_passthrough_flags() {
    let p = plan(&["clud", "--some-flag", "-p", "hello"]);
    assert!(p.command.contains(&"--some-flag".to_string()));
}

#[test]
fn test_passthrough_after_separator() {
    let p = plan(&["clud", "-p", "hello", "--", "--verbose"]);
    assert!(p.command.contains(&"--verbose".to_string()));
}

#[test]
fn test_is_github_url() {
    assert!(is_github_url("https://github.com/user/repo"));
    assert!(is_github_url("http://github.com/user/repo"));
    assert!(!is_github_url("https://gitlab.com/user/repo"));
    assert!(!is_github_url("not a url"));
}

#[test]
fn test_build_fix_prompt_no_url() {
    let prompt = build_fix_prompt(None);
    assert_eq!(prompt, FIX_PROMPT);
}

#[test]
fn test_build_fix_prompt_github_url() {
    let prompt = build_fix_prompt(Some("https://github.com/user/repo/actions/runs/999"));
    assert!(prompt.contains("runs/999"));
    assert!(prompt.contains("gh run view"));
}

#[test]
fn test_build_up_prompt_default() {
    let prompt = build_up_prompt(None, false);
    assert!(prompt.contains("<your one-line summary>"));
    assert!(!prompt.contains(" -p"));
}

#[test]
fn test_build_up_prompt_custom_message() {
    let prompt = build_up_prompt(Some("my msg"), false);
    assert!(prompt.contains("codeup -m \"my msg\""));
    assert!(!prompt.contains("<your one-line summary>"));
}

#[test]
fn test_build_up_prompt_publish() {
    let prompt = build_up_prompt(None, true);
    assert!(prompt.contains("-p"));
}

// -----------------------------------------------------------------
// Issue #61: --repeat scheduling tests
// -----------------------------------------------------------------
//
// These cover three areas:
//   1. parse_repeat_interval: every accepted form + the rejection
//      cases the issue calls out (negative, fractional, unknown
//      unit, overflow, empty, zero, missing unit, missing value).
//   2. repeat_implies_no_done_warning: the precedence ladder
//      (--repeat alone fires; explicit --no-done suppresses;
//      --done <path> suppresses + restores contract).
//   3. next_run_at_millis: the no-overlap invariant. The next run
//      time is always derived from *completion*, so a long-running
//      iteration pushes the schedule out instead of overlapping.

#[path = "tests/repeat_schedule.rs"]
mod repeat_schedule;

#[path = "tests/repeat_execution.rs"]
mod repeat_execution;
