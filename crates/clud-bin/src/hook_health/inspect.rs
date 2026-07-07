use std::collections::{BTreeSet, HashSet};
use std::fs;
use std::path::{Path, PathBuf};

use serde_json::Value as JsonValue;
use toml_edit::{DocumentMut, Item};

use super::codex_trust::{
    codex_hook_state_key, codex_project_key, has_trusted_hook_state, is_extended_key_for,
};
use super::types::{
    CodexProjectTrust, FrontendHookSummary, HookConfigError, HookFrontend, HookHealthReport,
    NormalizedHook,
};
use super::utils::{display_path, same_path};
use super::warnings::build_warnings;
use super::{
    CODEX_PRE_TOOL_USE_STATE, CURRENT_CODEX_HOOKS_FEATURE, FIX_HINT, LEGACY_CODEX_HOOKS_FEATURE,
    PRE_TOOL_USE,
};
use crate::loop_spec;

pub fn inspect_current() -> HookHealthReport {
    let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    let repo_root = loop_spec::git_root_from(&cwd);
    let home = hook_home_dir();
    inspect_paths(&repo_root, home.as_deref())
}

pub(in crate::hook_health) fn hook_home_dir() -> Option<PathBuf> {
    std::env::var_os("CLUD_HOOK_HOME")
        .map(PathBuf::from)
        .or_else(dirs::home_dir)
}

pub fn inspect_paths(repo_root: &Path, home: Option<&Path>) -> HookHealthReport {
    let claude = collect_claude(repo_root, home);
    let (codex, codex_project_trust) = collect_codex(repo_root, home);
    let codex_legacy_hook_feature_configs =
        collect_codex_legacy_hook_feature_configs(repo_root, home);
    let mut warnings = build_warnings(&claude, &codex);
    for config_path in &codex_legacy_hook_feature_configs {
        warnings.push(format!(
            "Codex config {} uses deprecated `[features].{LEGACY_CODEX_HOOKS_FEATURE}`; migrate it to `[features].{CURRENT_CODEX_HOOKS_FEATURE}`. {FIX_HINT}",
            display_path(config_path)
        ));
    }
    HookHealthReport {
        repo_root: repo_root.to_path_buf(),
        home: home.map(Path::to_path_buf),
        claude,
        codex,
        codex_project_trust,
        codex_legacy_hook_feature_configs,
        warnings,
    }
}

pub(in crate::hook_health) fn collect_claude(
    repo_root: &Path,
    home: Option<&Path>,
) -> FrontendHookSummary {
    let mut summary = FrontendHookSummary::new(HookFrontend::Claude);
    let mut paths = vec![
        repo_root.join(".claude").join("settings.json"),
        repo_root.join(".claude").join("settings.local.json"),
    ];
    if let Some(home) = home {
        paths.push(home.join(".claude").join("settings.json"));
    }

    for path in paths {
        if path.is_file() {
            let parsed = read_json_hooks(&path, HookFrontend::Claude, false);
            summary.hooks.extend(parsed.hooks);
            summary.warnings.extend(parsed.warnings);
            summary.parse_errors.extend(parsed.parse_errors);
        }
    }
    warn_on_claude_windows_stdin_bug(&mut summary);
    summary
}

pub(in crate::hook_health) fn collect_codex(
    repo_root: &Path,
    home: Option<&Path>,
) -> (FrontendHookSummary, Option<CodexProjectTrust>) {
    let mut summary = FrontendHookSummary::new(HookFrontend::Codex);
    let project_hooks = repo_root.join(".codex").join("hooks.json");
    let mut paths = vec![project_hooks.clone()];
    if let Some(home) = home {
        paths.push(home.join(".codex").join("hooks.json"));
    }

    for path in paths {
        if path.is_file() {
            let parsed = read_json_hooks(&path, HookFrontend::Codex, true);
            summary.hooks.extend(parsed.hooks);
            summary.warnings.extend(parsed.warnings);
            summary.parse_errors.extend(parsed.parse_errors);
        }
    }

    let project_trust = home.map(|home| inspect_codex_project_trust(repo_root, home));
    apply_codex_activity_state(&mut summary, &project_hooks, project_trust.as_ref(), home);
    warn_on_powershell_exit_code_risk(&mut summary);
    (summary, project_trust)
}

pub(in crate::hook_health) fn collect_codex_legacy_hook_feature_configs(
    repo_root: &Path,
    home: Option<&Path>,
) -> Vec<PathBuf> {
    let mut paths = Vec::new();
    if let Some(home) = home {
        paths.push(home.join(".codex").join("config.toml"));
    }
    paths.push(repo_root.join(".codex").join("config.toml"));

    let mut configs: Vec<PathBuf> = Vec::new();
    for path in paths {
        if path.is_file()
            && codex_config_has_legacy_hooks_feature(&path)
            && !configs.iter().any(|existing| same_path(existing, &path))
        {
            configs.push(path);
        }
    }
    configs
}

