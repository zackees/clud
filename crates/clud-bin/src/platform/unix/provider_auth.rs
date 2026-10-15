//! Unix half of `crate::provider_auth`: API keys are stored through the
//! `keyring` crate (macOS Keychain, Linux Secret Service).

use zeroize::Zeroizing;

use crate::provider_auth::SecretStoreError;

pub fn get(
    service: &'static str,
    account: &'static str,
) -> Result<Option<String>, SecretStoreError> {
    with_native_vault(service, account, |entry| match entry.get_password() {
        Ok(secret) => Ok(Some(secret)),
        Err(keyring::Error::NoEntry) => Ok(None),
        Err(_) => Err(SecretStoreError::Unavailable),
    })
}

pub fn set(
    service: &'static str,
    account: &'static str,
    secret: &str,
) -> Result<(), SecretStoreError> {
    let secret = Zeroizing::new(secret.to_owned());
    with_native_vault(service, account, move |entry| {
        entry
            .set_password(&secret)
            .map_err(|_| SecretStoreError::Unavailable)
    })
}

pub fn delete(service: &'static str, account: &'static str) -> Result<(), SecretStoreError> {
    with_native_vault(service, account, |entry| match entry.delete_credential() {
        Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
        Err(_) => Err(SecretStoreError::Unavailable),
    })
}

fn with_native_vault<T: Send + 'static>(
    service: &'static str,
    account: &'static str,
    operation: impl FnOnce(keyring::Entry) -> Result<T, SecretStoreError> + Send + 'static,
) -> Result<T, SecretStoreError> {
    let worker = std::thread::Builder::new()
        .name("clud-vault".to_string())
        .stack_size(4 * 1024 * 1024)
        .spawn(move || {
            let entry =
                keyring::Entry::new(service, account).map_err(|_| SecretStoreError::Unavailable)?;
            operation(entry)
        })
        .map_err(|_| SecretStoreError::Unavailable)?;
    worker.join().unwrap_or(Err(SecretStoreError::Unavailable))
}
