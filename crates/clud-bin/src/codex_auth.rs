//! Clud-managed ChatGPT subscription credentials (issue #629).
//!
//! This is deliberately separate from the Codex CLI's `~/.codex/auth.json`:
//! clud must be able to remove only its own credentials and must never let a
//! platform API key and subscription login implicitly select one another.

use std::fmt;
use std::fs::{self, File, OpenOptions};
use std::io::{Read, Write};
use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use fs4::fs_std::FileExt;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::args::CodexAuthSubcommand;

const AUTH_FILE: &str = "codex-auth.json";
const LOCK_FILE: &str = "codex-auth.lock";
const EXPERIMENTAL_ACK_MESSAGE: &str =
    "subscription authentication is experimental; re-run with --acknowledge-experimental";
const OAUTH_ISSUER: &str = "https://auth.openai.com";
// Compatibility contract researched from openai/codex `codex-rs/login`.
const CODEX_CLIENT_ID: &str = "app_EMoamEEZ73f0CkXaXp7hrann";
const CALLBACK_PATH: &str = "/auth/callback";

#[derive(Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SubscriptionCredentials {
    pub access_token: String,
    pub refresh_token: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub account_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub email: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expires_at_unix: Option<u64>,
}

impl fmt::Debug for SubscriptionCredentials {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("SubscriptionCredentials")
            .field("access_token", &"[redacted]")
            .field("refresh_token", &"[redacted]")
            .field(
                "account_id",
                &self.account_id.as_ref().map(|_| "[redacted]"),
            )
            .field("email", &self.email)
            .field("expires_at_unix", &self.expires_at_unix)
            .finish()
    }
}

/// Location of clud-owned credentials. It intentionally does not share the
/// Codex CLI location, so `clud auth logout codex` cannot remove another
/// application's credentials.
pub fn credentials_path_at(home: &Path) -> PathBuf {
    home.join(".clud").join(AUTH_FILE)
}

fn lock_path_at(home: &Path) -> PathBuf {
    home.join(".clud").join(LOCK_FILE)
}

fn acquire_lock(home: &Path) -> Result<File, String> {
    let path = lock_path_at(home);
    fs::create_dir_all(
        path.parent()
            .expect("credential lock path always has a parent"),
    )
    .map_err(|error| format!("could not create clud auth directory: {error}"))?;
    let lock = OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(false)
        .open(path)
        .map_err(|error| format!("could not open clud credential lock: {error}"))?;
    lock.lock_exclusive()
        .map_err(|error| format!("could not lock clud credentials: {error}"))?;
    Ok(lock)
}

pub fn load_at(home: &Path) -> Result<Option<SubscriptionCredentials>, String> {
    let path = credentials_path_at(home);
    match fs::read(&path) {
        Ok(raw) => serde_json::from_slice(&raw).map(Some).map_err(|_| {
            "clud subscription credentials are corrupted; run `clud auth logout codex`".to_string()
        }),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(format!(
            "could not read clud subscription credentials: {error}"
        )),
    }
}

pub fn remove_at(home: &Path) -> Result<bool, String> {
    let _lock = acquire_lock(home)?;
    match fs::remove_file(credentials_path_at(home)) {
        Ok(()) => Ok(true),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(format!(
            "could not remove clud subscription credentials: {error}"
        )),
    }
}

/// Atomically write credentials after serializing against other clud
/// processes. On Unix the file is created with mode 0600; on Windows the file
/// gets a protected owner-only DACL which is read back and verified.
pub fn save_at(home: &Path, credentials: &SubscriptionCredentials) -> Result<(), String> {
    let _lock = acquire_lock(home)?;
    save_locked(home, credentials)
}

