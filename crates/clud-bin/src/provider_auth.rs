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

use crate::args::{Args, Command, DeepseekAuthSubcommand};
use crate::backend::ModelProvider;
use crate::command;
use crate::provider_registry::{self, AnthropicCompatProvider};

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
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PreflightError {
    Missing,
    Unavailable,
    Cancelled,
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
        }
    }
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
        && !command::has_noninteractive_prompt(args)
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
    preflight_with(&store, interactive, || {
        prompt_secret(descriptor.display_name)
    })
}

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
    run_with(descriptor, subcommand, &store, &mut stdout, || {
        prompt_secret(descriptor.display_name)
    })
}

/// DeepSeek-scoped delegate kept for its existing call sites: `main.rs`'s
/// `clud deepseek-auth` deprecated-alias dispatch. Behavior and output are
/// unchanged from before this module became provider-generic.
pub fn run(subcommand: &DeepseekAuthSubcommand) -> i32 {
    let descriptor = provider_registry::descriptor_for(ModelProvider::DeepSeek)
        .expect("DeepSeek has an Anthropic-compat descriptor");
    run_for(descriptor, subcommand)
}

/// Read a secret from the terminal without echoing typed characters.
/// `display_name` names the provider prompted for (e.g. "DeepSeek", "Kimi")
/// so this one implementation serves every Anthropic-compat provider.
fn prompt_secret(display_name: &str) -> Result<String, ()> {
    eprint!("{display_name} API key: ");
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
    descriptor: &AnthropicCompatProvider,
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
                    eprintln!(
                        "{}-auth: no API key entered; nothing was stored",
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
    fn login_status_and_logout_use_only_the_injected_store() {
        let descriptor = deepseek_descriptor();
        let store = InMemorySecretStore::default();
        let mut output = Vec::new();
        let secret = "ds-test-secret";

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
        assert!(launch_is_interactive(&args, true, true));
        assert!(!launch_is_interactive(&args, false, true));
        assert!(!launch_is_interactive(&args, true, false));
    }

    #[test]
    fn detached_and_detachable_launches_are_never_interactive() {
        let detach = parse(&["clud", "--deepseek", "--detach"]);
        assert!(!launch_is_interactive(&detach, true, true));

        let detachable = parse(&["clud", "--deepseek", "--detachable"]);
        assert!(!launch_is_interactive(&detachable, true, true));
    }

    #[test]
    fn repeat_loop_launches_are_never_interactive() {
        let args = parse(&["clud", "--deepseek", "loop", "--repeat", "1h", "task"]);
        assert!(!launch_is_interactive(&args, true, true));
    }

    #[test]
    fn noninteractive_prompt_flags_disable_interactive_preflight() {
        let args = parse(&["clud", "--deepseek", "-p", "do the thing"]);
        assert!(!launch_is_interactive(&args, true, true));
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
