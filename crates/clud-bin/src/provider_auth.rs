//! Native-vault DeepSeek API-key management (issue #877).
//!
//! The API key is deliberately never serialized, rendered, or accepted on the
//! command line. Production uses the operating system credential vault; tests
//! inject an in-memory [`SecretStore`] fake instead.

use std::fmt;
use std::io::{self, Read, Write};
use std::time::Duration;

use crossterm::event::{self, Event, KeyCode, KeyEvent, KeyModifiers};
use crossterm::terminal;
use zeroize::Zeroizing;

use crate::args::{Args, Command, DeepseekAuthSubcommand};
use crate::backend::{Backend, ModelProvider};
use crate::command;
use crate::provider_registry::{self, AnthropicCompatProvider};
use crate::secret_redaction::mask_key;

/// DeepSeek's vault identifiers. Changing either literal orphans every
/// existing user's stored key: on non-Windows this is the `keyring` service
/// and account, and on Windows it is the two halves of the Credential
/// Manager target name (`{service}/{account}`, see [`vault_target`]).
pub const DEEPSEEK_VAULT_SERVICE: &str = "clud.deepseek";
pub const DEEPSEEK_VAULT_ACCOUNT: &str = "api-key-v1";

/// Kimi's vault identifiers (#937 Phase 3). Deliberately distinct from
/// DeepSeek's `vault_service` -- this is what gives the two providers
/// isolated credential records even though both use the `"api-key-v1"`
/// account name. Same continuity guarantee as the DeepSeek constants above:
/// changing either literal orphans every already-stored Kimi key.
pub const KIMI_VAULT_SERVICE: &str = "clud.kimi";
pub const KIMI_VAULT_ACCOUNT: &str = "api-key-v1";

/// OpenRouter uses the same vault-backed lifecycle as DeepSeek and Kimi, but
/// its credential is a distinct service record and is never interchangeable
/// with either provider's key.
pub const OPENROUTER_VAULT_SERVICE: &str = "clud.openrouter";
pub const OPENROUTER_VAULT_ACCOUNT: &str = "api-key-v1";

/// Composes the vault target identifier from a service and account. Shared
/// (not `cfg(windows)`-gated) so the identifier-freeze test can assert the
/// exact composition on every platform, even though only the Windows
/// Credential Manager path consumes it at runtime.
#[cfg_attr(not(windows), allow(dead_code))]
fn vault_target(service: &str, account: &str) -> String {
    format!("{service}/{account}")
}

/// A non-secret classification of a credential-vault failure.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SecretStoreError {
    Unavailable,
    Malformed,
}

impl fmt::Display for SecretStoreError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Unavailable => formatter.write_str("the native credential vault is unavailable"),
            Self::Malformed => {
                formatter.write_str("API key is malformed; re-enter a key without whitespace")
            }
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

/// Production adapter for the OS-native encrypted credential vault,
/// parameterized on the vault identifiers so multiple providers (DeepSeek,
/// and Kimi in Phase 2) can each hold a distinct record with no shared
/// global state.
pub struct NativeSecretStore {
    service: &'static str,
    account: &'static str,
}

impl NativeSecretStore {
    /// DeepSeek-scoped convenience constructor. Kept with its exact prior
    /// signature and behavior because it has call sites outside this file
    /// (`auth.rs`, `foreground_runtime.rs`) that this phase does not touch.
    /// Phase 2 migrates those external call sites to [`Self::new_for`].
    pub fn new() -> Result<Self, SecretStoreError> {
        Self::new_for(DEEPSEEK_VAULT_SERVICE, DEEPSEEK_VAULT_ACCOUNT)
    }

    /// General constructor taking explicit vault identifiers. Two instances
    /// built with different identifiers are fully independent records.
    pub fn new_for(service: &'static str, account: &'static str) -> Result<Self, SecretStoreError> {
        Ok(Self { service, account })
    }
}

/// Test-only vault (#901): with `CLUD_INTEGRATION_TESTS=1` and
/// `CLUD_TEST_SECRET_STORE_DIR` set in a *debug* build, secrets live in
/// owner-only files in that directory instead of the OS keyring, so an
/// integration test can run `clud auth login/status/logout` through the real
/// binary on a CI runner that has no keyring. Release builds never honour it.
pub const TEST_SECRET_STORE_DIR_ENV: &str = "CLUD_TEST_SECRET_STORE_DIR";

pub fn test_vault_dir() -> Option<std::path::PathBuf> {
    resolve_test_vault_dir(
        cfg!(debug_assertions),
        std::env::var_os("CLUD_INTEGRATION_TESTS").is_some_and(|value| value == "1"),
        std::env::var_os(TEST_SECRET_STORE_DIR_ENV),
    )
}

/// True when the test vault replaces the native one for this process.
pub fn test_vault_active() -> bool {
    test_vault_dir().is_some()
}

fn resolve_test_vault_dir(
    debug_build: bool,
    integration: bool,
    value: Option<std::ffi::OsString>,
) -> Option<std::path::PathBuf> {
    (debug_build && integration)
        .then_some(value?)
        .filter(|value| !value.is_empty())
        .map(std::path::PathBuf::from)
}

fn test_vault_path(dir: &std::path::Path, service: &str, account: &str) -> std::path::PathBuf {
    let safe = |text: &str| -> String {
        text.chars()
            .map(|c| if c.is_ascii_alphanumeric() { c } else { '_' })
            .collect()
    };
    dir.join(format!("{}--{}.secret", safe(service), safe(account)))
}