fn save_locked(home: &Path, credentials: &SubscriptionCredentials) -> Result<(), String> {
    let path = credentials_path_at(home);
    let temp = path.with_extension("tmp");
    let encoded = serde_json::to_vec(credentials)
        .map_err(|error| format!("could not encode clud credentials: {error}"))?;
    let mut options = OpenOptions::new();
    options.create(true).truncate(true).write(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options
        .open(&temp)
        .map_err(|error| format!("could not create clud credential file: {error}"))?;
    #[cfg(windows)]
    protect_windows_credential_file(&temp)?;
    file.write_all(&encoded)
        .and_then(|()| file.sync_all())
        .map_err(|error| format!("could not write clud credentials: {error}"))?;
    drop(file);
    fs::rename(&temp, &path)
        .map_err(|error| format!("could not replace clud credentials atomically: {error}"))?;
    #[cfg(windows)]
    protect_windows_credential_file(&path)?;
    Ok(())
}

/// Windows has no portable equivalent of Unix mode 0600.  Do not merely rely
/// on the profile directory's inherited ACL: network homes and redirected
/// profiles can make that inheritance broader than a credential file permits.
///
/// `OW` is the SDDL owner-rights principal, so the only explicit ACE grants
/// full access to the file's current owner.  The protected DACL disables
/// inherited entries.  Administrators with backup/restore privileges remain
/// an OS trust boundary; ordinary other local users cannot read this file.
#[cfg(windows)]
fn protect_windows_credential_file(path: &Path) -> Result<(), String> {
    use std::os::windows::ffi::OsStrExt;

    use windows::core::{w, PCWSTR};
    use windows::Win32::Foundation::{LocalFree, HLOCAL};
    use windows::Win32::Security::Authorization::{
        ConvertSecurityDescriptorToStringSecurityDescriptorW,
        ConvertStringSecurityDescriptorToSecurityDescriptorW, GetNamedSecurityInfoW,
        SetNamedSecurityInfoW, SDDL_REVISION_1, SE_FILE_OBJECT,
    };
    use windows::Win32::Security::{
        DACL_SECURITY_INFORMATION, PROTECTED_DACL_SECURITY_INFORMATION, PSECURITY_DESCRIPTOR,
        SE_DACL_PROTECTED,
    };

    const OWNER_ONLY_ACE: &str = "(A;;FA;;;OW)";
    let mut descriptor = PSECURITY_DESCRIPTOR::default();
    // SAFETY: the SDDL is a static NUL-terminated Windows string and Windows
    // allocates `descriptor`, which is freed on every path below.
    unsafe {
        ConvertStringSecurityDescriptorToSecurityDescriptorW(
            w!("D:P(A;;FA;;;OW)"),
            SDDL_REVISION_1,
            &mut descriptor,
            None,
        )
        .map_err(|error| format!("could not construct Windows credential ACL: {error}"))?;
    }
    let result = (|| {
        let mut present = windows::core::BOOL::default();
        let mut dacl = std::ptr::null_mut();
        let mut defaulted = windows::core::BOOL::default();
        // SAFETY: `descriptor` was returned by the conversion API and remains
        // live until the cleanup below.
        unsafe {
            windows::Win32::Security::GetSecurityDescriptorDacl(
                descriptor,
                &mut present,
                &mut dacl,
                &mut defaulted,
            )
            .map_err(|error| format!("could not inspect Windows credential ACL: {error}"))?;
        }
        if !present.as_bool() || dacl.is_null() {
            return Err("Windows credential ACL unexpectedly has no DACL".to_string());
        }
        let mut wide_path: Vec<u16> = path.as_os_str().encode_wide().collect();
        wide_path.push(0);
        // SAFETY: the path is NUL-terminated and `dacl` points into the live
        // descriptor.  No owner/group/SACL changes are requested.
        let status = unsafe {
            SetNamedSecurityInfoW(
                PCWSTR(wide_path.as_ptr()),
                SE_FILE_OBJECT,
                DACL_SECURITY_INFORMATION | PROTECTED_DACL_SECURITY_INFORMATION,
                None,
                None,
                Some(dacl.cast_const()),
                None,
            )
        };
        if status.0 != 0 {
            return Err(format!(
                "could not protect Windows credential file (error {})",
                status.0
            ));
        }

        let mut actual = PSECURITY_DESCRIPTOR::default();
        // SAFETY: all output pointers are valid for this call; Windows owns
        // the returned descriptor until the matching LocalFree below.
        let status = unsafe {
            GetNamedSecurityInfoW(
                PCWSTR(wide_path.as_ptr()),
                SE_FILE_OBJECT,
                DACL_SECURITY_INFORMATION,
                None,
                None,
                None,
                None,
                &mut actual,
            )
        };
        if status.0 != 0 {
            return Err(format!(
                "could not verify Windows credential ACL (error {})",
                status.0
            ));
        }
        let mut control = 0_u16;
        let mut revision = 0_u32;
        // SAFETY: `actual` is the live descriptor returned by
        // GetNamedSecurityInfoW above, and both output pointers are valid.
        unsafe {
            windows::Win32::Security::GetSecurityDescriptorControl(
                actual,
                &mut control,
                &mut revision,
            )
            .map_err(|error| {
                format!("could not inspect Windows credential ACL control: {error}")
            })?;
        }
        if control & SE_DACL_PROTECTED.0 == 0 {
            return Err("Windows credential ACL verification found inherited entries".to_string());
        }
        let verified = (|| {
            let mut rendered = windows::core::PWSTR::null();
            // SAFETY: `actual` is live until LocalFree and Windows allocates
            // `rendered`, which is freed before returning from this closure.
            unsafe {
                ConvertSecurityDescriptorToStringSecurityDescriptorW(
                    actual,
                    SDDL_REVISION_1,
                    DACL_SECURITY_INFORMATION,
                    &mut rendered,
                    None,
                )
                .map_err(|error| format!("could not render Windows credential ACL: {error}"))?;
            }
            let rendered_acl = unsafe { rendered.to_string() }
                .map_err(|error| format!("could not decode Windows credential ACL: {error}"));
            // SAFETY: `rendered` was allocated by the documented Windows API.
            unsafe { LocalFree(Some(HLOCAL(rendered.0.cast()))) };
            let rendered_acl = rendered_acl?;
            if !rendered_acl.contains(OWNER_ONLY_ACE) {
                return Err(
                    "Windows credential ACL verification did not find the owner-only ACE"
                        .to_string(),
                );
            }
            Ok(())
        })();
        // SAFETY: `actual` was allocated by GetNamedSecurityInfoW.
        unsafe { LocalFree(Some(HLOCAL(actual.0))) };
        verified
    })();
    // SAFETY: `descriptor` was allocated by the SDDL conversion API.
    unsafe { LocalFree(Some(HLOCAL(descriptor.0))) };
    result
}

/// Re-read after acquiring the shared lock, refresh at most once, and replace
/// the record atomically. Its transport is injected so this invariant can be
/// proven without an account or token-shaped fixture in a test log.
pub fn refresh_if_needed_at(
    home: &Path,
    now_unix: u64,
    refresh: impl FnOnce(&str) -> Result<(String, String, Option<u64>), String>,
) -> Result<SubscriptionCredentials, String> {
    let _lock = acquire_lock(home)?;
    let current = load_at(home)?.ok_or_else(|| "no clud ChatGPT subscription login".to_string())?;
    if current
        .expires_at_unix
        .is_none_or(|expiry| expiry > now_unix.saturating_add(60))
    {
        return Ok(current);
    }
    let (access_token, refresh_token, expires_at_unix) = refresh(&current.refresh_token)?;
    let replacement = SubscriptionCredentials {
        access_token,
        refresh_token,
        expires_at_unix,
        ..current
    };
    save_locked(home, &replacement)?;
    Ok(replacement)
}

pub fn load_fresh_at(home: &Path) -> Result<SubscriptionCredentials, String> {
    let now_unix = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    refresh_if_needed_at(home, now_unix, refresh_remote)
}

fn refresh_remote(refresh_token: &str) -> Result<(String, String, Option<u64>), String> {
    #[derive(Deserialize)]
    struct Tokens {
        access_token: String,
        refresh_token: String,
        #[serde(default)]
        expires_in: Option<u64>,
    }
    let response = ureq::post(&format!("{OAUTH_ISSUER}/oauth/token"))
        .send_form(&[
            ("grant_type", "refresh_token"),
            ("refresh_token", refresh_token),
            ("client_id", CODEX_CLIENT_ID),
        ])
        .map_err(|_| "the Codex login has expired -- run `clud auth login codex`".to_string())?;
    let tokens: Tokens = serde_json::from_reader(response.into_reader())
        .map_err(|_| "the Codex login has expired -- run `clud auth login codex`".to_string())?;
    if tokens.access_token.trim().is_empty() || tokens.refresh_token.trim().is_empty() {
        return Err("the Codex login has expired -- run `clud auth login codex`".to_string());
    }
    let expiry = tokens.expires_in.map(|seconds| {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs()
            .saturating_add(seconds)
    });
    Ok((tokens.access_token, tokens.refresh_token, expiry))
}

pub fn run(subcommand: &CodexAuthSubcommand, interrupted: &AtomicBool) -> i32 {
    let Some(home) = dirs::home_dir() else {
        eprintln!("codex-auth: home directory is unavailable");
        return 2;
    };
    match subcommand {
        CodexAuthSubcommand::Login {
            acknowledge_experimental,
            ..
        } if !acknowledge_experimental => {
            eprintln!("{EXPERIMENTAL_ACK_MESSAGE}");
            2
        }
        CodexAuthSubcommand::Login { no_browser, .. } => login(&home, !no_browser, interrupted),
        CodexAuthSubcommand::Status { json } => match load_at(&home) {
            Ok(Some(credentials)) if *json => {
                let expired = credentials_expired(&credentials, now_unix());
                println!(
                    "{}",
                    serde_json::json!({
                        "source": "clud_chatgpt_subscription",
                        "logged_in": !expired,
                        "account_id": credentials.account_id,
                        "email": credentials.email,
                        "expires_at_unix": credentials.expires_at_unix,
                        "refresh_required": expired,
                    })
                );
                if expired {
                    1
                } else {
                    0
                }
            }
            Ok(Some(credentials)) => {
                let expired = credentials_expired(&credentials, now_unix());
                println!("source: clud ChatGPT subscription");
                if let Some(email) = credentials.email {
                    println!("account: {email}");
                }
                if expired {
                    println!("status: refresh or login required");
                    1
                } else {
                    println!("status: logged in");
                    0
                }
            }
            Ok(None) if *json => {
                println!(
                    "{}",
                    serde_json::json!({"source": "none", "logged_in": false})
                );
                1
            }
            Ok(None) => {
                println!("source: none\nstatus: login required");
                1
            }
            Err(error) => {
                eprintln!("codex-auth: {error}");
                2
            }
        },
        CodexAuthSubcommand::Logout { json } => match remove_at(&home) {
            Ok(removed) if *json => {
                println!("{}", serde_json::json!({"removed": removed}));
                0
            }
            Ok(true) => {
                println!("clud subscription credentials removed");
                0
            }
            Ok(false) => {
                println!("no clud subscription credentials found");
                0
            }
            Err(error) => {
                eprintln!("codex-auth: {error}");
                2
            }
        },
    }
}

fn login(home: &Path, open_browser: bool, interrupted: &AtomicBool) -> i32 {
    let listener = match bind_callback_listener() {
        Ok(listener) => listener,
        Err(error) => {
            eprintln!("codex-auth: could not bind OAuth callback: {error}");
            return 2;
        }
    };
    let port = listener
        .local_addr()
        .expect("bound listener has address")
        .port();
    let (verifier, challenge) = pkce_pair().expect("OS random source available");
    let state = random_urlsafe(32).expect("OS random source available");
    let nonce = random_urlsafe(32).expect("OS random source available");
    let redirect_uri = format!("http://localhost:{port}{CALLBACK_PATH}");
    let authorize_url = authorization_url(&redirect_uri, &challenge, &state, &nonce);
    println!("Open this URL to sign in with ChatGPT:\n{authorize_url}");
    if open_browser {
        let _ = open::that(&authorize_url);
    }
    let code = match wait_for_callback_with(
        &listener,
        &state,
        Instant::now() + Duration::from_secs(10 * 60),
        || interrupted.load(Ordering::SeqCst),
    ) {
        Ok(code) => code,
        Err(error) => {
            eprintln!("codex-auth: {error}");
            return 1;
        }
    };
    match exchange_code(&code, &redirect_uri, &verifier, &nonce)
        .and_then(|credentials| save_at(home, &credentials))
    {
        Ok(()) => {
            println!("clud ChatGPT subscription login complete");
            0
        }
        Err(error) => {
            eprintln!("codex-auth: {error}");
            1
        }
    }
}

fn authorization_url(redirect_uri: &str, challenge: &str, state: &str, nonce: &str) -> String {
    format!(
        "{OAUTH_ISSUER}/oauth/authorize?response_type=code&client_id={CODEX_CLIENT_ID}&redirect_uri={}&scope={}&code_challenge={challenge}&code_challenge_method=S256&id_token_add_organizations=true&codex_cli_simplified_flow=true&state={state}&nonce={nonce}",
        form_encode(redirect_uri),
        form_encode("openid profile email offline_access api.connectors.read api.connectors.invoke"),
    )
}

fn now_unix() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

fn credentials_expired(credentials: &SubscriptionCredentials, now: u64) -> bool {
    credentials
        .expires_at_unix
        .is_some_and(|expiry| expiry <= now.saturating_add(60))
}

fn bind_callback_listener() -> Result<TcpListener, String> {
    TcpListener::bind(("127.0.0.1", 1455))
        .or_else(|_| TcpListener::bind(("127.0.0.1", 1457)))
        .map_err(|error| format!("ports 1455 and 1457 are unavailable: {error}"))
}

fn wait_for_callback_with(
    listener: &TcpListener,
    expected_state: &str,
    deadline: Instant,
    cancelled: impl Fn() -> bool,
) -> Result<String, String> {
    listener
        .set_nonblocking(true)
        .map_err(|error| format!("could not configure OAuth callback: {error}"))?;
    while Instant::now() < deadline {
        if cancelled() {
            return Err("OAuth sign-in cancelled".to_string());
        }
        match listener.accept() {
            Ok((mut stream, _)) => {
                let mut request = [0_u8; 8192];
                let read = stream
                    .read(&mut request)
                    .map_err(|error| format!("could not read OAuth callback: {error}"))?;
                let target = std::str::from_utf8(&request[..read])
                    .ok()
                    .and_then(|raw| raw.lines().next())
                    .and_then(|line| line.split_whitespace().nth(1))
                    .ok_or_else(|| "received malformed OAuth callback".to_string())?;
                let result = callback_code(target, expected_state);
                let response = if result.is_ok() {
                    b"HTTP/1.1 200 OK\r\nContent-Type: text/plain\r\nConnection: close\r\n\r\nSign-in complete. You may close this tab.".as_slice()
                } else {
                    b"HTTP/1.1 400 Bad Request\r\nContent-Type: text/plain\r\nConnection: close\r\n\r\nSign-in could not be completed.".as_slice()
                };
                let _ = stream.write_all(response);
                return result;
            }
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                thread::sleep(Duration::from_millis(25))
            }
            Err(error) => return Err(format!("could not accept OAuth callback: {error}")),
        }
    }
    Err("OAuth callback timed out; re-run `clud auth login codex`".to_string())
}

