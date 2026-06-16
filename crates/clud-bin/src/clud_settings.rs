//! User-level clud settings persisted under `~/.clud/settings.toml`.

use std::fs::{self, File, OpenOptions};
use std::io;
use std::path::{Path, PathBuf};

use fs4::fs_std::FileExt;
use toml_edit::{table, value, DocumentMut, Item};

use crate::backend::Backend;
use crate::launch_setup::LaunchSetupScope;

pub const CLUD_DIR_NAME: &str = ".clud";
pub const SETTINGS_FILE_NAME: &str = "settings.toml";
pub const LOCK_FILE_NAME: &str = "settings.lock";

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
                write!(f, "malformed TOML in {}: {error}", path.display())
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

pub fn load_auto_fix_hooks_enabled() -> Result<bool, SettingsError> {
    let home = home_dir().ok_or(SettingsError::NoHomeDir)?;
    load_auto_fix_hooks_enabled_at(&home)
}

pub fn load_auto_fix_hooks_enabled_at(home: &Path) -> Result<bool, SettingsError> {
    let path = settings_path_at(home);
    if !path.exists() {
        return Ok(true);
    }
    let lock_path = home.join(CLUD_DIR_NAME).join(LOCK_FILE_NAME);
    let _lock = acquire_lock(&lock_path)?;
    let text = fs::read_to_string(&path)?;
    if text.trim().is_empty() {
        return Ok(true);
    }
    let document = parse_settings(&path, &text)?;
    Ok(document
        .get("hook_health")
        .and_then(|item| item.get("auto_fix_hooks"))
        .and_then(Item::as_bool)
        .unwrap_or(true))
}

pub fn save_auto_fix_hooks_enabled(enabled: bool) -> Result<(), SettingsError> {
    let home = home_dir().ok_or(SettingsError::NoHomeDir)?;
    save_auto_fix_hooks_enabled_at(&home, enabled)
}

pub fn save_auto_fix_hooks_enabled_at(home: &Path, enabled: bool) -> Result<(), SettingsError> {
    let clud_dir = home.join(CLUD_DIR_NAME);
    fs::create_dir_all(&clud_dir)?;
    let lock_path = clud_dir.join(LOCK_FILE_NAME);
    let _lock = acquire_lock(&lock_path)?;

    let path = settings_path_at(home);
    let text = match fs::read_to_string(&path) {
        Ok(text) => text,
        Err(error) if error.kind() == io::ErrorKind::NotFound => String::new(),
        Err(error) => return Err(SettingsError::Io(error)),
    };
    let mut document = if text.trim().is_empty() {
        DocumentMut::new()
    } else {
        parse_settings(&path, &text)?
    };

    if document
        .get("hook_health")
        .and_then(Item::as_table)
        .is_none()
    {
        document["hook_health"] = table();
    }
    document["hook_health"]["auto_fix_hooks"] = value(enabled);

    let mut body = document.to_string();
    if !body.ends_with('\n') {
        body.push('\n');
    }
    fs::write(path, body)?;
    Ok(())
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
    let path = settings_path_at(home);
    if !path.exists() {
        return Ok(None);
    }
    let lock_path = home.join(CLUD_DIR_NAME).join(LOCK_FILE_NAME);
    let _lock = acquire_lock(&lock_path)?;
    let text = fs::read_to_string(&path)?;
    if text.trim().is_empty() {
        return Ok(None);
    }
    let document = parse_settings(&path, &text)?;
    let Some(scope) = document
        .get("launch_setup")
        .and_then(|item| item.get(backend_settings_key(backend)))
        .and_then(|item| item.get("scope"))
        .and_then(Item::as_str)
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
    let clud_dir = home.join(CLUD_DIR_NAME);
    fs::create_dir_all(&clud_dir)?;
    let lock_path = clud_dir.join(LOCK_FILE_NAME);
    let _lock = acquire_lock(&lock_path)?;

    let path = settings_path_at(home);
    let text = match fs::read_to_string(&path) {
        Ok(text) => text,
        Err(error) if error.kind() == io::ErrorKind::NotFound => String::new(),
        Err(error) => return Err(SettingsError::Io(error)),
    };
    let mut document = if text.trim().is_empty() {
        DocumentMut::new()
    } else {
        parse_settings(&path, &text)?
    };

    if document
        .get("launch_setup")
        .and_then(Item::as_table)
        .is_none()
    {
        document["launch_setup"] = table();
    }
    let backend_key = backend_settings_key(backend);
    if document["launch_setup"]
        .get(backend_key)
        .and_then(Item::as_table)
        .is_none()
    {
        document["launch_setup"][backend_key] = table();
    }
    document["launch_setup"][backend_key]["scope"] = value(scope.as_str());

    let mut body = document.to_string();
    if !body.ends_with('\n') {
        body.push('\n');
    }
    fs::write(path, body)?;
    Ok(())
}

pub fn save_rust_optimize_settings(settings: &RustOptimizeSettings) -> Result<(), SettingsError> {
    let home = home_dir().ok_or(SettingsError::NoHomeDir)?;
    save_rust_optimize_settings_at(&home, settings)
}

