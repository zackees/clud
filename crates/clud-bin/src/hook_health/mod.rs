//! Lightweight Claude/Codex `PreToolUse` hook parity diagnostics and repair.
//!
//! The launch path only warns. `--fix-hooks` is the explicit, repo-scoped
//! remediation path and is the only mode that writes deterministic config
//! changes or asks a backend agent to perform semantic hook translation.

mod codex_trust;
mod inspect;
mod prompts;
mod repairs;
mod types;
mod utils;
mod warnings;

use crate::args::{Args, Command as CliCommand};
use crate::backend::Backend;

pub use codex_trust::codex_project_key;
pub use inspect::{inspect_current, inspect_paths};
pub use repairs::plan_repairs;
pub use types::{
    CodexProjectTrust, DeterministicRepairError, FrontendHookSummary, HookConfigError,
    HookFrontend, HookHealthReport, NormalizedHook, RepairAction,
};

use inspect::inspect_current as inspect_current_impl;
use prompts::{run_backend_prompt, run_validation_followups};
use repairs::{apply_deterministic_repairs, deterministic_repair_actions};
use warnings::{print_dry_run_plan, print_report_warnings};

#[cfg(test)]
use codex_trust::{add_codex_project_trust, is_extended_key_for, migrate_codex_hooks_feature_flag};
#[cfg(test)]
use std::collections::BTreeSet;
#[cfg(test)]
use std::fs;
#[cfg(test)]
use std::path::{Path, PathBuf};

pub(in crate::hook_health) const PRE_TOOL_USE: &str = "PreToolUse";
pub(in crate::hook_health) const CODEX_PRE_TOOL_USE_STATE: &str = "pre_tool_use";
pub(in crate::hook_health) const CATCH_ALL_MATCHER: &str = "*";
pub(in crate::hook_health) const FIX_HINT: &str = "Run `clud --fix-hooks`.";
pub(in crate::hook_health) const LEGACY_CODEX_HOOKS_FEATURE: &str = "codex_hooks";
pub(in crate::hook_health) const CURRENT_CODEX_HOOKS_FEATURE: &str = "hooks";

pub fn should_check_launch(args: &Args) -> bool {
    if args.fix_hooks || args.clean_worktrees {
        return false;
    }
    if !args.codex {
        return false;
    }
    matches!(
        args.command,
        None | Some(CliCommand::Loop { .. })
            | Some(CliCommand::Up { .. })
            | Some(CliCommand::Rebase)
            | Some(CliCommand::Fix { .. })
    )
}

pub fn emit_launch_warnings() {
    let report = inspect_current_impl();
    for warning in report.warnings {
        eprintln!("[clud] warning: {warning}");
    }
}

pub fn apply_default_repairs() -> Result<usize, DeterministicRepairError> {
    let report = inspect_current_impl();
    apply_deterministic_repairs(deterministic_repair_actions(&report))
}

pub fn run_fix_hooks(args: &Args, selected_backend: Backend) -> i32 {
    let mut report = inspect_current_impl();
    print_report_warnings(&report);

    if args.dry_run {
        print_dry_run_plan(&report);
        return 0;
    }

    if let Err(error) = apply_deterministic_repairs(deterministic_repair_actions(&report)) {
        eprintln!("[clud] error: failed to update {error}");
        return 1;
    }

    report = inspect_current_impl();
    let prompt_actions = plan_repairs(&report)
        .into_iter()
        .filter_map(|action| match action {
            RepairAction::BackendPrompt { prompt, .. } => Some(prompt),
            RepairAction::ValidationPrompt { prompt, .. } => Some(prompt),
            RepairAction::AddCodexProjectTrust { .. }
            | RepairAction::MigrateCodexHooksFeatureFlag { .. }
            | RepairAction::NormalizeCodexBatchHookExitCode { .. } => None,
        })
        .collect::<Vec<_>>();

    for (idx, prompt) in prompt_actions.iter().enumerate() {
        eprintln!(
            "[clud] running hook migration prompt {}/{}",
            idx + 1,
            prompt_actions.len()
        );
        let exit_code = match run_backend_prompt(args, selected_backend, prompt.clone()) {
            Ok(code) => code,
            Err(error) => {
                eprintln!("[clud] error: {error}");
                return 1;
            }
        };
        if exit_code != 0 {
            eprintln!("[clud] hook migration prompt exited with {exit_code}");
            return exit_code;
        }
        report = inspect_current_impl();
        let validation_exit = run_validation_followups(args, selected_backend, &report);
        if validation_exit != 0 {
            return validation_exit;
        }
        print_report_warnings(&report);
    }

    let final_report = inspect_current_impl();
    if final_report.warnings.is_empty() {
        eprintln!("[clud] hook health check is clean");
    } else {
        eprintln!("[clud] remaining hook health warnings:");
        print_report_warnings(&final_report);
    }
    0
}

#[cfg(test)]
#[path = "../hook_health_tests.rs"]
mod tests;
