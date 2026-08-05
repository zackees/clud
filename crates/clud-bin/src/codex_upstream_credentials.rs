use super::*;

/// Platform API key credentials.
#[derive(Clone)]
pub struct ApiKeyCredentials {
    target: UpstreamTarget,
}

impl ApiKeyCredentials {
    pub fn new(api_key: impl AsRef<str>, base_url: Option<String>) -> Result<Self, UpstreamError> {
        let target = resolve_api_key_target(Some(api_key.as_ref().to_string()), base_url)?;
        Ok(Self { target })
    }

    /// Read the platform key from the environment.
    ///
    /// There is deliberately no fallback chain here: #629 requires that
    /// subscription auth and `OPENAI_API_KEY` never silently substitute for
    /// one another, so a missing key is an error rather than a downgrade.
    pub fn from_env() -> Result<Self, UpstreamError> {
        let target = resolve_api_key_target(
            std::env::var("OPENAI_API_KEY").ok(),
            std::env::var("OPENAI_BASE_URL").ok(),
        )?;
        Ok(Self { target })
    }
}

impl std::fmt::Debug for ApiKeyCredentials {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.debug_struct("ApiKeyCredentials").finish()
    }
}

impl CredentialSource for ApiKeyCredentials {
    fn resolve(&self) -> Result<UpstreamTarget, UpstreamError> {
        Ok(self.target.clone())
    }
}

/// Pure resolution so the policy is testable without touching process env.
/// A per-client identifier. Opaque upstream; it only has to be stable and
/// unique enough to correlate one session.
pub(super) fn new_session_id() -> String {
    let mut bytes = [0_u8; 16];
    if getrandom::fill(&mut bytes).is_err() {
        return "00000000-0000-4000-8000-000000000000".to_string();
    }
    let hex: String = bytes.iter().map(|byte| format!("{byte:02x}")).collect();
    format!(
        "{}-{}-4{}-8{}-{}",
        &hex[0..8],
        &hex[8..12],
        &hex[13..16],
        &hex[17..20],
        &hex[20..32]
    )
}

fn resolve_api_key_target(
    api_key: Option<String>,
    base_url: Option<String>,
) -> Result<UpstreamTarget, UpstreamError> {
    let key = api_key
        .map(|key| key.trim().to_string())
        .filter(|key| !key.is_empty())
        .ok_or(UpstreamError::Credentials("OPENAI_API_KEY is not set"))?;
    let base = base_url
        .map(|url| url.trim().to_string())
        .filter(|url| !url.is_empty())
        .unwrap_or_else(|| DEFAULT_BASE_URL.to_string());
    Ok(UpstreamTarget::new(base, format!("Bearer {key}")))
}

/// Credentials read from the Codex CLI's own `auth.json`.
///
/// This reads an existing login; it does **not** implement the OAuth/PKCE
/// flow, which stays #629's scope. It exists so the bridge can be validated
/// against a real subscription without inventing a second login path.
#[derive(Clone)]
pub struct CodexCliCredentials {
    target: UpstreamTarget,
}

/// ChatGPT subscription credentials created exclusively by `clud codex-auth`.
/// The presence of this record is an explicit persisted source selection: it
/// never falls back to `OPENAI_API_KEY`, and logout removes only this source.
#[derive(Clone)]
pub struct CludSubscriptionCredentials {
    target: UpstreamTarget,
}

impl CludSubscriptionCredentials {
    pub fn from_home() -> Result<Self, UpstreamError> {
        let home = dirs_home().ok_or(UpstreamError::Credentials("no home directory"))?;
        let credentials = match codex_auth::load_fresh_at(&home) {
            Ok(credentials) => credentials,
            Err(error) if error == "no clud ChatGPT subscription login" => {
                return Err(UpstreamError::Credentials(
                    "no clud ChatGPT subscription login",
                ));
            }
            Err(_) => {
                return Err(UpstreamError::Credentials(
                    "the Codex login has expired -- run `clud codex-auth login`",
                ));
            }
        };
        Self::from_record(credentials)
    }

    pub fn from_record(credentials: SubscriptionCredentials) -> Result<Self, UpstreamError> {
        if credentials.access_token.trim().is_empty() {
            return Err(UpstreamError::Credentials(
                "clud subscription credentials have no access token",
            ));
        }
        if credentials
            .expires_at_unix
            .is_some_and(|expiry| token_expiry_reached(expiry, SystemTime::now()))
        {
            return Err(UpstreamError::Credentials(CLUD_CREDENTIALS_EXPIRED));
        }
        Ok(Self {
            target: UpstreamTarget::new(
                CODEX_BACKEND_BASE_URL,
                format!("Bearer {}", credentials.access_token),
            )
            .with_account_id(credentials.account_id)
            .with_header("originator", CODEX_ORIGINATOR),
        })
    }
}

