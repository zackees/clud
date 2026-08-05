//! Upstream Responses client for the Codex bridge (issue #627 phase 3, step 4).
//!
//! Two responsibilities, kept apart on purpose:
//!
//! - [`CredentialSource`] yields the base URL, auth material, account headers,
//!   and model policy. Putting this behind a trait is what lets #629's
//!   subscription auth reuse every line of translation without touching it.
//! - [`UpstreamClient`] performs one streaming `POST /v1/responses`, handing
//!   each byte chunk to a sink as it arrives.
//!
//! ## The retry boundary
//!
//! The issue's rule is "never replay a request after downstream-visible output
//! has begun", and step 1 is why it is absolute: once `write_event_stream` has
//! flushed a single frame, the 200 and its headers are already on the wire, so
//! there is no status code left to change and a replay would duplicate content
//! the user has already seen. The client therefore tracks whether the sink has
//! accepted anything, and refuses to retry once it has — regardless of how
//! retryable the failure looks.
//!
//! ## Secrets
//!
//! No client header is forwarded upstream. The allowlist is empty by
//! construction: this module builds every outbound header itself, so the
//! harness's own Anthropic bearer cannot leak into an upstream request. In the
//! other direction, transport failures are classified into fixed strings
//! rather than carrying the library's message, because those messages embed
//! the URL and would put the endpoint into logs.

use std::error::Error as _;
use std::io::Read;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant, SystemTime};

use crate::codex_auth::{self, SubscriptionCredentials};
use crate::codex_model::ModelSpec;

pub const DEFAULT_BASE_URL: &str = "https://api.openai.com";
/// ChatGPT-subscription auth is a *different backend*, not just a different
/// token (`openai/codex` `model-provider-info`). Endpoint, system-prompt
/// placement, and required headers all change with it.
pub const CODEX_BACKEND_BASE_URL: &str = "https://chatgpt.com/backend-api/codex";
/// Identifies the client to the Codex backend.
pub const CODEX_ORIGINATOR: &str = "codex_cli_rs";
/// Latest stable `openai/codex` release verified for the ChatGPT backend.
///
/// Keep this separate from clud's package version: the backend interprets it
/// as a Codex compatibility version. See the request-header regression test.
pub const CODEX_CLIENT_VERSION: &str = "0.146.0";
const CODEX_BETA_HEADER_VALUE: &str = "responses=experimental";
const DEFAULT_CONNECT_TIMEOUT: Duration = Duration::from_secs(30);
/// Idle timeout between reads, not a cap on the whole turn: a model may think
/// for minutes before its first token, which is healthy, whereas a socket that
/// yields nothing for this long is not.
const DEFAULT_READ_TIMEOUT: Duration = Duration::from_secs(120);
const DEFAULT_OVERALL_TIMEOUT: Duration = Duration::from_secs(3600);
const DEFAULT_MAX_RESPONSE_BYTES: usize = 64 * 1024 * 1024;
/// Retry budget for a failure we positively recognise as transient.
const DEFAULT_MAX_ATTEMPTS: u32 = 4;
/// Retry budget for a 5xx whose body matches neither the permanent nor the
/// transient signatures. Deliberately smaller than the transient budget: #764
/// documents how treating every unrecognised 5xx as fully retryable is what
/// turns one bad request into a credential-burning cascade upstream.
const DEFAULT_UNKNOWN_MAX_ATTEMPTS: u32 = 2;
const DEFAULT_RETRY_DELAY: Duration = Duration::from_millis(500);
/// Ceiling on any single backoff sleep, including one derived from
/// `Retry-After`: a generous server hint must not pin a turn open.
const DEFAULT_MAX_RETRY_DELAY: Duration = Duration::from_secs(8);
/// Ceiling on time spent sleeping between attempts, across the whole request.
const DEFAULT_MAX_RETRY_ELAPSED: Duration = Duration::from_secs(30);
/// How much of an error body is read before classification. Enough for a JSON
/// error envelope, small enough to bound allocation on a hostile response.
const ERROR_BODY_PREFIX_LIMIT: usize = 8 * 1024;
/// Treat a login this close to its expiry as already expired, so a turn cannot
/// start on a token that dies mid-flight.
const TOKEN_EXPIRY_SKEW: Duration = Duration::from_secs(60);

/// The one credential failure a user can fix without reading a log.
///
/// Shared rather than duplicated because [`crate::codex_pipeline`] matches on
/// it to decide that this reason — unlike the others, which name environment
/// variables — is safe and useful to forward to the harness.
pub const CREDENTIALS_EXPIRED: &str = "the Codex login has expired -- run `codex login`";
/// Expiry guidance for clud's separately-managed subscription credential.
pub const CLUD_CREDENTIALS_EXPIRED: &str =
    "the Codex login has expired -- run `clud codex-auth login`";

/// Whether a failure could ever succeed on a fresh attempt.
///
/// This is the distinction the retry loop was missing (#764): upstream returns
/// *permanent* rejections wearing a 5xx costume — a model that needs a newer
/// client, an unsupported parameter — and retrying those can never succeed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FailureClass {
    /// Retrying cannot help. Fail on the first attempt.
    Permanent,
    /// Recognised as retryable: transport, 408/429, or a 5xx whose body reads
    /// like an outage.
    Transient,
    /// A 5xx we do not recognise. Retried, but on a reduced budget.
    Unknown,
    /// The account is out of quota or credits. Retrying is not merely
    /// unhelpful, it is actively wrong: a multi-day exhaustion would burn the
    /// whole budget in under a second and report nothing the user can act on.
    /// Split out of [`Self::Permanent`] because it is the one failure with a
    /// *reset time* and a remedy, and the client-facing message says so.
    Exhausted,
}

/// A non-2xx upstream response, reduced to what is safe to keep.
///
/// The raw body is **never** retained. It is read to a bounded prefix, used to
/// classify the failure, mined for a scrubbed one-line detail, and dropped. The
/// previous code discarded the whole response unread, which left an operator
/// unable to tell a Cloudflare edge blip from a hard rejection.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UpstreamFailure {
    status: u16,
    class: FailureClass,
    /// `x-request-id`. Opaque correlation handle, safe to log and to name in a
    /// client-facing message.
    request_id: Option<String>,
    /// `cf-ray`. Its presence is what identifies a failure as edge-side rather
    /// than application-side.
    cf_ray: Option<String>,
    /// `Retry-After`, already clamped to the configured ceiling.
    retry_after: Option<Duration>,
    /// `resets_in_seconds` / `resets_at` from a `usage_limit_reached` body.
    resets_in: Option<Duration>,
    /// Scrubbed reason for logs. Token-shaped runs are redacted, and this never
    /// travels downstream.
    detail: Option<String>,
}

impl UpstreamFailure {
    pub fn status(&self) -> u16 {
        self.status
    }

    pub fn class(&self) -> FailureClass {
        self.class
    }

    pub fn request_id(&self) -> Option<&str> {
        self.request_id.as_deref()
    }

    pub fn cf_ray(&self) -> Option<&str> {
        self.cf_ray.as_deref()
    }

    pub fn retry_after(&self) -> Option<Duration> {
        self.retry_after
    }

    pub fn resets_in(&self) -> Option<Duration> {
        self.resets_in
    }

    pub fn detail(&self) -> Option<&str> {
        self.detail.as_deref()
    }

    /// Attempt budget this failure earns.
    fn max_attempts(&self, config: &UpstreamConfig) -> u32 {
        match self.class {
            FailureClass::Permanent | FailureClass::Exhausted => 1,
            FailureClass::Transient => config.max_attempts.max(1),
            FailureClass::Unknown => config.unknown_max_attempts.max(1),
        }
    }

    /// One line for the operator log. Safe by construction: every field here is
    /// either a status, an opaque id, or already scrubbed.
    pub fn diagnostic(&self) -> String {
        let mut parts = vec![
            format!("status={}", self.status),
            format!("class={:?}", self.class),
        ];
        if let Some(id) = &self.request_id {
            parts.push(format!("request-id={id}"));
        }
        if let Some(ray) = &self.cf_ray {
            parts.push(format!("cf-ray={ray}"));
        }
        if let Some(after) = self.retry_after {
            parts.push(format!("retry-after={}s", after.as_secs()));
        }
        if let Some(resets) = self.resets_in {
            parts.push(format!("resets-in={}s", resets.as_secs()));
        }
        if let Some(detail) = &self.detail {
            parts.push(format!("detail={detail:?}"));
        }
        parts.join(" ")
    }