#[cfg(not(windows))]
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

    // Lowercase: xwin's vendored SDK (used by the cross-compiled CI build)
    // normalizes lib filenames to lowercase, and lld-link on a case-sensitive
    // host filesystem fails to find "Advapi32.lib". Native MSVC builds on
    // Windows work with either case since NTFS is case-insensitive.
    #[link(name = "advapi32")]
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

    fn wide(value: &str) -> Vec<u16> {
        value.encode_utf16().chain(Some(0)).collect()
    }

    fn is_missing() -> bool {
        // SAFETY: GetLastError reads the current thread's Win32 failure code
        // immediately after a Credential Manager call has returned false.
        unsafe { GetLastError() == ERROR_NOT_FOUND }
    }

    pub fn get(target: &str) -> Result<Option<String>, SecretStoreError> {
        let target = wide(target);
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

    pub fn set(target: &str, secret: &str) -> Result<(), SecretStoreError> {
        let mut target = wide(target);
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

    pub fn delete(target: &str) -> Result<(), SecretStoreError> {
        let target = wide(target);
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
        if let Some(dir) = test_vault_dir() {
            return match std::fs::read_to_string(test_vault_path(&dir, self.service, self.account))
            {
                Ok(secret) => Ok(Some(secret)),
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
                Err(_) => Err(SecretStoreError::Unavailable),
            };
        }
        #[cfg(windows)]
        return windows_vault::get(&vault_target(self.service, self.account));
        #[cfg(not(windows))]
        with_native_vault(self.service, self.account, |entry| {
            match entry.get_password() {
                Ok(secret) => Ok(Some(secret)),
                Err(keyring::Error::NoEntry) => Ok(None),
                Err(_) => Err(SecretStoreError::Unavailable),
            }
        })
    }

    fn set(&self, secret: &str) -> Result<(), SecretStoreError> {
        if let Some(dir) = test_vault_dir() {
            return crate::fs_private::write_private_atomic(
                &test_vault_path(&dir, self.service, self.account),
                secret.as_bytes(),
            )
            .map_err(|_| SecretStoreError::Unavailable);
        }
        #[cfg(windows)]
        return windows_vault::set(&vault_target(self.service, self.account), secret);
        #[cfg(not(windows))]
        {
            let secret = Zeroizing::new(secret.to_owned());
            with_native_vault(self.service, self.account, move |entry| {
                entry
                    .set_password(&secret)
                    .map_err(|_| SecretStoreError::Unavailable)
            })
        }
    }

    fn delete(&self) -> Result<(), SecretStoreError> {
        if let Some(dir) = test_vault_dir() {
            return match std::fs::remove_file(test_vault_path(&dir, self.service, self.account)) {
                Ok(()) => Ok(()),
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
                Err(_) => Err(SecretStoreError::Unavailable),
            };
        }
        #[cfg(windows)]
        return windows_vault::delete(&vault_target(self.service, self.account));
        #[cfg(not(windows))]
        with_native_vault(self.service, self.account, |entry| {
            match entry.delete_credential() {
                Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
                Err(_) => Err(SecretStoreError::Unavailable),
            }
        })
    }
}

/// Sanitized failure from launch-time credential preflight.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PreflightError {
    Missing,
    Unavailable,
    Cancelled,
    Malformed {
        fingerprint: String,
    },
    Rejected {
        status: u16,
        message: String,
        masked: String,
        fingerprint: String,
    },
}

