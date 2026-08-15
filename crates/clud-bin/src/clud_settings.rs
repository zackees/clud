//! User-level clud settings persisted under `~/.clud/settings.json`.

use std::fs::{self, File, OpenOptions};
use std::io;
use std::path::{Path, PathBuf};

use fs4::fs_std::FileExt;
use serde_json::{json, Map, Value};
use toml_edit::{DocumentMut, Item};

use crate::backend::{
    resolve_launch_target, Backend, HarnessSelection, LaunchTargetError, ModelProvider,
};
use crate::launch_setup::LaunchSetupScope;
use crate::provider_catalog::{self, EffortLevel};

pub const CLUD_DIR_NAME: &str = ".clud";
pub const SETTINGS_FILE_NAME: &str = "settings.json";
pub const LEGACY_SETTINGS_FILE_NAME: &str = "settings.toml";
pub const LOCK_FILE_NAME: &str = "settings.lock";
pub const DEFAULT_CODEX_GITHUB_PLUGIN_CONFIG_OVERRIDE: &str =
    "plugins.\"github@openai-curated\".enabled=false";
const CODEX_CONFIG_OVERRIDES_NOTE: &str =
    "clud passes these strings as repeated `codex -c` config overrides before the Codex subcommand. Edit config_overrides to change plugin/connector behavior.";
const SHELL_DISABLE_POWERSHELL_NOTE: &str =
    "When true, clud injects a PreToolUse hook into Claude and Codex that denies any Bash/Shell call resolving to powershell.exe / pwsh / *.ps1. For Claude it also sets CLAUDE_CODE_USE_POWERSHELL_TOOL=0 + CLAUDE_CODE_GIT_BASH_PATH to a vendored bash. Also sets CLUD_DISABLE_POWERSHELL=1 in the backend env so skills/CLAUDE.md content can branch on it. Per-backend overrides under shell.claude.disable_powershell / shell.codex.disable_powershell take precedence; null inherits the top-level value. Default false. See https://github.com/zackees/clud/issues/447.";