    /// Build from a status plus the response's headers and body prefix.
    ///
    /// Split from the transport so classification is testable without a socket.
    pub fn from_parts(
        status: u16,
        header: impl Fn(&str) -> Option<String>,
        body_prefix: &str,
        max_retry_delay: Duration,
    ) -> Self {
        let request_id = header("x-request-id").filter(|value| !value.trim().is_empty());
        let cf_ray = header("cf-ray").filter(|value| !value.trim().is_empty());
        let retry_after = header("retry-after")
            .and_then(|value| value.trim().parse::<u64>().ok())
            .map(Duration::from_secs)
            .map(|value| value.min(max_retry_delay));
        let resets_in = parse_resets_in(body_prefix);
        let detail = error_detail(body_prefix);
        let class = classify(status, body_prefix);
        Self {
            status,
            class,
            request_id,
            cf_ray,
            retry_after,
            resets_in,
            detail,
        }
    }
}

/// Body signatures that mean "this request will never succeed as written".
///
/// Kept as substrings of the lowercased body rather than a JSON shape, because
/// the same rejection arrives as a 400, a 422 or a 502 depending on which hop
/// produced it.
const PERMANENT_SIGNATURES: &[&str] = &[
    "requires a newer version",
    "unsupported_value",
    "unsupported_parameter",
    "unsupported_country_region_territory",
    "invalid_request_error",
    "unknown provider",
    "model_not_found",
    "does not exist",
    "invalid_api_key",
];

/// Body signatures that mark an account as out of quota or credits.
///
/// Checked before every other rule, including the 408/429-are-transient rule:
/// the ChatGPT backend reports exhaustion as a 429, which is indistinguishable
/// from an ordinary throttle by status alone. That ambiguity is why a real
/// exhaustion was retried three times in ~750ms and reported as a bare
/// "status 429".
const EXHAUSTED_SIGNATURES: &[&str] = &[
    "insufficient_quota",
    "usage_limit_reached",
    "quota_exceeded",
    "out of credits",
    "credit balance",
    "billing_hard_limit_reached",
];

/// Body signatures that positively mark a 5xx as an outage rather than a
/// rejection. Anything else at 5xx is [`FailureClass::Unknown`].
const TRANSIENT_SIGNATURES: &[&str] = &[
    "bad gateway",
    "gateway timeout",
    "service unavailable",
    "temporarily unavailable",
    "overloaded",
    "try again",
    "circuit_open",
    "circuit open",
    "cloudflare",
    "internal server error",
    "server_error",
    "timeout",
];

/// Statuses the bridge passes through untouched; retrying them cannot help.
const NON_RETRYABLE_STATUSES: &[u16] = &[400, 401, 403, 404, 405, 409, 413, 422];

fn classify(status: u16, body_prefix: &str) -> FailureClass {
    let body = body_prefix.to_ascii_lowercase();
    if EXHAUSTED_SIGNATURES
        .iter()
        .any(|signature| body.contains(signature))
    {
        return FailureClass::Exhausted;
    }
    if PERMANENT_SIGNATURES
        .iter()
        .any(|signature| body.contains(signature))
    {
        return FailureClass::Permanent;
    }
    if NON_RETRYABLE_STATUSES.contains(&status) {
        return FailureClass::Permanent;
    }
    // 408 and 429 are retryable by definition; 429 additionally carries a reset
    // hint that the retry loop honours.
    if status == 408 || status == 429 {
        return FailureClass::Transient;
    }
    if status >= 500 {
        if body.trim().is_empty()
            || TRANSIENT_SIGNATURES
                .iter()
                .any(|signature| body.contains(signature))
        {
            return FailureClass::Transient;
        }
        return FailureClass::Unknown;
    }
    FailureClass::Permanent
}

/// Pull `resets_in_seconds` (or `resets_at`, as a delta) out of a rate-limit
/// body. Unparsed until CLIProxyAPI#1666; clud made the same omission.
fn parse_resets_in(body_prefix: &str) -> Option<Duration> {
    let value: serde_json::Value = serde_json::from_str(body_prefix).ok()?;
    for pointer in [
        "/error/resets_in_seconds",
        "/resets_in_seconds",
        "/error/resets_in",
        "/resets_in",
    ] {
        if let Some(seconds) = value.pointer(pointer).and_then(serde_json::Value::as_u64) {
            return Some(Duration::from_secs(seconds));
        }
    }
    None
}

/// A short, scrubbed reason for the operator log.
///
/// Prefers the enum-like `code`/`type` fields. `message` is free text written
/// by upstream and can quote account identifiers or key fragments, so it is
/// passed through [`scrub`] before it is kept, and it never leaves the log.
fn error_detail(body_prefix: &str) -> Option<String> {
    let value: serde_json::Value = serde_json::from_str(body_prefix).ok()?;
    let at = |pointer: &str| {
        value
            .pointer(pointer)
            .and_then(serde_json::Value::as_str)
            .map(str::trim)
            .filter(|text| !text.is_empty())
            .map(str::to_string)
    };
    let code = at("/error/code")
        .or_else(|| at("/error/type"))
        .or_else(|| at("/code"));
    let message = at("/error/message").or_else(|| at("/message"));
    let detail = match (code, message) {
        (Some(code), Some(message)) => format!("{code}: {}", scrub(&message)),
        (Some(code), None) => code,
        (None, Some(message)) => scrub(&message),
        (None, None) => return None,
    };
    Some(truncate_chars(&detail, 200))
}

/// Redact token-shaped runs from free text.
///
/// Deliberately blunt: a long unbroken alphanumeric run in an error message is
/// far more likely to be a key, an org id or an account id than a word worth
/// keeping.
fn scrub(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut run = String::new();
    let flush = |run: &mut String, out: &mut String| {
        if run.len() >= 20 || run.starts_with("sk-") || run.starts_with("org_") {
            out.push_str("[redacted]");
        } else {
            out.push_str(run);
        }
        run.clear();
    };
    for character in text.chars() {
        if character.is_ascii_alphanumeric() || character == '-' || character == '_' {
            run.push(character);
        } else {
            flush(&mut run, &mut out);
            out.push(character);
        }
    }
    flush(&mut run, &mut out);
    out
}