impl PreflightError {
    /// Sanitized, provider-specific failure message shown to the user.
    /// `Missing` and `Cancelled` name the provider by its descriptor's
    /// `display_name`, and `Missing` points at its exact `login_command`
    /// rather than a hardcoded one -- this is what lets a second
    /// Anthropic-compat provider reuse this error type verbatim.
    pub fn describe(self, descriptor: &AnthropicCompatProvider) -> String {
        match self {
            Self::Missing => format!(
                "{} credentials are not configured; run `{}`",
                descriptor.display_name, descriptor.login_command
            ),
            Self::Unavailable => {
                "the native credential vault is unavailable; retry after unlocking it".to_string()
            }
            Self::Cancelled => {
                format!("{} credential entry was cancelled", descriptor.display_name)
            }
            Self::Malformed { fingerprint } => format!(
                "{} stored API key is malformed ({fingerprint}); re-enter it with {}",
                descriptor.display_name, descriptor.login_command
            ),
            Self::Rejected {
                status,
                message,
                masked,
                fingerprint,
            } => format!(
                "{} rejected this key ({status} {message}; stored key {masked}, {fingerprint}); run {}",
                descriptor.display_name, descriptor.login_command
            ),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum ProbeOutcome {
    Accepted,
    Rejected { status: u16, message: String },
    Unavailable,
}

fn key_fingerprint(key: &str) -> String {
    use sha2::{Digest, Sha256};
    let digest = format!("{:x}", Sha256::digest(key.as_bytes()));
    format!("{} chars, sha256 {}", key.chars().count(), &digest[..12])
}

fn stored_key_is_well_formed(key: &str) -> bool {
    key == key.trim() && crate::args::looks_like_api_key(key)
}

/// Read-only presence check for bare-launch routing. This does not probe the
/// network: the ordinary launch preflight remains responsible for rejection.
pub fn has_well_formed_stored_key(
    descriptor: &'static AnthropicCompatProvider,
) -> Result<bool, SecretStoreError> {
    let store = NativeSecretStore::new_for(descriptor.vault_service, descriptor.vault_account)?;
    let key = store.get()?.map(Zeroizing::new);
    Ok(key
        .as_deref()
        .is_some_and(|key| stored_key_is_well_formed(key)))
}

fn sanitize_provider_message(message: &str, key: &str) -> String {
    let masked = mask_key(key);
    let without_exact_key = message.replace(key, &masked);
    let credential_pattern =
        regex::Regex::new(r"sk-[A-Za-z0-9_-]{8,}").expect("static API-key redaction pattern");
    let redacted = credential_pattern
        .replace_all(&without_exact_key, masked.as_str())
        .into_owned();
    redacted
        .chars()
        .filter(|character| !character.is_control())
        .take(200)
        .collect()
}

fn probe_provider_key(descriptor: &AnthropicCompatProvider, key: &str) -> ProbeOutcome {
    let url = descriptor.credential_probe_url;
    let secret = Zeroizing::new(key.to_owned());
    probe_with_deadline(Duration::from_secs(5), move || probe_key_at(url, &secret))
}

fn probe_with_deadline(
    timeout: Duration,
    probe: impl FnOnce() -> ProbeOutcome + Send + 'static,
) -> ProbeOutcome {
    let (sender, receiver) = std::sync::mpsc::channel();
    if std::thread::Builder::new()
        .name("clud-credential-probe".to_string())
        .spawn(move || {
            let _ = sender.send(probe());
        })
        .is_err()
    {
        return ProbeOutcome::Unavailable;
    }
    receiver
        .recv_timeout(timeout)
        .unwrap_or(ProbeOutcome::Unavailable)
}

fn probe_key_at(url: &str, key: &str) -> ProbeOutcome {
    let agent = ureq::AgentBuilder::new()
        .timeout(Duration::from_secs(5))
        .redirects(0)
        .build();
    let result = agent
        .get(url)
        .set("Authorization", &format!("Bearer {key}"))
        .call();
    match result {
        Ok(response) if response.status() == 200 => ProbeOutcome::Accepted,
        Err(ureq::Error::Status(status @ (401 | 403), response)) => {
            let mut body = String::new();
            let _ = response.into_reader().take(4096).read_to_string(&mut body);
            let message = serde_json::from_str::<serde_json::Value>(&body)
                .ok()
                .and_then(|json| {
                    json.pointer("/error/message")
                        .and_then(|value| value.as_str())
                        .map(str::to_owned)
                })
                .unwrap_or_else(|| {
                    body.lines()
                        .next()
                        .unwrap_or("authentication failed")
                        .to_string()
                });
            ProbeOutcome::Rejected {
                status,
                message: sanitize_provider_message(&message, key),
            }
        }
        _ => ProbeOutcome::Unavailable,
    }
}

/// Store an API key passed on the command line (`clud --deepseek <API_KEY>`)
/// in `descriptor`'s native-vault record. Returns whether the stored value
/// changed, so the caller only announces a real update.
pub fn store_inline_api_key(
    descriptor: &'static AnthropicCompatProvider,
    key: &str,
) -> Result<bool, SecretStoreError> {
    let store = NativeSecretStore::new_for(descriptor.vault_service, descriptor.vault_account)?;
    store_inline_api_key_with(&store, key)
}

fn store_inline_api_key_with(store: &dyn SecretStore, key: &str) -> Result<bool, SecretStoreError> {
    if !stored_key_is_well_formed(key) {
        return Err(SecretStoreError::Malformed);
    }
    if store.get()?.as_deref() == Some(key) {
        return Ok(false);
    }
    store.set(key)?;
    Ok(true)
}

/// Returns the descriptor of the provider needing a credential preflight
/// before this launch may proceed, or `None` when no preflight applies:
/// `--dry-run` (which must make zero vault calls) or a provider with no
/// Anthropic-compat descriptor (Claude, Codex).
pub fn launch_preflight_target(
    provider: ModelProvider,
    dry_run: bool,
) -> Option<&'static AnthropicCompatProvider> {
    if dry_run {
        return None;
    }
    provider_registry::descriptor_for(provider)
}

/// Whether a DeepSeek launch may prompt for a missing key. Takes the terminal
/// checks as parameters (rather than calling `IsTerminal` itself) so the
/// decision is unit-testable without a real tty.
pub fn launch_is_interactive(
    args: &Args,
    backend: Backend,
    stdin_is_terminal: bool,
    stderr_is_terminal: bool,
) -> bool {
    let repeat = matches!(
        &args.command,
        Some(Command::Loop {
            repeat: Some(_),
            ..
        })
    );
    stdin_is_terminal
        && stderr_is_terminal
        && !command::has_noninteractive_prompt(args, backend)
        && !args.detach
        && !args.detachable
        && !repeat
}

/// Preflight the native vault immediately before accepting a launch for
/// `descriptor`'s provider. A missing key may be entered only for a truly
/// interactive foreground launch.
pub fn preflight_native(
    descriptor: &'static AnthropicCompatProvider,
    interactive: bool,
) -> Result<(), PreflightError> {
    let store = NativeSecretStore::new_for(descriptor.vault_service, descriptor.vault_account)
        .map_err(|_| PreflightError::Unavailable)?;
    preflight_checked_with(
        &store,
        interactive,
        || prompt_secret(descriptor.display_name),
        |key| probe_provider_key(descriptor, key),
    )
}

fn preflight_checked_with(
    store: &dyn SecretStore,
    interactive: bool,
    read_secret: impl FnOnce() -> Result<String, ()>,
    probe: impl FnOnce(&str) -> ProbeOutcome,
) -> Result<(), PreflightError> {
    let key = match store.get().map_err(|_| PreflightError::Unavailable)? {
        Some(key) => key,
        None if !interactive => return Err(PreflightError::Missing),
        None => {
            let key = read_secret().map_err(|_| PreflightError::Cancelled)?;
            if !stored_key_is_well_formed(&key) {
                return Err(PreflightError::Malformed {
                    fingerprint: key_fingerprint(&key),
                });
            }
            assess_key(&key, probe)?;
            store.set(&key).map_err(|_| PreflightError::Unavailable)?;
            return Ok(());
        }
    };
    assess_key(&key, probe)
}

fn assess_key(key: &str, probe: impl FnOnce(&str) -> ProbeOutcome) -> Result<(), PreflightError> {
    if !stored_key_is_well_formed(key) {
        return Err(PreflightError::Malformed {
            fingerprint: key_fingerprint(key),
        });
    }
    match probe(key) {
        ProbeOutcome::Accepted => Ok(()),
        ProbeOutcome::Rejected { status, message } => Err(PreflightError::Rejected {
            status,
            message: sanitize_provider_message(&message, key),
            masked: mask_key(key),
            fingerprint: key_fingerprint(key),
        }),
        ProbeOutcome::Unavailable => {
            eprintln!("[clud] warning: could not validate provider API key; continuing offline");
            Ok(())
        }
    }
}

#[cfg(test)]
fn preflight_with(
    store: &dyn SecretStore,
    interactive: bool,
    read_secret: impl FnOnce() -> Result<String, ()>,
) -> Result<(), PreflightError> {
    match store.get().map_err(|_| PreflightError::Unavailable)? {
        Some(_) => Ok(()),
        None if !interactive => Err(PreflightError::Missing),
        None => {
            let secret = read_secret()
                .ok()
                .filter(|secret| !secret.trim().is_empty())
                .map(Zeroizing::new)
                .ok_or(PreflightError::Cancelled)?;
            store.set(&secret).map_err(|_| PreflightError::Unavailable)
        }
    }
}

/// Runs an action-first auth subcommand for any Anthropic-compat provider,
/// built from `descriptor`'s vault identifiers and names rather than a
/// hardcoded provider. This is what lets a second provider (e.g. Kimi in a
/// later phase) reuse the exact same login/status/logout implementation.
pub fn run_for(
    descriptor: &'static AnthropicCompatProvider,
    subcommand: &DeepseekAuthSubcommand,
) -> i32 {
    let store = match NativeSecretStore::new_for(descriptor.vault_service, descriptor.vault_account)
    {
        Ok(store) => store,
        Err(error) => {
            eprintln!(
                "{}-auth: {error}; retry after unlocking the vault",
                descriptor.settings_id
            );
            return 2;
        }
    };
    let mut stdout = io::stdout().lock();
    run_with_probe(
        descriptor,
        subcommand,
        &store,
        &mut stdout,
        || prompt_secret(descriptor.display_name),
        |key| probe_provider_key(descriptor, key),
    )
}

/// DeepSeek-scoped delegate kept for its existing call sites: `main.rs`'s
/// `clud deepseek-auth` deprecated-alias dispatch. Behavior and output are
/// unchanged from before this module became provider-generic.
pub fn run(subcommand: &DeepseekAuthSubcommand) -> i32 {
    let descriptor = provider_registry::descriptor_for(ModelProvider::DeepSeek)
        .expect("DeepSeek has an Anthropic-compat descriptor");
    run_for(descriptor, subcommand)
}

/// Read a secret from the terminal while echoing only one asterisk per
/// accepted character. The typed characters themselves never reach stderr.
/// `display_name` names the provider prompted for (e.g. "DeepSeek", "Kimi")
/// so this one implementation serves every Anthropic-compat provider.
fn prompt_secret(display_name: &str) -> Result<String, ()> {
    eprint!("{display_name} API key: ");
    io::stderr().flush().map_err(|_| ())?;
    terminal::enable_raw_mode().map_err(|_| ())?;
    let result = (|| {
        let mut secret = String::new();
        loop {
            if let Event::Key(key) = event::read().map_err(|_| ())? {
                if !key.kind.is_press() {
                    continue;
                }
                match handle_secret_key(&mut secret, key, &mut io::stderr())? {
                    SecretInputAction::Continue => {}
                    SecretInputAction::Accept => return Ok(secret),
                    SecretInputAction::Cancel => return Err(()),
                }
            }
        }
    })();
    let _ = terminal::disable_raw_mode();
    eprintln!();
    result
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SecretInputAction {
    Continue,
    Accept,
    Cancel,
}

fn handle_secret_key(
    secret: &mut String,
    key: KeyEvent,
    output: &mut dyn Write,
) -> Result<SecretInputAction, ()> {
    match key.code {
        KeyCode::Enter => Ok(SecretInputAction::Accept),
        KeyCode::Esc => Ok(SecretInputAction::Cancel),
        KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => {
            Ok(SecretInputAction::Cancel)
        }
        KeyCode::Backspace => {
            if secret.pop().is_some() {
                output.write_all(b"\x08 \x08").map_err(|_| ())?;
                output.flush().map_err(|_| ())?;
            }
            Ok(SecretInputAction::Continue)
        }
        KeyCode::Char(character) => {
            secret.push(character);
            output.write_all(b"*").map_err(|_| ())?;
            output.flush().map_err(|_| ())?;
            Ok(SecretInputAction::Continue)
        }
        _ => Ok(SecretInputAction::Continue),
    }
}

fn write_credential_status(stdout: &mut dyn Write, json: bool, status: &str, fingerprint: &str) {
    if json {
        let _ = writeln!(
            stdout,
            "{}",
            serde_json::json!({
                "source": "native_vault",
                "configured": true,
                "status": status,
                "fingerprint": fingerprint,
            })
        );
    } else {
        let _ = writeln!(
            stdout,
            "source: native credential vault\nstatus: {status}\nstored key: {fingerprint}"
        );
    }
}

fn run_with_probe(
    descriptor: &AnthropicCompatProvider,
    subcommand: &DeepseekAuthSubcommand,
    store: &dyn SecretStore,
    stdout: &mut dyn Write,
    read_secret: impl FnOnce() -> Result<String, ()>,
    probe: impl FnOnce(&str) -> ProbeOutcome,
) -> i32 {
    match subcommand {
        DeepseekAuthSubcommand::Login => {
            let secret = match read_secret() {
                Ok(secret) if stored_key_is_well_formed(&secret) => Zeroizing::new(secret),
                _ => {
                    eprintln!(
                        "{}-auth: invalid API key; nothing was stored",
                        descriptor.settings_id
                    );
                    return 2;
                }
            };
            match store.set(&secret) {
                Ok(()) => {
                    let _ = writeln!(
                        stdout,
                        "{} API key stored in the native credential vault",
                        descriptor.display_name
                    );
                    0
                }
                Err(error) => {
                    eprintln!(
                        "{}-auth: {error}; retry after unlocking the vault",
                        descriptor.settings_id
                    );
                    2
                }
            }
        }
        DeepseekAuthSubcommand::Status { json } => match store.get() {
            Ok(Some(key)) => match assess_key(&key, probe) {
                Ok(()) if *json => {
                    let _ = writeln!(
                        stdout,
                        "{}",
                        serde_json::json!({"source": "native_vault", "configured": true})
                    );
                    0
                }
                Ok(()) => {
                    let _ = writeln!(
                        stdout,
                        "source: native credential vault\nstatus: configured"
                    );
                    0
                }
                Err(PreflightError::Malformed { fingerprint }) => {
                    write_credential_status(stdout, *json, "malformed", &fingerprint);
                    2
                }
                Err(PreflightError::Rejected { fingerprint, .. }) => {
                    write_credential_status(stdout, *json, "rejected", &fingerprint);
                    2
                }
                Err(_) => {
                    unreachable!("stored key assessment has only malformed and rejected errors")
                }
            },
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
                eprintln!(
                    "{}-auth: {error}; retry after unlocking the vault",
                    descriptor.settings_id
                );
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
                    "{} API key removed from the native credential vault",
                    descriptor.display_name
                );
                0
            }
            Err(error) => {
                eprintln!(
                    "{}-auth: {error}; retry after unlocking the vault",
                    descriptor.settings_id
                );
                2
            }
        },
    }
}

#[cfg(test)]
fn run_with(
    descriptor: &AnthropicCompatProvider,
    subcommand: &DeepseekAuthSubcommand,
    store: &dyn SecretStore,
    stdout: &mut dyn Write,
    read_secret: impl FnOnce() -> Result<String, ()>,
) -> i32 {
    run_with_probe(descriptor, subcommand, store, stdout, read_secret, |_| {
        ProbeOutcome::Accepted
    })
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use super::*;

    /// Credential-continuity guarantee: these literals identify DeepSeek's
    /// existing vault record. Changing either string, or the `{service}/
    /// {account}` composition rule the Windows path uses, orphans every
    /// currently-stored key — non-Windows keyring lookups and Windows
    /// `CredReadW` calls would target a record that no longer matches what
    /// was written before this refactor.
    #[test]
    fn deepseek_vault_identifiers_are_frozen() {
        assert_eq!(DEEPSEEK_VAULT_SERVICE, "clud.deepseek");
        assert_eq!(DEEPSEEK_VAULT_ACCOUNT, "api-key-v1");
        assert_eq!(
            vault_target(DEEPSEEK_VAULT_SERVICE, DEEPSEEK_VAULT_ACCOUNT),
            "clud.deepseek/api-key-v1"
        );

        // The zero-arg convenience constructor other modules still call
        // must keep resolving to exactly these identifiers.
        let store = NativeSecretStore::new().unwrap();
        assert_eq!(store.service, DEEPSEEK_VAULT_SERVICE);
        assert_eq!(store.account, DEEPSEEK_VAULT_ACCOUNT);
    }

    /// Construction-level only: proves two `NativeSecretStore` instances
    /// built with different identifiers are independent records with no
    /// shared global state, without touching the real host vault.
    #[test]
    fn distinct_identifiers_produce_distinct_stores() {
        let deepseek = NativeSecretStore::new_for("clud.deepseek", "api-key-v1").unwrap();
        let kimi = NativeSecretStore::new_for("clud.kimi", "api-key-v1").unwrap();

        assert_eq!(deepseek.service, "clud.deepseek");
        assert_eq!(kimi.service, "clud.kimi");
        assert_ne!(deepseek.service, kimi.service);
        assert_eq!(deepseek.account, kimi.account);
    }

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

    fn deepseek_descriptor() -> &'static AnthropicCompatProvider {
        provider_registry::descriptor_for(ModelProvider::DeepSeek).unwrap()
    }