pub(in crate::hook_health) fn codex_config_has_legacy_hooks_feature(path: &Path) -> bool {
    let Ok(text) = fs::read_to_string(path) else {
        return false;
    };
    let Ok(document) = text.parse::<DocumentMut>() else {
        return false;
    };
    document
        .get("features")
        .and_then(|item| item.get(LEGACY_CODEX_HOOKS_FEATURE))
        .is_some()
}

pub(in crate::hook_health) fn apply_codex_activity_state(
    summary: &mut FrontendHookSummary,
    project_hooks: &Path,
    project_trust: Option<&CodexProjectTrust>,
    home: Option<&Path>,
) {
    let trusted_hook_keys = home
        .map(|home| trusted_codex_hook_state_keys(&home.join(".codex").join("config.toml")))
        .unwrap_or_default();

    let mut missing_trust_sources = BTreeSet::new();
    for hook in &mut summary.hooks {
        let mut active = true;
        if same_path(&hook.source_path, project_hooks) {
            let trusted_project = project_trust
                .map(|trust| trust.canonical_trusted)
                .unwrap_or(false);
            if !trusted_project {
                active = false;
            }
        }
        let state_key =
            codex_hook_state_key(&hook.source_path, hook.group_index, hook.handler_index);
        if !has_trusted_hook_state(&trusted_hook_keys, &state_key) {
            active = false;
            missing_trust_sources.insert(display_path(&hook.source_path));
        }
        hook.active = active;
    }

    if let Some(trust) = project_trust {
        let has_project_hooks = summary
            .hooks
            .iter()
            .any(|hook| same_path(&hook.source_path, project_hooks));
        if has_project_hooks && !trust.canonical_trusted {
            if trust.extended_trusted {
                summary.warnings.push(format!(
                    "Codex hooks exist in {}, but Codex may not discover them because this repo is trusted only under an extended `\\\\?\\...` path key. {FIX_HINT}",
                    display_path(project_hooks)
                ));
            } else {
                summary.warnings.push(format!(
                    "Codex hooks exist in {}, but Codex may not discover them because this repo is not trusted under its canonical path key `{}`. {FIX_HINT}",
                    display_path(project_hooks),
                    trust.canonical_key
                ));
            }
        }
    }

    for source in missing_trust_sources {
        summary.warnings.push(format!(
            "Codex PreToolUse hooks in {source} may need review/trust before they are active. Run `/hooks` in Codex and trust the hook entry."
        ));
    }
}

fn inspect_codex_project_trust(repo_root: &Path, home: &Path) -> CodexProjectTrust {
    let config_path = home.join(".codex").join("config.toml");
    let canonical_key = codex_project_key(repo_root);
    let mut trust = CodexProjectTrust {
        config_path: config_path.clone(),
        canonical_key: canonical_key.clone(),
        canonical_present: false,
        canonical_trusted: false,
        extended_trusted: false,
        parse_error: None,
    };

    let Ok(text) = fs::read_to_string(&config_path) else {
        return trust;
    };
    let document = match text.parse::<DocumentMut>() {
        Ok(document) => document,
        Err(error) => {
            trust.parse_error = Some(error.to_string());
            return trust;
        }
    };
    let Some(projects) = document.get("projects").and_then(Item::as_table) else {
        return trust;
    };
    for (key, item) in projects.iter() {
        let trusted = item
            .get("trust_level")
            .and_then(Item::as_str)
            .map(|level| level.eq_ignore_ascii_case("trusted"))
            .unwrap_or(false);
        if key == canonical_key {
            trust.canonical_present = true;
            trust.canonical_trusted = trusted;
        } else if is_extended_key_for(key, &canonical_key) && trusted {
            trust.extended_trusted = true;
        }
    }
    trust
}

fn trusted_codex_hook_state_keys(config_path: &Path) -> HashSet<String> {
    let Ok(text) = fs::read_to_string(config_path) else {
        return HashSet::new();
    };
    let Ok(document) = text.parse::<DocumentMut>() else {
        return HashSet::new();
    };
    let Some(state) = document
        .get("hooks")
        .and_then(|item| item.get("state"))
        .and_then(Item::as_table)
    else {
        return HashSet::new();
    };
    state
        .iter()
        .filter_map(|(key, item)| {
            let trusted = item.get("trusted_hash").and_then(Item::as_str).is_some();
            trusted.then(|| key.to_string())
        })
        .collect()
}