fn truncate_chars(text: &str, limit: usize) -> String {
    if text.chars().count() <= limit {
        return text.to_string();
    }
    text.chars().take(limit).collect::<String>() + "..."
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UpstreamError {
    /// No usable credentials. Deliberately never names the variable's value.
    Credentials(&'static str),
    /// Upstream returned a non-2xx status, reduced to a classified summary.
    /// The raw body is never carried: it can contain account identifiers and
    /// key fragments.
    Status(UpstreamFailure),
    /// Classified transport failure. A fixed string, never the library's
    /// message, which embeds the URL.
    Transport(&'static str),
    /// The response exceeded the configured byte budget.
    TooLarge,
    /// The overall deadline elapsed.
    Timeout,
    /// The caller asked to stop.
    Cancelled,
    /// The sink rejected a chunk — normally the downstream client hung up.
    Downstream(&'static str),
}

impl std::fmt::Display for UpstreamError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Credentials(what) => {
                write!(formatter, "upstream credentials unavailable: {what}")
            }
            Self::Status(failure) => {
                write!(formatter, "upstream returned status {}", failure.status)
            }
            Self::Transport(what) => write!(formatter, "upstream transport failure: {what}"),
            Self::TooLarge => formatter.write_str("upstream response exceeded the size budget"),
            Self::Timeout => formatter.write_str("upstream request timed out"),
            Self::Cancelled => formatter.write_str("upstream request cancelled"),
            Self::Downstream(what) => write!(formatter, "downstream sink failed: {what}"),
        }
    }
}

impl std::error::Error for UpstreamError {}

impl UpstreamError {
    /// Whether this failure could plausibly succeed on a fresh attempt.
    ///
    /// Note this is only half the decision: [`UpstreamClient::stream`] also
    /// requires that nothing has reached the sink yet, and consults
    /// [`UpstreamError::max_attempts`] for how much budget the failure earns.
    pub fn is_retryable(&self) -> bool {
        match self {
            Self::Transport(_) | Self::Timeout => true,
            Self::Status(failure) => !matches!(
                failure.class,
                FailureClass::Permanent | FailureClass::Exhausted
            ),
            Self::Credentials(_) | Self::TooLarge | Self::Cancelled | Self::Downstream(_) => false,
        }
    }

    /// Total attempts this failure is worth, including the one already spent.
    ///
    /// A transport failure gets the full transient budget; a classified status
    /// gets whatever its class earns.
    pub fn max_attempts(&self, config: &UpstreamConfig) -> u32 {
        match self {
            Self::Transport(_) | Self::Timeout => config.max_attempts.max(1),
            Self::Status(failure) => failure.max_attempts(config),
            _ => 1,
        }
    }

    /// A server-supplied hint about when to try again, already clamped.
    pub fn retry_hint(&self) -> Option<Duration> {
        match self {
            Self::Status(failure) => failure.retry_after,
            _ => None,
        }
    }

    /// The classified summary, when this is a status failure.
    pub fn failure(&self) -> Option<&UpstreamFailure> {
        match self {
            Self::Status(failure) => Some(failure),
            _ => None,
        }
    }
}

/// Everything needed to address the upstream, resolved per request.
#[derive(Clone)]
pub struct UpstreamTarget {
    base_url: String,
    authorization: String,
    extra_headers: Vec<(String, String)>,
    model_override: Option<ModelSpec>,
    account_id: Option<String>,
    prompt_cache_key: Option<String>,
}

impl UpstreamTarget {
    pub fn new(base_url: impl Into<String>, authorization: impl Into<String>) -> Self {
        Self {
            base_url: base_url.into(),
            authorization: authorization.into(),
            extra_headers: Vec::new(),
            model_override: None,
            account_id: None,
            prompt_cache_key: None,
        }
    }

    /// ChatGPT account id. Sent as `ChatGPT-Account-ID`, and its presence is
    /// what marks a target as the Codex backend.
    pub fn with_account_id(mut self, account_id: Option<String>) -> Self {
        self.account_id = account_id;
        self
    }

    /// Buys prompt-cache hits across turns; omitting it pays full input price.
    pub fn with_prompt_cache_key(mut self, key: Option<String>) -> Self {
        self.prompt_cache_key = key;
        self
    }

    pub fn prompt_cache_key(&self) -> Option<&str> {
        self.prompt_cache_key.as_deref()
    }

    /// Whether this target is the ChatGPT Codex backend rather than the
    /// platform API. Drives system-prompt placement (#750 A3).
    pub fn uses_codex_backend(&self) -> bool {
        self.base_url.contains("chatgpt.com")
    }

    pub fn with_header(mut self, name: impl Into<String>, value: impl Into<String>) -> Self {
        self.extra_headers.push((name.into(), value.into()));
        self
    }

    pub fn with_model_override(mut self, model: Option<ModelSpec>) -> Self {
        self.model_override = model;
        self
    }

    pub fn model_override(&self) -> Option<&ModelSpec> {
        self.model_override.as_ref()
    }

    /// Absolute URL of the Responses endpoint.
    ///
    /// A base that already names the endpoint is left alone, so an operator
    /// can point at a gateway that exposes it at a fixed path.
    pub fn responses_url(&self) -> String {
        let trimmed = self.base_url.trim_end_matches('/');
        if trimmed.ends_with("/responses") {
            trimmed.to_string()
        } else if trimmed.ends_with("/v1") || self.uses_codex_backend() {
            // The Codex backend exposes `/responses` directly under its base;
            // it has no `/v1` segment.
            format!("{trimmed}/responses")
        } else {
            format!("{trimmed}/v1/responses")
        }
    }
}

/// Neither the bearer nor the endpoint appears here: error snapshots and log
/// lines must not become a credential map.
impl std::fmt::Debug for UpstreamTarget {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("UpstreamTarget")
            .field("base_url", &"[redacted]")
            .field("authorization", &"[redacted]")
            .field("extra_headers", &self.extra_headers.len())
            .field("model_override", &self.model_override)
            .field(
                "account_id",
                &self.account_id.as_ref().map(|_| "[redacted]"),
            )
            .finish()
    }
}

/// Where upstream credentials come from.
///
/// #629 adds a subscription-backed implementation; because translation only
/// ever sees this trait, none of it has to change when that lands.
pub trait CredentialSource: Send + Sync {
    fn resolve(&self) -> Result<UpstreamTarget, UpstreamError>;
}

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
fn new_session_id() -> String {
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

type RetryObserver =
    std::sync::Arc<dyn Fn(&UpstreamError, u32, u32, Option<Duration>) + Send + Sync>;

pub struct UpstreamClient<C: CredentialSource> {
    credentials: C,
    config: UpstreamConfig,
    /// Stable for the life of the client so upstream can correlate a turn.
    session_id: String,
    retry_observer: Option<RetryObserver>,
}

impl<C: CredentialSource> std::fmt::Debug for UpstreamClient<C> {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("UpstreamClient")
            .field("config", &self.config)
            .finish()
    }
}

impl<C: CredentialSource> UpstreamClient<C> {
    pub fn new(credentials: C, config: UpstreamConfig) -> Self {
        Self {
            credentials,
            config,
            session_id: new_session_id(),
            retry_observer: None,
        }
    }

    pub fn with_retry_observer(
        mut self,
        observer: impl Fn(&UpstreamError, u32, u32, Option<Duration>) + Send + Sync + 'static,
    ) -> Self {
        self.retry_observer = Some(std::sync::Arc::new(observer));
        self
    }

    pub fn session_id(&self) -> &str {
        &self.session_id
    }

    pub fn credentials(&self) -> &C {
        &self.credentials
    }

    /// Stream one Responses request, handing each chunk to `sink` as it lands.
    ///
    /// `cancel` is polled between reads. A blocking read cannot be interrupted,
    /// so cancellation latency is bounded by `read_timeout` rather than being
    /// immediate; that is the cost of not putting an async runtime behind a
    /// synchronous bridge.
    pub fn stream(
        &self,
        body: &[u8],
        cancel: &AtomicBool,
        sink: &mut dyn FnMut(&[u8]) -> Result<(), UpstreamError>,
    ) -> Result<StreamOutcome, UpstreamError> {
        let delivered = AtomicBool::new(false);
        let mut legacy_sink = |chunk: &[u8]| {
            // Preserve the legacy raw-byte commit boundary for direct callers.
            // The pipeline uses `stream_with_commit` to mark only frames it
            // has actually made downstream-visible.
            delivered.store(true, Ordering::Release);
            sink(chunk)
        };
        self.stream_with_commit(body, cancel, &mut legacy_sink, &delivered)
    }

    pub(crate) fn stream_with_commit(
        &self,
        body: &[u8],
        cancel: &AtomicBool,
        sink: &mut dyn FnMut(&[u8]) -> Result<(), UpstreamError>,
        delivered: &AtomicBool,
    ) -> Result<StreamOutcome, UpstreamError> {
        let target = self.credentials.resolve()?;
        let deadline = Instant::now() + self.config.overall_timeout;
        let mut attempt = 0_u32;
        let mut slept = Duration::ZERO;

        loop {
            attempt += 1;
            if cancel.load(Ordering::Acquire) {
                return Err(UpstreamError::Cancelled);
            }
            match self.attempt(&target, body, cancel, deadline, sink, delivered) {
                Ok(bytes) => {
                    return Ok(StreamOutcome {
                        attempts: attempt,
                        bytes,
                    })
                }
                Err(error) => {
                    // The absolute rule, unchanged (DD-029): once anything has
                    // reached the sink the response is committed, however
                    // retryable the failure looks. Everything below only ever
                    // widens the *pre-commit* window.
                    let budget = error.max_attempts(&self.config);
                    let retry_allowed = !delivered.load(Ordering::Acquire)
                        && error.is_retryable()
                        && attempt < budget
                        && Instant::now() < deadline;
                    if !retry_allowed {
                        if let Some(observer) = &self.retry_observer {
                            observer(&error, attempt, budget, None);
                        }
                        return Err(error);
                    }
                    // A server hint wins over our own guess, but is clamped by
                    // the same ceiling so it cannot pin the turn open.
                    let backoff = match error.retry_hint() {
                        Some(hint) => hint.min(self.config.max_retry_delay),
                        None => backoff_delay(attempt, &self.config, jitter_fraction()),
                    };
                    if slept + backoff > self.config.max_retry_elapsed {
                        if let Some(observer) = &self.retry_observer {
                            observer(&error, attempt, budget, None);
                        }
                        return Err(error);
                    }
                    if let Some(observer) = &self.retry_observer {
                        observer(&error, attempt, budget, Some(backoff));
                    }
                    slept += backoff;
                    if wait_or_cancel(backoff, cancel) {
                        return Err(UpstreamError::Cancelled);
                    }
                }
            }
        }
    }