    #[test]
    fn a_rejected_stored_key_fails_preflight_before_launch() {
        let store = InMemorySecretStore {
            secret: Mutex::new(Some("sk-0123456789abcdef0123456789abcdef".to_string())),
            unavailable: false,
        };
        let result = preflight_checked_with(
            &store,
            false,
            || unreachable!(),
            |_| ProbeOutcome::Rejected {
                status: 401,
                message: "Authentication Fails".to_string(),
            },
        );
        let error = result.unwrap_err();
        assert!(matches!(&error, PreflightError::Rejected { .. }));
        let diagnostic = error.describe(deepseek_descriptor());
        assert!(diagnostic.contains("****cdef"));
        assert!(diagnostic.contains("35 chars, sha256"));
        assert!(!diagnostic.contains("sk-0123456789abcdef0123456789abcdef"));
    }

    #[test]
    fn rejected_interactive_key_is_not_saved() {
        let store = InMemorySecretStore::default();
        let result = preflight_checked_with(
            &store,
            true,
            || Ok("sk-0123456789abcdef0123456789abcdef".to_string()),
            |_| ProbeOutcome::Rejected {
                status: 401,
                message: "invalid key".to_string(),
            },
        );
        assert!(matches!(result, Err(PreflightError::Rejected { .. })));
        assert_eq!(store.get().unwrap(), None);
    }