fn callback_code(target: &str, expected_state: &str) -> Result<String, String> {
    if target.split('?').next() != Some(CALLBACK_PATH) {
        return Err("OAuth callback used an unexpected path".to_string());
    }
    let query = target
        .split_once('?')
        .map(|(_, query)| query)
        .unwrap_or_default();
    let value = |key: &str| {
        query.split('&').find_map(|part| {
            part.split_once('=')
                .filter(|(name, _)| *name == key)
                .map(|(_, value)| form_decode(value))
        })
    };
    if value("state").as_deref() != Some(expected_state) {
        return Err("OAuth callback state did not match; sign-in was rejected".to_string());
    }
    if value("error").is_some() {
        return Err("ChatGPT denied the sign-in request".to_string());
    }
    value("code")
        .filter(|code| !code.is_empty())
        .ok_or_else(|| "OAuth callback did not include an authorization code".to_string())
}

fn exchange_code(
    code: &str,
    redirect_uri: &str,
    verifier: &str,
    expected_nonce: &str,
) -> Result<SubscriptionCredentials, String> {
    #[derive(Deserialize)]
    struct Tokens {
        access_token: String,
        refresh_token: String,
        #[serde(default)]
        id_token: Option<String>,
        #[serde(default)]
        expires_in: Option<u64>,
    }
    let response = ureq::post(&format!("{OAUTH_ISSUER}/oauth/token"))
        .send_form(&[
            ("grant_type", "authorization_code"),
            ("code", code),
            ("redirect_uri", redirect_uri),
            ("client_id", CODEX_CLIENT_ID),
            ("code_verifier", verifier),
        ])
        .map_err(|_| "OAuth token exchange failed; re-run `clud auth login codex`".to_string())?;
    let tokens: Tokens = serde_json::from_reader(response.into_reader())
        .map_err(|_| "OAuth token exchange returned an invalid response".to_string())?;
    let claims = tokens
        .id_token
        .as_deref()
        .and_then(safe_identity_claims)
        .ok_or_else(|| "OAuth token exchange did not return a valid identity token".to_string())?;
    if claims.nonce.as_deref() != Some(expected_nonce) {
        return Err("OAuth identity token nonce did not match; sign-in was rejected".to_string());
    }
    let expires_at_unix = claims.expires_at_unix.or_else(|| {
        tokens
            .expires_in
            .map(|seconds| now_unix().saturating_add(seconds))
    });
    Ok(SubscriptionCredentials {
        access_token: tokens.access_token,
        refresh_token: tokens.refresh_token,
        account_id: claims.account_id,
        email: claims.email,
        expires_at_unix,
    })
}

