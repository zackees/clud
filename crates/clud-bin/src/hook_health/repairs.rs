use std::collections::{BTreeMap, BTreeSet};
use std::fs;

use super::codex_trust::{add_codex_project_trust, migrate_codex_hooks_feature_flag};
use super::prompts::{catch_all_migration_prompt, migration_prompt, validation_prompt_actions};
use super::types::{
    DeterministicRepairError, HookFrontend, HookHealthReport, NormalizedHook, RepairAction,
};
use super::utils::{display_path, same_path};
use super::{CATCH_ALL_MATCHER, CURRENT_CODEX_HOOKS_FEATURE, LEGACY_CODEX_HOOKS_FEATURE};

pub fn plan_repairs(report: &HookHealthReport) -> Vec<RepairAction> {
    let mut actions = Vec::new();
    actions.extend(validation_prompt_actions(report));
    actions.extend(
        report
            .codex_legacy_hook_feature_configs
            .iter()
            .cloned()
            .map(|config_path| RepairAction::MigrateCodexHooksFeatureFlag { config_path }),
    );

    let project_hooks = report.repo_root.join(".codex").join("hooks.json");
    let has_project_codex_hooks = report
        .codex
        .hooks
        .iter()
        .any(|hook| same_path(&hook.source_path, &project_hooks));
    if let Some(trust) = &report.codex_project_trust {
        if has_project_codex_hooks && !trust.canonical_present && trust.parse_error.is_none() {
            actions.push(RepairAction::AddCodexProjectTrust {
                config_path: trust.config_path.clone(),
                project_key: trust.canonical_key.clone(),
            });
        }
    }

    let mut risky_codex_hook_paths = BTreeSet::new();
    for hook in &report.codex.hooks {
        if hook_command_needs_last_exit_code(hook.command.as_deref()) {
            risky_codex_hook_paths.insert(hook.source_path.clone());
        }
    }
    actions.extend(
        risky_codex_hook_paths
            .into_iter()
            .map(|hooks_path| RepairAction::NormalizeCodexBatchHookExitCode { hooks_path }),
    );

    let claude_by_matcher = report.claude.hook_by_matcher();
    let codex_by_matcher = report.codex.hook_by_matcher();
    plan_backend_prompts(
        &mut actions,
        HookFrontend::Claude,
        HookFrontend::Codex,
        &claude_by_matcher,
        &codex_by_matcher,
    );
    plan_backend_prompts(
        &mut actions,
        HookFrontend::Codex,
        HookFrontend::Claude,
        &codex_by_matcher,
        &claude_by_matcher,
    );
    actions
}

fn plan_backend_prompts(
    actions: &mut Vec<RepairAction>,
    source: HookFrontend,
    target: HookFrontend,
    source_by_matcher: &BTreeMap<String, NormalizedHook>,
    target_by_matcher: &BTreeMap<String, NormalizedHook>,
) {
    let missing_hooks = source_by_matcher
        .iter()
        .filter_map(|(matcher, hook)| {
            (!matcher_is_covered_by(source, target_by_matcher, matcher)).then_some(hook)
        })
        .collect::<Vec<_>>();

    if target == HookFrontend::Codex {
        let mut handled = BTreeSet::new();
        for hooks in codex_catch_all_groups(&missing_hooks).values() {
            if hooks.len() < 2 {
                continue;
            }
            let first = hooks[0];
            actions.push(RepairAction::BackendPrompt {
                source,
                target,
                matcher: CATCH_ALL_MATCHER.to_string(),
                source_path: first.source_path.clone(),
                prompt: catch_all_migration_prompt(hooks, target),
            });
            handled.extend(hooks.iter().map(|hook| hook.matcher.clone()));
        }

        for hook in missing_hooks {
            if handled.contains(&hook.matcher) {
                continue;
            }
            actions.push(RepairAction::BackendPrompt {
                source,
                target,
                matcher: hook.matcher.clone(),
                source_path: hook.source_path.clone(),
                prompt: migration_prompt(hook, target),
            });
        }
        return;
    }

    for hook in missing_hooks {
        actions.push(RepairAction::BackendPrompt {
            source,
            target,
            matcher: hook.matcher.clone(),
            source_path: hook.source_path.clone(),
            prompt: migration_prompt(hook, target),
        });
    }
}

fn matcher_is_covered_by(
    source: HookFrontend,
    target_by_matcher: &BTreeMap<String, NormalizedHook>,
    source_matcher: &str,
) -> bool {
    target_by_matcher.contains_key(source_matcher)
        || (source_matcher != CATCH_ALL_MATCHER
            && target_by_matcher.contains_key(CATCH_ALL_MATCHER))
        || (source == HookFrontend::Codex
            && source_matcher == CATCH_ALL_MATCHER
            && !target_by_matcher.is_empty())
}