    #[test]
    fn probe_deadline_bounds_launch_even_when_request_does_not_finish() {
        let start = std::time::Instant::now();
        let result = probe_with_deadline(Duration::from_millis(10), || {
            std::thread::sleep(Duration::from_millis(100));
            ProbeOutcome::Accepted
        });
        assert_eq!(result, ProbeOutcome::Unavailable);
        assert!(start.elapsed() < Duration::from_millis(90));
    }

    #[test]
    fn an_unreachable_probe_does_not_block_a_valid_stored_key() {
        let key = "sk-0123456789abcdef0123456789abcdef";
        let store = InMemorySecretStore {
            secret: Mutex::new(Some(key.to_string())),
            unavailable: false,
        };
        assert_eq!(
            preflight_checked_with(
                &store,
                false,
                || unreachable!(),
                |_| ProbeOutcome::Unavailable
            ),
            Ok(())
        );
        assert_eq!(store.get().unwrap().as_deref(), Some(key));
    }

    #[test]
    fn status_distinguishes_rejected_from_malformed_without_echoing_secrets() {
        let descriptor = deepseek_descriptor();
        let good_shape = "sk-0123456789abcdef0123456789abcdef";
        let store = InMemorySecretStore {
            secret: Mutex::new(Some(good_shape.to_string())),
            unavailable: false,
        };
        let mut output = Vec::new();
        let code = run_with_probe(
            descriptor,
            &DeepseekAuthSubcommand::Status { json: false },
            &store,
            &mut output,
            || unreachable!(),
            |_| ProbeOutcome::Rejected {
                status: 401,
                message: "Authentication Fails".to_string(),
            },
        );
        let report = String::from_utf8(output).unwrap();
        assert_eq!(code, 2);
        assert!(report.contains("status: rejected"));
        assert!(report.contains("sha256"));
        assert!(!report.contains(good_shape));

        *store.secret.lock().unwrap() =
            Some("sk-0123456789abcdef\u{200b}0123456789abcdef".to_string());
        let mut output = Vec::new();
        let code = run_with_probe(
            descriptor,
            &DeepseekAuthSubcommand::Status { json: false },
            &store,
            &mut output,
            || unreachable!(),
            |_| panic!("zero-width corruption must not reach the network"),
        );
        assert_eq!(code, 2);
        assert!(String::from_utf8(output)
            .unwrap()
            .contains("status: malformed"));

        *store.secret.lock().unwrap() = Some(format!("{good_shape} "));
        let mut output = Vec::new();
        let code = run_with_probe(
            descriptor,
            &DeepseekAuthSubcommand::Status { json: false },
            &store,
            &mut output,
            || unreachable!(),
            |_| panic!("malformed keys must not reach the network"),
        );
        let report = String::from_utf8(output).unwrap();
        assert_eq!(code, 2);
        assert!(report.contains("status: malformed"));
        assert!(!report.contains("status: rejected"));
        assert!(!report.contains(good_shape));
    }