fn token_expiry_reached(expiry: u64, now: SystemTime) -> bool {
    now.duration_since(SystemTime::UNIX_EPOCH)
        .is_ok_and(|elapsed| elapsed + TOKEN_EXPIRY_SKEW >= Duration::from_secs(expiry))
}

impl std::fmt::Debug for CludSubscriptionCredentials {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("CludSubscriptionCredentials")
            .finish()
    }
}

impl CredentialSource for CludSubscriptionCredentials {
    fn resolve(&self) -> Result<UpstreamTarget, UpstreamError> {
        Ok(self.target.clone())
    }
}

impl CodexCliCredentials {
    /// Load from `$CODEX_HOME/auth.json`, defaulting to `~/.codex`.
    pub fn from_codex_home() -> Result<Self, UpstreamError> {
        let home = std::env::var_os("CODEX_HOME")
            .map(std::path::PathBuf::from)
            .or_else(|| dirs_home().map(|home| home.join(".codex")))
            .ok_or(UpstreamError::Credentials("no Codex home directory"))?;
        let raw = std::fs::read(home.join("auth.json"))
            .map_err(|_| UpstreamError::Credentials("Codex auth.json is not readable"))?;
        Self::from_auth_json(&raw)
    }

    /// Parse an `auth.json` document. Split out so the shape is testable
    /// without touching a real credential file.
    pub fn from_auth_json(raw: &[u8]) -> Result<Self, UpstreamError> {
        let document: serde_json::Value = serde_json::from_slice(raw)
            .map_err(|_| UpstreamError::Credentials("Codex auth.json is not valid JSON"))?;
        let access_token = document
            .pointer("/tokens/access_token")
            .and_then(serde_json::Value::as_str)
            .filter(|token| !token.trim().is_empty())
            .ok_or(UpstreamError::Credentials(
                "Codex auth.json has no access token",
            ))?;
        // Full refresh is #629's scope. This is only the guardrail: a login
        // that has already expired must fail with an instruction the user can
        // act on, rather than an opaque upstream error a retry cannot fix.
        if token_is_expired(access_token, SystemTime::now()) {
            return Err(UpstreamError::Credentials(CREDENTIALS_EXPIRED));
        }
        let account_id = document
            .pointer("/tokens/account_id")
            .and_then(serde_json::Value::as_str)
            .filter(|id| !id.trim().is_empty())
            .map(str::to_string);
        Ok(Self {
            target: UpstreamTarget::new(CODEX_BACKEND_BASE_URL, format!("Bearer {access_token}"))
                .with_account_id(account_id),
        })
    }
}

/// Whether a bearer is a JWT whose `exp` claim has passed.
///
/// The signature is deliberately not verified — that is the issuer's job, and
/// clud has no key. This only reads a claim to avoid *starting* a turn on a
/// token that is already dead. A bearer that is not a JWT, or carries no `exp`,
/// is treated as live: opaque tokens are legitimate and must keep working.
fn token_is_expired(token: &str, now: SystemTime) -> bool {
    let Some(expiry) = token_expiry(token) else {
        return false;
    };
    let Ok(elapsed) = now.duration_since(SystemTime::UNIX_EPOCH) else {
        return false;
    };
    elapsed + TOKEN_EXPIRY_SKEW >= Duration::from_secs(expiry)
}

/// The `exp` claim of a JWT, if the bearer is one.
fn token_expiry(token: &str) -> Option<u64> {
    let payload = token.split('.').nth(1)?;
    let decoded = base64url_decode(payload)?;
    let claims: serde_json::Value = serde_json::from_slice(&decoded).ok()?;
    claims.get("exp").and_then(serde_json::Value::as_u64)
}

/// Minimal unpadded base64url decoder. A dependency for one claim read would be
/// a poor trade, and the alphabet is fixed.
fn base64url_decode(input: &str) -> Option<Vec<u8>> {
    const ALPHABET: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";
    let mut buffer = 0_u32;
    let mut bits = 0_u32;
    let mut out = Vec::with_capacity(input.len() * 3 / 4);
    for byte in input.bytes() {
        if byte == b'=' {
            break;
        }
        let value = ALPHABET.iter().position(|candidate| *candidate == byte)? as u32;
        buffer = (buffer << 6) | value;
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push((buffer >> bits) as u8);
        }
    }
    Some(out)
}

