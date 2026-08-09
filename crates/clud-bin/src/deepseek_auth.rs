//! Native-vault DeepSeek API-key management (issue #877).
//!
//! The API key is deliberately never serialized, rendered, or accepted on the
//! command line. Production uses the operating system credential vault; tests
//! inject an in-memory [`SecretStore`] fake instead.

use std::fmt;
use std::io::{self, Write};

use crossterm::event::{self, Event, KeyCode, KeyModifiers};
use crossterm::terminal;
use zeroize::Zeroizing;

use crate::args::DeepseekAuthSubcommand;

#[cfg(not(windows))]
const SERVICE_NAME: &str = "clud.deepseek";
#[cfg(not(windows))]
const ACCOUNT_NAME: &str = "api-key-v1";

/// A non-secret classification of a credential-vault failure.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SecretStoreError {
    Unavailable,
}

impl fmt::Display for SecretStoreError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Unavailable => formatter.write_str("the native credential vault is unavailable"),
        }
    }
}

impl std::error::Error for SecretStoreError {}

/// Minimal secret-store boundary. It intentionally exposes no enumeration,
/// serialization, or debug surface for secret values.
pub trait SecretStore {
    fn get(&self) -> Result<Option<String>, SecretStoreError>;
    fn set(&self, secret: &str) -> Result<(), SecretStoreError>;
    fn delete(&self) -> Result<(), SecretStoreError>;
}

/// Production adapter for the OS-native encrypted credential vault.
pub struct NativeSecretStore;

impl NativeSecretStore {
    pub fn new() -> Result<Self, SecretStoreError> {
        Ok(Self)
    }
}

#[cfg(not(windows))]
fn with_native_vault<T: Send + 'static>(
    operation: impl FnOnce(keyring::Entry) -> Result<T, SecretStoreError> + Send + 'static,
) -> Result<T, SecretStoreError> {
    let worker = std::thread::Builder::new()
        .name("clud-deepseek-vault".to_string())
        .stack_size(4 * 1024 * 1024)
        .spawn(move || {
            let entry = keyring::Entry::new(SERVICE_NAME, ACCOUNT_NAME)
                .map_err(|_| SecretStoreError::Unavailable)?;
            operation(entry)
        })
        .map_err(|_| SecretStoreError::Unavailable)?;
    worker.join().unwrap_or(Err(SecretStoreError::Unavailable))
}

#[cfg(windows)]
mod windows_vault {
    use std::slice;

    use std::ffi::c_void;

    use zeroize::Zeroizing;

    const CRED_TYPE_GENERIC: u32 = 1;
    const CRED_PERSIST_LOCAL_MACHINE: u32 = 2;
    const ERROR_NOT_FOUND: u32 = 1168;

    #[repr(C)]
    struct FileTime {
        low: u32,
        high: u32,
    }

    #[repr(C)]
    struct Credential {
        flags: u32,
        credential_type: u32,
        target_name: *mut u16,
        comment: *mut u16,
        last_written: FileTime,
        blob_size: u32,
        blob: *mut u8,
        persist: u32,
        attribute_count: u32,
        attributes: *mut c_void,
        target_alias: *mut u16,
        user_name: *mut u16,
    }

    #[link(name = "Advapi32")]
    unsafe extern "system" {
        fn CredReadW(
            target: *const u16,
            credential_type: u32,
            flags: u32,
            credential: *mut *mut Credential,
        ) -> i32;
        fn CredWriteW(credential: *const Credential, flags: u32) -> i32;
        fn CredDeleteW(target: *const u16, credential_type: u32, flags: u32) -> i32;
        fn CredFree(buffer: *const c_void);
    }

    unsafe extern "system" {
        fn GetLastError() -> u32;
    }

    use super::SecretStoreError;

    const TARGET: &str = "clud.deepseek/api-key-v1";

    fn wide(value: &str) -> Vec<u16> {
        value.encode_utf16().chain(Some(0)).collect()
    }

    fn is_missing() -> bool {
        // SAFETY: GetLastError reads the current thread's Win32 failure code
        // immediately after a Credential Manager call has returned false.
        unsafe { GetLastError() == ERROR_NOT_FOUND }
    }

    pub fn get() -> Result<Option<String>, SecretStoreError> {
        let target = wide(TARGET);
        let mut credential = std::ptr::null_mut();
        // SAFETY: `target` is NUL-terminated and Windows initializes the output
        // pointer only on success, which we free after copying the blob.
        let result = unsafe { CredReadW(target.as_ptr(), CRED_TYPE_GENERIC, 0, &mut credential) };
        if result == 0 {
            return if is_missing() {
                Ok(None)
            } else {
                Err(SecretStoreError::Unavailable)
            };
        }
        // SAFETY: `credential` was allocated by CredReadW. Its blob is valid
        // for `CredentialBlobSize` bytes until the matching CredFree below.
        let secret = unsafe {
            let bytes = slice::from_raw_parts((*credential).blob, (*credential).blob_size as usize);
            String::from_utf8(bytes.to_vec()).map_err(|_| SecretStoreError::Unavailable)
        };
        // SAFETY: the pointer came from CredReadW and is freed exactly once.
        unsafe { CredFree(credential.cast::<std::ffi::c_void>()) };
        secret.map(Some)
    }