fn codex_catch_all_groups<'a>(
    hooks: &[&'a NormalizedHook],
) -> BTreeMap<String, Vec<&'a NormalizedHook>> {
    let mut groups = BTreeMap::new();
    for hook in hooks {
        if hook.matcher == CATCH_ALL_MATCHER {
            continue;
        }
        let Some(command) = hook.command.as_deref().map(str::trim) else {
            continue;
        };
        if command.is_empty() {
            continue;
        }
        groups
            .entry(command.to_string())
            .or_insert_with(Vec::new)
            .push(*hook);
    }
    groups
}

pub(in crate::hook_health) fn deterministic_repair_actions(
    report: &HookHealthReport,
) -> Vec<RepairAction> {
    plan_repairs(report)
        .into_iter()
        .filter(|action| {
            matches!(
                action,
                RepairAction::AddCodexProjectTrust { .. }
                    | RepairAction::MigrateCodexHooksFeatureFlag { .. }
                    | RepairAction::NormalizeCodexBatchHookExitCode { .. }
            )
        })
        .collect()
}

pub(in crate::hook_health) fn apply_deterministic_repairs(
    actions: Vec<RepairAction>,
) -> Result<usize, DeterministicRepairError> {
    let mut applied = 0;
    for action in actions {
        match action {
            RepairAction::AddCodexProjectTrust {
                config_path,
                project_key,
            } => {
                add_codex_project_trust(&config_path, &project_key).map_err(|error| {
                    DeterministicRepairError {
                        path: config_path.clone(),
                        error,
                    }
                })?;
                applied += 1;
                eprintln!(
                    "[clud] added Codex project trust entry `{project_key}` to {}",
                    display_path(&config_path)
                );
            }
            RepairAction::MigrateCodexHooksFeatureFlag { config_path } => {
                migrate_codex_hooks_feature_flag(&config_path).map_err(|error| {
                    DeterministicRepairError {
                        path: config_path.clone(),
                        error,
                    }
                })?;
                applied += 1;
                eprintln!(
                    "[clud] migrated deprecated Codex `{LEGACY_CODEX_HOOKS_FEATURE}` feature flag to `{CURRENT_CODEX_HOOKS_FEATURE}` in {}",
                    display_path(&config_path)
                );
            }
            RepairAction::NormalizeCodexBatchHookExitCode { hooks_path } => {
                normalize_codex_batch_hook_exit_codes(&hooks_path).map_err(|error| {
                    DeterministicRepairError {
                        path: hooks_path.clone(),
                        error,
                    }
                })?;
                applied += 1;
                eprintln!(
                    "[clud] updated Codex hook batch wrapper exit-code propagation in {}",
                    display_path(&hooks_path)
                );
            }
            RepairAction::BackendPrompt { .. } | RepairAction::ValidationPrompt { .. } => {}
        }
    }
    Ok(applied)
}

pub(in crate::hook_health) fn hook_command_needs_last_exit_code(command: Option<&str>) -> bool {
    if !cfg!(target_os = "windows") {
        return false;
    }
    let Some(command) = command else {
        return false;
    };
    let lower = command.to_ascii_lowercase();
    (lower.contains(".cmd") || lower.contains(".bat")) && !lower.contains("$lastexitcode")
}

fn normalize_codex_batch_hook_exit_codes(path: &std::path::Path) -> std::io::Result<()> {
    let text = fs::read_to_string(path)?;
    let mut json: serde_json::Value = serde_json::from_str(&text)
        .map_err(|error| std::io::Error::new(std::io::ErrorKind::InvalidData, error))?;
    let mut changed = 0usize;
    normalize_value(&mut json, &mut changed);
    if changed == 0 {
        return Ok(());
    }
    let mut body = serde_json::to_string_pretty(&json).map_err(std::io::Error::other)?;
    body.push('\n');
    fs::write(path, body)
}

fn normalize_value(value: &mut serde_json::Value, changed: &mut usize) {
    match value {
        serde_json::Value::Object(map) => {
            if let Some(command) = map
                .get("command")
                .and_then(serde_json::Value::as_str)
                .map(str::to_string)
            {
                if hook_command_needs_last_exit_code(Some(command.as_str())) {
                    map.insert(
                        "command".to_string(),
                        serde_json::Value::String(format!(
                            "{}; exit $LASTEXITCODE",
                            command.trim_end()
                        )),
                    );
                    *changed += 1;
                    return;
                }
            }
            for value in map.values_mut() {
                normalize_value(value, changed);
            }
        }
        serde_json::Value::Array(values) => {
            for value in values {
                normalize_value(value, changed);
            }
        }
        _ => {}
    }
}