pub fn save_rust_optimize_settings_at(
    home: &Path,
    settings: &RustOptimizeSettings,
) -> Result<(), SettingsError> {
    let clud_dir = home.join(CLUD_DIR_NAME);
    fs::create_dir_all(&clud_dir)?;
    let lock_path = clud_dir.join(LOCK_FILE_NAME);
    let _lock = acquire_lock(&lock_path)?;

    let path = settings_path_at(home);
    let text = match fs::read_to_string(&path) {
        Ok(text) => text,
        Err(error) if error.kind() == io::ErrorKind::NotFound => String::new(),
        Err(error) => return Err(SettingsError::Io(error)),
    };
    let mut document = if text.trim().is_empty() {
        DocumentMut::new()
    } else {
        parse_settings(&path, &text)?
    };

    if document.get("optimize").and_then(Item::as_table).is_none() {
        document["optimize"] = table();
    }
    if document["optimize"]
        .get("rust")
        .and_then(Item::as_table)
        .is_none()
    {
        document["optimize"]["rust"] = table();
    }
    document["optimize"]["rust"]["use_soldr_shims"] = value(settings.use_soldr_shims);
    document["optimize"]["rust"]["install_soldr"] = value(settings.install_soldr);
    document["optimize"]["rust"]["soldr_version"] = value(settings.soldr_version.clone());

    let mut body = document.to_string();
    if !body.ends_with('\n') {
        body.push('\n');
    }
    fs::write(path, body)?;
    Ok(())
}

pub fn load_rust_optimize_settings_at(
    home: &Path,
) -> Result<Option<RustOptimizeSettings>, SettingsError> {
    let path = settings_path_at(home);
    if !path.exists() {
        return Ok(None);
    }
    let lock_path = home.join(CLUD_DIR_NAME).join(LOCK_FILE_NAME);
    let _lock = acquire_lock(&lock_path)?;
    let text = fs::read_to_string(&path)?;
    if text.trim().is_empty() {
        return Ok(None);
    }
    let document = parse_settings(&path, &text)?;
    let Some(table) = document
        .get("optimize")
        .and_then(|item| item.get("rust"))
        .and_then(Item::as_table)
    else {
        return Ok(None);
    };
    let defaults = RustOptimizeSettings::default();
    Ok(Some(RustOptimizeSettings {
        use_soldr_shims: table
            .get("use_soldr_shims")
            .and_then(Item::as_bool)
            .unwrap_or(defaults.use_soldr_shims),
        install_soldr: table
            .get("install_soldr")
            .and_then(Item::as_bool)
            .unwrap_or(defaults.install_soldr),
        soldr_version: table
            .get("soldr_version")
            .and_then(Item::as_str)
            .unwrap_or(&defaults.soldr_version)
            .to_string(),
    }))
}

fn parse_settings(path: &Path, text: &str) -> Result<DocumentMut, SettingsError> {
    text.parse::<DocumentMut>()
        .map_err(|error| SettingsError::Parse {
            path: path.to_path_buf(),
            error: error.to_string(),
        })
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
    fn saves_auto_fix_hooks_sticky_opt_out_and_reset() {
        let home = tempdir().unwrap();

        save_auto_fix_hooks_enabled_at(home.path(), false).unwrap();
        assert!(!load_auto_fix_hooks_enabled_at(home.path()).unwrap());

        save_auto_fix_hooks_enabled_at(home.path(), true).unwrap();
        assert!(load_auto_fix_hooks_enabled_at(home.path()).unwrap());

        let text = fs::read_to_string(settings_path_at(home.path())).unwrap();
        assert!(text.contains("[hook_health]"), "{text}");
        assert!(text.contains("auto_fix_hooks = true"), "{text}");
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
        assert!(
            text.contains("[launch_setup.codex]"),
            "settings TOML should use a backend-scoped table: {text}"
        );
        assert!(
            text.contains("scope = \"global\""),
            "settings TOML should persist the selected global scope: {text}"
        );
    }

    #[test]
    fn preserves_existing_settings_when_saving_scope() {
        let home = tempdir().unwrap();
        let path = settings_path_at(home.path());
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(
            &path,
            "[unrelated]\nvalue = \"kept\"\n\n[launch_setup.claude]\nscope = \"session-only\"\n",
        )
        .unwrap();

        save_launch_setup_scope_at(home.path(), Backend::Codex, LaunchSetupScope::Global).unwrap();

        let text = fs::read_to_string(path).unwrap();
        assert!(text.contains("[unrelated]"), "{text}");
        assert!(text.contains("value = \"kept\""), "{text}");
        assert!(text.contains("[launch_setup.claude]"), "{text}");
        assert!(text.contains("[launch_setup.codex]"), "{text}");
    }

    #[test]
    fn auto_fix_hooks_setting_preserves_existing_settings() {
        let home = tempdir().unwrap();
        let path = settings_path_at(home.path());
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(
            &path,
            "[unrelated]\nvalue = \"kept\"\n\n[launch_setup.codex]\nscope = \"global\"\n",
        )
        .unwrap();

        save_auto_fix_hooks_enabled_at(home.path(), false).unwrap();

        let text = fs::read_to_string(path).unwrap();
        assert!(text.contains("[unrelated]"), "{text}");
        assert!(text.contains("value = \"kept\""), "{text}");
        assert!(text.contains("[launch_setup.codex]"), "{text}");
        assert!(text.contains("[hook_health]"), "{text}");
        assert!(text.contains("auto_fix_hooks = false"), "{text}");
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
        assert!(text.contains("[optimize.rust]"), "{text}");
        assert!(text.contains("use_soldr_shims = true"), "{text}");
        assert!(text.contains("install_soldr = false"), "{text}");
        assert!(text.contains("soldr_version = \"1.2.3\""), "{text}");
    }

    #[test]
    fn rust_optimize_settings_preserve_existing_settings() {
        let home = tempdir().unwrap();
        let path = settings_path_at(home.path());
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&path, "[unrelated]\nvalue = \"kept\"\n").unwrap();

        save_rust_optimize_settings_at(home.path(), &RustOptimizeSettings::default()).unwrap();

        let text = fs::read_to_string(path).unwrap();
        assert!(text.contains("[unrelated]"), "{text}");
        assert!(text.contains("value = \"kept\""), "{text}");
        assert!(text.contains("[optimize.rust]"), "{text}");
    }
}