#[derive(Deserialize)]
struct IdentityClaims {
    #[serde(default)]
    email: Option<String>,
    #[serde(default, rename = "exp")]
    expires_at_unix: Option<u64>,
    #[serde(default)]
    nonce: Option<String>,
    #[serde(default, rename = "https://api.openai.com/auth")]
    auth: Option<AuthClaims>,
}

#[derive(Deserialize)]
struct AuthClaims {
    #[serde(default)]
    chatgpt_account_id: Option<String>,
}

struct SafeIdentityClaims {
    email: Option<String>,
    account_id: Option<String>,
    expires_at_unix: Option<u64>,
    nonce: Option<String>,
}

/// Decode only display-safe JWT claims. The token itself is never retained or
/// printed, and signature verification remains the issuer's responsibility.
fn safe_identity_claims(token: &str) -> Option<SafeIdentityClaims> {
    let payload = token.split('.').nth(1)?;
    let bytes =
        base64::Engine::decode(&base64::engine::general_purpose::URL_SAFE_NO_PAD, payload).ok()?;
    let claims: IdentityClaims = serde_json::from_slice(&bytes).ok()?;
    Some(SafeIdentityClaims {
        email: claims.email,
        account_id: claims.auth.and_then(|auth| auth.chatgpt_account_id),
        expires_at_unix: claims.expires_at_unix,
        nonce: claims.nonce,
    })
}

