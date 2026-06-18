//! User-level clud settings persisted under `~/.clud/settings.json`.

use std::fs::{self, File, OpenOptions};
use std::io;
use std::path::{Path, PathBuf};

use fs4::fs_std::FileExt;
use serde_json::{json, Map, Value};
use toml_edit::{DocumentMut, Item};

use crate::backend::Backend;
use crate::launch_setup::LaunchSetupScope;

pub const CLUD_DIR_NAME: &str = ".clud";
pub const SETTINGS_FILE_NAME: &str = "settings.json";
pub const LEGACY_SETTINGS_FILE_NAME: &str = "settings.toml";
pub const LOCK_FILE_NAME: &str = "settings.lock";
pub const DEFAULT_CODEX_GITHUB_PLUGIN_CONFIG_OVERRIDE: &str =
    "plugins.\"github@openai-curated\".enabled=false";
const CODEX_CONFIG_OVERRIDES_NOTE: &str =
    "clud passes these strings as repeated `codex -c` config overrides before the Codex subcommand. Edit config_overrides to change plugin/connector behavior.";

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
    Parse { path: PathBuf, error: String },
}

impl std::fmt::Display for SettingsError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SettingsError::NoHomeDir => write!(f, "could not resolve user home directory"),
            SettingsError::Io(error) => write!(f, "{error}"),
            SettingsError::Parse { path, error } => {
                write!(f, "malformed settings in {}: {error}", path.display())
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
            seed_codex_config_override_defaults(&mut document);
            write_settings(&path, &document)?;
            Ok(default_codex_config_overrides())
        }
        None => Ok(default_codex_config_overrides()),
    }
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
        let launch_setup = object_entry(document, "launch_setup");
        let entry = launch_setup
            .entry(backend_settings_key(backend).to_string())
            .or_insert_with(|| json!({}));
        if !entry.is_object() {
            *entry = json!({});
        }
        entry.as_object_mut().unwrap().insert(
            "scope".to_string(),
            Value::String(scope.as_str().to_string()),
        );
    })
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
    let codex = object_entry(document, "codex");
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

fn object_entry<'a>(document: &'a mut Value, key: &str) -> &'a mut Map<String, Value> {
    if !document.is_object() {
        *document = json!({});
    }
    let root = document.as_object_mut().unwrap();
    let entry = root.entry(key.to_string()).or_insert_with(|| json!({}));
    if !entry.is_object() {
        *entry = json!({});
    }
    entry.as_object_mut().unwrap()
}

