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

use std::io::Read;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

pub const DEFAULT_BASE_URL: &str = "https://api.openai.com";
const DEFAULT_CONNECT_TIMEOUT: Duration = Duration::from_secs(30);
/// Idle timeout between reads, not a cap on the whole turn: a model may think
/// for minutes before its first token, which is healthy, whereas a socket that
/// yields nothing for this long is not.
const DEFAULT_READ_TIMEOUT: Duration = Duration::from_secs(120);
const DEFAULT_OVERALL_TIMEOUT: Duration = Duration::from_secs(3600);
const DEFAULT_MAX_RESPONSE_BYTES: usize = 64 * 1024 * 1024;
const DEFAULT_MAX_ATTEMPTS: u32 = 3;
const DEFAULT_RETRY_DELAY: Duration = Duration::from_millis(250);

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UpstreamError {
    /// No usable credentials. Deliberately never names the variable's value.
    Credentials(&'static str),
    /// Upstream returned a non-2xx status. The body is not carried: it can
    /// contain account identifiers and key fragments.
    Status(u16),
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
            Self::Status(status) => write!(formatter, "upstream returned status {status}"),
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
    /// requires that nothing has reached the sink yet.
    pub fn is_retryable(&self) -> bool {
        match self {
            Self::Transport(_) => true,
            Self::Status(status) => *status == 408 || *status == 429 || *status >= 500,
            Self::Credentials(_)
            | Self::TooLarge
            | Self::Timeout
            | Self::Cancelled
            | Self::Downstream(_) => false,
        }
    }
}

/// Everything needed to address the upstream, resolved per request.
#[derive(Clone)]
pub struct UpstreamTarget {
    base_url: String,
    authorization: String,
    extra_headers: Vec<(String, String)>,
    model_override: Option<String>,
}

impl UpstreamTarget {
    pub fn new(base_url: impl Into<String>, authorization: impl Into<String>) -> Self {
        Self {
            base_url: base_url.into(),
            authorization: authorization.into(),
            extra_headers: Vec::new(),
            model_override: None,
        }
    }

    pub fn with_header(mut self, name: impl Into<String>, value: impl Into<String>) -> Self {
        self.extra_headers.push((name.into(), value.into()));
        self
    }

    pub fn with_model_override(mut self, model: Option<String>) -> Self {
        self.model_override = model;
        self
    }

    pub fn model_override(&self) -> Option<&str> {
        self.model_override.as_deref()
    }

    /// Absolute URL of the Responses endpoint.
    ///
    /// A base that already names the endpoint is left alone, so an operator
    /// can point at a gateway that exposes it at a fixed path.
    pub fn responses_url(&self) -> String {
        let trimmed = self.base_url.trim_end_matches('/');
        if trimmed.ends_with("/v1/responses") {
            trimmed.to_string()
        } else if trimmed.ends_with("/v1") {
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

#[derive(Debug, Clone)]
pub struct UpstreamConfig {
    pub connect_timeout: Duration,
    pub read_timeout: Duration,
    pub overall_timeout: Duration,
    pub max_response_bytes: usize,
    pub max_attempts: u32,
    pub retry_delay: Duration,
}

impl Default for UpstreamConfig {
    fn default() -> Self {
        Self {
            connect_timeout: DEFAULT_CONNECT_TIMEOUT,
            read_timeout: DEFAULT_READ_TIMEOUT,
            overall_timeout: DEFAULT_OVERALL_TIMEOUT,
            max_response_bytes: DEFAULT_MAX_RESPONSE_BYTES,
            max_attempts: DEFAULT_MAX_ATTEMPTS,
            retry_delay: DEFAULT_RETRY_DELAY,
        }
    }
}

/// Outcome of one completed stream.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StreamOutcome {
    pub attempts: u32,
    pub bytes: usize,
}

pub struct UpstreamClient<C: CredentialSource> {
    credentials: C,
    config: UpstreamConfig,
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
        }
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
        let target = self.credentials.resolve()?;
        let deadline = Instant::now() + self.config.overall_timeout;
        let mut delivered = false;
        let mut attempt = 0_u32;

        loop {
            attempt += 1;
            if cancel.load(Ordering::Acquire) {
                return Err(UpstreamError::Cancelled);
            }
            match self.attempt(&target, body, cancel, deadline, sink, &mut delivered) {
                Ok(bytes) => {
                    return Ok(StreamOutcome {
                        attempts: attempt,
                        bytes,
                    })
                }
                Err(error) => {
                    // The absolute rule: once anything has reached the sink the
                    // response is committed, however retryable the failure is.
                    if delivered
                        || !error.is_retryable()
                        || attempt >= self.config.max_attempts.max(1)
                        || Instant::now() >= deadline
                    {
                        return Err(error);
                    }
                    let backoff = self.config.retry_delay * attempt;
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
        delivered: &mut bool,
    ) -> Result<usize, UpstreamError> {
        let agent = ureq::AgentBuilder::new()
            .timeout_connect(self.config.connect_timeout)
            .timeout_read(self.config.read_timeout)
            .build();

        // Every header is constructed here. Nothing from the downstream request
        // is copied, so the harness's Anthropic bearer cannot travel upstream.
        let mut request = agent
            .post(&target.responses_url())
            .set("Content-Type", "application/json")
            .set("Accept", "text/event-stream")
            .set("Authorization", &target.authorization);
        for (name, value) in &target.extra_headers {
            request = request.set(name, value);
        }

        let response = match request.send_bytes(body) {
            Ok(response) => response,
            Err(ureq::Error::Status(status, _)) => return Err(UpstreamError::Status(status)),
            Err(ureq::Error::Transport(_)) => {
                return Err(UpstreamError::Transport("connection failed"));
            }
        };

        let mut reader = response.into_reader();
        let mut buffer = [0_u8; 8192];
        let mut total = 0_usize;
        loop {
            if cancel.load(Ordering::Acquire) {
                return Err(UpstreamError::Cancelled);
            }
            if Instant::now() >= deadline {
                return Err(UpstreamError::Timeout);
            }
            let count = match reader.read(&mut buffer) {
                Ok(0) => break,
                Ok(count) => count,
                Err(error) if error.kind() == std::io::ErrorKind::TimedOut => {
                    return Err(UpstreamError::Transport("read timed out"));
                }
                Err(_) => return Err(UpstreamError::Transport("read failed")),
            };
            total += count;
            if total > self.config.max_response_bytes {
                return Err(UpstreamError::TooLarge);
            }
            // Set *before* calling the sink: a sink that fails midway has still
            // potentially written bytes downstream, so the request is committed
            // either way and must never be replayed.
            *delivered = true;
            sink(&buffer[..count])?;
        }
        Ok(total)
    }
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

        assert_eq!(error, UpstreamError::Status(401));
        assert_eq!(server.hits(), 1, "a 401 must not be retried");
        let rendered = format!("{error} {error:?}");
        assert!(!rendered.contains("sk-secret-abc"));
        assert!(!rendered.contains("org_9"));
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
        assert_eq!(error, UpstreamError::Status(500));
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

    #[test]
    fn retry_classification_matches_the_documented_policy() {
        for status in [408, 429, 500, 502, 503] {
            assert!(UpstreamError::Status(status).is_retryable(), "{status}");
        }
        for status in [400, 401, 403, 404, 422] {
            assert!(!UpstreamError::Status(status).is_retryable(), "{status}");
        }
        assert!(UpstreamError::Transport("connection failed").is_retryable());
        for error in [
            UpstreamError::Credentials("x"),
            UpstreamError::TooLarge,
            UpstreamError::Timeout,
            UpstreamError::Cancelled,
            UpstreamError::Downstream("x"),
        ] {
            assert!(!error.is_retryable(), "{error:?}");
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
            .with_model_override(Some("gpt-5.6-codex".into()));
        assert_eq!(target.model_override(), Some("gpt-5.6-codex"));
        assert_eq!(
            UpstreamTarget::new("https://gw.test", "Bearer k").model_override(),
            None
        );
    }
}
