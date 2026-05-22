//! Lightweight Claude/Codex `PreToolUse` hook parity diagnostics and repair.
//!
//! The launch path only warns. `--fix-hooks` is the explicit, repo-scoped
//! remediation path and is the only mode that writes deterministic config
//! changes or asks a backend agent to perform semantic hook translation.

use crate::args::{Args, Command as CliCommand};
use crate::backend::{self, Backend};
use crate::command;
use crate::loop_spec;
use crate::subprocess;
use running_process_core::{NativeProcess, ProcessConfig, StderrMode, StdinMode};
use serde_json::Value as JsonValue;
use std::collections::{BTreeMap, BTreeSet, HashSet};
use std::fmt;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use toml_edit::{table, value, DocumentMut, Item};

const PRE_TOOL_USE: &str = "PreToolUse";
const CODEX_PRE_TOOL_USE_STATE: &str = "pre_tool_use";
const FIX_HINT: &str = "Run `clud --fix-hooks`.";

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum HookFrontend {
    Claude,
    Codex,
}

impl HookFrontend {
    fn display_name(self) -> &'static str {
        match self {
            HookFrontend::Claude => "Claude",
            HookFrontend::Codex => "Codex",
        }
    }

    fn config_name(self) -> &'static str {
        match self {
            HookFrontend::Claude => "Claude Code",
            HookFrontend::Codex => "Codex",
        }
    }

    fn lower(self) -> &'static str {
        match self {
            HookFrontend::Claude => "claude",
            HookFrontend::Codex => "codex",
        }
    }
}

impl fmt::Display for HookFrontend {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.lower())
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NormalizedHook {
    pub frontend: HookFrontend,
    pub matcher: String,
    pub source_path: PathBuf,
    pub group_index: usize,
    pub handler_index: usize,
    pub command: Option<String>,
    pub active: bool,
}

#[derive(Debug, Clone)]
pub struct FrontendHookSummary {
    pub frontend: HookFrontend,
    pub hooks: Vec<NormalizedHook>,
    pub warnings: Vec<String>,
    pub parse_errors: Vec<HookConfigError>,
}

impl FrontendHookSummary {
    fn new(frontend: HookFrontend) -> Self {
        Self {
            frontend,
            hooks: Vec::new(),
            warnings: Vec::new(),
            parse_errors: Vec::new(),
        }
    }

    fn active_hooks(&self) -> Vec<&NormalizedHook> {
        self.hooks.iter().filter(|hook| hook.active).collect()
    }

    #[cfg(test)]
    fn all_matchers(&self) -> BTreeSet<String> {
        self.hooks
            .iter()
            .map(|hook| hook.matcher.clone())
            .collect::<BTreeSet<_>>()
    }

    fn active_matchers(&self) -> BTreeSet<String> {
        self.hooks
            .iter()
            .filter(|hook| hook.active)
            .map(|hook| hook.matcher.clone())
            .collect::<BTreeSet<_>>()
    }

