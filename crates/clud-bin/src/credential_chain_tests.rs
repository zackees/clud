use super::*;

/// A native vault that is never usable: a headless host with no unlocked
/// Secret Service (#1891).
struct LockedVault;

impl SecretStore for LockedVault {
    fn get(&self) -> Result<Option<String>, SecretStoreError> {
        Err(SecretStoreError::Unavailable)
    }
    fn set(&self, _secret: &str) -> Result<(), SecretStoreError> {
        Err(SecretStoreError::Unavailable)
    }
    fn delete(&self) -> Result<(), SecretStoreError> {
        Err(SecretStoreError::Unavailable)
    }
}

#[derive(Default)]
struct WorkingVault(Mutex<Option<String>>);

impl SecretStore for WorkingVault {
    fn get(&self) -> Result<Option<String>, SecretStoreError> {
        Ok(self.0.lock().unwrap().clone())
    }
    fn set(&self, secret: &str) -> Result<(), SecretStoreError> {
        *self.0.lock().unwrap() = Some(secret.to_string());
        Ok(())
    }
    fn delete(&self) -> Result<(), SecretStoreError> {
        *self.0.lock().unwrap() = None;
        Ok(())
    }
}

const KEY: &str = "sk-or-v1-0123456789abcdef";

fn no_env(_: &str) -> Option<String> {
    None
}

fn env_has_key(name: &str) -> Option<String> {
    (name == "OPENROUTER_API_KEY").then(|| KEY.to_string())
}

fn chain<V: SecretStore>(
    vault: V,
    dir: &tempfile::TempDir,
    lookup: fn(&str) -> Option<String>,
) -> FallbackSecretStore<V> {
    FallbackSecretStore::new(
        vault,
        "clud.openrouter",
        "api-key-v1",
        Some(dir.path().join("credentials").join("openrouter.secret")),
        Some("OPENROUTER_API_KEY"),
    )
    .with_env_lookup(lookup)
}

#[test]
fn locked_vault_without_a_fallback_still_fails_as_before() {
    let store = FallbackSecretStore::new(LockedVault, "s", "a", None, None);
    assert_eq!(store.get(), Err(SecretStoreError::Unavailable));
    assert_eq!(store.set(KEY), Err(SecretStoreError::Unavailable));
}

#[test]
fn locked_vault_login_status_and_launch_use_the_file() {
    let dir = tempfile::tempdir().unwrap();
    let store = chain(LockedVault, &dir, no_env);
    assert_eq!(store.get(), Ok(None), "nothing stored is not an error");
    store.set(KEY).unwrap();
    assert_eq!(store.backend(), CredentialBackend::File);
    let fresh = chain(LockedVault, &dir, no_env);
    assert_eq!(fresh.get().unwrap().as_deref(), Some(KEY));
    assert_eq!(fresh.backend(), CredentialBackend::File);
    fresh.delete().unwrap();
    assert_eq!(fresh.get(), Ok(None));
}

#[cfg(unix)]
#[test]
fn fallback_file_and_dir_are_owner_only() {
    use std::os::unix::fs::PermissionsExt;
    let dir = tempfile::tempdir().unwrap();
    chain(LockedVault, &dir, no_env).set(KEY).unwrap();
    let file = dir.path().join("credentials").join("openrouter.secret");
    let mode = |p: &Path| std::fs::metadata(p).unwrap().permissions().mode() & 0o777;
    assert_eq!(mode(&file), 0o600);
    assert_eq!(mode(file.parent().unwrap()), 0o700);
}

#[cfg(unix)]
#[test]
fn a_group_readable_file_is_refused() {
    use std::os::unix::fs::PermissionsExt;
    let dir = tempfile::tempdir().unwrap();
    let store = chain(LockedVault, &dir, no_env);
    store.set(KEY).unwrap();
    let file = dir.path().join("credentials").join("openrouter.secret");
    std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o644)).unwrap();
    assert_eq!(store.get(), Err(SecretStoreError::InsecureFile));
}

#[test]
fn env_var_is_a_read_only_last_resort() {
    let dir = tempfile::tempdir().unwrap();
    let store = chain(LockedVault, &dir, env_has_key);
    assert_eq!(store.get().unwrap().as_deref(), Some(KEY));
    assert_eq!(store.backend(), CredentialBackend::Env);
    assert!(
        !dir.path().join("credentials").exists(),
        "an env key is never written to disk"
    );
}

#[test]
fn a_working_vault_wins_and_clears_a_stale_file() {
    let dir = tempfile::tempdir().unwrap();
    chain(LockedVault, &dir, no_env)
        .set("sk-or-v1-stalestalestale")
        .unwrap();
    let store = chain(WorkingVault::default(), &dir, env_has_key);
    // The vault is empty, so the file copy still serves the read...
    assert_eq!(
        store.get().unwrap().as_deref(),
        Some("sk-or-v1-stalestalestale")
    );
    // ...until a login lands in the vault, which removes the file copy.
    store.set(KEY).unwrap();
    assert_eq!(store.backend(), CredentialBackend::NativeVault);
    assert!(!dir
        .path()
        .join("credentials")
        .join("openrouter.secret")
        .exists());
    assert_eq!(store.get().unwrap().as_deref(), Some(KEY));
    assert_eq!(store.backend(), CredentialBackend::NativeVault);
}