#[derive(Debug)]
pub(in crate::hook_health) struct ParsedHooks {
    pub(in crate::hook_health) hooks: Vec<NormalizedHook>,
    pub(in crate::hook_health) warnings: Vec<String>,
    pub(in crate::hook_health) parse_errors: Vec<HookConfigError>,
}

pub(in crate::hook_health) fn read_json_hooks(
    path: &Path,
    frontend: HookFrontend,
    warn_legacy_codex_shape: bool,
) -> ParsedHooks {
    let mut parsed = ParsedHooks {
        hooks: Vec::new(),
        warnings: Vec::new(),
        parse_errors: Vec::new(),
    };
    let text = match fs::read_to_string(path) {
        Ok(text) => text,
        Err(error) => {
            parsed.warnings.push(format!(
                "could not read {} hook config {}: {error}",
                frontend.config_name(),
                display_path(path)
            ));
            return parsed;
        }
    };
    let json: JsonValue = match serde_json::from_str(&text) {
        Ok(json) => json,
        Err(error) => {
            parsed.parse_errors.push(HookConfigError {
                frontend,
                path: path.to_path_buf(),
                error: error.to_string(),
            });
            parsed.warnings.push(format!(
                "could not parse {} hook config {}: {error}",
                frontend.config_name(),
                display_path(path)
            ));
            return parsed;
        }
    };

    if warn_legacy_codex_shape && json.get(PRE_TOOL_USE).is_some() {
        parsed.warnings.push(format!(
            "Codex hook config {} uses legacy root-level `{PRE_TOOL_USE}` shape that Codex ignores. {FIX_HINT}",
            display_path(path)
        ));
    }

    let Some(groups) = json
        .get("hooks")
        .and_then(|hooks| hooks.get(PRE_TOOL_USE))
        .or_else(|| {
            json.get("hooks")
                .and_then(|hooks| hooks.get(CODEX_PRE_TOOL_USE_STATE))
        })
        .and_then(JsonValue::as_array)
    else {
        return parsed;
    };

    for (group_index, group) in groups.iter().enumerate() {
        let matcher = group
            .get("matcher")
            .and_then(JsonValue::as_str)
            .map(str::trim)
            .filter(|matcher| !matcher.is_empty())
            .unwrap_or("*")
            .to_string();
        let handlers = group
            .get("hooks")
            .and_then(JsonValue::as_array)
            .cloned()
            .unwrap_or_else(|| vec![JsonValue::Null]);
        for (handler_index, handler) in handlers.iter().enumerate() {
            parsed.hooks.push(NormalizedHook {
                frontend,
                matcher: matcher.clone(),
                source_path: path.to_path_buf(),
                group_index,
                handler_index,
                command: handler
                    .get("command")
                    .and_then(JsonValue::as_str)
                    .map(ToOwned::to_owned),
                active: !matches!(frontend, HookFrontend::Codex),
            });
        }
    }
    parsed
}

pub(in crate::hook_health) fn warn_on_powershell_exit_code_risk(summary: &mut FrontendHookSummary) {
    if !cfg!(target_os = "windows") {
        return;
    }
    let mut risky_sources = BTreeSet::new();
    for hook in &summary.hooks {
        let Some(command) = hook.command.as_deref() else {
            continue;
        };
        let lower = command.to_ascii_lowercase();
        if (lower.contains(".cmd") || lower.contains(".bat")) && !lower.contains("$lastexitcode") {
            risky_sources.insert(display_path(&hook.source_path));
        }
    }
    for source in risky_sources {
        summary.warnings.push(format!(
            "Codex hook command in {source} uses a Windows batch wrapper without explicit `$LASTEXITCODE` propagation; a blocking hook may fail open."
        ));
    }
}

fn warn_on_claude_windows_stdin_bug(summary: &mut FrontendHookSummary) {
    if !cfg!(target_os = "windows") || summary.hooks.is_empty() {
        return;
    }

    let sources = summary
        .hooks
        .iter()
        .map(|hook| display_path(&hook.source_path))
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect::<Vec<_>>()
        .join(", ");
    summary.warnings.push(format!(
        "Claude Code Windows hooks in {sources} may hit the upstream hook stdin bug cluster \
         (https://github.com/anthropics/claude-code/issues/53177, duplicate root \
         https://github.com/anthropics/claude-code/issues/36156): hook timeout with Python \
         blocked in `sys.stdin.read()` / `json.load(sys.stdin)` means the hook is probably \
         waiting for stdin EOF, while a clud policy denial exits deterministically with deny \
         output / exit code 2. Workaround: in Claude settings, set \
         `env.CLAUDE_CODE_GIT_BASH_PATH` to Git for Windows `bin\\bash.exe`, not \
         `git-bash.exe`; locate it with `where bash`."
    ));
}
