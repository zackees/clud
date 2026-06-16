use std::collections::BTreeSet;
use std::path::PathBuf;

use running_process::{NativeProcess, ProcessConfig, StderrMode, StdinMode};

use super::inspect::inspect_current;
use super::types::{HookConfigError, HookFrontend, HookHealthReport, NormalizedHook, RepairAction};
use super::utils::{display_path, join_matchers};
use super::CATCH_ALL_MATCHER;
use crate::args::Args;
use crate::backend::{self, Backend};
use crate::command;
use crate::subprocess;

pub(in crate::hook_health) fn migration_prompt(
    hook: &NormalizedHook,
    target: HookFrontend,
) -> String {
    let target_path = match target {
        HookFrontend::Claude => ".claude/settings.json",
        HookFrontend::Codex => ".codex/hooks.json",
    };
    format!(
        "\
You are fixing Claude/Codex PreToolUse hook parity for this repository.

Migrate or copy exactly one hook from {source} to {target}.

Source config: {source_path}
Target config: {target_path}
Event: PreToolUse
Matcher/tool pattern: {matcher}

Preserve the hook intent conservatively. Claude and Codex PreToolUse hooks are similar but not full parity: permission decisions, input rewriting, context injection, matcher names, and tool coverage can differ. Do not pretend unsupported target behavior is enforceable; document unsupported semantics in the target config comment or adjacent note if needed.

After editing the target config, validate that the target hook config parses. If parsing fails, fix only the affected target hook/config file and reparse. Stop when the migrated hook parses cleanly or when the migration is blocked by unsupported target semantics.
",
        source = hook.frontend.display_name(),
        target = target.display_name(),
        source_path = display_path(&hook.source_path),
        target_path = target_path,
        matcher = hook.matcher
    )
}

pub(in crate::hook_health) fn catch_all_migration_prompt(
    hooks: &[&NormalizedHook],
    target: HookFrontend,
) -> String {
    let first = hooks[0];
    let target_path = match target {
        HookFrontend::Claude => ".claude/settings.json",
        HookFrontend::Codex => ".codex/hooks.json",
    };
    let matchers = hooks
        .iter()
        .map(|hook| hook.matcher.clone())
        .collect::<BTreeSet<_>>();
    let command = first.command.as_deref().unwrap_or("(no command recorded)");
    format!(
        "\
You are fixing Claude/Codex PreToolUse hook parity for this repository.

Migrate or copy these equivalent hooks from {source} to {target} as one Codex catch-all hook.

Source config: {source_path}
Target config: {target_path}
Event: PreToolUse
Matcher/tool pattern: {catch_all}
Source matchers covered by the catch-all hook: {matchers}
Shared command: {command}

Use matcher \"*\" for the target Codex hook so one reviewed hook can cover all matching tools and avoid repeated Codex trust approvals for the same command. Preserve the hook intent conservatively. Claude and Codex PreToolUse hooks are similar but not full parity: permission decisions, input rewriting, context injection, matcher names, and tool coverage can differ. Do not pretend unsupported target behavior is enforceable; document unsupported semantics in the target config comment or adjacent note if needed.

After editing the target config, validate that the target hook config parses. If parsing fails, fix only the affected target hook/config file and reparse. Stop when the migrated hook parses cleanly or when the migration is blocked by unsupported target semantics.
",
        source = first.frontend.display_name(),
        target = target.display_name(),
        source_path = display_path(&first.source_path),
        target_path = target_path,
        catch_all = CATCH_ALL_MATCHER,
        matchers = join_matchers(&matchers),
        command = command
    )
}

pub(in crate::hook_health) fn validation_prompt_actions(
    report: &HookHealthReport,
) -> Vec<RepairAction> {
    report
        .claude
        .parse_errors
        .iter()
        .chain(report.codex.parse_errors.iter())
        .map(|error| RepairAction::ValidationPrompt {
            frontend: error.frontend,
            config_path: error.path.clone(),
            prompt: validation_prompt(error),
        })
        .collect()
}

pub(in crate::hook_health) fn validation_prompt(error: &HookConfigError) -> String {
    format!(
        "\
You are fixing a malformed {frontend} hook config.

Config file: {path}
Parser error: {error}

Fix only this hook config file. The expected PreToolUse shape is a JSON object with a `hooks` object containing a `PreToolUse` array. Each array item should include a matcher/tool pattern and a `hooks` handler array. Reparse the file after editing and stop when it parses cleanly.
",
        frontend = error.frontend.config_name(),
        path = display_path(&error.path),
        error = error.error
    )
}

pub(in crate::hook_health) fn run_validation_followups(
    args: &Args,
    selected_backend: Backend,
    report: &HookHealthReport,
) -> i32 {
    let mut current = report.clone();
    for attempt in 0..3 {
        let prompts = validation_prompt_actions(&current)
            .into_iter()
            .filter_map(|action| match action {
                RepairAction::ValidationPrompt { prompt, .. } => Some(prompt),
                _ => None,
            })
            .collect::<Vec<_>>();
        if prompts.is_empty() {
            return 0;
        }
        for prompt in prompts {
            eprintln!(
                "[clud] running hook validation follow-up prompt {}/3",
                attempt + 1
            );
            match run_backend_prompt(args, selected_backend, prompt) {
                Ok(0) => {}
                Ok(code) => return code,
                Err(error) => {
                    eprintln!("[clud] error: {error}");
                    return 1;
                }
            }
        }
        current = inspect_current();
    }
    eprintln!("[clud] hook config still fails to parse after validation follow-up prompts");
    1
}

pub(in crate::hook_health) fn run_backend_prompt(
    args: &Args,
    selected_backend: Backend,
    prompt: String,
) -> Result<i32, String> {
    let backend_path = backend::find_backend(selected_backend)
        .map(|path| path.to_string_lossy().to_string())
        .ok_or_else(|| {
            format!(
                "{} not found on PATH; cannot run hook migration prompt",
                selected_backend.executable_name()
            )
        })?;
    let launch_args = Args {
        prompt: Some(prompt),
        message: None,
        continue_session: false,
        resume: None,
        claude: matches!(selected_backend, Backend::Claude),
        codex: matches!(selected_backend, Backend::Codex),
        subprocess: true,
        pty: false,
        graphics: crate::graphics::GraphicsMode::Off,
        graphics_image: None,
        demo_gfx_sixel: false,
        model: args.model.clone(),
        safe: args.safe,
        dry_run: false,
        detach: false,
        detachable: false,
        session_name: None,
        transcript: None,
        backlog_size: None,
        verbose: args.verbose,
        no_dnd: true,
        clean_worktrees: false,
        fix_hooks: false,
        no_fix_hooks: false,
        stale_after: "1d".to_string(),
        yes: false,
        force: false,
        experimental_daemon_centralized: false,
        daemon_state_dir: None,
        daemon_mode: None,
        no_daemon: true,
        command: None,
        passthrough: args.passthrough.clone(),
    };
    let plan = command::build_launch_plan(&launch_args, selected_backend, &backend_path);
    let config = ProcessConfig {
        command: subprocess::command_spec_for_subprocess(plan.command),
        cwd: plan.cwd.map(PathBuf::from),
        env: None,
        capture: false,
        stderr_mode: StderrMode::Stdout,
        creationflags: None,
        create_process_group: false,
        stdin_mode: StdinMode::Inherit,
        nice: None,
    };
    let process = NativeProcess::new(config);
    process
        .start()
        .map_err(|error| format!("failed to start backend prompt: {error}"))?;
    process
        .wait(None)
        .map_err(|error| format!("waiting for backend prompt: {error}"))
}