    pub fn set(secret: &str) -> Result<(), SecretStoreError> {
        let mut target = wide(TARGET);
        let mut user = wide("clud");
        let mut blob = Zeroizing::new(secret.as_bytes().to_vec());
        // SAFETY: CREDENTIALW is a Win32 C struct containing only integer,
        // pointer, and FILETIME fields, all of which permit zero initialization.
        // The fields Credential Manager requires are set below before use.
        let mut credential: Credential = unsafe { std::mem::zeroed() };
        credential.credential_type = CRED_TYPE_GENERIC;
        credential.target_name = target.as_mut_ptr();
        credential.blob_size = blob
            .len()
            .try_into()
            .map_err(|_| SecretStoreError::Unavailable)?;
        credential.blob = blob.as_mut_ptr();
        credential.persist = CRED_PERSIST_LOCAL_MACHINE;
        credential.user_name = user.as_mut_ptr();
        // SAFETY: every pointer in `credential` remains live for this call.
        if unsafe { CredWriteW(&credential, 0) } == 0 {
            Err(SecretStoreError::Unavailable)
        } else {
            Ok(())
        }
    }

    pub fn delete() -> Result<(), SecretStoreError> {
        let target = wide(TARGET);
        // SAFETY: `target` is NUL-terminated and lives for this call.
        if unsafe { CredDeleteW(target.as_ptr(), CRED_TYPE_GENERIC, 0) } != 0 || is_missing() {
            Ok(())
        } else {
            Err(SecretStoreError::Unavailable)
        }
    }
}

impl SecretStore for NativeSecretStore {
    fn get(&self) -> Result<Option<String>, SecretStoreError> {
        #[cfg(windows)]
        return windows_vault::get();
        #[cfg(not(windows))]
        with_native_vault(|entry| match entry.get_password() {
            Ok(secret) => Ok(Some(secret)),
            Err(keyring::Error::NoEntry) => Ok(None),
            Err(_) => Err(SecretStoreError::Unavailable),
        })
    }

    fn set(&self, secret: &str) -> Result<(), SecretStoreError> {
        #[cfg(windows)]
        return windows_vault::set(secret);
        #[cfg(not(windows))]
        {
            let secret = Zeroizing::new(secret.to_owned());
            with_native_vault(move |entry| {
                entry
                    .set_password(&secret)
                    .map_err(|_| SecretStoreError::Unavailable)
            })
        }
    }

    fn delete(&self) -> Result<(), SecretStoreError> {
        #[cfg(windows)]
        return windows_vault::delete();
        #[cfg(not(windows))]
        with_native_vault(|entry| match entry.delete_credential() {
            Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
            Err(_) => Err(SecretStoreError::Unavailable),
        })
    }
}

pub fn run(subcommand: &DeepseekAuthSubcommand) -> i32 {
    let store = match NativeSecretStore::new() {
        Ok(store) => store,
        Err(error) => {
            eprintln!("deepseek-auth: {error}; retry after unlocking the vault");
            return 2;
        }
    };
    let mut stdout = io::stdout().lock();
    run_with(subcommand, &store, &mut stdout, prompt_secret)
}

/// Read a secret from the terminal without echoing typed characters.
fn prompt_secret() -> Result<String, ()> {
    eprint!("DeepSeek API key: ");
    io::stderr().flush().map_err(|_| ())?;
    terminal::enable_raw_mode().map_err(|_| ())?;
    let result = (|| {
        let mut secret = String::new();
        loop {
            match event::read().map_err(|_| ())? {
                Event::Key(key) if key.kind.is_press() => match key.code {
                    KeyCode::Enter => return Ok(secret),
                    KeyCode::Esc => return Err(()),
                    KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                        return Err(())
                    }
                    KeyCode::Backspace => {
                        secret.pop();
                    }
                    KeyCode::Char(character) => secret.push(character),
                    _ => {}
                },
                _ => {}
            }
        }
    })();
    let _ = terminal::disable_raw_mode();
    eprintln!();
    result
}