fn pkce_pair() -> Result<(String, String), getrandom::Error> {
    let verifier = random_urlsafe(64)?;
    let challenge = base64::Engine::encode(
        &base64::engine::general_purpose::URL_SAFE_NO_PAD,
        Sha256::digest(verifier.as_bytes()),
    );
    Ok((verifier, challenge))
}

fn random_urlsafe(bytes: usize) -> Result<String, getrandom::Error> {
    let mut raw = vec![0_u8; bytes];
    getrandom::fill(&mut raw)?;
    Ok(base64::Engine::encode(
        &base64::engine::general_purpose::URL_SAFE_NO_PAD,
        raw,
    ))
}

fn form_encode(value: &str) -> String {
    value
        .bytes()
        .flat_map(|byte| match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                vec![byte as char]
            }
            _ => format!("%{byte:02X}").chars().collect(),
        })
        .collect()
}
fn form_decode(value: &str) -> String {
    let mut out = Vec::new();
    let bytes = value.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            if let Ok(hex) = u8::from_str_radix(&value[i + 1..i + 3], 16) {
                out.push(hex);
                i += 3;
                continue;
            }
        }
        out.push(if bytes[i] == b'+' { b' ' } else { bytes[i] });
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::{Arc, Barrier};

    #[test]
    fn clud_auth_path_is_distinct_from_codex_cli_auth() {
        let home = Path::new("home");
        assert_eq!(
            credentials_path_at(home),
            Path::new("home/.clud/codex-auth.json")
        );
    }

    #[test]
    fn credential_store_round_trip_is_clud_owned() {
        let home = tempfile::TempDir::new().unwrap();
        let credentials = SubscriptionCredentials {
            access_token: "access-secret".to_string(),
            refresh_token: "refresh-secret".to_string(),
            account_id: Some("acct-1".to_string()),
            email: Some("person@example.test".to_string()),
            expires_at_unix: Some(123),
        };
        save_at(home.path(), &credentials).unwrap();
        assert_eq!(load_at(home.path()).unwrap(), Some(credentials));
        assert!(!home.path().join(".codex/auth.json").exists());
        assert!(remove_at(home.path()).unwrap());
        assert_eq!(load_at(home.path()).unwrap(), None);
    }

    #[test]
    fn credential_debug_never_exposes_tokens_or_account_id() {
        let credentials = SubscriptionCredentials {
            access_token: "access-secret".to_string(),
            refresh_token: "refresh-secret".to_string(),
            account_id: Some("acct-secret".to_string()),
            email: Some("person@example.test".to_string()),
            expires_at_unix: Some(123),
        };
        let debug = format!("{credentials:?}");
        for secret in ["access-secret", "refresh-secret", "acct-secret"] {
            assert!(!debug.contains(secret));
        }
    }

    #[test]
    fn pkce_and_callback_validation_follow_codex_compatibility_contract() {
        let (verifier, challenge) = pkce_pair().unwrap();
        assert!((43..=128).contains(&verifier.len()));
        assert_eq!(challenge.len(), 43);
        assert_eq!(
            callback_code("/auth/callback?code=abc&state=right", "right").unwrap(),
            "abc"
        );
        assert!(callback_code("/auth/callback?code=abc&state=wrong", "right").is_err());
        assert!(callback_code("/wrong?code=abc&state=right", "right").is_err());
        let authorize = authorization_url(
            "http://localhost:1455/auth/callback",
            &challenge,
            "state",
            "nonce",
        );
        assert!(authorize.contains("code_challenge_method=S256"));
        assert!(authorize.contains("state=state&nonce=nonce"));
    }

    #[test]
    fn status_requires_refresh_within_expiry_skew() {
        let credentials = SubscriptionCredentials {
            access_token: "access".to_string(),
            refresh_token: "refresh".to_string(),
            account_id: None,
            email: None,
            expires_at_unix: Some(160),
        };
        assert!(credentials_expired(&credentials, 100));
        assert!(!credentials_expired(&credentials, 99));
    }

    #[test]
    fn callback_wait_has_stable_timeout_and_cancellation_outcomes() {
        let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let timeout = wait_for_callback_with(&listener, "state", Instant::now(), || false);
        assert_eq!(
            timeout.unwrap_err(),
            "OAuth callback timed out; re-run `clud auth login codex`"
        );
        let cancelled = wait_for_callback_with(
            &listener,
            "state",
            Instant::now() + Duration::from_secs(1),
            || true,
        );
        assert_eq!(cancelled.unwrap_err(), "OAuth sign-in cancelled");
    }

    #[test]
    fn refresh_replaces_only_an_expired_record() {
        let home = tempfile::TempDir::new().unwrap();
        save_at(
            home.path(),
            &SubscriptionCredentials {
                access_token: "old-access".to_string(),
                refresh_token: "old-refresh".to_string(),
                account_id: Some("acct".to_string()),
                email: None,
                expires_at_unix: Some(100),
            },
        )
        .unwrap();
        let refreshed = refresh_if_needed_at(home.path(), 100, |token| {
            assert_eq!(token, "old-refresh");
            Ok((
                "new-access".to_string(),
                "new-refresh".to_string(),
                Some(200),
            ))
        })
        .unwrap();
        assert_eq!(refreshed.access_token, "new-access");
        assert_eq!(
            load_at(home.path()).unwrap().unwrap().refresh_token,
            "new-refresh"
        );
    }

    #[test]
    fn concurrent_refresh_contenders_share_one_atomic_replacement() {
        let home = tempfile::TempDir::new().unwrap();
        save_at(
            home.path(),
            &SubscriptionCredentials {
                access_token: "old-access".to_string(),
                refresh_token: "old-refresh".to_string(),
                account_id: None,
                email: None,
                expires_at_unix: Some(100),
            },
        )
        .unwrap();
        let refreshes = Arc::new(AtomicUsize::new(0));
        let barrier = Arc::new(Barrier::new(6));
        let mut contenders = Vec::new();
        for _ in 0..6 {
            let home = home.path().to_path_buf();
            let refreshes = Arc::clone(&refreshes);
            let barrier = Arc::clone(&barrier);
            contenders.push(thread::spawn(move || {
                barrier.wait();
                refresh_if_needed_at(&home, 100, |_| {
                    refreshes.fetch_add(1, Ordering::SeqCst);
                    Ok((
                        "new-access".to_string(),
                        "new-refresh".to_string(),
                        Some(200),
                    ))
                })
                .unwrap()
            }));
        }
        for contender in contenders {
            assert_eq!(contender.join().unwrap().access_token, "new-access");
        }
        assert_eq!(refreshes.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn identity_claims_extract_only_status_fields() {
        let payload = base64::Engine::encode(
            &base64::engine::general_purpose::URL_SAFE_NO_PAD,
            br#"{"email":"person@example.test","exp":123,"nonce":"expected-nonce","https://api.openai.com/auth":{"chatgpt_account_id":"acct-1"}}"#,
        );
        let claims = safe_identity_claims(&format!("x.{payload}.y")).unwrap();
        assert_eq!(claims.email.as_deref(), Some("person@example.test"));
        assert_eq!(claims.account_id.as_deref(), Some("acct-1"));
        assert_eq!(claims.expires_at_unix, Some(123));
        assert_eq!(claims.nonce.as_deref(), Some("expected-nonce"));
    }
}
