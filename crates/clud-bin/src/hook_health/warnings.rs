use std::collections::BTreeSet;

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