    fn attempt(
        &self,
        target: &UpstreamTarget,
        body: &[u8],
        cancel: &AtomicBool,
        deadline: Instant,
        sink: &mut dyn FnMut(&[u8]) -> Result<(), UpstreamError>,
        delivered: &AtomicBool,
    ) -> Result<usize, UpstreamError> {
        let attempt_deadline = self
            .config
            .first_frame_timeout
            .map(|timeout| Instant::now() + timeout);
        let agent = ureq::AgentBuilder::new()
            .timeout_connect(self.config.connect_timeout)
            .timeout_read(self.config.read_timeout)
            .build();

        // Every header is constructed here. Nothing from the downstream request
        // is copied, so the harness's Anthropic bearer cannot travel upstream.
        // Identity headers the Codex client sends. `session-id`/`thread-id`
        // are hyphenated upstream, not underscored.
        let mut request = agent
            .post(&target.responses_url())
            .set("Content-Type", "application/json")
            .set("Accept", "text/event-stream")
            .set("Authorization", &target.authorization)
            .set("originator", CODEX_ORIGINATOR)
            .set("OpenAI-Beta", CODEX_BETA_HEADER_VALUE)
            .set("version", CODEX_CLIENT_VERSION)
            .set("session-id", &self.session_id)
            .set("thread-id", &self.session_id)
            .set("x-client-request-id", &self.session_id)
            .set(
                "User-Agent",
                &format!("{CODEX_ORIGINATOR}/{CODEX_CLIENT_VERSION} (clud)"),
            );
        if let Some(account_id) = target.account_id.as_deref() {
            request = request.set("ChatGPT-Account-ID", account_id);
        }
        for (name, value) in &target.extra_headers {
            request = request.set(name, value);
        }

        let response = match match attempt_deadline {
            Some(deadline) => request
                .timeout(deadline.saturating_duration_since(Instant::now()))
                .send_bytes(body),
            None => request.send_bytes(body),
        } {
            Ok(response) => response,
            // The response is *read*, not discarded. Its headers carry the
            // `cf-ray` that distinguishes an edge failure from an application
            // one, and its body is what separates a permanent rejection from a
            // transient outage (#764). Neither survives past classification.
            Err(ureq::Error::Status(status, response)) => {
                return Err(UpstreamError::Status(capture_failure(
                    status,
                    response,
                    self.config.max_retry_delay,
                )));
            }
            Err(ureq::Error::Transport(error)) => {
                return Err(
                    if attempt_deadline.is_some_and(|at| Instant::now() >= at)
                        && is_timeout_transport(&error)
                    {
                        UpstreamError::Timeout
                    } else {
                        UpstreamError::Transport("connection failed")
                    },
                );
            }
        };

        let mut reader = response.into_reader();
        let first_frame_deadline = attempt_deadline;
        let mut buffer = [0_u8; 8192];
        let mut total = 0_usize;
        loop {
            if cancel.load(Ordering::Acquire) {
                return Err(UpstreamError::Cancelled);
            }
            if Instant::now() >= deadline
                || (!delivered.load(Ordering::Acquire)
                    && first_frame_deadline.is_some_and(|at| Instant::now() >= at))
            {
                return Err(UpstreamError::Timeout);
            }
            let count = match reader.read(&mut buffer) {
                Ok(0) => break,
                Ok(count) => count,
                Err(error) if error.kind() == std::io::ErrorKind::TimedOut => {
                    return Err(
                        if !delivered.load(Ordering::Acquire)
                            && first_frame_deadline.is_some_and(|at| Instant::now() >= at)
                        {
                            UpstreamError::Timeout
                        } else {
                            UpstreamError::Transport("read timed out")
                        },
                    );
                }
                Err(_) => return Err(UpstreamError::Transport("read failed")),
            };
            total += count;
            if total > self.config.max_response_bytes {
                return Err(UpstreamError::TooLarge);
            }
            // The pipeline alone knows whether the chunk became a complete,
            // downstream-visible frame, so it owns the commit marker.
            sink(&buffer[..count])?;
        }
        Ok(total)
    }
}

/// Whether ureq wrapped an underlying socket timeout while parsing a response.
fn is_timeout_transport(error: &ureq::Transport) -> bool {
    error
        .source()
        .and_then(|source| source.downcast_ref::<std::io::Error>())
        .is_some_and(|source| source.kind() == std::io::ErrorKind::TimedOut)
}

/// Reduce an error response to a classified summary, then drop it.
///
/// Reading the body is bounded by [`ERROR_BODY_PREFIX_LIMIT`]; a body that
/// cannot be read at all still yields a usable classification from the status.
fn capture_failure(
    status: u16,
    response: ureq::Response,
    max_retry_delay: Duration,
) -> UpstreamFailure {
    let header = |name: &str| response.header(name).map(str::to_string);
    let request_id = header("x-request-id");
    let cf_ray = header("cf-ray");
    let retry_after = header("retry-after");

    let mut body = Vec::new();
    let mut reader = response.into_reader().take(ERROR_BODY_PREFIX_LIMIT as u64);
    let _ = reader.read_to_end(&mut body);
    let body_prefix = String::from_utf8_lossy(&body);

    UpstreamFailure::from_parts(
        status,
        |name| match name {
            "x-request-id" => request_id.clone(),
            "cf-ray" => cf_ray.clone(),
            "retry-after" => retry_after.clone(),
            _ => None,
        },
        &body_prefix,
        max_retry_delay,
    )
}

/// Exponential backoff with jitter, clamped to the configured ceiling.
///
/// `jitter` is a fraction in `[0, 1)` supplied by the caller so the policy is a
/// pure function and can be tested without sampling randomness. Jitter matters
/// here: a fixed window makes every client retry in lockstep, which is how a
/// recovering upstream gets knocked straight back over.
fn backoff_delay(attempt: u32, config: &UpstreamConfig, jitter: f64) -> Duration {
    let exponent = attempt.saturating_sub(1).min(16);
    let base = config
        .retry_delay
        .saturating_mul(1_u32 << exponent)
        .min(config.max_retry_delay);
    // Jitter spans the lower half of the window, so the delay stays within
    // `[base/2, base]` and can never exceed the ceiling.
    let span = base.mul_f64(0.5);
    let clamped = jitter.clamp(0.0, 1.0);
    base - span.mul_f64(clamped)
}

/// A jitter fraction in `[0, 1)`. Falls back to the midpoint if the OS entropy
/// source is unavailable — a fixed delay is worse than a jittered one, but far
/// better than failing the request over it.
fn jitter_fraction() -> f64 {
    let mut bytes = [0_u8; 4];
    if getrandom::fill(&mut bytes).is_err() {
        return 0.5;
    }
    f64::from(u32::from_le_bytes(bytes)) / f64::from(u32::MAX)
}