    fn hook_by_matcher(&self) -> BTreeMap<String, NormalizedHook> {
        let mut hooks = BTreeMap::new();
        for hook in &self.hooks {
            hooks
                .entry(hook.matcher.clone())
                .or_insert_with(|| hook.clone());
        }
        hooks
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HookConfigError {
    pub frontend: HookFrontend,
    pub path: PathBuf,
    pub error: String,
}

#[derive(Debug, Clone, Default)]
pub struct CodexProjectTrust {
    pub config_path: PathBuf,
    pub canonical_key: String,
    pub canonical_present: bool,
    pub canonical_trusted: bool,
    pub extended_trusted: bool,
    pub parse_error: Option<String>,
}

#[derive(Debug, Clone)]
pub struct HookHealthReport {
    pub repo_root: PathBuf,
    pub home: Option<PathBuf>,
    pub claude: FrontendHookSummary,
    pub codex: FrontendHookSummary,
    pub codex_project_trust: Option<CodexProjectTrust>,
    pub warnings: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RepairAction {
    AddCodexProjectTrust {
        config_path: PathBuf,
        project_key: String,
    },
    BackendPrompt {
        source: HookFrontend,
        target: HookFrontend,
        matcher: String,
        source_path: PathBuf,
        prompt: String,
    },
    ValidationPrompt {
        frontend: HookFrontend,
        config_path: PathBuf,
        prompt: String,
    },
}

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
    let report = inspect_current();
    for warning in report.warnings {
        eprintln!("[clud] warning: {warning}");
    }
}

pub fn run_fix_hooks(args: &Args, selected_backend: Backend) -> i32 {
    let mut report = inspect_current();
    print_report_warnings(&report);

    if args.dry_run {
        print_dry_run_plan(&report);
        return 0;
    }

    let deterministic = plan_repairs(&report)
        .into_iter()
        .filter_map(|action| match action {
            RepairAction::AddCodexProjectTrust {
                config_path,
                project_key,
            } => Some((config_path, project_key)),
            RepairAction::BackendPrompt { .. } | RepairAction::ValidationPrompt { .. } => None,
        })
        .collect::<Vec<_>>();

    for (config_path, project_key) in deterministic {
        match add_codex_project_trust(&config_path, &project_key) {
            Ok(()) => eprintln!(
                "[clud] added Codex project trust entry `{project_key}` to {}",
                display_path(&config_path)
            ),
            Err(error) => {
                eprintln!(
                    "[clud] error: failed to update {}: {error}",
                    display_path(&config_path)
                );
                return 1;
            }
        }
    }

    report = inspect_current();
    let prompt_actions = plan_repairs(&report)
        .into_iter()
        .filter_map(|action| match action {
            RepairAction::BackendPrompt { prompt, .. } => Some(prompt),
            RepairAction::ValidationPrompt { prompt, .. } => Some(prompt),
            RepairAction::AddCodexProjectTrust { .. } => None,
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
        report = inspect_current();
        let validation_exit = run_validation_followups(args, selected_backend, &report);
        if validation_exit != 0 {
            return validation_exit;
        }
        print_report_warnings(&report);
    }

    let final_report = inspect_current();
    if final_report.warnings.is_empty() {
        eprintln!("[clud] hook health check is clean");
    } else {
        eprintln!("[clud] remaining hook health warnings:");
        print_report_warnings(&final_report);
    }
    0
}

pub fn inspect_current() -> HookHealthReport {
    let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    let repo_root = loop_spec::git_root_from(&cwd);
    let home = hook_home_dir();
    inspect_paths(&repo_root, home.as_deref())
}

fn hook_home_dir() -> Option<PathBuf> {
    std::env::var_os("CLUD_HOOK_HOME")
        .map(PathBuf::from)
        .or_else(dirs::home_dir)
}

pub fn inspect_paths(repo_root: &Path, home: Option<&Path>) -> HookHealthReport {
    let claude = collect_claude(repo_root, home);
    let (codex, codex_project_trust) = collect_codex(repo_root, home);
    let warnings = build_warnings(&claude, &codex);
    HookHealthReport {
        repo_root: repo_root.to_path_buf(),
        home: home.map(Path::to_path_buf),
        claude,
        codex,
        codex_project_trust,
        warnings,
    }
}

pub fn plan_repairs(report: &HookHealthReport) -> Vec<RepairAction> {
    let mut actions = Vec::new();
    actions.extend(validation_prompt_actions(report));

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

    let claude_by_matcher = report.claude.hook_by_matcher();
    let codex_by_matcher = report.codex.hook_by_matcher();
    for (matcher, hook) in &claude_by_matcher {
        if !codex_by_matcher.contains_key(matcher) {
            actions.push(RepairAction::BackendPrompt {
                source: HookFrontend::Claude,
                target: HookFrontend::Codex,
                matcher: matcher.clone(),
                source_path: hook.source_path.clone(),
                prompt: migration_prompt(hook, HookFrontend::Codex),
            });
        }
    }
    for (matcher, hook) in &codex_by_matcher {
        if !claude_by_matcher.contains_key(matcher) {
            actions.push(RepairAction::BackendPrompt {
                source: HookFrontend::Codex,
                target: HookFrontend::Claude,
                matcher: matcher.clone(),
                source_path: hook.source_path.clone(),
                prompt: migration_prompt(hook, HookFrontend::Claude),
            });
        }
    }
    actions
}

fn collect_claude(repo_root: &Path, home: Option<&Path>) -> FrontendHookSummary {
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
    summary
}

fn collect_codex(
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

fn apply_codex_activity_state(
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

fn build_warnings(claude: &FrontendHookSummary, codex: &FrontendHookSummary) -> Vec<String> {
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
        && claude_matchers != codex_matchers
    {
        warnings.push(format!(
            "Claude and Codex PreToolUse hook matchers differ (Claude: {}; Codex: {}). {FIX_HINT}",
            join_matchers(&claude_matchers),
            join_matchers(&codex_matchers)
        ));
    }
    warnings
}

#[derive(Debug)]
struct ParsedHooks {
    hooks: Vec<NormalizedHook>,
    warnings: Vec<String>,
    parse_errors: Vec<HookConfigError>,
}

fn read_json_hooks(
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

fn warn_on_powershell_exit_code_risk(summary: &mut FrontendHookSummary) {
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

fn migration_prompt(hook: &NormalizedHook, target: HookFrontend) -> String {
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

fn validation_prompt_actions(report: &HookHealthReport) -> Vec<RepairAction> {
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

fn validation_prompt(error: &HookConfigError) -> String {
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

fn run_validation_followups(
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

fn run_backend_prompt(
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
        model: args.model.clone(),
        safe: args.safe,
        dry_run: false,
        detach: false,
        detachable: false,
        session_name: None,
        backlog_size: None,
        verbose: args.verbose,
        no_dnd: true,
        clean_worktrees: false,
        fix_hooks: false,
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

fn print_report_warnings(report: &HookHealthReport) {
    for warning in &report.warnings {
        eprintln!("[clud] warning: {warning}");
    }
}

fn print_dry_run_plan(report: &HookHealthReport) {
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

fn add_codex_project_trust(config_path: &Path, project_key: &str) -> io::Result<()> {
    let text = fs::read_to_string(config_path).unwrap_or_default();
    let mut document = if text.trim().is_empty() {
        DocumentMut::new()
    } else {
        text.parse::<DocumentMut>()
            .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error.to_string()))?
    };
    if document.get("projects").is_none() {
        document["projects"] = table();
    }
    if document["projects"].get(project_key).is_none() {
        document["projects"][project_key] = table();
    }
    document["projects"][project_key]["trust_level"] = value("trusted");
    if let Some(parent) = config_path.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::write(config_path, document.to_string())
}

fn codex_hook_state_key(source_path: &Path, group_index: usize, handler_index: usize) -> String {
    format!(
        "{}:{CODEX_PRE_TOOL_USE_STATE}:{group_index}:{handler_index}",
        source_path.to_string_lossy()
    )
}

fn has_trusted_hook_state(keys: &HashSet<String>, expected: &str) -> bool {
    keys.iter()
        .any(|key| key == expected || normalized_state_key(key) == normalized_state_key(expected))
}

fn normalized_state_key(key: &str) -> String {
    let Some((path, suffix)) = key.split_once(&format!(":{CODEX_PRE_TOOL_USE_STATE}:")) else {
        return key.to_string();
    };
    format!(
        "{}:{CODEX_PRE_TOOL_USE_STATE}:{suffix}",
        normalize_project_path_key(path)
    )
}

pub fn codex_project_key(path: &Path) -> String {
    normalize_project_path_key(&path.to_string_lossy())
}

fn normalize_project_path_key(raw: &str) -> String {
    let mut s = raw.replace('/', "\\");
    if let Some(stripped) = s.strip_prefix(r"\\?\") {
        s = stripped.to_string();
    }
    if looks_like_windows_path(&s) {
        s.to_ascii_lowercase()
    } else {
        raw.to_string()
    }
}

fn looks_like_windows_path(path: &str) -> bool {
    let bytes = path.as_bytes();
    bytes.len() >= 2 && bytes[1] == b':' || path.starts_with(r"\\?\")
}

fn is_extended_key_for(key: &str, canonical_key: &str) -> bool {
    key.starts_with(r"\\?\") && normalize_project_path_key(key) == canonical_key
}

fn same_path(left: &Path, right: &Path) -> bool {
    if cfg!(target_os = "windows") {
        left.to_string_lossy()
            .eq_ignore_ascii_case(&right.to_string_lossy())
    } else {
        left == right
    }
}

fn join_matchers(matchers: &BTreeSet<String>) -> String {
    matchers.iter().cloned().collect::<Vec<_>>().join(", ")
}

fn display_path(path: &Path) -> String {
    path.to_string_lossy().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    fn write(path: &Path, body: &str) {
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, body).unwrap();
    }

    fn trusted_state_for(path: &Path, group: usize, handler: usize) -> String {
        format!(
            "[hooks.state.'{}:{}:{group}:{handler}']\ntrusted_hash = \"sha256:test\"\n",
            path.to_string_lossy(),
            CODEX_PRE_TOOL_USE_STATE
        )
    }

    #[test]
    fn claude_settings_parser_normalizes_pre_tool_matchers() {
        let temp = tempdir().unwrap();
        let repo = temp.path().join("repo");
        let home = temp.path().join("home");
        write(
            &repo.join(".claude").join("settings.json"),
            r#"{"hooks":{"PreToolUse":[{"matcher":"Bash","hooks":[{"type":"command","command":"python check.py"}]}]}}"#,
        );

        let report = inspect_paths(&repo, Some(&home));

        assert_eq!(
            report.claude.all_matchers(),
            BTreeSet::from(["Bash".to_string()])
        );
        assert!(report.claude.hooks[0].active);
    }

    #[test]
    fn codex_current_shape_parses_and_legacy_shape_warns() {
        let temp = tempdir().unwrap();
        let repo = temp.path().join("repo");
        let home = temp.path().join("home");
        let current = repo.join(".codex").join("hooks.json");
        write(
            &current,
            r#"{"hooks":{"PreToolUse":[{"matcher":"*","hooks":[{"type":"command","command":"python hook.py"}]}]}}"#,
        );
        write(
            &home.join(".codex").join("config.toml"),
            &format!(
                "[projects.'{}']\ntrust_level = \"trusted\"\n{}",
                codex_project_key(&repo),
                trusted_state_for(&current, 0, 0)
            ),
        );

        let report = inspect_paths(&repo, Some(&home));
        assert_eq!(
            report.codex.all_matchers(),
            BTreeSet::from(["*".to_string()])
        );
        assert!(report.codex.hooks[0].active);

        write(
            &current,
            r#"{"PreToolUse":[{"matcher":"Bash","hooks":[]}]}"#,
        );
        let report = inspect_paths(&repo, Some(&home));
        assert!(report
            .warnings
            .iter()
            .any(|warning| warning.contains("legacy root-level")));
    }

    #[test]
    fn one_sided_hook_warnings_are_bidirectional() {
        let temp = tempdir().unwrap();
        let repo = temp.path().join("repo");
        let home = temp.path().join("home");
        write(
            &repo.join(".claude").join("settings.json"),
            r#"{"hooks":{"PreToolUse":[{"matcher":"Bash","hooks":[{}]}]}}"#,
        );
        let report = inspect_paths(&repo, Some(&home));
        assert!(report
            .warnings
            .iter()
            .any(|warning| warning.contains("Claude PreToolUse hooks exist")
                && warning.contains("clud --fix-hooks")));

        fs::remove_file(repo.join(".claude").join("settings.json")).unwrap();
        let codex = repo.join(".codex").join("hooks.json");
        write(
            &codex,
            r#"{"hooks":{"PreToolUse":[{"matcher":"Bash","hooks":[{}]}]}}"#,
        );
        write(
            &home.join(".codex").join("config.toml"),
            &format!(
                "[projects.'{}']\ntrust_level = \"trusted\"\n{}",
                codex_project_key(&repo),
                trusted_state_for(&codex, 0, 0)
            ),
        );
        let report = inspect_paths(&repo, Some(&home));
        assert!(report
            .warnings
            .iter()
            .any(|warning| warning.contains("Codex PreToolUse hooks exist")
                && warning.contains("clud --fix-hooks")));
    }

    #[test]
    fn matcher_mismatch_warns() {
        let temp = tempdir().unwrap();
        let repo = temp.path().join("repo");
        let home = temp.path().join("home");
        write(
            &repo.join(".claude").join("settings.json"),
            r#"{"hooks":{"PreToolUse":[{"matcher":"Bash","hooks":[{}]}]}}"#,
        );
        let codex = repo.join(".codex").join("hooks.json");
        write(
            &codex,
            r#"{"hooks":{"PreToolUse":[{"matcher":"Read","hooks":[{}]}]}}"#,
        );
        write(
            &home.join(".codex").join("config.toml"),
            &format!(
                "[projects.'{}']\ntrust_level = \"trusted\"\n{}",
                codex_project_key(&repo),
                trusted_state_for(&codex, 0, 0)
            ),
        );

        let report = inspect_paths(&repo, Some(&home));

        assert!(report
            .warnings
            .iter()
            .any(|warning| warning.contains("matchers differ")));
    }

    #[test]
    fn windows_codex_project_key_normalizes_extended_path() {
        let path = PathBuf::from(r"\\?\C:\Users\Niteris\Dev\Repo");
        assert_eq!(codex_project_key(&path), r"c:\users\niteris\dev\repo");
        assert!(is_extended_key_for(
            r"\\?\C:\Users\Niteris\Dev\Repo",
            r"c:\users\niteris\dev\repo"
        ));
    }

    #[test]
    fn extended_path_only_trust_config_warns() {
        let temp = tempdir().unwrap();
        let repo = PathBuf::from(r"C:\Users\Niteris\Dev\Repo");
        let home = temp.path().join("home");
        let codex = repo.join(".codex").join("hooks.json");
        write(
            &codex,
            r#"{"hooks":{"PreToolUse":[{"matcher":"*","hooks":[{}]}]}}"#,
        );
        write(
            &home.join(".codex").join("config.toml"),
            r#"[projects.'\\?\C:\Users\Niteris\Dev\Repo']
trust_level = "trusted"
"#,
        );

        let report = inspect_paths(&repo, Some(&home));

        assert!(report
            .warnings
            .iter()
            .any(|warning| warning.contains("extended `\\\\?\\...` path key")));
    }

    #[test]
    fn deterministic_trust_repair_adds_canonical_key() {
        let temp = tempdir().unwrap();
        let config = temp.path().join("home").join(".codex").join("config.toml");
        add_codex_project_trust(&config, r"c:\users\niteris\dev\repo").unwrap();

        let text = fs::read_to_string(config).unwrap();
        assert!(text.contains(r#"trust_level = "trusted""#));
        assert!(text.contains(r#"c:\users\niteris\dev\repo"#));
        assert!(!text.contains("trusted_hash"));
    }

    #[test]
    fn per_hook_prompt_planning_uses_one_action_per_missing_matcher() {
        let temp = tempdir().unwrap();
        let repo = temp.path().join("repo");
        let home = temp.path().join("home");
        write(
            &repo.join(".claude").join("settings.json"),
            r#"{"hooks":{"PreToolUse":[{"matcher":"Bash","hooks":[{}]},{"matcher":"Read","hooks":[{}]}]}}"#,
        );

        let report = inspect_paths(&repo, Some(&home));
        let prompts = plan_repairs(&report)
            .into_iter()
            .filter(|action| matches!(action, RepairAction::BackendPrompt { .. }))
            .collect::<Vec<_>>();

        assert_eq!(prompts.len(), 2);
        assert!(prompts.iter().all(|action| match action {
            RepairAction::BackendPrompt { prompt, .. } => prompt.contains("exactly one hook"),
            _ => false,
        }));
    }
}