const GIT_PR_WAIT_FAIL_FAST_NOTE: &str =
    "When true, cmd-scan denies raw `gh pr checks --watch` / `gh run watch` and hand-rolled PR-status polling loops, pointing the agent at the bundled fail-fast waiter (`clud tool run github/pr_merge_watch.py <PR>`) instead. Off by default; toggle with `clud settings`.";

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct GlobalLaunchPreferences {
    pub model_provider: Option<ModelProvider>,
    pub harness: Option<HarnessSelection>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct GlobalSettingsPatch {
    pub model_provider: Option<ModelProvider>,
    pub harness: Option<HarnessSelection>,
    pub pr_wait_fail_fast: Option<bool>,
    pub provider_profiles: Vec<ProviderProfilePatch>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProviderProfile {
    pub provider: ModelProvider,
    pub model: Option<String>,
    pub harness: Option<HarnessSelection>,
    pub effort: Option<EffortLevel>,
    pub context_window: Option<String>,
}

impl ProviderProfile {
    pub fn selection_defaults(&self) -> provider_catalog::ProviderSelectionDefaults<'_> {
        provider_catalog::ProviderSelectionDefaults {
            model: self.model.as_deref(),
            effort: self.effort,
            context_window: self.context_window.as_deref(),
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ProviderProfilePatch {
    pub provider: Option<ModelProvider>,
    pub model: Option<String>,
    pub harness: Option<HarnessSelection>,
    pub effort: Option<Option<EffortLevel>>,
    pub context_window: Option<Option<String>>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct LaunchPreferencesSnapshot {
    pub global: GlobalLaunchPreferences,
    pub provider_profiles: Vec<ProviderProfile>,
}

impl LaunchPreferencesSnapshot {
    pub fn profile(&self, provider: ModelProvider) -> Option<&ProviderProfile> {
        self.provider_profiles
            .iter()
            .find(|profile| profile.provider == provider)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RustOptimizeSettings {
    pub use_soldr_shims: bool,
    pub install_soldr: bool,
    pub soldr_version: String,
}

impl Default for RustOptimizeSettings {
    fn default() -> Self {
        Self {
            use_soldr_shims: true,
            install_soldr: true,
            soldr_version: "0.7.11".to_string(),
        }
    }
}

#[derive(Debug)]
pub enum SettingsError {
    NoHomeDir,
    Io(io::Error),
    Parse {
        path: PathBuf,
        error: String,
    },
    InvalidLaunchTarget(LaunchTargetError),
    InvalidProviderProfile {
        provider: ModelProvider,
        error: String,
    },
}

impl std::fmt::Display for SettingsError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SettingsError::NoHomeDir => write!(f, "could not resolve user home directory"),
            SettingsError::Io(error) => write!(f, "{error}"),
            SettingsError::Parse { path, error } => {
                write!(f, "malformed settings in {}: {error}", path.display())
            }
            SettingsError::InvalidLaunchTarget(error) => {
                write!(f, "invalid launch preferences: {error}")
            }
            SettingsError::InvalidProviderProfile { provider, error } => {
                write!(f, "invalid providers.{provider} settings: {error}")
            }
        }
    }
}

impl std::error::Error for SettingsError {}

impl From<io::Error> for SettingsError {
    fn from(error: io::Error) -> Self {
        SettingsError::Io(error)
    }
}

pub fn settings_path_at(home: &Path) -> PathBuf {
    home.join(CLUD_DIR_NAME).join(SETTINGS_FILE_NAME)
}

pub fn legacy_settings_path_at(home: &Path) -> PathBuf {
    home.join(CLUD_DIR_NAME).join(LEGACY_SETTINGS_FILE_NAME)
}

pub fn default_codex_config_overrides() -> Vec<String> {
    vec![DEFAULT_CODEX_GITHUB_PLUGIN_CONFIG_OVERRIDE.to_string()]
}

pub fn home_dir_path() -> Result<PathBuf, SettingsError> {
    home_dir().ok_or(SettingsError::NoHomeDir)
}

pub fn seeded_global_settings_document() -> Value {
    let mut document = json!({});
    seed_global_settings_defaults(&mut document);
    document
}

pub fn seed_global_settings_defaults(document: &mut Value) {
    if let Some(shell) = seed_object_entry(document, "shell") {
        shell
            .entry("disable_powershell".to_string())
            .or_insert(Value::Bool(false));
    }

    if let Some(hook_health) = seed_object_entry(document, "hook_health") {
        hook_health
            .entry("auto_fix_hooks".to_string())
            .or_insert(Value::Bool(true));
    }

    if let Some(git) = seed_object_entry(document, "git") {
        git.entry("pr_wait_fail_fast".to_string())
            .or_insert(Value::Bool(false));
    }

    if let Some(daemon) = seed_object_entry(document, "daemon") {
        // #645: a daemon that has no owned work may retire after fifteen
        // minutes. Zero remains the documented explicit opt-out.
        daemon
            .entry("idle_timeout_secs".to_string())
            .or_insert(json!(900));
        let proc_sampler = daemon
            .entry("proc_sampler".to_string())
            .or_insert_with(|| json!({}));
        if let Some(proc_sampler) = proc_sampler.as_object_mut() {
            proc_sampler
                .entry("interval_ms".to_string())
                .or_insert(json!(2_000));
        }
        // #465 AC 1: the dead-originator sweep period and its grace window are
        // incident-response knobs, so they belong in settings rather than only
        // in an env var a running daemon cannot see changed.
        let orphan_sweep = daemon
            .entry("orphan_sweep".to_string())
            .or_insert_with(|| json!({}));
        if let Some(orphan_sweep) = orphan_sweep.as_object_mut() {
            orphan_sweep
                .entry("interval_ms".to_string())
                .or_insert(json!(60_000));
            orphan_sweep
                .entry("grace_ms".to_string())
                .or_insert(json!(10_000));
        }
    }

    seed_codex_config_override_defaults(document);
}

pub fn load_or_init_global_settings() -> Result<Value, SettingsError> {
    let home = home_dir_path()?;
    load_or_init_global_settings_at(&home)
}

pub fn load_or_init_global_settings_at(home: &Path) -> Result<Value, SettingsError> {
    let clud_dir = home.join(CLUD_DIR_NAME);
    let lock_path = clud_dir.join(LOCK_FILE_NAME);
    let _lock = acquire_lock(&lock_path)?;
    let path = settings_path_at(home);
    let mut document = read_settings_or_legacy(home)?;
    let original = document.clone();
    seed_global_settings_defaults(&mut document);
    if document != original || !path.is_file() {
        write_settings(&path, &document)?;
    }
    Ok(document)
}

pub fn read_settings_json_file(path: &Path) -> Result<Value, SettingsError> {
    let text = fs::read_to_string(path)?;
    if text.trim().is_empty() {
        return Ok(json!({}));
    }
    parse_json_settings(path, &text)
}

pub fn write_settings_json_file(path: &Path, document: &Value) -> Result<(), SettingsError> {
    write_settings(path, document)
}

pub fn merged_settings_document(global: &Value, local: Option<&Value>) -> Value {
    let mut merged = seeded_global_settings_document();
    merge_settings_into(&mut merged, global);
    if let Some(local) = local {
        merge_settings_into(&mut merged, local);
    }
    merged
}

pub fn load_or_init_codex_config_overrides(
    write_default: bool,
) -> Result<Vec<String>, SettingsError> {
    let home = home_dir().ok_or(SettingsError::NoHomeDir)?;
    load_or_init_codex_config_overrides_at(&home, write_default)
}

pub fn load_or_init_codex_config_overrides_at(
    home: &Path,
    write_default: bool,
) -> Result<Vec<String>, SettingsError> {
    let clud_dir = home.join(CLUD_DIR_NAME);
    let lock_path = clud_dir.join(LOCK_FILE_NAME);
    let _lock = acquire_lock(&lock_path)?;
    let path = settings_path_at(home);
    let mut document = read_settings_or_legacy(home)?;

    match read_codex_config_overrides(&document, &path)? {
        Some(overrides) => Ok(overrides),
        None if write_default => {
            seed_global_settings_defaults(&mut document);
            write_settings(&path, &document)?;
            Ok(default_codex_config_overrides())
        }
        None => Ok(default_codex_config_overrides()),
    }
}

pub fn load_default_backend() -> Result<Option<Backend>, SettingsError> {
    Ok(load_default_model_provider()?.map(ModelProvider::native_harness))
}

pub fn load_default_model_provider() -> Result<Option<ModelProvider>, SettingsError> {
    let home = home_dir().ok_or(SettingsError::NoHomeDir)?;
    load_default_model_provider_at(&home)
}

pub fn load_default_backend_at(home: &Path) -> Result<Option<Backend>, SettingsError> {
    Ok(load_default_model_provider_at(home)?.map(ModelProvider::native_harness))
}

pub fn load_default_model_provider_at(home: &Path) -> Result<Option<ModelProvider>, SettingsError> {
    Ok(load_global_launch_preferences_at(home)?.model_provider)
}

pub fn load_global_launch_preferences() -> Result<GlobalLaunchPreferences, SettingsError> {
    let home = home_dir().ok_or(SettingsError::NoHomeDir)?;
    load_global_launch_preferences_at(&home)
}

pub fn load_global_launch_preferences_read_only() -> Result<GlobalLaunchPreferences, SettingsError>
{
    let home = home_dir().ok_or(SettingsError::NoHomeDir)?;
    let document = read_settings_or_legacy(&home)?;
    Ok(global_launch_preferences_from_document(&document))
}

/// Read global launch preferences and every configured provider profile from
/// one document snapshot without seeding or writing defaults.
pub fn load_launch_preferences_read_only() -> Result<LaunchPreferencesSnapshot, SettingsError> {
    let home = home_dir().ok_or(SettingsError::NoHomeDir)?;
    load_launch_preferences_read_only_at(&home)
}

pub fn load_launch_preferences_read_only_at(
    home: &Path,
) -> Result<LaunchPreferencesSnapshot, SettingsError> {
    let document = read_settings_or_legacy(home)?;
    launch_preferences_from_document(&document)
}

fn launch_preferences_from_document(
    document: &Value,
) -> Result<LaunchPreferencesSnapshot, SettingsError> {
    let mut provider_profiles = Vec::new();
    for &provider in ModelProvider::ALL {
        if let Some(profile) = provider_profile_from_document(document, provider)? {
            provider_profiles.push(profile);
        }
    }
    Ok(LaunchPreferencesSnapshot {
        global: global_launch_preferences_from_document(document),
        provider_profiles,
    })
}

fn provider_profile_from_document(
    document: &Value,
    provider: ModelProvider,
) -> Result<Option<ProviderProfile>, SettingsError> {
    let Some(value) = document
        .get("providers")
        .and_then(|providers| providers.get(provider.as_str()))
    else {
        return Ok(None);
    };
    let Some(profile) = value.as_object() else {
        return Err(invalid_provider_profile(provider, "expected an object"));
    };
    let read_string = |key: &str| -> Result<Option<&str>, SettingsError> {
        match profile.get(key) {
            None | Some(Value::Null) => Ok(None),
            Some(Value::String(value)) => Ok(Some(value.as_str())),
            Some(_) => Err(invalid_provider_profile(
                provider,
                format!("{key} must be a string"),
            )),
        }
    };

    let model = read_string("model")?
        .map(|value| {
            let entry = provider_catalog::model_by_cli_id(value).ok_or_else(|| {
                invalid_provider_profile(
                    provider,
                    format!("model '{value}' is not a canonical catalog ID"),
                )
            })?;
            if entry.provider != provider {
                return Err(invalid_provider_profile(
                    provider,
                    format!("model '{value}' belongs to provider '{}'", entry.provider),
                ));
            }
            Ok(value.to_string())
        })
        .transpose()?;
    let harness = read_string("harness")?
        .map(|value| {
            HarnessSelection::from_settings_str(value).ok_or_else(|| {
                invalid_provider_profile(
                    provider,
                    format!("harness '{value}' must be default, claude, or codex"),
                )
            })
        })
        .transpose()?;
    let effort = read_string("effort")?
        .map(|value| {
            EffortLevel::parse(value).ok_or_else(|| {
                invalid_provider_profile(provider, format!("unknown effort '{value}'"))
            })
        })
        .transpose()?;
    let context_window = read_string("context_window")?
        .map(|value| match value {
            "auto" | "1m" => Ok(value.to_string()),
            _ => Err(invalid_provider_profile(
                provider,
                format!("context_window '{value}' must be auto or 1m"),
            )),
        })
        .transpose()?;

    provider_catalog::resolve(
        Some(provider),
        model.as_deref(),
        effort.map(EffortLevel::as_str),
        context_window.as_deref(),
    )
    .map_err(|error| invalid_provider_profile(provider, error.to_string()))?;
    if harness == Some(HarnessSelection::Codex) && provider != ModelProvider::Codex {
        return Err(invalid_provider_profile(
            provider,
            "the Codex harness cannot run this provider",
        ));
    }
    Ok(Some(ProviderProfile {
        provider,
        model,
        harness,
        effort,
        context_window,
    }))
}

fn invalid_provider_profile(provider: ModelProvider, error: impl Into<String>) -> SettingsError {
    SettingsError::InvalidProviderProfile {
        provider,
        error: error.into(),
    }
}

pub fn load_global_launch_preferences_at(
    home: &Path,
) -> Result<GlobalLaunchPreferences, SettingsError> {
    let lock_path = home.join(CLUD_DIR_NAME).join(LOCK_FILE_NAME);
    let _lock = acquire_lock(&lock_path)?;
    let document = read_settings_or_legacy(home)?;
    Ok(global_launch_preferences_from_document(&document))
}

fn global_launch_preferences_from_document(document: &Value) -> GlobalLaunchPreferences {
    let model_provider = document
        .get("backend")
        .and_then(|item| item.get("default"))
        .and_then(Value::as_str)
        .and_then(ModelProvider::from_settings_str)
        .or_else(|| {
            infer_default_backend_from_launch_setup(document).map(Backend::as_model_provider)
        });
    let harness = document
        .get("harness")
        .and_then(|item| item.get("default"))
        .and_then(Value::as_str)
        .and_then(HarnessSelection::from_settings_str);
    GlobalLaunchPreferences {
        model_provider,
        harness,
    }
}

pub fn save_default_backend(backend: Backend) -> Result<(), SettingsError> {
    let home = home_dir().ok_or(SettingsError::NoHomeDir)?;
    save_default_backend_at(&home, backend)
}

pub fn save_default_backend_at(home: &Path, backend: Backend) -> Result<(), SettingsError> {
    save_settings_patch_at(
        home,
        GlobalSettingsPatch {
            model_provider: Some(backend.as_model_provider()),
            ..GlobalSettingsPatch::default()
        },
    )
}

pub fn save_global_launch_setup_selection(
    backend: Backend,
    scope: LaunchSetupScope,
) -> Result<(), SettingsError> {
    let home = home_dir().ok_or(SettingsError::NoHomeDir)?;
    save_global_launch_setup_selection_at(&home, backend, scope)
}

pub fn save_global_launch_setup_selection_at(
    home: &Path,
    backend: Backend,
    scope: LaunchSetupScope,
) -> Result<(), SettingsError> {
    save_settings_transaction_at(
        home,
        GlobalSettingsPatch {
            model_provider: Some(backend.as_model_provider()),
            ..GlobalSettingsPatch::default()
        },
        Some(scope),
    )
}

pub fn save_global_launch_preferences(
    provider: Option<ModelProvider>,
    harness: Option<HarnessSelection>,
    scope: LaunchSetupScope,
) -> Result<(), SettingsError> {
    let home = home_dir().ok_or(SettingsError::NoHomeDir)?;
    save_global_launch_preferences_at(&home, provider, harness, scope)
}

pub fn save_global_launch_preferences_at(
    home: &Path,
    provider: Option<ModelProvider>,
    harness: Option<HarnessSelection>,
    scope: LaunchSetupScope,
) -> Result<(), SettingsError> {
    save_settings_transaction_at(
        home,
        GlobalSettingsPatch {
            model_provider: provider,
            harness,
            ..GlobalSettingsPatch::default()
        },
        Some(scope),
    )
}

pub fn save_settings_patch(patch: GlobalSettingsPatch) -> Result<(), SettingsError> {
    let home = home_dir().ok_or(SettingsError::NoHomeDir)?;
    save_settings_patch_at(&home, patch)
}

pub fn save_settings_patch_at(
    home: &Path,
    patch: GlobalSettingsPatch,
) -> Result<(), SettingsError> {
    save_settings_transaction_at(home, patch, None)
}

fn save_settings_transaction_at(
    home: &Path,
    patch: GlobalSettingsPatch,
    setup_scope: Option<LaunchSetupScope>,
) -> Result<(), SettingsError> {
    let clud_dir = home.join(CLUD_DIR_NAME);
    fs::create_dir_all(&clud_dir)?;
    let lock_path = clud_dir.join(LOCK_FILE_NAME);
    let _lock = acquire_lock(&lock_path)?;
    let path = settings_path_at(home);
    let mut document = read_settings_or_legacy(home)?;
    seed_global_settings_defaults(&mut document);

    if let Some(provider) = patch.model_provider {
        set_default_model_provider(&mut document, provider);
    }
    if let Some(harness) = patch.harness {
        set_default_harness(&mut document, harness);
    }
    if let Some(enabled) = patch.pr_wait_fail_fast {
        object_entry(&mut document, "git")
            .insert("pr_wait_fail_fast".to_string(), Value::Bool(enabled));
    }
    for profile in patch.provider_profiles {
        let Some(provider) = profile.provider else {
            continue;
        };
        set_provider_profile_patch(&mut document, provider, profile);
    }

    // Validate the complete post-patch document before the atomic write. This
    // catches cross-field capability errors such as Flash + 1m.
    let _ = launch_preferences_from_document(&document)?;

    let launch = global_launch_preferences_from_document(&document);
    let target = resolve_launch_target(
        false,
        false,
        false,
        None,
        launch.model_provider,
        launch.harness,
    )
    .map_err(SettingsError::InvalidLaunchTarget)?;
    if let Some(scope) = setup_scope {
        set_launch_setup_scope(&mut document, target.effective_harness, scope);
    }
    write_settings(&path, &document)
}

pub fn load_auto_fix_hooks_enabled() -> Result<bool, SettingsError> {
    let home = home_dir().ok_or(SettingsError::NoHomeDir)?;
    load_auto_fix_hooks_enabled_at(&home)
}

pub fn load_auto_fix_hooks_enabled_at(home: &Path) -> Result<bool, SettingsError> {
    let lock_path = home.join(CLUD_DIR_NAME).join(LOCK_FILE_NAME);
    let _lock = acquire_lock(&lock_path)?;
    let document = read_settings_or_legacy(home)?;
    Ok(document
        .get("hook_health")
        .and_then(|item| item.get("auto_fix_hooks"))
        .and_then(Value::as_bool)
        .unwrap_or(true))
}

pub fn save_auto_fix_hooks_enabled(enabled: bool) -> Result<(), SettingsError> {
    let home = home_dir().ok_or(SettingsError::NoHomeDir)?;
    save_auto_fix_hooks_enabled_at(&home, enabled)
}

pub fn save_auto_fix_hooks_enabled_at(home: &Path, enabled: bool) -> Result<(), SettingsError> {
    with_settings_document(home, |document| {
        object_entry(document, "hook_health")
            .insert("auto_fix_hooks".to_string(), Value::Bool(enabled));
    })
}

/// PR-wait fail-fast git command improvements (`clud settings` toggle).
/// Off by default — see `GIT_PR_WAIT_FAIL_FAST_NOTE`.
pub fn load_pr_wait_fail_fast_enabled() -> Result<bool, SettingsError> {
    let home = home_dir().ok_or(SettingsError::NoHomeDir)?;
    load_pr_wait_fail_fast_enabled_at(&home)
}

pub fn load_pr_wait_fail_fast_enabled_at(home: &Path) -> Result<bool, SettingsError> {
    let lock_path = home.join(CLUD_DIR_NAME).join(LOCK_FILE_NAME);
    let _lock = acquire_lock(&lock_path)?;
    let document = read_settings_or_legacy(home)?;
    Ok(document
        .get("git")
        .and_then(|item| item.get("pr_wait_fail_fast"))
        .and_then(Value::as_bool)
        .unwrap_or(false))
}

pub fn save_pr_wait_fail_fast_enabled(enabled: bool) -> Result<(), SettingsError> {
    let home = home_dir().ok_or(SettingsError::NoHomeDir)?;
    save_pr_wait_fail_fast_enabled_at(&home, enabled)
}

pub fn save_pr_wait_fail_fast_enabled_at(home: &Path, enabled: bool) -> Result<(), SettingsError> {
    with_settings_document(home, |document| {
        let git = object_entry(document, "git");
        git.entry("pr_wait_fail_fast_note".to_string())
            .or_insert_with(|| Value::String(GIT_PR_WAIT_FAIL_FAST_NOTE.to_string()));
        git.insert("pr_wait_fail_fast".to_string(), Value::Bool(enabled));
    })
}

pub fn load_launch_setup_scope(
    backend: Backend,
) -> Result<Option<LaunchSetupScope>, SettingsError> {
    let home = home_dir().ok_or(SettingsError::NoHomeDir)?;
    load_launch_setup_scope_at(&home, backend)
}

pub fn load_launch_setup_scope_at(
    home: &Path,
    backend: Backend,
) -> Result<Option<LaunchSetupScope>, SettingsError> {
    let lock_path = home.join(CLUD_DIR_NAME).join(LOCK_FILE_NAME);
    let _lock = acquire_lock(&lock_path)?;
    let document = read_settings_or_legacy(home)?;
    let Some(scope) = document
        .get("launch_setup")
        .and_then(|item| item.get(backend_settings_key(backend)))
        .and_then(|item| item.get("scope"))
        .and_then(Value::as_str)
    else {
        return Ok(None);
    };
    Ok(LaunchSetupScope::from_settings_str(scope))
}

pub fn save_launch_setup_scope(
    backend: Backend,
    scope: LaunchSetupScope,
) -> Result<(), SettingsError> {
    let home = home_dir().ok_or(SettingsError::NoHomeDir)?;
    save_launch_setup_scope_at(&home, backend, scope)
}

pub fn save_launch_setup_scope_at(
    home: &Path,
    backend: Backend,
    scope: LaunchSetupScope,
) -> Result<(), SettingsError> {
    with_settings_document(home, |document| {
        set_launch_setup_scope(document, backend, scope);
    })
}

pub fn load_shell_disable_powershell_for_backend(backend: Backend) -> Result<bool, SettingsError> {
    let home = home_dir().ok_or(SettingsError::NoHomeDir)?;
    load_shell_disable_powershell_for_backend_at(&home, backend)
}

pub fn load_shell_disable_powershell_for_backend_at(
    home: &Path,
    backend: Backend,
) -> Result<bool, SettingsError> {
    let lock_path = home.join(CLUD_DIR_NAME).join(LOCK_FILE_NAME);
    let _lock = acquire_lock(&lock_path)?;
    let document = read_settings_or_legacy(home)?;
    Ok(resolve_shell_disable_powershell(&document, backend))
}

pub fn save_shell_disable_powershell(enabled: bool) -> Result<(), SettingsError> {
    let home = home_dir().ok_or(SettingsError::NoHomeDir)?;
    save_shell_disable_powershell_at(&home, enabled)
}

pub fn save_shell_disable_powershell_at(home: &Path, enabled: bool) -> Result<(), SettingsError> {
    with_settings_document(home, |document| {
        let shell = object_entry(document, "shell");
        shell
            .entry("disable_powershell_note".to_string())
            .or_insert_with(|| Value::String(SHELL_DISABLE_POWERSHELL_NOTE.to_string()));
        shell.insert("disable_powershell".to_string(), Value::Bool(enabled));
    })
}

pub fn save_shell_disable_powershell_for_backend(
    backend: Backend,
    enabled: Option<bool>,
) -> Result<(), SettingsError> {
    let home = home_dir().ok_or(SettingsError::NoHomeDir)?;
    save_shell_disable_powershell_for_backend_at(&home, backend, enabled)
}

pub fn save_shell_disable_powershell_for_backend_at(
    home: &Path,
    backend: Backend,
    enabled: Option<bool>,
) -> Result<(), SettingsError> {
    with_settings_document(home, |document| {
        let shell = object_entry(document, "shell");
        shell
            .entry("disable_powershell_note".to_string())
            .or_insert_with(|| Value::String(SHELL_DISABLE_POWERSHELL_NOTE.to_string()));
        let backend_entry = shell
            .entry(backend_settings_key(backend).to_string())
            .or_insert_with(|| json!({}));
        if !backend_entry.is_object() {
            *backend_entry = json!({});
        }
        let backend_obj = backend_entry.as_object_mut().unwrap();
        match enabled {
            Some(value) => {
                backend_obj.insert("disable_powershell".to_string(), Value::Bool(value));
            }
            None => {
                backend_obj.insert("disable_powershell".to_string(), Value::Null);
            }
        }
    })
}

fn resolve_shell_disable_powershell(document: &Value, backend: Backend) -> bool {
    let shell = document.get("shell");
    if let Some(per_backend) = shell
        .and_then(|item| item.get(backend_settings_key(backend)))
        .and_then(|item| item.get("disable_powershell"))
        .and_then(Value::as_bool)
    {
        return per_backend;
    }
    shell
        .and_then(|item| item.get("disable_powershell"))
        .and_then(Value::as_bool)
        .unwrap_or(false)
}

pub fn save_rust_optimize_settings(settings: &RustOptimizeSettings) -> Result<(), SettingsError> {
    let home = home_dir().ok_or(SettingsError::NoHomeDir)?;
    save_rust_optimize_settings_at(&home, settings)
}

pub fn save_rust_optimize_settings_at(
    home: &Path,
    settings: &RustOptimizeSettings,
) -> Result<(), SettingsError> {
    with_settings_document(home, |document| {
        let optimize = object_entry(document, "optimize");
        optimize.insert(
            "rust".to_string(),
            json!({
                "use_soldr_shims": settings.use_soldr_shims,
                "install_soldr": settings.install_soldr,
                "soldr_version": settings.soldr_version.clone(),
            }),
        );
    })
}

pub fn load_rust_optimize_settings_at(
    home: &Path,
) -> Result<Option<RustOptimizeSettings>, SettingsError> {
    let lock_path = home.join(CLUD_DIR_NAME).join(LOCK_FILE_NAME);
    let _lock = acquire_lock(&lock_path)?;
    let document = read_settings_or_legacy(home)?;
    rust_optimize_from_json(&document)
}

/// Settings for the foreground CPU-burn banner (#466). Defaults: enabled,
/// 30 s heartbeat. JSON shape:
///
/// ```json
/// { "foreground": { "cpu_banner": { "enabled": false, "heartbeat_secs": 60 } } }
/// ```
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CpuBannerSettings {
    pub enabled: bool,
    pub heartbeat_secs: u64,
}

impl Default for CpuBannerSettings {
    fn default() -> Self {
        Self {
            enabled: true,
            heartbeat_secs: 30,
        }
    }
}

pub fn load_cpu_banner_settings() -> Result<CpuBannerSettings, SettingsError> {
    let home = home_dir().ok_or(SettingsError::NoHomeDir)?;
    load_cpu_banner_settings_at(&home)
}

pub fn load_cpu_banner_settings_at(home: &Path) -> Result<CpuBannerSettings, SettingsError> {
    let lock_path = home.join(CLUD_DIR_NAME).join(LOCK_FILE_NAME);
    let _lock = acquire_lock(&lock_path)?;
    let document = read_settings_or_legacy(home)?;
    let mut out = CpuBannerSettings::default();
    let Some(section) = document
        .get("foreground")
        .and_then(|item| item.get("cpu_banner"))
    else {
        return Ok(out);
    };
    if let Some(enabled) = section.get("enabled").and_then(Value::as_bool) {
        out.enabled = enabled;
    }
    if let Some(secs) = section.get("heartbeat_secs").and_then(Value::as_u64) {
        out.heartbeat_secs = secs;
    }
    Ok(out)
}

fn with_settings_document<F>(home: &Path, mutate: F) -> Result<(), SettingsError>
where
    F: FnOnce(&mut Value),
{
    let clud_dir = home.join(CLUD_DIR_NAME);
    fs::create_dir_all(&clud_dir)?;
    let lock_path = clud_dir.join(LOCK_FILE_NAME);
    let _lock = acquire_lock(&lock_path)?;
    let path = settings_path_at(home);
    let mut document = read_settings_or_legacy(home)?;
    seed_global_settings_defaults(&mut document);
    mutate(&mut document);
    write_settings(&path, &document)
}

fn read_settings_or_legacy(home: &Path) -> Result<Value, SettingsError> {
    let path = settings_path_at(home);
    match fs::read_to_string(&path) {
        Ok(text) if text.trim().is_empty() => return Ok(json!({})),
        Ok(text) => return parse_json_settings(&path, &text),
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Err(error) => return Err(SettingsError::Io(error)),
    }

    let legacy_path = legacy_settings_path_at(home);
    match fs::read_to_string(&legacy_path) {
        Ok(text) if text.trim().is_empty() => Ok(json!({})),
        Ok(text) => parse_legacy_toml_settings(&legacy_path, &text),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(json!({})),
        Err(error) => Err(SettingsError::Io(error)),
    }
}

fn parse_json_settings(path: &Path, text: &str) -> Result<Value, SettingsError> {
    let value: Value = serde_json::from_str(text).map_err(|error| SettingsError::Parse {
        path: path.to_path_buf(),
        error: error.to_string(),
    })?;
    if value.is_object() {
        Ok(value)
    } else {
        Err(SettingsError::Parse {
            path: path.to_path_buf(),
            error: "root must be a JSON object".to_string(),
        })
    }
}

fn parse_legacy_toml_settings(path: &Path, text: &str) -> Result<Value, SettingsError> {
    let document = text
        .parse::<DocumentMut>()
        .map_err(|error| SettingsError::Parse {
            path: path.to_path_buf(),
            error: error.to_string(),
        })?;
    let mut root = json!({});

    if let Some(enabled) = document
        .get("hook_health")
        .and_then(|item| item.get("auto_fix_hooks"))
        .and_then(Item::as_bool)
    {
        object_entry(&mut root, "hook_health")
            .insert("auto_fix_hooks".to_string(), Value::Bool(enabled));
    }

    if let Some(default_backend) = document
        .get("backend")
        .and_then(|item| item.get("default"))
        .and_then(Item::as_str)
        .and_then(Backend::from_settings_str)
    {
        object_entry(&mut root, "backend").insert(
            "default".to_string(),
            Value::String(default_backend.executable_name().to_string()),
        );
    }

    if let Some(launch_setup) = document.get("launch_setup").and_then(Item::as_table) {
        for (backend, item) in launch_setup.iter() {
            if let Some(scope) = item
                .get("scope")
                .and_then(Item::as_str)
                .and_then(LaunchSetupScope::from_settings_str)
            {
                object_entry(&mut root, "launch_setup").insert(
                    backend.to_string(),
                    json!({ "scope": scope.as_str().to_string() }),
                );
            }
        }
    }

    if let Some(rust) = document
        .get("optimize")
        .and_then(|item| item.get("rust"))
        .and_then(Item::as_table)
    {
        let defaults = RustOptimizeSettings::default();
        object_entry(&mut root, "optimize").insert(
            "rust".to_string(),
            json!({
                "use_soldr_shims": rust
                    .get("use_soldr_shims")
                    .and_then(Item::as_bool)
                    .unwrap_or(defaults.use_soldr_shims),
                "install_soldr": rust
                    .get("install_soldr")
                    .and_then(Item::as_bool)
                    .unwrap_or(defaults.install_soldr),
                "soldr_version": rust
                    .get("soldr_version")
                    .and_then(Item::as_str)
                    .unwrap_or(&defaults.soldr_version),
            }),
        );
    }

    if let Some(shell) = document.get("shell").and_then(Item::as_table) {
        if let Some(enabled) = shell.get("disable_powershell").and_then(Item::as_bool) {
            object_entry(&mut root, "shell")
                .insert("disable_powershell".to_string(), Value::Bool(enabled));
        }
        for backend_key in ["claude", "codex"] {
            let Some(per_backend) = shell.get(backend_key).and_then(Item::as_table) else {
                continue;
            };
            let Some(value) = per_backend
                .get("disable_powershell")
                .and_then(Item::as_bool)
            else {
                continue;
            };
            object_entry(&mut root, "shell").insert(
                backend_key.to_string(),
                json!({ "disable_powershell": value }),
            );
        }
    }

    Ok(root)
}

fn rust_optimize_from_json(
    document: &Value,
) -> Result<Option<RustOptimizeSettings>, SettingsError> {
    let Some(table) = document
        .get("optimize")
        .and_then(|item| item.get("rust"))
        .and_then(Value::as_object)
    else {
        return Ok(None);
    };
    let defaults = RustOptimizeSettings::default();
    Ok(Some(RustOptimizeSettings {
        use_soldr_shims: table
            .get("use_soldr_shims")
            .and_then(Value::as_bool)
            .unwrap_or(defaults.use_soldr_shims),
        install_soldr: table
            .get("install_soldr")
            .and_then(Value::as_bool)
            .unwrap_or(defaults.install_soldr),
        soldr_version: table
            .get("soldr_version")
            .and_then(Value::as_str)
            .unwrap_or(&defaults.soldr_version)
            .to_string(),
    }))
}

fn write_settings(path: &Path, document: &Value) -> Result<(), SettingsError> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let mut body =
        serde_json::to_string_pretty(document).map_err(|error| SettingsError::Parse {
            path: path.to_path_buf(),
            error: error.to_string(),
        })?;
    body.push('\n');
    fs::write(path, body)?;
    Ok(())
}

fn read_codex_config_overrides(
    document: &Value,
    path: &Path,
) -> Result<Option<Vec<String>>, SettingsError> {
    let Some(value) = document
        .get("codex")
        .and_then(|item| item.get("config_overrides"))
    else {
        return Ok(None);
    };
    if value.is_null() {
        return Ok(None);
    }
    let Some(items) = value.as_array() else {
        return Err(SettingsError::Parse {
            path: path.to_path_buf(),
            error: "codex.config_overrides must be an array of strings".to_string(),
        });
    };
    let mut overrides = Vec::with_capacity(items.len());
    for item in items {
        let Some(text) = item.as_str() else {
            return Err(SettingsError::Parse {
                path: path.to_path_buf(),
                error: "codex.config_overrides must be an array of strings".to_string(),
            });
        };
        if !text.trim().is_empty() {
            overrides.push(text.to_string());
        }
    }
    Ok(Some(overrides))
}

fn seed_codex_config_override_defaults(document: &mut Value) {
    let Some(codex) = seed_object_entry(document, "codex") else {
        return;
    };
    codex
        .entry("config_overrides_note".to_string())
        .or_insert_with(|| Value::String(CODEX_CONFIG_OVERRIDES_NOTE.to_string()));
    codex
        .entry("config_overrides".to_string())
        .or_insert_with(|| {
            Value::Array(
                default_codex_config_overrides()
                    .into_iter()
                    .map(Value::String)
                    .collect(),
            )
        });
}

fn merge_settings_into(target: &mut Value, source: &Value) {
    let Some(source_obj) = source.as_object() else {
        return;
    };
    if !target.is_object() {
        *target = json!({});
    }
    let target_obj = target.as_object_mut().unwrap();
    for (key, source_value) in source_obj {
        if source_value.is_null() {
            continue;
        }
        match target_obj.get_mut(key) {
            Some(target_value) if target_value.is_object() && source_value.is_object() => {
                merge_settings_into(target_value, source_value);
            }
            Some(target_value) => {
                *target_value = source_value.clone();
            }
            None if source_value.is_object() => {
                let mut nested = json!({});
                merge_settings_into(&mut nested, source_value);
                target_obj.insert(key.clone(), nested);
            }
            None => {
                target_obj.insert(key.clone(), source_value.clone());
            }
        }
    }
}

#[path = "clud_settings_document.rs"]
mod clud_settings_document;
use clud_settings_document::{
    backend_settings_key, infer_default_backend_from_launch_setup, object_entry, seed_object_entry,
    set_default_harness, set_default_model_provider, set_launch_setup_scope,
    set_provider_profile_patch,
};
fn acquire_lock(path: &Path) -> io::Result<LockGuard> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(path)?;
    FileExt::lock_exclusive(&file)
        .map_err(|error| io::Error::other(format!("lock {}: {error}", path.display())))?;
    Ok(LockGuard { _file: file })
}

struct LockGuard {
    _file: File,
}

fn home_dir() -> Option<PathBuf> {
    #[cfg(windows)]
    {
        if let Some(path) = std::env::var_os("USERPROFILE") {
            if !path.is_empty() {
                return Some(PathBuf::from(path));
            }
        }
    }
    if let Some(path) = std::env::var_os("HOME") {
        if !path.is_empty() {
            return Some(PathBuf::from(path));
        }
    }
    None
}

#[cfg(test)]
#[path = "clud_settings_tests.rs"]
mod tests;