/// Sleep in short slices so cancellation is observed during backoff.
/// Returns true when cancelled.
fn wait_or_cancel(total: Duration, cancel: &AtomicBool) -> bool {
    let slice = Duration::from_millis(25);
    let mut waited = Duration::ZERO;
    while waited < total {
        if cancel.load(Ordering::Acquire) {
            return true;
        }
        let step = slice.min(total - waited);
        std::thread::sleep(step);
        waited += step;
    }
    cancel.load(Ordering::Acquire)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use std::net::{Ipv4Addr, TcpListener};
    use std::sync::atomic::AtomicUsize;
    use std::sync::{Arc, Mutex};

    /// A scripted local HTTP server. Each connection consumes one script entry
    /// and records the request it received.
    struct FakeUpstream {
        base_url: String,
        requests: Arc<Mutex<Vec<String>>>,
        hits: Arc<AtomicUsize>,
        handle: Option<std::thread::JoinHandle<()>>,
        shutdown: Arc<AtomicBool>,
    }

    impl FakeUpstream {
        /// `script` yields one response body (raw HTTP) per connection; the
        /// last entry repeats once exhausted.
        fn start(script: Vec<Vec<u8>>) -> Self {
            let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
            listener.set_nonblocking(true).unwrap();
            let addr = listener.local_addr().unwrap();
            let requests = Arc::new(Mutex::new(Vec::new()));
            let hits = Arc::new(AtomicUsize::new(0));
            let shutdown = Arc::new(AtomicBool::new(false));

            let thread_requests = Arc::clone(&requests);
            let thread_hits = Arc::clone(&hits);
            let thread_shutdown = Arc::clone(&shutdown);
            let handle = std::thread::spawn(move || {
                while !thread_shutdown.load(Ordering::Acquire) {
                    let (mut stream, _) = match listener.accept() {
                        Ok(pair) => pair,
                        Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                            std::thread::sleep(Duration::from_millis(5));
                            continue;
                        }
                        Err(_) => break,
                    };
                    stream.set_nonblocking(false).unwrap();
                    stream
                        .set_read_timeout(Some(Duration::from_secs(5)))
                        .unwrap();

                    // Read headers, then exactly Content-Length bytes.
                    let mut raw = Vec::new();
                    let mut byte = [0_u8; 1];
                    while !raw.windows(4).any(|window| window == b"\r\n\r\n") {
                        match stream.read(&mut byte) {
                            Ok(0) | Err(_) => break,
                            Ok(_) => raw.push(byte[0]),
                        }
                    }
                    let head = String::from_utf8_lossy(&raw).to_string();
                    let length = head
                        .lines()
                        .find_map(|line| {
                            let (name, value) = line.split_once(':')?;
                            name.eq_ignore_ascii_case("content-length")
                                .then(|| value.trim().parse::<usize>().ok())?
                        })
                        .unwrap_or(0);
                    let mut body = vec![0_u8; length];
                    if length > 0 {
                        let _ = stream.read_exact(&mut body);
                    }
                    thread_requests
                        .lock()
                        .unwrap()
                        .push(format!("{head}{}", String::from_utf8_lossy(&body)));

                    let index = thread_hits.fetch_add(1, Ordering::AcqRel);
                    let reply = script
                        .get(index)
                        .or_else(|| script.last())
                        .cloned()
                        .unwrap_or_default();
                    let _ = stream.write_all(&reply);
                    let _ = stream.flush();
                    let _ = stream.shutdown(std::net::Shutdown::Both);
                }
            });

            Self {
                base_url: format!("http://{addr}"),
                requests,
                hits,
                handle: Some(handle),
                shutdown,
            }
        }

        fn hits(&self) -> usize {
            self.hits.load(Ordering::Acquire)
        }

        fn requests(&self) -> Vec<String> {
            self.requests.lock().unwrap().clone()
        }
    }

    impl Drop for FakeUpstream {
        fn drop(&mut self) {
            self.shutdown.store(true, Ordering::Release);
            if let Some(handle) = self.handle.take() {
                let _ = handle.join();
            }
        }
    }

    fn sse_response(body: &str) -> Vec<u8> {
        format!(
            "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
            body.len()
        )
        .into_bytes()
    }

    fn status_response(status: u16) -> Vec<u8> {
        let body = r#"{"error":{"message":"key sk-secret-abc for org org_9"}}"#;
        format!(
            "HTTP/1.1 {status} Err\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
            body.len()
        )
        .into_bytes()
    }

    fn client(base_url: &str, config: UpstreamConfig) -> UpstreamClient<ApiKeyCredentials> {
        UpstreamClient::new(
            ApiKeyCredentials::new("sk-test-key", Some(base_url.to_string())).unwrap(),
            config,
        )
    }

    fn fast_config() -> UpstreamConfig {
        UpstreamConfig {
            connect_timeout: Duration::from_secs(2),
            read_timeout: Duration::from_secs(2),
            first_frame_timeout: None,
            overall_timeout: Duration::from_secs(10),
            retry_delay: Duration::from_millis(10),
            ..UpstreamConfig::default()
        }
    }

    #[test]
    fn resolves_the_endpoint_from_several_base_url_shapes() {
        let cases = [
            (
                "https://api.openai.com",
                "https://api.openai.com/v1/responses",
            ),
            (
                "https://api.openai.com/",
                "https://api.openai.com/v1/responses",
            ),
            ("https://gw.test/v1", "https://gw.test/v1/responses"),
            (
                "https://gw.test/v1/responses",
                "https://gw.test/v1/responses",
            ),
            (
                "https://gw.test/v1/responses/",
                "https://gw.test/v1/responses",
            ),
        ];
        for (base, expected) in cases {
            let target = UpstreamTarget::new(base, "Bearer x");
            assert_eq!(target.responses_url(), expected, "base {base}");
        }
    }

    #[test]
    fn api_key_resolution_has_no_implicit_fallback() {
        // Present -> used, with the default base.
        let target = resolve_api_key_target(Some("  sk-abc  ".into()), None).unwrap();
        assert_eq!(
            target.responses_url(),
            "https://api.openai.com/v1/responses"
        );

        // Absent or blank -> a hard error, never a downgrade to another source.
        for missing in [None, Some(String::new()), Some("   ".into())] {
            let error =
                resolve_api_key_target(missing, None).expect_err("missing key must not resolve");
            assert_eq!(
                error,
                UpstreamError::Credentials("OPENAI_API_KEY is not set")
            );
        }

        let custom =
            resolve_api_key_target(Some("k".into()), Some("https://gw.test".into())).unwrap();
        assert_eq!(custom.responses_url(), "https://gw.test/v1/responses");
    }

    #[test]
    fn clud_subscription_is_a_distinct_codex_backend_source() {
        let credentials = CludSubscriptionCredentials::from_record(SubscriptionCredentials {
            access_token: "subscription-secret".to_string(),
            refresh_token: "refresh-secret".to_string(),
            account_id: Some("account-123".to_string()),
            email: None,
            expires_at_unix: None,
        })
        .unwrap();
        let target = credentials.resolve().unwrap();
        assert!(target.uses_codex_backend());
        assert!(target.responses_url().ends_with("/codex/responses"));
        assert!(!format!("{target:?}").contains("subscription-secret"));
        assert!(!format!("{credentials:?}").contains("refresh-secret"));
    }

    #[test]
    fn expired_subscription_never_falls_back_to_an_api_key() {
        let result = ResolvedCredentials::resolve_with(
            Err(UpstreamError::Credentials(CLUD_CREDENTIALS_EXPIRED)),
            || panic!("an expired subscription must not select OPENAI_API_KEY"),
        );
        assert!(matches!(
            result,
            Err(UpstreamError::Credentials(CLUD_CREDENTIALS_EXPIRED))
        ));
    }

    #[test]
    fn diagnostics_never_carry_the_key_or_endpoint() {
        let target = UpstreamTarget::new("https://secret.example/v1", "Bearer sk-secret-abc")
            .with_header("OpenAI-Beta", "responses=v1");
        let rendered = format!("{target:?}");
        assert!(!rendered.contains("sk-secret-abc"));
        assert!(!rendered.contains("secret.example"));

        let credentials = ApiKeyCredentials::new("sk-secret-abc", None).unwrap();
        assert!(!format!("{credentials:?}").contains("sk-secret-abc"));
        let client = UpstreamClient::new(credentials, UpstreamConfig::default());
        assert!(!format!("{client:?}").contains("sk-secret-abc"));

        // Errors carry a classification, not the library's message.
        assert!(
            !format!("{}", UpstreamError::Transport("connection failed"))
                .contains("secret.example")
        );
    }

    #[test]
    fn streams_chunks_to_the_sink_in_order() {
        let payload = "event: a\ndata: 1\n\nevent: b\ndata: 2\n\n";
        let server = FakeUpstream::start(vec![sse_response(payload)]);
        let client = client(&server.base_url, fast_config());
        let mut seen = Vec::new();
        let outcome = client
            .stream(b"{}", &AtomicBool::new(false), &mut |chunk| {
                seen.extend_from_slice(chunk);
                Ok(())
            })
            .unwrap();

        assert_eq!(outcome.attempts, 1);
        assert_eq!(String::from_utf8(seen).unwrap(), payload);
        assert_eq!(outcome.bytes, payload.len());
    }

    #[test]
    fn first_frame_timeout_is_pre_commit_and_retryable() {
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
        let base_url = format!("http://{}", listener.local_addr().unwrap());
        let server = std::thread::spawn(move || {
            for _ in 0..2 {
                let (mut stream, _) = listener.accept().unwrap();
                stream
                    .set_read_timeout(Some(Duration::from_secs(1)))
                    .unwrap();
                let mut request = [0_u8; 1024];
                let _ = stream.read(&mut request);
                std::thread::sleep(Duration::from_millis(80));
                let _ = stream.write_all(&sse_response("data: too late\n\n"));
            }
        });
        let client = client(
            &base_url,
            UpstreamConfig {
                first_frame_timeout: Some(Duration::from_millis(20)),
                read_timeout: Duration::from_secs(1),
                max_attempts: 2,
                retry_delay: Duration::ZERO,
                ..fast_config()
            },
        );
        let mut seen = Vec::new();
        let error = client
            .stream(b"{}", &AtomicBool::new(false), &mut |chunk| {
                seen.extend_from_slice(chunk);
                Ok(())
            })
            .unwrap_err();

        assert_eq!(error, UpstreamError::Timeout);
        assert!(
            seen.is_empty(),
            "a first-frame timeout must not commit output"
        );
        server.join().unwrap();
    }

    #[test]
    fn a_healthy_first_frame_arriving_within_its_budget_succeeds() {
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
        let base_url = format!("http://{}", listener.local_addr().unwrap());
        let server = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            stream
                .set_read_timeout(Some(Duration::from_secs(1)))
                .unwrap();
            let mut request = [0_u8; 1024];
            let _ = stream.read(&mut request);
            std::thread::sleep(Duration::from_millis(80));
            stream
                .write_all(&sse_response("data: healthy\n\n"))
                .unwrap();
        });
        let client = client(
            &base_url,
            UpstreamConfig {
                first_frame_timeout: Some(Duration::from_millis(200)),
                read_timeout: Duration::from_secs(1),
                ..fast_config()
            },
        );
        let mut seen = Vec::new();
        let outcome = client
            .stream(b"{}", &AtomicBool::new(false), &mut |chunk| {
                seen.extend_from_slice(chunk);
                Ok(())
            })
            .unwrap();

        assert_eq!(outcome.attempts, 1);
        assert_eq!(seen, b"data: healthy\n\n");
        server.join().unwrap();
    }

    #[test]
    fn request_carries_only_headers_we_construct() {
        let server = FakeUpstream::start(vec![sse_response("data: ok\n\n")]);
        let credentials =
            ApiKeyCredentials::new("sk-upstream-key", Some(server.base_url.clone())).unwrap();
        let client = UpstreamClient::new(credentials, fast_config());
        client
            .stream(
                br#"{"model":"m"}"#,
                &AtomicBool::new(false),
                &mut |_| Ok(()),
            )
            .unwrap();

        let request = server.requests().remove(0);
        assert!(request.starts_with("POST /v1/responses HTTP/1.1"));
        assert!(request.contains("Authorization: Bearer sk-upstream-key"));
        assert!(request.contains("Accept: text/event-stream"));
        assert!(request.contains("OpenAI-Beta: responses=experimental"));
        // openai/codex 0.146.0 sends `codex_cli_rs/<Codex version>` and a
        // separate `version` header. Keep clud's own version out of both.
        assert!(request.contains("User-Agent: codex_cli_rs/0.146.0 (clud)"));
        assert!(request.contains("version: 0.146.0"));
        assert!(request.contains(r#"{"model":"m"}"#));
        // The harness's own downstream bearer must never appear upstream.
        assert!(!request.contains("x-api-key"));
        assert!(!request.to_lowercase().contains("anthropic"));
    }

    #[test]
    fn retryable_status_before_any_output_is_retried() {
        let server = FakeUpstream::start(vec![
            status_response(503),
            status_response(429),
            sse_response("data: recovered\n\n"),
        ]);
        let client = client(&server.base_url, fast_config());
        let mut seen = Vec::new();
        let outcome = client
            .stream(b"{}", &AtomicBool::new(false), &mut |chunk| {
                seen.extend_from_slice(chunk);
                Ok(())
            })
            .unwrap();

        assert_eq!(outcome.attempts, 3);
        assert_eq!(server.hits(), 3);
        assert_eq!(String::from_utf8(seen).unwrap(), "data: recovered\n\n");
    }

    #[test]
    fn non_retryable_status_fails_immediately_and_hides_the_body() {
        let server = FakeUpstream::start(vec![status_response(401)]);
        let client = client(&server.base_url, fast_config());
        let error = client
            .stream(b"{}", &AtomicBool::new(false), &mut |_| Ok(()))
            .unwrap_err();

        let failure = error.failure().expect("a status failure");
        assert_eq!(failure.status(), 401);
        assert_eq!(failure.class(), FailureClass::Permanent);
        assert_eq!(server.hits(), 1, "a 401 must not be retried");
        // Reading the body to classify it must not turn the error into a
        // credential map: the scrubber runs before anything is retained.
        let rendered = format!("{error} {error:?} {}", failure.diagnostic());
        assert!(!rendered.contains("sk-secret-abc"), "{rendered}");
        assert!(!rendered.contains("org_9"), "{rendered}");
    }

    #[test]
    fn attempts_are_capped() {
        let server = FakeUpstream::start(vec![status_response(500)]);
        let client = client(
            &server.base_url,
            UpstreamConfig {
                max_attempts: 2,
                ..fast_config()
            },
        );
        let error = client
            .stream(b"{}", &AtomicBool::new(false), &mut |_| Ok(()))
            .unwrap_err();
        assert_eq!(error.failure().map(UpstreamFailure::status), Some(500));
        assert_eq!(server.hits(), 2);
    }

    /// The rule that step 1 makes absolute: once a byte has reached the sink,
    /// the response is committed and no failure may cause a replay.
    #[test]
    fn a_failure_after_output_has_begun_is_never_replayed() {
        // Claim a long body, deliver part of it, then hang up mid-stream.
        let truncated = b"HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: 4096\r\nConnection: close\r\n\r\ndata: partial\n\n".to_vec();
        let server = FakeUpstream::start(vec![truncated, sse_response("data: retry\n\n")]);
        let client = client(&server.base_url, fast_config());

        let mut seen = Vec::new();
        let result = client.stream(b"{}", &AtomicBool::new(false), &mut |chunk| {
            seen.extend_from_slice(chunk);
            Ok(())
        });

        assert!(result.is_err(), "a truncated body must surface as an error");
        assert_eq!(
            server.hits(),
            1,
            "the request was replayed after downstream output had begun"
        );
        assert_eq!(String::from_utf8(seen).unwrap(), "data: partial\n\n");
    }

    #[test]
    fn a_failing_sink_stops_the_stream_and_is_not_retried() {
        let server = FakeUpstream::start(vec![sse_response("data: one\n\n")]);
        let client = client(&server.base_url, fast_config());
        let error = client
            .stream(b"{}", &AtomicBool::new(false), &mut |_| {
                Err(UpstreamError::Downstream("client hung up"))
            })
            .unwrap_err();

        assert_eq!(error, UpstreamError::Downstream("client hung up"));
        assert_eq!(server.hits(), 1);
    }

    #[test]
    fn cancellation_is_observed_before_the_request() {
        let server = FakeUpstream::start(vec![sse_response("data: x\n\n")]);
        let client = client(&server.base_url, fast_config());
        let cancel = AtomicBool::new(true);
        let error = client.stream(b"{}", &cancel, &mut |_| Ok(())).unwrap_err();
        assert_eq!(error, UpstreamError::Cancelled);
        assert_eq!(server.hits(), 0);
    }

    #[test]
    fn cancellation_during_backoff_stops_the_retry_loop() {
        let server = FakeUpstream::start(vec![status_response(500)]);
        let client = client(
            &server.base_url,
            UpstreamConfig {
                retry_delay: Duration::from_millis(400),
                max_attempts: 5,
                ..fast_config()
            },
        );
        let cancel = Arc::new(AtomicBool::new(false));
        let flag = Arc::clone(&cancel);
        std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(120));
            flag.store(true, Ordering::Release);
        });
        let error = client.stream(b"{}", &cancel, &mut |_| Ok(())).unwrap_err();
        assert_eq!(error, UpstreamError::Cancelled);
        assert!(server.hits() < 5, "backoff did not observe cancellation");
    }

    #[test]
    fn oversized_responses_are_refused() {
        let server = FakeUpstream::start(vec![sse_response(&"x".repeat(4096))]);
        let client = client(
            &server.base_url,
            UpstreamConfig {
                max_response_bytes: 1024,
                ..fast_config()
            },
        );
        let error = client
            .stream(b"{}", &AtomicBool::new(false), &mut |_| Ok(()))
            .unwrap_err();
        assert_eq!(error, UpstreamError::TooLarge);
    }

    /// Build a failure without a socket, so classification is testable alone.
    fn failure_from(status: u16, body: &str) -> UpstreamFailure {
        UpstreamFailure::from_parts(status, |_| None, body, DEFAULT_MAX_RETRY_DELAY)
    }

    fn body_response(status: u16, body: &str, headers: &[(&str, &str)]) -> Vec<u8> {
        let extra: String = headers
            .iter()
            .map(|(name, value)| format!("{name}: {value}\r\n"))
            .collect();
        format!(
            "HTTP/1.1 {status} Err\r\nContent-Type: application/json\r\n{extra}Content-Length: {}\r\nConnection: close\r\n\r\n{body}",
            body.len()
        )
        .into_bytes()
    }

    #[test]
    fn every_known_failure_signature_classifies_as_intended() {
        // Keep the signature tables load-bearing: changing or moving a signature
        // must not silently alter its retry class.
        for (class, signatures) in [
            (FailureClass::Permanent, PERMANENT_SIGNATURES),
            (FailureClass::Exhausted, EXHAUSTED_SIGNATURES),
            (FailureClass::Transient, TRANSIENT_SIGNATURES),
        ] {
            assert!(
                !signatures.is_empty(),
                "{class:?} signatures must not be empty"
            );
            for signature in signatures {
                let body = format!(r#"{{"error":{{"message":"{signature}"}}}}"#);
                assert_eq!(
                    failure_from(502, &body).class(),
                    class,
                    "signature {signature:?} no longer classifies as {class:?}",
                );
            }
        }
    }

    #[test]
    fn retry_classification_matches_the_documented_policy() {
        // Recognised outages stay retryable.
        for status in [408, 429, 500, 502, 503] {
            let failure = failure_from(status, r#"{"error":{"message":"try again"}}"#);
            assert_eq!(failure.class(), FailureClass::Transient, "{status}");
            assert!(UpstreamError::Status(failure).is_retryable(), "{status}");
        }
        // Statuses the bridge passes through are never retried.
        for status in [400, 401, 403, 404, 422] {
            let failure = failure_from(status, "");
            assert_eq!(failure.class(), FailureClass::Permanent, "{status}");
            assert!(!UpstreamError::Status(failure).is_retryable(), "{status}");
        }
        assert!(UpstreamError::Transport("connection failed").is_retryable());
        for error in [
            UpstreamError::Credentials("x"),
            UpstreamError::TooLarge,
            UpstreamError::Cancelled,
            UpstreamError::Downstream("x"),
        ] {
            assert!(!error.is_retryable(), "{error:?}");
        }
    }

    /// The reversal this change exists for: a 502 can carry a *permanent*
    /// rejection, and retrying it can never succeed.
    ///
    /// Regression guard for the sub2api#4020 shape, where `gpt-5.6-sol` is
    /// refused with a version-gate message wrapped in a 502.
    #[test]
    fn a_permanent_rejection_wrapped_in_a_502_is_attempted_exactly_once() {
        let body = r#"{"error":{"message":"The 'gpt-5.6-sol' model requires a newer version of Codex. Please upgrade."}}"#;
        let server = FakeUpstream::start(vec![body_response(502, body, &[])]);
        let client = client(&server.base_url, fast_config());

        let error = client
            .stream(b"{}", &AtomicBool::new(false), &mut |_| Ok(()))
            .unwrap_err();

        let failure = error.failure().expect("a status failure");
        assert_eq!(failure.status(), 502);
        assert_eq!(failure.class(), FailureClass::Permanent);
        assert_eq!(
            server.hits(),
            1,
            "a permanent rejection must not be retried, however 5xx it looks"
        );
    }

    /// A 5xx we cannot read gets a reduced budget rather than the full ladder:
    /// treating every unrecognised 5xx as fully retryable is what produced the
    /// credential-burning cascade this policy is designed to avoid.
    #[test]
    fn an_unrecognised_5xx_retries_on_the_reduced_budget() {
        let body = r#"{"error":{"message":"something we have never seen"}}"#;
        assert_eq!(failure_from(500, body).class(), FailureClass::Unknown);

        let server = FakeUpstream::start(vec![body_response(500, body, &[])]);
        let client = client(
            &server.base_url,
            UpstreamConfig {
                max_attempts: 6,
                unknown_max_attempts: 2,
                ..fast_config()
            },
        );
        let error = client
            .stream(b"{}", &AtomicBool::new(false), &mut |_| Ok(()))
            .unwrap_err();

        assert_eq!(
            error.failure().map(UpstreamFailure::class),
            Some(FailureClass::Unknown)
        );
        assert_eq!(
            server.hits(),
            2,
            "an unrecognised 5xx must use the reduced budget, not max_attempts"
        );
    }

    #[test]
    fn correlation_identifiers_are_captured_from_the_error_response() {
        let body = r#"{"error":{"code":"server_error","message":"bad gateway"}}"#;
        let server = FakeUpstream::start(vec![body_response(
            502,
            body,
            &[
                ("cf-ray", "9a1b2c3d4e5f-HKG"),
                ("x-request-id", "req_abc123"),
            ],
        )]);
        let client = client(
            &server.base_url,
            UpstreamConfig {
                max_attempts: 1,
                ..fast_config()
            },
        );
        let error = client
            .stream(b"{}", &AtomicBool::new(false), &mut |_| Ok(()))
            .unwrap_err();

        let failure = error.failure().expect("a status failure");
        assert_eq!(failure.cf_ray(), Some("9a1b2c3d4e5f-HKG"));
        assert_eq!(failure.request_id(), Some("req_abc123"));
        // Both must reach the operator log, which is the whole point: a cf-ray
        // is what identifies a failure as edge-side rather than provider-side.
        let diagnostic = failure.diagnostic();
        assert!(diagnostic.contains("9a1b2c3d4e5f-HKG"), "{diagnostic}");
        assert!(diagnostic.contains("req_abc123"), "{diagnostic}");
        assert!(diagnostic.contains("class=Transient"), "{diagnostic}");
    }

    #[test]
    fn a_retry_after_header_is_honoured_and_clamped() {
        // Far larger than the ceiling: a generous hint must not pin the turn.
        let server = FakeUpstream::start(vec![
            body_response(503, "unavailable", &[("retry-after", "3600")]),
            sse_response("data: ok\n\n"),
        ]);
        let config = UpstreamConfig {
            max_retry_delay: Duration::from_millis(80),
            ..fast_config()
        };
        let client = client(&server.base_url, config);

        let started = Instant::now();
        let outcome = client
            .stream(b"{}", &AtomicBool::new(false), &mut |_| Ok(()))
            .unwrap();
        let elapsed = started.elapsed();

        assert_eq!(outcome.attempts, 2);
        assert!(
            elapsed < Duration::from_secs(5),
            "a 3600s Retry-After was not clamped: waited {elapsed:?}"
        );
    }

    /// A 429 whose body says the plan is exhausted is **not** a throttle.
    ///
    /// This assertion used to read `Transient`, which meant a multi-day
    /// exhaustion burned three attempts in ~750ms and was reported as a bare
    /// "status 429". Status alone cannot tell the two apart -- only the body
    /// can, which is why it is classified before the 408/429 rule runs.
    #[test]
    fn an_exhausted_plan_is_not_retried_however_it_is_spelled() {
        let body = r#"{"error":{"code":"usage_limit_reached","resets_in_seconds":529498}}"#;
        let failure = failure_from(429, body);
        assert_eq!(failure.class(), FailureClass::Exhausted);
        assert_eq!(failure.resets_in(), Some(Duration::from_secs(529_498)));
        assert_eq!(
            failure.max_attempts(&UpstreamConfig::default()),
            1,
            "an exhausted plan must be attempted exactly once"
        );

        for spelling in [
            r#"{"error":{"code":"insufficient_quota"}}"#,
            r#"{"error":{"type":"quota_exceeded"}}"#,
            r#"{"error":{"message":"You are out of credits"}}"#,
        ] {
            assert_eq!(
                failure_from(429, spelling).class(),
                FailureClass::Exhausted,
                "{spelling}"
            );
        }
    }

    /// A 429 with no exhaustion signature stays retryable.
    #[test]
    fn an_ordinary_throttle_is_still_transient() {
        let failure = failure_from(429, r#"{"error":{"code":"rate_limit_exceeded"}}"#);
        assert_eq!(failure.class(), FailureClass::Transient);
        assert!(failure.max_attempts(&UpstreamConfig::default()) > 1);
    }

    #[test]
    fn backoff_grows_exponentially_is_capped_and_is_jittered() {
        let config = UpstreamConfig {
            retry_delay: Duration::from_millis(100),
            max_retry_delay: Duration::from_millis(1000),
            ..UpstreamConfig::default()
        };
        // Without jitter the ladder doubles until it hits the ceiling.
        let ladder: Vec<Duration> = (1..=6)
            .map(|attempt| backoff_delay(attempt, &config, 0.0))
            .collect();
        assert_eq!(ladder[0], Duration::from_millis(100));
        assert_eq!(ladder[1], Duration::from_millis(200));
        assert_eq!(ladder[2], Duration::from_millis(400));
        assert_eq!(ladder[3], Duration::from_millis(800));
        for delay in &ladder {
            assert!(
                *delay <= config.max_retry_delay,
                "{delay:?} exceeded the cap"
            );
        }
        assert_eq!(
            ladder[4], config.max_retry_delay,
            "the ladder must saturate"
        );
        assert_eq!(ladder[5], config.max_retry_delay);

        // Jitter spreads each step over the lower half of its window, so two
        // clients that fail together do not retry in lockstep.
        let spread: Vec<Duration> = [0.0, 0.25, 0.5, 0.75, 1.0]
            .into_iter()
            .map(|fraction| backoff_delay(3, &config, fraction))
            .collect();
        assert!(
            spread.windows(2).all(|pair| pair[0] > pair[1]),
            "jitter produced no variation: {spread:?}"
        );
        assert_eq!(spread[0], Duration::from_millis(400));
        assert_eq!(spread[4], Duration::from_millis(200));
        for delay in &spread {
            assert!(*delay >= Duration::from_millis(200) && *delay <= Duration::from_millis(400));
        }
        // An out-of-range fraction must not escape the window.
        assert_eq!(backoff_delay(3, &config, 5.0), Duration::from_millis(200));
        assert_eq!(backoff_delay(3, &config, -5.0), Duration::from_millis(400));
    }

    #[test]
    fn the_total_retry_sleep_is_bounded() {
        let server = FakeUpstream::start(vec![body_response(503, "service unavailable", &[])]);
        let client = client(
            &server.base_url,
            UpstreamConfig {
                max_attempts: 20,
                retry_delay: Duration::from_millis(40),
                max_retry_delay: Duration::from_millis(80),
                max_retry_elapsed: Duration::from_millis(200),
                ..fast_config()
            },
        );
        let started = Instant::now();
        let error = client
            .stream(b"{}", &AtomicBool::new(false), &mut |_| Ok(()))
            .unwrap_err();
        let elapsed = started.elapsed();

        assert_eq!(error.failure().map(UpstreamFailure::status), Some(503));
        assert!(
            elapsed < Duration::from_secs(5),
            "the elapsed-sleep budget did not bound the loop: {elapsed:?}"
        );
        assert!(
            server.hits() < 20,
            "the budget must stop the loop before max_attempts"
        );
    }

    #[test]
    fn the_scrubber_redacts_token_shaped_runs() {
        let scrubbed = scrub("key sk-secret-abc for org org_9 and AKIAIOSFODNN7EXAMPLEXYZ ok");
        assert!(!scrubbed.contains("sk-secret-abc"), "{scrubbed}");
        assert!(!scrubbed.contains("org_9"), "{scrubbed}");
        assert!(!scrubbed.contains("AKIAIOSFODNN7EXAMPLEXYZ"), "{scrubbed}");
        // Ordinary words survive, or the detail would be useless.
        assert!(scrubbed.contains("key"), "{scrubbed}");
        assert!(scrubbed.contains("for"), "{scrubbed}");
        assert!(scrubbed.contains("ok"), "{scrubbed}");
    }

    #[test]
    fn an_expired_codex_login_is_refused_with_an_actionable_message() {
        let expired = auth_json_with_expiry(1_000_000_000);
        let error = CodexCliCredentials::from_auth_json(expired.as_bytes()).unwrap_err();
        assert_eq!(
            error,
            UpstreamError::Credentials("the Codex login has expired -- run `codex login`")
        );

        // A live token still resolves.
        let live_expiry = SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .unwrap()
            .as_secs()
            + 86_400;
        let live = auth_json_with_expiry(live_expiry);
        assert!(CodexCliCredentials::from_auth_json(live.as_bytes()).is_ok());

        // An opaque (non-JWT) bearer is legitimate and must keep working.
        let opaque = r#"{"tokens":{"access_token":"opaque-token","account_id":"acct"}}"#;
        assert!(CodexCliCredentials::from_auth_json(opaque.as_bytes()).is_ok());
    }

    /// Build an `auth.json` whose access token is an unsigned JWT with `exp`.
    fn auth_json_with_expiry(exp: u64) -> String {
        let claims = base64url_encode(format!(r#"{{"exp":{exp}}}"#).as_bytes());
        let header = base64url_encode(br#"{"alg":"none"}"#);
        format!(r#"{{"tokens":{{"access_token":"{header}.{claims}.sig","account_id":"acct"}}}}"#)
    }

    fn base64url_encode(input: &[u8]) -> String {
        const ALPHABET: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";
        let mut out = String::new();
        for chunk in input.chunks(3) {
            let mut buffer = 0_u32;
            for (index, byte) in chunk.iter().enumerate() {
                buffer |= u32::from(*byte) << (16 - 8 * index);
            }
            let characters = chunk.len() + 1;
            for index in 0..characters {
                let value = (buffer >> (18 - 6 * index)) & 0x3F;
                out.push(ALPHABET[value as usize] as char);
            }
        }
        out
    }

    #[test]
    fn base64url_round_trips() {
        let samples: [&[u8]; 6] = [b"", b"a", b"ab", b"abc", b"abcd", br#"{"exp":123}"#];
        for sample in samples {
            let encoded = base64url_encode(sample);
            assert_eq!(
                base64url_decode(&encoded).as_deref(),
                Some(sample),
                "sample {sample:?} encoded as {encoded}"
            );
        }
    }

    /// A connection refused before any byte is a transport failure, and is
    /// retried up to the cap rather than surfacing on the first try.
    #[test]
    fn unreachable_upstream_is_a_classified_transport_error() {
        // Bind and drop to obtain a port nothing is listening on.
        let port = {
            let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
            listener.local_addr().unwrap().port()
        };
        let client = client(
            &format!("http://127.0.0.1:{port}"),
            UpstreamConfig {
                max_attempts: 2,
                retry_delay: Duration::from_millis(5),
                connect_timeout: Duration::from_millis(500),
                ..fast_config()
            },
        );
        let error = client
            .stream(b"{}", &AtomicBool::new(false), &mut |_| Ok(()))
            .unwrap_err();
        assert_eq!(error, UpstreamError::Transport("connection failed"));
    }

    #[test]
    fn model_override_travels_with_the_target() {
        let target = UpstreamTarget::new("https://gw.test", "Bearer k")
            .with_model_override(Some(ModelSpec::parse("terra@high").unwrap()));
        assert_eq!(
            target.model_override().map(ModelSpec::display),
            Some("gpt-5.6-terra@high".to_string())
        );
        assert_eq!(
            UpstreamTarget::new("https://gw.test", "Bearer k").model_override(),
            None
        );
    }
}

/// Manual, network-touching validation against a real account.
///
/// Ignored by default so CI never runs it. #631 needs a way to re-validate
/// against a live subscription without inventing a second harness:
/// `cargo test -p clud --lib live_probe -- --ignored --nocapture`.
#[cfg(test)]
mod live_probe {
    use super::*;
    use std::sync::atomic::AtomicBool;

    #[test]
    #[ignore = "requires real credentials and network access"]
    fn a_real_upstream_request_streams_back() {
        let credentials = ResolvedCredentials::resolve_default().expect("credentials");
        eprintln!("credential source = {}", credentials.describe());
        let target = credentials.resolve().expect("target");
        assert!(target.responses_url().ends_with("/responses"));

        let client = UpstreamClient::new(credentials, UpstreamConfig::default());
        let body = br#"{"model":"gpt-5.6-terra","input":[{"type":"message","role":"user","content":[{"type":"input_text","text":"Say BRIDGED"}]}],"stream":true,"store":false,"include":["reasoning.encrypted_content"],"reasoning":{"effort":"medium"}}"#;
        let mut received = String::new();
        let outcome = client
            .stream(body, &AtomicBool::new(false), &mut |chunk| {
                received.push_str(&String::from_utf8_lossy(chunk));
                Ok(())
            })
            .expect("a real upstream request must succeed");

        assert_eq!(outcome.attempts, 1);
        assert!(received.contains("response.created"), "{received:.200}");
        assert!(received.contains("response.completed"));
    }
}