fn dirs_home() -> Option<std::path::PathBuf> {
    std::env::var_os("USERPROFILE")
        .or_else(|| std::env::var_os("HOME"))
        .map(std::path::PathBuf::from)
}

impl std::fmt::Debug for CodexCliCredentials {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.debug_struct("CodexCliCredentials").finish()
    }
}

impl CredentialSource for CodexCliCredentials {
    fn resolve(&self) -> Result<UpstreamTarget, UpstreamError> {
        Ok(self.target.clone())
    }
}

/// The credential source the bridge uses, resolved once per launch.
#[derive(Debug, Clone)]
pub enum ResolvedCredentials {
    ApiKey(ApiKeyCredentials),
    Subscription(CludSubscriptionCredentials),
}

impl ResolvedCredentials {
    /// A clud-managed subscription record is an explicit, persisted selection
    /// made by `clud codex-auth login`; while it exists no API-key fallback is
    /// attempted. Without it, only the platform API-key path is considered.
    pub fn resolve_default() -> Result<Self, UpstreamError> {
        Self::resolve_with(
            CludSubscriptionCredentials::from_home(),
            ApiKeyCredentials::from_env,
        )
    }

    fn resolve_with(
        subscription: Result<CludSubscriptionCredentials, UpstreamError>,
        api_key: impl FnOnce() -> Result<ApiKeyCredentials, UpstreamError>,
    ) -> Result<Self, UpstreamError> {
        match subscription {
            Ok(credentials) => Ok(Self::Subscription(credentials)),
            Err(UpstreamError::Credentials("no clud ChatGPT subscription login")) => {
                api_key().map(Self::ApiKey)
            }
            Err(error) => Err(error),
        }
    }

    pub fn describe(&self) -> &'static str {
        match self {
            Self::ApiKey(_) => "OPENAI_API_KEY",
            Self::Subscription(_) => "clud ChatGPT subscription",
        }
    }
}

impl CredentialSource for ResolvedCredentials {
    fn resolve(&self) -> Result<UpstreamTarget, UpstreamError> {
        match self {
            Self::ApiKey(credentials) => credentials.resolve(),
            Self::Subscription(credentials) => credentials.resolve(),
        }
    }
}

#[derive(Debug, Clone)]
pub struct UpstreamConfig {
    pub connect_timeout: Duration,
    pub read_timeout: Duration,
    /// Maximum time to the first upstream byte, while downstream can still
    /// choose a non-200 status and retry safely. `None` preserves the idle
    /// read timeout for callers that deliberately do not impose this budget.
    pub first_frame_timeout: Option<Duration>,
    pub overall_timeout: Duration,
    pub max_response_bytes: usize,
    /// Attempt budget for a recognised transient failure.
    pub max_attempts: u32,
    /// Attempt budget for an unrecognised 5xx.
    pub unknown_max_attempts: u32,
    /// Base of the exponential backoff.
    pub retry_delay: Duration,
    /// Ceiling on one sleep, including a `Retry-After`-derived one.
    pub max_retry_delay: Duration,
    /// Ceiling on total time spent sleeping between attempts.
    pub max_retry_elapsed: Duration,
}

impl Default for UpstreamConfig {
    fn default() -> Self {
        Self {
            connect_timeout: DEFAULT_CONNECT_TIMEOUT,
            read_timeout: DEFAULT_READ_TIMEOUT,
            first_frame_timeout: None,
            overall_timeout: DEFAULT_OVERALL_TIMEOUT,
            max_response_bytes: DEFAULT_MAX_RESPONSE_BYTES,
            max_attempts: DEFAULT_MAX_ATTEMPTS,
            unknown_max_attempts: DEFAULT_UNKNOWN_MAX_ATTEMPTS,
            retry_delay: DEFAULT_RETRY_DELAY,
            max_retry_delay: DEFAULT_MAX_RETRY_DELAY,
            max_retry_elapsed: DEFAULT_MAX_RETRY_ELAPSED,
        }
    }
}

/// Outcome of one completed stream.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StreamOutcome {
    pub attempts: u32,
    pub bytes: usize,
}

pub(super) type RetryObserver =
    std::sync::Arc<dyn Fn(&UpstreamError, u32, u32, Option<Duration>) + Send + Sync>;