fn backend_settings_key(backend: Backend) -> &'static str {
    backend.executable_name()
}

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
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn missing_settings_file_has_no_launch_setup_scope() {
        let home = tempdir().unwrap();
        assert_eq!(
            load_launch_setup_scope_at(home.path(), Backend::Codex).unwrap(),
            None
        );
    }

    #[test]
    fn missing_settings_file_defaults_auto_fix_hooks_enabled() {
        let home = tempdir().unwrap();
        assert!(load_auto_fix_hooks_enabled_at(home.path()).unwrap());
    }

    #[test]
    fn missing_codex_overrides_default_without_writing_on_dry_run() {
        let home = tempdir().unwrap();

        assert_eq!(
            load_or_init_codex_config_overrides_at(home.path(), false).unwrap(),
            default_codex_config_overrides()
        );
        assert!(!settings_path_at(home.path()).exists());
    }

    #[test]
    fn first_run_codex_overrides_are_documented_in_settings_json() {
        let home = tempdir().unwrap();

        assert_eq!(
            load_or_init_codex_config_overrides_at(home.path(), true).unwrap(),
            default_codex_config_overrides()
        );

        let text = fs::read_to_string(settings_path_at(home.path())).unwrap();
        let json: Value = serde_json::from_str(&text).unwrap();
        assert_eq!(
            json["codex"]["config_overrides"][0],
            DEFAULT_CODEX_GITHUB_PLUGIN_CONFIG_OVERRIDE
        );
        assert!(
            json["codex"]["config_overrides_note"]
                .as_str()
                .unwrap()
                .contains("codex -c"),
            "{text}"
        );
    }

    #[test]
    fn existing_codex_overrides_are_user_owned() {
        let home = tempdir().unwrap();
        let path = settings_path_at(home.path());
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&path, r#"{"codex":{"config_overrides":[]}}"#).unwrap();

        assert_eq!(
            load_or_init_codex_config_overrides_at(home.path(), true).unwrap(),
            Vec::<String>::new()
        );
        let text = fs::read_to_string(path).unwrap();
        assert!(!text.contains(DEFAULT_CODEX_GITHUB_PLUGIN_CONFIG_OVERRIDE));
    }

    #[test]
    fn saves_auto_fix_hooks_sticky_opt_out_and_reset() {
        let home = tempdir().unwrap();

        save_auto_fix_hooks_enabled_at(home.path(), false).unwrap();
        assert!(!load_auto_fix_hooks_enabled_at(home.path()).unwrap());

        save_auto_fix_hooks_enabled_at(home.path(), true).unwrap();
        assert!(load_auto_fix_hooks_enabled_at(home.path()).unwrap());

        let text = fs::read_to_string(settings_path_at(home.path())).unwrap();
        let json: Value = serde_json::from_str(&text).unwrap();
        assert_eq!(json["hook_health"]["auto_fix_hooks"], true);
    }

    #[test]
    fn saves_launch_setup_scope_per_backend() {
        let home = tempdir().unwrap();

        save_launch_setup_scope_at(home.path(), Backend::Codex, LaunchSetupScope::Global).unwrap();

        assert_eq!(
            load_launch_setup_scope_at(home.path(), Backend::Codex).unwrap(),
            Some(LaunchSetupScope::Global)
        );
        assert_eq!(
            load_launch_setup_scope_at(home.path(), Backend::Claude).unwrap(),
            None
        );
        let text = fs::read_to_string(settings_path_at(home.path())).unwrap();
        let json: Value = serde_json::from_str(&text).unwrap();
        assert_eq!(json["launch_setup"]["codex"]["scope"], "global");
    }

    #[test]
    fn preserves_existing_settings_when_saving_scope() {
        let home = tempdir().unwrap();
        let path = settings_path_at(home.path());
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(
            &path,
            r#"{"unrelated":{"value":"kept"},"launch_setup":{"claude":{"scope":"session-only"}}}"#,
        )
        .unwrap();

        save_launch_setup_scope_at(home.path(), Backend::Codex, LaunchSetupScope::Global).unwrap();

        let text = fs::read_to_string(path).unwrap();
        let json: Value = serde_json::from_str(&text).unwrap();
        assert_eq!(json["unrelated"]["value"], "kept");
        assert_eq!(json["launch_setup"]["claude"]["scope"], "session-only");
        assert_eq!(json["launch_setup"]["codex"]["scope"], "global");
    }

    #[test]
    fn auto_fix_hooks_setting_preserves_existing_settings() {
        let home = tempdir().unwrap();
        let path = settings_path_at(home.path());
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(
            &path,
            r#"{"unrelated":{"value":"kept"},"launch_setup":{"codex":{"scope":"global"}}}"#,
        )
        .unwrap();

        save_auto_fix_hooks_enabled_at(home.path(), false).unwrap();

        let text = fs::read_to_string(path).unwrap();
        let json: Value = serde_json::from_str(&text).unwrap();
        assert_eq!(json["unrelated"]["value"], "kept");
        assert_eq!(json["launch_setup"]["codex"]["scope"], "global");
        assert_eq!(json["hook_health"]["auto_fix_hooks"], false);
    }

    #[test]
    fn saves_rust_optimize_settings() {
        let home = tempdir().unwrap();
        let settings = RustOptimizeSettings {
            use_soldr_shims: true,
            install_soldr: false,
            soldr_version: "1.2.3".to_string(),
        };

        save_rust_optimize_settings_at(home.path(), &settings).unwrap();

        assert_eq!(
            load_rust_optimize_settings_at(home.path()).unwrap(),
            Some(settings)
        );
        let text = fs::read_to_string(settings_path_at(home.path())).unwrap();
        let json: Value = serde_json::from_str(&text).unwrap();
        assert_eq!(json["optimize"]["rust"]["use_soldr_shims"], true);
        assert_eq!(json["optimize"]["rust"]["install_soldr"], false);
        assert_eq!(json["optimize"]["rust"]["soldr_version"], "1.2.3");
    }

    #[test]
    fn rust_optimize_settings_preserve_existing_settings() {
        let home = tempdir().unwrap();
        let path = settings_path_at(home.path());
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&path, r#"{"unrelated":{"value":"kept"}}"#).unwrap();

        save_rust_optimize_settings_at(home.path(), &RustOptimizeSettings::default()).unwrap();

        let text = fs::read_to_string(path).unwrap();
        let json: Value = serde_json::from_str(&text).unwrap();
        assert_eq!(json["unrelated"]["value"], "kept");
        assert!(json["optimize"]["rust"].is_object());
    }

    #[test]
    fn legacy_toml_is_read_and_migrated_on_next_save() {
        let home = tempdir().unwrap();
        let legacy = legacy_settings_path_at(home.path());
        fs::create_dir_all(legacy.parent().unwrap()).unwrap();
        fs::write(
            &legacy,
            "[hook_health]\nauto_fix_hooks = false\n\n[launch_setup.codex]\nscope = \"global\"\n\n[optimize.rust]\nuse_soldr_shims = true\ninstall_soldr = false\nsoldr_version = \"9.9.9\"\n",
        )
        .unwrap();

        assert!(!load_auto_fix_hooks_enabled_at(home.path()).unwrap());
        assert_eq!(
            load_launch_setup_scope_at(home.path(), Backend::Codex).unwrap(),
            Some(LaunchSetupScope::Global)
        );
        assert_eq!(
            load_rust_optimize_settings_at(home.path()).unwrap(),
            Some(RustOptimizeSettings {
                use_soldr_shims: true,
                install_soldr: false,
                soldr_version: "9.9.9".to_string(),
            })
        );

        save_auto_fix_hooks_enabled_at(home.path(), true).unwrap();
        let text = fs::read_to_string(settings_path_at(home.path())).unwrap();
        let json: Value = serde_json::from_str(&text).unwrap();
        assert_eq!(json["hook_health"]["auto_fix_hooks"], true);
        assert_eq!(json["launch_setup"]["codex"]["scope"], "global");
        assert_eq!(json["optimize"]["rust"]["soldr_version"], "9.9.9");
    }
}
