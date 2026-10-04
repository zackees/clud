use std::collections::{BTreeMap, BTreeSet};

use super::repairs::plan_repairs;
use super::types::{FrontendHookSummary, HookHealthReport, RepairAction};
use super::utils::{display_path, join_matchers};
use super::{CATCH_ALL_MATCHER, CURRENT_CODEX_HOOKS_FEATURE, FIX_HINT, LEGACY_CODEX_HOOKS_FEATURE};

pub(in crate::hook_health) fn build_warnings(
    claude: &FrontendHookSummary,
    codex: &FrontendHookSummary,
) -> Vec<String> {
    let mut warnings = Vec::new();
    warnings.extend(claude.warnings.iter().cloned());
    warnings.extend(codex.warnings.iter().cloned());
    warnings.extend(uv_run_hook_warnings(claude));
    warnings.extend(uv_run_hook_warnings(codex));

    let claude_active = claude.active_hooks();
    let codex_active = codex.active_hooks();
    if !claude_active.is_empty() && codex_active.is_empty() {
        warnings.push(format!(
            "Claude PreToolUse hooks exist, but Codex PreToolUse hooks are missing or inactive. {FIX_HINT}"
        ));
    }
    if !codex.hooks.is_empty() && claude_active.is_empty() {
        warnings.push(format!(
            "Codex PreToolUse hooks exist, but Claude PreToolUse hooks are missing or inactive. {FIX_HINT}"
        ));
    }

    let claude_matchers = claude.active_matchers();
    let codex_matchers = codex.active_matchers();
    if !claude_matchers.is_empty()
        && !codex_matchers.is_empty()
        && !matcher_sets_are_compatible(&claude_matchers, &codex_matchers)
    {
        warnings.push(format!(
            "Claude and Codex PreToolUse hook matchers differ (Claude: {}; Codex: {}). {FIX_HINT}",
            join_matchers(&claude_matchers),
            join_matchers(&codex_matchers)
        ));
    }
    warnings
}

pub(in crate::hook_health) fn uv_run_hook_warnings(summary: &FrontendHookSummary) -> Vec<String> {
    let mut by_source = BTreeMap::new();
    for hook in summary.active_hooks() {
        if hook
            .command
            .as_deref()
            .is_some_and(uv_run_without_safe_flag)
        {
            *by_source.entry(&hook.source_path).or_insert(0usize) += 1;
        }
    }
    by_source
        .into_iter()
        .map(|(source, count)| {
            format!(
                "{count} {} PreToolUse hook(s) in {} use `uv run` without `--no-sync` or `--no-project`. A dependency pin changed in the working tree can break every tool call. Use `uv run --no-sync` with an installed environment, or `--no-project` for stdlib-only scripts. `--frozen` alone still syncs.",
                summary.frontend.display_name(),
                display_path(source)
            )
        })
        .collect()
}

pub(super) fn uv_run_without_safe_flag(command: &str) -> bool {
    let words = shell_words::split(command)
        .unwrap_or_else(|_| command.split_whitespace().map(str::to_string).collect());
    words.windows(2).enumerate().any(|(index, pair)| {
        if !matches!(pair[0].as_str(), "uv" | "uv.exe") || pair[1] != "run" {
            return false;
        }
        let mut i = index + 2;
        let mut no_sync = false;
        while let Some(word) = words.get(i) {
            if word == "--" || !word.starts_with('-') {
                break;
            }
            if matches!(
                word.as_str(),
                "--script" | "--module" | "--gui-script" | "-m" | "-s"
            ) || word.starts_with("--script=")
                || word.starts_with("--module=")
                || word.starts_with("--gui-script=")
            {
                break;
            }
            if matches!(word.as_str(), "--no-sync" | "--no-project") {
                no_sync = true;
            }
            let takes_value = !word.contains('=')
                && (crate::block_bad_cmd::UV_RUN_OPTIONS_WITH_VALUE.contains(&word.as_str())
                    || crate::block_bad_cmd::UV_RUN_SHORT_OPTIONS_WITH_VALUE
                        .contains(&word.as_str()));
            i += if takes_value { 2 } else { 1 };
        }
        !no_sync
    })
}

pub(in crate::hook_health) fn matcher_sets_are_compatible(
    claude_matchers: &BTreeSet<String>,
    codex_matchers: &BTreeSet<String>,
) -> bool {
    claude_matchers == codex_matchers || codex_matchers.contains(CATCH_ALL_MATCHER)
}

pub(in crate::hook_health) fn print_report_warnings(report: &HookHealthReport) {
    for warning in &report.warnings {
        eprintln!("[clud] warning: {warning}");
    }
}

pub(in crate::hook_health) fn print_dry_run_plan(report: &HookHealthReport) {
    let actions = plan_repairs(report);
    println!("hook health dry-run");
    if report.warnings.is_empty() {
        println!("warnings: none");
    } else {
        println!("warnings:");
        for warning in &report.warnings {
            println!("- {warning}");
        }
    }
    if actions.is_empty() {
        println!("repair actions: none");
        return;
    }
    println!("repair actions:");
    for action in actions {
        match action {
            RepairAction::AddCodexProjectTrust {
                config_path,
                project_key,
            } => println!(
                "- add Codex project trust key `{project_key}` to {}",
                display_path(&config_path)
            ),
            RepairAction::MigrateCodexHooksFeatureFlag { config_path } => println!(
                "- migrate deprecated Codex `[features].{LEGACY_CODEX_HOOKS_FEATURE}` to `[features].{CURRENT_CODEX_HOOKS_FEATURE}` in {}",
                display_path(&config_path)
            ),
            RepairAction::NormalizeCodexBatchHookExitCode { hooks_path } => println!(
                "- add explicit `$LASTEXITCODE` propagation to Codex batch hook commands in {}",
                display_path(&hooks_path)
            ),
            RepairAction::BackendPrompt {
                source,
                target,
                matcher,
                source_path,
                ..
            } => println!(
                "- run one {source}->{target} migration prompt for matcher `{matcher}` from {}",
                display_path(&source_path)
            ),
            RepairAction::ValidationPrompt {
                frontend,
                config_path,
                ..
            } => println!(
                "- run one {} validation prompt for {}",
                frontend,
                display_path(&config_path)
            ),
        }
    }
}