    #[test]
    fn secret_prompt_echoes_asterisks_and_erases_one_on_backspace() {
        let mut secret = String::new();
        let mut output = Vec::new();
        for character in ['s', 'k', '-'] {
            assert_eq!(
                handle_secret_key(
                    &mut secret,
                    KeyEvent::new(KeyCode::Char(character), KeyModifiers::NONE),
                    &mut output,
                ),
                Ok(SecretInputAction::Continue)
            );
        }
        assert_eq!(secret, "sk-");
        assert_eq!(output, b"***");
        handle_secret_key(
            &mut secret,
            KeyEvent::new(KeyCode::Backspace, KeyModifiers::NONE),
            &mut output,
        )
        .unwrap();
        assert_eq!(secret, "sk");
        assert_eq!(output, b"***\x08 \x08");
        assert!(!output.contains(&b's'));
        assert!(!output.contains(&b'k'));
    }

    #[test]
    fn provider_probe_classifies_401_and_transport_failure_without_key_echo() {
        use std::net::TcpListener;
        let key = "sk-0123456789abcdef0123456789abcdef";
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}/probe", listener.local_addr().unwrap());
        let worker = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut request = [0_u8; 2048];
            let size = stream.read(&mut request).unwrap();
            assert!(String::from_utf8_lossy(&request[..size]).contains("Authorization: Bearer "));
            write!(
                stream,
                "HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\n{{}}"
            )
            .unwrap();
        });
        assert_eq!(probe_key_at(&url, key), ProbeOutcome::Accepted);
        worker.join().unwrap();

        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}/probe", listener.local_addr().unwrap());
        let worker = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut request = [0_u8; 2048];
            let size = stream.read(&mut request).unwrap();
            let request = String::from_utf8_lossy(&request[..size]);
            assert!(request.contains("Authorization: Bearer sk-0123456789abcdef0123456789abcdef"));
            let body = "{\"error\":{\"message\":\"Authentication Fails, sk-0123456789abcdef0123456789abcdef invalid\"}}";
            write!(
                stream,
                "HTTP/1.1 401 Unauthorized\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            )
            .unwrap();
        });
        let rejected = probe_key_at(&url, key);
        worker.join().unwrap();
        assert!(matches!(
            rejected,
            ProbeOutcome::Rejected { status: 401, .. }
        ));
        if let ProbeOutcome::Rejected { message, .. } = rejected {
            assert!(message.contains("****cdef"));
            assert!(!message.contains(key));
        }

        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}/probe", listener.local_addr().unwrap());
        let worker = std::thread::spawn(move || {
            let (stream, _) = listener.accept().unwrap();
            drop(stream);
        });
        assert_eq!(probe_key_at(&url, key), ProbeOutcome::Unavailable);
        worker.join().unwrap();
    }

    #[test]
    fn login_status_and_logout_use_only_the_injected_store() {
        let descriptor = deepseek_descriptor();
        let store = InMemorySecretStore::default();
        let mut output = Vec::new();
        let secret = "sk-0123456789abcdef0123456789abcdef";

        assert_eq!(
            run_with(
                descriptor,
                &DeepseekAuthSubcommand::Login,
                &store,
                &mut output,
                || Ok(secret.to_string()),
            ),
            0
        );
        assert!(!String::from_utf8_lossy(&output).contains(secret));
        assert_eq!(store.get().unwrap().as_deref(), Some(secret));

        output.clear();
        assert_eq!(
            run_with(
                descriptor,
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
                descriptor,
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
    fn an_inline_key_rejects_corrupted_pastes_and_reports_only_real_changes() {
        let store = InMemorySecretStore::default();
        let key = "sk-0123456789abcdef0123456789abcdef";
        assert_eq!(
            store_inline_api_key_with(&store, &format!("  {key}\r\n")),
            Err(SecretStoreError::Malformed)
        );
        assert_eq!(store.get().unwrap(), None);
        assert!(store_inline_api_key_with(&store, key).unwrap());
        assert_eq!(store.get().unwrap().as_deref(), Some(key));
        assert!(
            !store_inline_api_key_with(&store, key).unwrap(),
            "re-passing the same key is not a change"
        );
        assert!(store_inline_api_key_with(&store, "sk-ffffffffffffffffffffffff").unwrap());
        assert_eq!(
            store_inline_api_key_with(&store, "   "),
            Err(SecretStoreError::Malformed)
        );
        assert_eq!(
            store.get().unwrap().as_deref(),
            Some("sk-ffffffffffffffffffffffff")
        );

        // The preflight then finds the key and never prompts.
        assert_eq!(preflight_with(&store, true, || unreachable!()), Ok(()));

        let broken = InMemorySecretStore {
            unavailable: true,
            ..InMemorySecretStore::default()
        };
        assert_eq!(
            store_inline_api_key_with(&broken, key),
            Err(SecretStoreError::Unavailable)
        );
    }

    /// Windows stores keys through Credential Manager directly (not the
    /// `keyring` crate). Round-trip a key through the real API so a Windows
    /// regression in the storage path fails CI rather than a user's launch.
    #[cfg(windows)]
    #[test]
    fn windows_credential_manager_round_trips_an_api_key() {
        let target = format!("clud.test-inline-api-key/{}", std::process::id());
        let key = "sk-0123456789abcdef0123456789abcdef";
        if let Err(error) = windows_vault::set(&target, key) {
            eprintln!("SKIP: Credential Manager unavailable on this runner: {error}");
            return;
        }
        let read = windows_vault::get(&target);
        let _ = windows_vault::delete(&target);
        assert_eq!(read, Ok(Some(key.to_string())));
        assert_eq!(windows_vault::get(&target), Ok(None));
    }

    #[test]
    fn login_rejects_empty_input_without_storing_anything() {
        let store = InMemorySecretStore::default();
        let mut output = Vec::new();
        assert_eq!(
            run_with(
                deepseek_descriptor(),
                &DeepseekAuthSubcommand::Login,
                &store,
                &mut output,
                || Ok("   ".to_string()),
            ),
            2
        );
        assert_eq!(store.get().unwrap(), None);
        assert_eq!(
            run_with(
                deepseek_descriptor(),
                &DeepseekAuthSubcommand::Login,
                &store,
                &mut output,
                || Ok("sk-0123456789abcdef0123456789abcdef ".to_string()),
            ),
            2
        );
        assert_eq!(store.get().unwrap(), None);
    }

    fn parse(argv: &[&str]) -> Args {
        Args::parse_from_raw(argv.iter().map(|s| s.to_string()).collect())
    }

    #[test]
    fn preflight_target_is_deepseeks_descriptor_and_never_set_for_dry_run() {
        assert_eq!(
            launch_preflight_target(ModelProvider::DeepSeek, false),
            Some(deepseek_descriptor())
        );
        // The dry-run-makes-zero-vault-calls guarantee: no descriptor is ever
        // returned for a dry run, regardless of provider.
        assert_eq!(launch_preflight_target(ModelProvider::DeepSeek, true), None);
        assert_eq!(launch_preflight_target(ModelProvider::Claude, true), None);
        assert_eq!(launch_preflight_target(ModelProvider::Codex, true), None);
        // Claude and Codex have no Anthropic-compat descriptor, dry run or not.
        assert_eq!(launch_preflight_target(ModelProvider::Claude, false), None);
        assert_eq!(launch_preflight_target(ModelProvider::Codex, false), None);
    }

    #[test]
    fn preflight_error_messages_name_the_descriptors_provider_and_login_command() {
        let descriptor = deepseek_descriptor();
        assert_eq!(
            PreflightError::Missing.describe(descriptor),
            "DeepSeek credentials are not configured; run `clud auth login deepseek`"
        );
        assert_eq!(
            PreflightError::Unavailable.describe(descriptor),
            "the native credential vault is unavailable; retry after unlocking it"
        );
        assert_eq!(
            PreflightError::Cancelled.describe(descriptor),
            "DeepSeek credential entry was cancelled"
        );
    }

    #[test]
    fn interactive_launch_requires_a_real_tty_on_both_streams() {
        let args = parse(&["clud", "--deepseek"]);
        assert!(launch_is_interactive(&args, Backend::DeepSeek, true, true));
        assert!(!launch_is_interactive(
            &args,
            Backend::DeepSeek,
            false,
            true
        ));
        assert!(!launch_is_interactive(
            &args,
            Backend::DeepSeek,
            true,
            false
        ));
    }

    #[test]
    fn detached_and_detachable_launches_are_never_interactive() {
        let detach = parse(&["clud", "--deepseek", "--detach"]);
        assert!(!launch_is_interactive(
            &detach,
            Backend::DeepSeek,
            true,
            true
        ));

        let detachable = parse(&["clud", "--deepseek", "--detachable"]);
        assert!(!launch_is_interactive(
            &detachable,
            Backend::DeepSeek,
            true,
            true
        ));
    }

    #[test]
    fn repeat_loop_launches_are_never_interactive() {
        let args = parse(&["clud", "--deepseek", "loop", "--repeat", "1h", "task"]);
        assert!(!launch_is_interactive(&args, Backend::DeepSeek, true, true));
    }

    #[test]
    fn noninteractive_prompt_flags_disable_interactive_preflight() {
        let args = parse(&["clud", "--deepseek", "-p", "do the thing"]);
        assert!(!launch_is_interactive(&args, Backend::DeepSeek, true, true));
    }

    #[test]
    fn preflight_prompts_only_for_interactive_missing_credentials() {
        let store = InMemorySecretStore::default();
        assert_eq!(
            preflight_with(&store, false, || unreachable!()),
            Err(PreflightError::Missing)
        );
        assert_eq!(
            preflight_with(&store, true, || Ok("ds-test-secret".to_string())),
            Ok(())
        );
        assert_eq!(store.get().unwrap().as_deref(), Some("ds-test-secret"));
        assert_eq!(preflight_with(&store, false, || unreachable!()), Ok(()));
    }

    #[test]
    fn preflight_noninteractive_missing_credentials_never_reads_input() {
        let store = InMemorySecretStore::default();
        assert_eq!(
            preflight_with(&store, false, || unreachable!()),
            Err(PreflightError::Missing)
        );
    }

    #[test]
    fn preflight_interactive_cancelled_entry_leaves_the_vault_untouched() {
        let store = InMemorySecretStore::default();
        assert_eq!(
            preflight_with(&store, true, || Err(())),
            Err(PreflightError::Cancelled)
        );
        assert_eq!(store.get().unwrap(), None);
    }

    fn kimi_descriptor() -> &'static AnthropicCompatProvider {
        provider_registry::descriptor_for(ModelProvider::Kimi).unwrap()
    }

    /// #936: a first interactive `clud --kimi` with no stored key prompts,
    /// probes, stores into Kimi's own record, and lets the launch continue --
    /// no prior `clud auth login kimi` needed. The next launch reads it back
    /// without prompting.
    #[test]
    fn kimi_first_interactive_launch_prompts_probes_stores_and_continues() {
        assert_eq!(
            launch_preflight_target(ModelProvider::Kimi, false),
            Some(kimi_descriptor())
        );
        let key = "sk-kimi0123456789abcdef0123456789ab";
        let store = InMemorySecretStore::default();
        let mut probed = Vec::new();
        assert_eq!(
            preflight_checked_with(
                &store,
                true,
                || Ok(key.to_string()),
                |candidate| {
                    probed.push(candidate.to_string());
                    ProbeOutcome::Accepted
                },
            ),
            Ok(())
        );
        assert_eq!(probed, vec![key.to_string()], "probed once, before storing");
        assert_eq!(store.get().unwrap().as_deref(), Some(key));
        assert_eq!(
            preflight_checked_with(&store, false, || unreachable!(), |_| ProbeOutcome::Accepted),
            Ok(())
        );
    }

    /// Esc cancels; Enter on an empty (or blank) entry submits a malformed key.
    /// Either way nothing is stored and nothing is sent to Moonshot.
    #[test]
    fn kimi_cancelled_or_empty_interactive_entry_is_never_stored_or_probed() {
        let store = InMemorySecretStore::default();
        assert_eq!(
            preflight_checked_with(&store, true, || Err(()), |_| unreachable!()),
            Err(PreflightError::Cancelled)
        );
        for blank in ["", "   "] {
            assert!(matches!(
                preflight_checked_with(&store, true, || Ok(blank.to_string()), |_| unreachable!()),
                Err(PreflightError::Malformed { .. })
            ));
        }
        assert_eq!(store.get().unwrap(), None);
        assert_eq!(
            PreflightError::Cancelled.describe(kimi_descriptor()),
            "Kimi credential entry was cancelled"
        );
    }

    /// #936: every descriptor provider (Kimi included) fails fast with its own
    /// login command -- never a prompt -- when the launch is detached,
    /// detachable, a repeat loop, a `-p` prompt, or lacks a terminal.
    #[test]
    fn every_api_key_provider_prompts_only_on_a_true_interactive_foreground_launch() {
        for descriptor in provider_registry::ANTHROPIC_COMPAT_PROVIDERS {
            let flag = descriptor.cli_flag;
            let interactive = parse(&["clud", flag]);
            assert!(
                launch_is_interactive(&interactive, Backend::Claude, true, true),
                "{flag}"
            );
            assert!(
                !launch_is_interactive(&interactive, Backend::Claude, false, true),
                "{flag}"
            );
            assert!(
                !launch_is_interactive(&interactive, Backend::Claude, true, false),
                "{flag}"
            );
            for argv in [
                vec!["clud", flag, "--detach"],
                vec!["clud", flag, "--detachable"],
                vec!["clud", flag, "loop", "--repeat", "1h", "task"],
                vec!["clud", flag, "-p", "do the thing"],
            ] {
                assert!(
                    !launch_is_interactive(&parse(&argv), Backend::Claude, true, true),
                    "{argv:?} must never prompt"
                );
            }
            let store = InMemorySecretStore::default();
            assert_eq!(
                preflight_checked_with(&store, false, || unreachable!(), |_| unreachable!()),
                Err(PreflightError::Missing)
            );
            assert_eq!(
                PreflightError::Missing.describe(descriptor),
                format!(
                    "{} credentials are not configured; run `clud auth login {}`",
                    descriptor.display_name, descriptor.settings_id
                )
            );
            assert_eq!(launch_preflight_target(descriptor.provider, true), None);
        }
    }

    #[test]
    fn preflight_unavailable_vault_is_sanitized() {
        let store = InMemorySecretStore {
            unavailable: true,
            ..InMemorySecretStore::default()
        };
        assert_eq!(
            preflight_with(&store, false, || unreachable!()),
            Err(PreflightError::Unavailable)
        );
    }

    /// #901: the file-backed test vault needs a debug build, the explicit
    /// integration opt-in and a non-empty directory. A release build ignores
    /// it even with both variables set.
    #[test]
    fn test_vault_requires_debug_build_and_integration_opt_in() {
        let dir = Some(std::ffi::OsString::from("/tmp/vault"));
        assert_eq!(
            resolve_test_vault_dir(true, true, dir.clone()),
            Some(std::path::PathBuf::from("/tmp/vault"))
        );
        assert_eq!(resolve_test_vault_dir(false, true, dir.clone()), None);
        assert_eq!(resolve_test_vault_dir(true, false, dir), None);
        assert_eq!(resolve_test_vault_dir(true, true, Some("".into())), None);
        assert_eq!(resolve_test_vault_dir(true, true, None), None);
    }

    #[test]
    fn test_vault_paths_keep_provider_records_apart_and_filename_safe() {
        let dir = std::path::Path::new("/v");
        let deepseek = test_vault_path(dir, DEEPSEEK_VAULT_SERVICE, DEEPSEEK_VAULT_ACCOUNT);
        let openrouter = test_vault_path(dir, OPENROUTER_VAULT_SERVICE, OPENROUTER_VAULT_ACCOUNT);
        assert_ne!(deepseek, openrouter);
        let name = deepseek.file_name().unwrap().to_string_lossy().into_owned();
        assert!(name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-' || c == '.'));
    }

    #[test]
    fn status_reports_login_required_when_nothing_is_stored() {
        let store = InMemorySecretStore::default();
        let mut output = Vec::new();
        assert_eq!(
            run_with(
                deepseek_descriptor(),
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
                deepseek_descriptor(),
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