fn run_with(
    subcommand: &DeepseekAuthSubcommand,
    store: &dyn SecretStore,
    stdout: &mut dyn Write,
    read_secret: impl FnOnce() -> Result<String, ()>,
) -> i32 {
    match subcommand {
        DeepseekAuthSubcommand::Login => {
            let secret = match read_secret() {
                Ok(secret) if !secret.trim().is_empty() => Zeroizing::new(secret),
                _ => {
                    eprintln!("deepseek-auth: no API key entered; nothing was stored");
                    return 2;
                }
            };
            match store.set(&secret) {
                Ok(()) => {
                    let _ = writeln!(
                        stdout,
                        "DeepSeek API key stored in the native credential vault"
                    );
                    0
                }
                Err(error) => {
                    eprintln!("deepseek-auth: {error}; retry after unlocking the vault");
                    2
                }
            }
        }
        DeepseekAuthSubcommand::Status { json } => match store.get() {
            Ok(Some(_)) if *json => {
                let _ = writeln!(
                    stdout,
                    "{}",
                    serde_json::json!({"source": "native_vault", "configured": true})
                );
                0
            }
            Ok(Some(_)) => {
                let _ = writeln!(
                    stdout,
                    "source: native credential vault\nstatus: configured"
                );
                0
            }
            Ok(None) if *json => {
                let _ = writeln!(
                    stdout,
                    "{}",
                    serde_json::json!({"source": "native_vault", "configured": false})
                );
                1
            }
            Ok(None) => {
                let _ = writeln!(
                    stdout,
                    "source: native credential vault\nstatus: login required"
                );
                1
            }
            Err(error) => {
                eprintln!("deepseek-auth: {error}; retry after unlocking the vault");
                2
            }
        },
        DeepseekAuthSubcommand::Logout { json } => match store.delete() {
            Ok(()) if *json => {
                let _ = writeln!(stdout, "{}", serde_json::json!({"removed": true}));
                0
            }
            Ok(()) => {
                let _ = writeln!(
                    stdout,
                    "DeepSeek API key removed from the native credential vault"
                );
                0
            }
            Err(error) => {
                eprintln!("deepseek-auth: {error}; retry after unlocking the vault");
                2
            }
        },
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use super::*;

    #[derive(Default)]
    struct InMemorySecretStore {
        secret: Mutex<Option<String>>,
        unavailable: bool,
    }

    impl SecretStore for InMemorySecretStore {
        fn get(&self) -> Result<Option<String>, SecretStoreError> {
            if self.unavailable {
                return Err(SecretStoreError::Unavailable);
            }
            Ok(self.secret.lock().unwrap().clone())
        }

        fn set(&self, secret: &str) -> Result<(), SecretStoreError> {
            if self.unavailable {
                return Err(SecretStoreError::Unavailable);
            }
            *self.secret.lock().unwrap() = Some(secret.to_string());
            Ok(())
        }

        fn delete(&self) -> Result<(), SecretStoreError> {
            if self.unavailable {
                return Err(SecretStoreError::Unavailable);
            }
            *self.secret.lock().unwrap() = None;
            Ok(())
        }
    }

    #[test]
    fn login_status_and_logout_use_only_the_injected_store() {
        let store = InMemorySecretStore::default();
        let mut output = Vec::new();
        let secret = "ds-test-secret";

        assert_eq!(
            run_with(&DeepseekAuthSubcommand::Login, &store, &mut output, || Ok(
                secret.to_string()
            ),),
            0
        );
        assert!(!String::from_utf8_lossy(&output).contains(secret));
        assert_eq!(store.get().unwrap().as_deref(), Some(secret));

        output.clear();
        assert_eq!(
            run_with(
                &DeepseekAuthSubcommand::Status { json: true },
                &store,
                &mut output,
                || unreachable!(),
            ),
            0
        );
        assert_eq!(
            String::from_utf8(output).unwrap(),
            "{\"configured\":true,\"source\":\"native_vault\"}\n"
        );

        let mut output = Vec::new();
        assert_eq!(
            run_with(
                &DeepseekAuthSubcommand::Logout { json: false },
                &store,
                &mut output,
                || unreachable!(),
            ),
            0
        );
        assert_eq!(store.get().unwrap(), None);
    }

    #[test]
    fn login_rejects_empty_input_without_storing_anything() {
        let store = InMemorySecretStore::default();
        let mut output = Vec::new();
        assert_eq!(
            run_with(&DeepseekAuthSubcommand::Login, &store, &mut output, || Ok(
                "   ".to_string()
            ),),
            2
        );
        assert_eq!(store.get().unwrap(), None);
    }

    #[test]
    fn status_reports_login_required_when_nothing_is_stored() {
        let store = InMemorySecretStore::default();
        let mut output = Vec::new();
        assert_eq!(
            run_with(
                &DeepseekAuthSubcommand::Status { json: false },
                &store,
                &mut output,
                || unreachable!(),
            ),
            1
        );
        assert_eq!(
            String::from_utf8(output).unwrap(),
            "source: native credential vault\nstatus: login required\n"
        );
    }

    #[test]
    fn unavailable_vault_is_sanitized() {
        let store = InMemorySecretStore {
            unavailable: true,
            ..InMemorySecretStore::default()
        };
        let mut output = Vec::new();
        assert_eq!(
            run_with(
                &DeepseekAuthSubcommand::Status { json: false },
                &store,
                &mut output,
                || unreachable!(),
            ),
            2
        );
        assert!(output.is_empty());
    }
}
