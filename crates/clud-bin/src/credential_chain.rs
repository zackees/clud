//! Credential fallback chain for API-key providers (#1891).
//!
//! The OS-native vault is preferred, but a headless Linux host (SSH, no
//! desktop session) usually has no Secret Service, or one whose login
//! collection stays locked because unlocking needs a GUI prompter. There the
//! vault reports `Unavailable` and clud used to be unusable. The chain is:
//!
//! 1. the native vault (read and write);
//! 2. an owner-only file under `~/.clud/credentials` (read and write), used
//!    only when the vault is unavailable or holds nothing;
//! 3. the provider's API-key environment variable (read only, never
//!    persisted).
//!
//! A working vault always wins, so desktop users see no change.

use std::path::{Path, PathBuf};
use std::sync::Mutex;

use crate::provider_auth::{CredentialBackend, SecretStore, SecretStoreError};

/// `~/.clud/credentials`, or `None` when no home directory resolves.
pub fn default_credentials_dir() -> Option<PathBuf> {
    crate::home::user_home().map(|home| home.join(".clud").join("credentials"))
}

/// The vault-first credential chain. `V` is the native vault; tests inject a
/// fake that always reports `Unavailable`.
pub struct FallbackSecretStore<V> {
    vault: V,
    pub(crate) service: &'static str,
    pub(crate) account: &'static str,
    file: Option<PathBuf>,
    env_var: Option<&'static str>,
    env_lookup: fn(&str) -> Option<String>,
    last: Mutex<CredentialBackend>,
}

impl<V: SecretStore> FallbackSecretStore<V> {
    pub fn new(
        vault: V,
        service: &'static str,
        account: &'static str,
        file: Option<PathBuf>,
        env_var: Option<&'static str>,
    ) -> Self {
        Self {
            vault,
            service,
            account,
            file,
            env_var,
            env_lookup: |name| std::env::var(name).ok(),
            last: Mutex::new(CredentialBackend::NativeVault),
        }
    }

    #[cfg(test)]
    fn with_env_lookup(mut self, lookup: fn(&str) -> Option<String>) -> Self {
        self.env_lookup = lookup;
        self
    }

    fn record(&self, backend: CredentialBackend) {
        if let Ok(mut last) = self.last.lock() {
            *last = backend;
        }
    }

    fn env_value(&self) -> Option<String> {
        let value = (self.env_lookup)(self.env_var?)?;
        let value = value.trim();
        (!value.is_empty()).then(|| value.to_string())
    }
}

impl<V: SecretStore> SecretStore for FallbackSecretStore<V> {
    fn get(&self) -> Result<Option<String>, SecretStoreError> {
        let vault_unavailable = match self.vault.get() {
            Ok(Some(secret)) => {
                self.record(CredentialBackend::NativeVault);
                return Ok(Some(secret));
            }
            Ok(None) => false,
            Err(SecretStoreError::Unavailable) => true,
            Err(error) => return Err(error),
        };
        if let Some(path) = &self.file {
            if let Some(secret) = read_file(path)? {
                self.record(CredentialBackend::File);
                return Ok(Some(secret));
            }
        }
        if let Some(secret) = self.env_value() {
            self.record(CredentialBackend::Env);
            return Ok(Some(secret));
        }
        // With a usable file fallback, "nothing stored" is the truth and
        // `clud auth login` will work, so say that instead of failing.
        if vault_unavailable && self.file.is_none() {
            return Err(SecretStoreError::Unavailable);
        }
        // Nothing stored anywhere: report the preferred store.
        self.record(CredentialBackend::NativeVault);
        Ok(None)
    }

    fn set(&self, secret: &str) -> Result<(), SecretStoreError> {
        match self.vault.set(secret) {
            Ok(()) => {
                // A key stored while the vault was locked must not shadow it.
                if let Some(path) = &self.file {
                    let _ = remove_file(path);
                }
                self.record(CredentialBackend::NativeVault);
                Ok(())
            }
            Err(SecretStoreError::Unavailable) => {
                let path = self.file.as_deref().ok_or(SecretStoreError::Unavailable)?;
                write_file(path, secret)?;
                self.record(CredentialBackend::File);
                Ok(())
            }
            Err(error) => Err(error),
        }
    }

    fn delete(&self) -> Result<(), SecretStoreError> {
        let vault = self.vault.delete();
        let Some(path) = &self.file else {
            return vault;
        };
        remove_file(path)?;
        match vault {
            Ok(()) | Err(SecretStoreError::Unavailable) => Ok(()),
            Err(error) => Err(error),
        }
    }

    fn backend(&self) -> CredentialBackend {
        self.last
            .lock()
            .map_or(CredentialBackend::NativeVault, |last| *last)
    }
}

fn read_file(path: &Path) -> Result<Option<String>, SecretStoreError> {
    match std::fs::read_to_string(path) {
        Ok(secret) => {
            ensure_owner_only(path)?;
            let secret = secret.trim().to_string();
            Ok((!secret.is_empty()).then_some(secret))
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(_) => Err(SecretStoreError::Unavailable),
    }
}

#[cfg(unix)]
fn ensure_owner_only(path: &Path) -> Result<(), SecretStoreError> {
    use std::os::unix::fs::PermissionsExt;
    let mode = std::fs::metadata(path)
        .map_err(|_| SecretStoreError::Unavailable)?
        .permissions()
        .mode();
    if mode & 0o077 == 0 {
        Ok(())
    } else {
        Err(SecretStoreError::InsecureFile)
    }
}

// Windows files are written with the owner-only protected DACL by
// `fs_private`; there is no mode bit to check.
#[cfg(not(unix))]
fn ensure_owner_only(_path: &Path) -> Result<(), SecretStoreError> {
    Ok(())
}

fn write_file(path: &Path, secret: &str) -> Result<(), SecretStoreError> {
    if let Some(dir) = path.parent() {
        create_private_dir(dir).map_err(|_| SecretStoreError::Unavailable)?;
    }
    crate::fs_private::write_private_atomic(path, secret.as_bytes())
        .map_err(|_| SecretStoreError::Unavailable)
}

#[cfg(unix)]
fn create_private_dir(dir: &Path) -> std::io::Result<()> {
    use std::os::unix::fs::{DirBuilderExt, PermissionsExt};
    std::fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(dir)?;
    std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700))
}

#[cfg(not(unix))]
fn create_private_dir(dir: &Path) -> std::io::Result<()> {
    std::fs::create_dir_all(dir)
}

fn remove_file(path: &Path) -> Result<(), SecretStoreError> {
    match std::fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(_) => Err(SecretStoreError::Unavailable),
    }
}

#[cfg(test)]
#[path = "credential_chain_tests.rs"]
mod tests;
