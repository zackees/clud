//! Authenticated loopback bridge used by Codex-provider launches through the
//! Claude harness (issue #626).

use crate::bridge_log::{unix_ms, BridgeLog};
use crate::codex_history::{ConversationKey, ConversationStore, HistoryLimits};
use crate::codex_model::ModelSpec;
use crate::codex_pipeline::{Pipeline, PipelineError, ProviderFailure};
use crate::codex_sse::InBandFailure;
use crate::codex_upstream::{
    ApiKeyCredentials, FailureClass, ResolvedCredentials, UpstreamClient, UpstreamConfig,
    UpstreamError, UpstreamFailure,
};
use base64::Engine as _;
use std::collections::HashMap;
use std::fmt;
use std::io::{self, Read, Write};
use std::net::{IpAddr, Ipv4Addr, Shutdown, TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

/// The bridge must not be stricter than the endpoint it impersonates. Claude
/// Code sizes its requests against the real Anthropic API, so a cap below what
/// that API accepts turns a legitimate request into a bridge-only `413` that
/// looks like a client bug. A single base64 screenshot already exceeds the
/// fixture-era 1 MiB; see `a_representative_request_fits_the_body_cap`.
/// What the operator log and the stderr banner say when the exhaustion
/// arrived in-band. The client already received the translator's own
/// synthesized `error` frame; this is the copy for the human watching the
/// terminal.
const IN_BAND_QUOTA_MESSAGE: &str =
    "upstream account quota exhausted mid-stream -- check your plan usage, or switch providers with --claude";

const DEFAULT_MAX_BODY_BYTES: usize = 32 * 1024 * 1024;
const DEFAULT_MAX_HEADER_BYTES: usize = 32 * 1024;
/// Header reads stay on a short absolute deadline: it is the slowloris
/// defence, and a well-behaved client sends its headers in one segment.
const DEFAULT_HEADER_TIMEOUT: Duration = Duration::from_secs(5);
/// Body reads get their own budget. A large transcript or image upload is
/// legitimately slower than a header, but is still bounded.
const DEFAULT_BODY_TIMEOUT: Duration = Duration::from_secs(30);
/// Streaming responses are governed by an *idle* timeout between frames, not
/// by a total deadline: a model that thinks for ten minutes before its first
/// token is healthy, whereas a socket that accepts no bytes for five minutes
/// is not. Phase 3 replaces the fixture frames but keeps this primitive.
const DEFAULT_STREAM_IDLE_TIMEOUT: Duration = Duration::from_secs(300);
/// Maximum wait for the first upstream byte. It is deliberately generous:
/// lengthy model reasoning is healthy, but a timeout here still leaves the
/// downstream status uncommitted.
const DEFAULT_FIRST_FRAME_TIMEOUT: Duration = Duration::from_secs(300);
/// Claude Code issues several requests at once: the foreground turn plus
/// background side-model calls and any subagents. The bound still exists, but
/// exceeding it now queues in the listen backlog rather than failing.
///
/// Set to 1: a single worker keeps the bridge's host footprint flat no matter
/// how many bridges a process stands up. Forensics captured 15 bridges
/// constructed inside one millisecond in a single pid, each advertising a
/// 16-worker ceiling; the ceilings are lazy, but the advertised total is the
/// number an operator has to reason about when the host is already saturated.
/// Excess connections wait in the listen backlog for up to
/// `DEFAULT_ADMISSION_WAIT` rather than being rejected.
const DEFAULT_MAX_CONCURRENCY: usize = 1;
/// How long a connection may wait in the kernel's listen backlog for a worker
/// slot before the bridge accepts it only to answer 503. Queueing beats
/// rejecting -- a 503 surfaces to the user as a hard API error, whereas a short
/// wait is invisible -- but an unbounded wait would hang the client instead.
const DEFAULT_ADMISSION_WAIT: Duration = Duration::from_secs(10);
const ACCEPT_POLL: Duration = Duration::from_millis(5);
/// Upper bound on a single blocking read. Reads are resumed until their phase
/// deadline expires; the cap exists so a worker parked on a quiet socket still
/// observes shutdown promptly instead of holding teardown for its full budget.
const READ_POLL: Duration = Duration::from_millis(100);
/// Pseudo-status meaning "stop without replying": the connection is being torn
/// down, so there is nobody left to receive an error document.
const ABANDON: u16 = 0;

type ActiveConnections = Arc<Mutex<HashMap<usize, TcpStream>>>;
type SharedBridgeLog = Arc<Mutex<BridgeLog>>;

/// Resource and test-seam policy for one bridge launch.
#[derive(Clone)]
pub struct BridgeConfig {
    pub max_body_bytes: usize,
    pub max_header_bytes: usize,
    pub header_timeout: Duration,
    pub body_timeout: Duration,
    pub stream_idle_timeout: Duration,
    pub first_frame_timeout: Duration,
    pub max_concurrency: usize,
    pub admission_wait: Duration,
    /// Default model+effort selection, from `--model` on the launch. `None`
    /// keeps the built-in default. A request that names its own model still
    /// wins over this.
    default_model: Option<ModelSpec>,
    history_limits: HistoryLimits,
    log_path: Option<std::path::PathBuf>,
    log_max_bytes: usize,
    test_upstream_url: Option<String>,
    #[cfg(test)]
    request_hold: Duration,
    #[cfg(test)]
    frame_hold: Duration,
    #[cfg(test)]
    admission_notifier: Option<std::sync::mpsc::SyncSender<()>>,
}

impl Default for BridgeConfig {
    fn default() -> Self {
        Self {
            max_body_bytes: DEFAULT_MAX_BODY_BYTES,
            max_header_bytes: DEFAULT_MAX_HEADER_BYTES,
            header_timeout: DEFAULT_HEADER_TIMEOUT,
            body_timeout: DEFAULT_BODY_TIMEOUT,
            stream_idle_timeout: DEFAULT_STREAM_IDLE_TIMEOUT,
            first_frame_timeout: DEFAULT_FIRST_FRAME_TIMEOUT,
            max_concurrency: DEFAULT_MAX_CONCURRENCY,
            admission_wait: DEFAULT_ADMISSION_WAIT,
            default_model: None,
            history_limits: HistoryLimits::default(),
            log_path: default_bridge_log_path(),
            log_max_bytes: crate::bridge_log::DEFAULT_MAX_BYTES,
            test_upstream_url: test_upstream_override_from_process(),
            #[cfg(test)]
            request_hold: Duration::ZERO,
            #[cfg(test)]
            frame_hold: Duration::ZERO,
            #[cfg(test)]
            admission_notifier: None,
        }
    }
}

impl BridgeConfig {
    /// Pin the selection used when a request carries no model of its own.
    pub fn with_default_model(mut self, model: Option<ModelSpec>) -> Self {
        self.default_model = model;
        self
    }

    #[cfg(test)]
    fn with_test_upstream_url(mut self, url: Option<String>) -> Self {
        self.test_upstream_url = url;
        self
    }

    #[cfg(test)]
    fn with_history_limits(mut self, limits: HistoryLimits) -> Self {
        self.history_limits = limits;
        self
    }

    #[cfg(test)]
    fn with_log_path(mut self, path: std::path::PathBuf) -> Self {
        self.log_path = Some(path);
        self
    }

    #[cfg(test)]
    fn with_log_max_bytes(mut self, max_bytes: usize) -> Self {
        self.log_max_bytes = max_bytes;
        self
    }

    #[cfg(test)]
    fn with_request_hold(mut self, hold: Duration) -> Self {
        self.request_hold = hold;
        self
    }

    /// Delay inserted between streamed frames so a test can observe partial
    /// delivery. Without it, a fixture stream completes too fast to prove the
    /// difference between progressive flushing and one buffered write.
    #[cfg(test)]
    fn with_frame_hold(mut self, hold: Duration) -> Self {
        self.frame_hold = hold;
        self
    }

    #[cfg(test)]
    fn with_admission_notifier(mut self, notifier: std::sync::mpsc::SyncSender<()>) -> Self {
        self.admission_notifier = Some(notifier);
        self
    }
}

impl fmt::Debug for BridgeConfig {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("BridgeConfig")
            .field("max_body_bytes", &self.max_body_bytes)
            .field("max_header_bytes", &self.max_header_bytes)
            .field("header_timeout", &self.header_timeout)
            .field("body_timeout", &self.body_timeout)
            .field("stream_idle_timeout", &self.stream_idle_timeout)
            .field("first_frame_timeout", &self.first_frame_timeout)
            .field("max_concurrency", &self.max_concurrency)
            .field("admission_wait", &self.admission_wait)
            .field(
                "default_model",
                &self.default_model.as_ref().map(ModelSpec::display),
            )
            .field("history_limits", &self.history_limits)
            .field(
                "test_upstream_url",
                &self.test_upstream_url.as_ref().map(|_| "[redacted]"),
            )
            .finish()
    }
}

/// Startup/shutdown failures deliberately contain no endpoint or token text.
#[derive(Debug)]
pub enum BridgeError {
    Bind(io::Error),
    Random(String),
    Spawn(io::Error),
    Join,
    /// The launch named a model or effort that does not parse. Carries the
    /// selector's own message, which names the valid values.
    Model(String),
    /// The launch supplied Claude settings that could not be composed with
    /// the bridge's session-local lifecycle hooks.
    Settings(String),
}

impl fmt::Display for BridgeError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Bind(error) => write!(formatter, "failed to bind loopback bridge: {error}"),
            Self::Random(error) => {
                write!(formatter, "failed to create bridge credentials: {error}")
            }
            Self::Spawn(error) => write!(formatter, "failed to start bridge worker: {error}"),
            Self::Join => formatter.write_str("bridge worker panicked during shutdown"),
            Self::Model(error) => write!(formatter, "{error}"),
            Self::Settings(error) => write!(formatter, "{error}"),
        }
    }
}

impl std::error::Error for BridgeError {}

/// Owns the listener, bearer, shutdown signal, and join handle for a launch.
/// Debug output is intentionally structural: even the ephemeral base URL is
/// omitted so error snapshots cannot become an accidental credential map.
pub struct BridgeHandle {
    #[cfg(test)]
    socket_addr: std::net::SocketAddr,
    base_url: String,
    bearer_token: String,
    shutdown: Arc<AtomicBool>,
    active: Arc<AtomicUsize>,
    conversations: ConversationStore,
    connections: ActiveConnections,
    log: Option<SharedBridgeLog>,
    join: Option<JoinHandle<()>>,
}

impl BridgeHandle {
    pub fn start(config: BridgeConfig) -> Result<Self, BridgeError> {
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).map_err(BridgeError::Bind)?;
        listener.set_nonblocking(true).map_err(BridgeError::Bind)?;
        let socket_addr = listener.local_addr().map_err(BridgeError::Bind)?;
        debug_assert_eq!(socket_addr.ip(), IpAddr::V4(Ipv4Addr::LOCALHOST));

        let mut token_bytes = [0_u8; 32];
        getrandom::fill(&mut token_bytes)
            .map_err(|error| BridgeError::Random(error.to_string()))?;
        let bearer_token = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(token_bytes);
        let base_url = format!("http://{socket_addr}");
        let shutdown = Arc::new(AtomicBool::new(false));
        let active = Arc::new(AtomicUsize::new(0));
        let conversations = ConversationStore::new(config.history_limits);
        let connections = Arc::new(Mutex::new(HashMap::new()));
        let log = config.log_path.clone().map(|path| {
            Arc::new(Mutex::new(BridgeLog::with_max_bytes(
                path,
                config.log_max_bytes,
            )))
        });
        let (ready_tx, ready_rx) = std::sync::mpsc::sync_channel(0);
        let thread_shutdown = Arc::clone(&shutdown);
        let thread_active = Arc::clone(&active);
        let thread_conversations = conversations.clone();
        let thread_connections = Arc::clone(&connections);
        let thread_log = log.clone();
        let thread_token = bearer_token.clone();
        let join = thread::Builder::new()
            .name("clud-codex-bridge".to_string())
            .spawn(move || {
                let _ = ready_tx.send(());
                serve(
                    listener,
                    config,
                    thread_token,
                    thread_shutdown,
                    thread_active,
                    thread_conversations,
                    thread_connections,
                    thread_log,
                )
            })
            .map_err(BridgeError::Spawn)?;
        ready_rx.recv().map_err(|_| BridgeError::Join)?;

        Ok(Self {
            #[cfg(test)]
            socket_addr,
            base_url,
            bearer_token,
            shutdown,
            active,
            conversations,
            connections,
            log,
            join: Some(join),
        })
    }

    #[cfg(test)]
    pub(crate) fn socket_addr(&self) -> std::net::SocketAddr {
        self.socket_addr
    }

    pub(crate) fn base_url(&self) -> &str {
        &self.base_url
    }

    pub(crate) fn bearer_token(&self) -> &str {
        &self.bearer_token
    }

    #[cfg(test)]
    fn active_requests(&self) -> usize {
        self.active.load(Ordering::Acquire)
    }

    pub fn shutdown(&mut self) -> Result<(), BridgeError> {
        let connections = lock_connections(&self.connections);
        self.shutdown.store(true, Ordering::Release);
        for stream in connections.values() {
            let _ = stream.shutdown(Shutdown::Both);
        }
        drop(connections);
        let was_running = self.join.is_some();
        if let Some(join) = self.join.take() {
            join.join().map_err(|_| BridgeError::Join)?;
        }
        if was_running {
            self.conversations.clear();
            if let Some(log) = &self.log {
                let mut log = lock_log(log);
                log.flush();
                if log.has_records() {
                    eprintln!("[clud] codex bridge log: {}", log.path().display());
                }
            }
        }
        Ok(())
    }
}

impl fmt::Debug for BridgeHandle {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("BridgeHandle")
            .field("active", &self.join.is_some())
            .field("active_requests", &self.active.load(Ordering::Acquire))
            .finish()
    }
}

impl Drop for BridgeHandle {
    fn drop(&mut self) {
        let _ = self.shutdown();
    }
}

#[allow(clippy::too_many_arguments)]
fn serve(
    listener: TcpListener,
    config: BridgeConfig,
    bearer_token: String,
    shutdown: Arc<AtomicBool>,
    active: Arc<AtomicUsize>,
    conversations: ConversationStore,
    connections: ActiveConnections,
    log: Option<SharedBridgeLog>,
) {
    let mut workers = Vec::<JoinHandle<()>>::new();
    let mut next_worker_id = 0_usize;
    // When every slot is busy, decline to *accept*: pending connections wait in
    // the kernel's listen backlog instead of being answered 503. A short wait is
    // invisible to the user; a 503 is a hard API error. `full_since` bounds that
    // wait so a wedged worker cannot hang a client indefinitely.
    let mut full_since: Option<Instant> = None;
    while !shutdown.load(Ordering::Acquire) {
        workers.retain(|worker| !worker.is_finished());
        let limit = config.max_concurrency.max(1);
        if active.load(Ordering::Acquire) >= limit {
            let waiting_since = *full_since.get_or_insert_with(Instant::now);
            if waiting_since.elapsed() < config.admission_wait {
                thread::sleep(ACCEPT_POLL);
                continue;
            }
        } else {
            full_since = None;
        }
        match listener.accept() {
            Ok((stream, _peer)) => {
                // The listener is non-blocking so the accept loop can poll for
                // shutdown. On Windows an accepted socket inherits that mode,
                // and a non-blocking socket ignores SO_RCVTIMEO: every read
                // that outruns the client's next segment returns WouldBlock,
                // which the readers below classify as a timeout. The result is
                // an instant 408 for any request whose body lands in a
                // separate segment from its headers. Restore blocking mode per
                // connection so the read deadlines are the real bound.
                if stream.set_nonblocking(false).is_err() {
                    continue;
                }
                if !reserve_worker(&active, limit) {
                    // Only reachable once `admission_wait` has elapsed: the
                    // backstop that keeps a client from waiting forever.
                    reject_busy(stream, config.header_timeout, &shutdown, log.as_ref());
                    continue;
                }
                let shutdown_stream = match stream.try_clone() {
                    Ok(stream) => stream,
                    Err(_) => {
                        active.fetch_sub(1, Ordering::AcqRel);
                        continue;
                    }
                };
                let worker_id = next_worker_id;
                next_worker_id = next_worker_id.wrapping_add(1);
                {
                    let mut registered = lock_connections(&connections);
                    registered.insert(worker_id, shutdown_stream);
                    if shutdown.load(Ordering::Acquire) {
                        if let Some(stream) = registered.get(&worker_id) {
                            let _ = stream.shutdown(Shutdown::Both);
                        }
                    }
                }
                let worker_active = Arc::clone(&active);
                let worker_conversations = conversations.clone();
                let worker_connections = Arc::clone(&connections);
                let worker_shutdown = Arc::clone(&shutdown);
                let worker_config = config.clone();
                let worker_token = bearer_token.clone();
                let worker_log = log.clone();
                match thread::Builder::new()
                    .name("clud-codex-bridge-request".to_string())
                    .spawn(move || {
                        let _guard = ActiveWorker {
                            active: worker_active,
                            connections: worker_connections,
                            worker_id,
                        };
                        handle_connection(
                            stream,
                            &worker_config,
                            &worker_token,
                            &worker_shutdown,
                            &worker_conversations,
                            worker_log.as_ref(),
                        );
                    }) {
                    Ok(worker) => workers.push(worker),
                    Err(_) => {
                        active.fetch_sub(1, Ordering::AcqRel);
                        lock_connections(&connections).remove(&worker_id);
                    }
                }
            }
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                thread::sleep(ACCEPT_POLL);
            }
            Err(_) => break,
        }
    }
    drop(listener);
    for worker in workers {
        let _ = worker.join();
    }
}

fn reserve_worker(active: &AtomicUsize, limit: usize) -> bool {
    active
        .fetch_update(Ordering::AcqRel, Ordering::Acquire, |current| {
            (current < limit).then_some(current + 1)
        })
        .is_ok()
}

fn lock_connections(
    connections: &ActiveConnections,
) -> std::sync::MutexGuard<'_, HashMap<usize, TcpStream>> {
    connections
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

struct ActiveWorker {
    active: Arc<AtomicUsize>,
    connections: ActiveConnections,
    worker_id: usize,
}

impl Drop for ActiveWorker {
    fn drop(&mut self) {
        lock_connections(&self.connections).remove(&self.worker_id);
        self.active.fetch_sub(1, Ordering::AcqRel);
    }
}

fn reject_busy(
    mut stream: TcpStream,
    timeout: Duration,
    shutdown: &AtomicBool,
    log: Option<&SharedBridgeLog>,
) {
    record_rejection(log, 503, "admission_cap");
    // Consume the already-buffered request headers before closing. On Windows,
    // closing a TCP socket with unread inbound bytes sends RST and can discard
    // the 503 that was just written.
    let drain_timeout = timeout.min(Duration::from_millis(100));
    let deadline = Instant::now() + drain_timeout;
    let _ = read_headers(&mut stream, DEFAULT_MAX_HEADER_BYTES, deadline, shutdown);
    if stream.set_write_timeout(Some(timeout)).is_err() {
        return;
    }
    let _ = write_response(
        &mut stream,
        503,
        "application/json",
        br#"{"error":{"type":"overloaded_error","message":"bridge busy"}}"#,
        false,
    );
}

fn handle_connection(
    mut stream: TcpStream,
    config: &BridgeConfig,
    bearer_token: &str,
    shutdown: &AtomicBool,
    conversations: &ConversationStore,
    log: Option<&SharedBridgeLog>,
) {
    #[cfg(test)]
    if let Some(notifier) = &config.admission_notifier {
        let _ = notifier.try_send(());
    }
    #[cfg(test)]
    if !config.request_hold.is_zero() {
        thread::sleep(config.request_hold);
    }
    // Each phase gets its own budget. Sharing one deadline across header read,
    // body read, and response write is what made a real (multi-minute) model
    // response indistinguishable from a stalled socket.
    let header_deadline = Instant::now() + config.header_timeout;
    if stream
        .set_write_timeout(Some(config.header_timeout))
        .is_err()
    {
        return;
    }

    let parsed = match read_headers(
        &mut stream,
        config.max_header_bytes,
        header_deadline,
        shutdown,
    ) {
        Ok(parsed) => parsed,
        Err(ABANDON) => return,
        Err(status) => {
            record_rejection(log, status, "request_headers");
            let _ = write_error(&mut stream, status);
            return;
        }
    };

    if parsed.content_length > config.max_body_bytes {
        record_rejection(log, 413, "request_body_too_large");
        let _ = write_error(&mut stream, 413);
        return;
    }
    let expected = format!("Bearer {bearer_token}");
    if !parsed
        .authorization
        .as_deref()
        .is_some_and(|provided| constant_time_eq(provided.as_bytes(), expected.as_bytes()))
    {
        record_rejection(log, 401, "bearer_mismatch");
        let _ = write_error(&mut stream, 401);
        return;
    }
    let conversation_key =
        ConversationKey::from_headers(parsed.session_id.as_deref(), parsed.agent_id.as_deref());

    // Route on the path alone. Claude Code sends `POST /v1/messages?beta=true`,
    // so matching the raw request target 404s a request that is perfectly
    // valid -- a defect only a live client surfaces, since the mock probe
    // sends a bare path.
    let route = parsed
        .path
        .split('?')
        .next()
        .unwrap_or(parsed.path.as_str())
        .to_string();
    match (parsed.method.as_str(), route.as_str()) {
        ("POST", "/_clud/context/compact") => {
            let body_deadline = Instant::now() + config.body_timeout;
            match read_context_control_body(
                &mut stream,
                parsed.body_prefix,
                parsed.content_length,
                body_deadline,
                shutdown,
                ContextControl::Compact,
            ) {
                Ok(body_key) => serve_context_compact(
                    &mut stream,
                    config,
                    shutdown,
                    conversations,
                    log,
                    body_key.unwrap_or_else(|| conversation_key.clone()),
                ),
                Err(ABANDON) => {}
                Err(status) => {
                    record_rejection(log, status, "context_control_body");
                    let _ = write_error(&mut stream, status);
                }
            }
        }
        ("POST", "/_clud/context/clear") => {
            let body_deadline = Instant::now() + config.body_timeout;
            match read_context_control_body(
                &mut stream,
                parsed.body_prefix,
                parsed.content_length,
                body_deadline,
                shutdown,
                ContextControl::Clear,
            ) {
                Ok(body_key) => serve_context_clear(
                    &mut stream,
                    conversations,
                    body_key.unwrap_or_else(|| conversation_key.clone()),
                ),
                Err(ABANDON) => {}
                Err(status) => {
                    record_rejection(log, status, "context_control_body");
                    let _ = write_error(&mut stream, status);
                }
            }
        }
        ("HEAD", "/v1/messages") => {
            let _ = write_response(&mut stream, 200, "application/json", b"", true);
        }
        ("POST", "/v1/messages/count_tokens") => {
            record_rejection(log, 404, "token_counting_unsupported");
            let _ = write_response(
                &mut stream,
                404,
                "application/json",
                br#"{"error":{"type":"not_found_error","message":"token counting is not supported by the Codex bridge"}}"#,
                false,
            );
        }
        ("POST", "/v1/messages") => {
            let body_deadline = Instant::now() + config.body_timeout;
            let body = match read_body(
                &mut stream,
                parsed.body_prefix,
                parsed.content_length,
                body_deadline,
                shutdown,
            ) {
                Ok(body) => body,
                Err(ABANDON) => return,
                Err(status) => {
                    record_rejection(log, status, "request_body");
                    let _ = write_error(&mut stream, status);
                    return;
                }
            };
            let json: serde_json::Value = match serde_json::from_slice(&body) {
                Ok(json) => json,
                Err(_) => {
                    record_rejection(log, 400, "invalid_json");
                    let _ = write_error(&mut stream, 400);
                    return;
                }
            };
            let streaming = json.get("stream").and_then(serde_json::Value::as_bool) == Some(true);
            serve_messages(
                &mut stream,
                config,
                shutdown,
                conversations,
                &body,
                streaming,
                &conversation_key,
                log,
            );
        }
        _ => {
            if std::env::var_os("CLUD_CODEX_BRIDGE_DEBUG").is_some_and(|value| value == "1") {
                eprintln!(
                    "[clud] codex bridge: unhandled {} {}",
                    parsed.method, parsed.path
                );
            }
            record_rejection(log, 404, "unrouted_request");
            let _ = write_error(&mut stream, 404);
        }
    }
}

#[derive(Clone, Copy)]
enum ContextControl {
    Compact,
    Clear,
}

fn read_context_control_body(
    stream: &mut TcpStream,
    prefix: Vec<u8>,
    content_length: usize,
    deadline: Instant,
    shutdown: &AtomicBool,
    control: ContextControl,
) -> Result<Option<ConversationKey>, u16> {
    let body = read_body(stream, prefix, content_length, deadline, shutdown)?;
    if body.is_empty() {
        return Ok(None);
    }
    let value: serde_json::Value = serde_json::from_slice(&body).map_err(|_| 400_u16)?;
    let matches_lifecycle_event = match control {
        ContextControl::Compact => {
            value
                .get("hook_event_name")
                .and_then(serde_json::Value::as_str)
                == Some("PreCompact")
                && matches!(
                    value.get("trigger").and_then(serde_json::Value::as_str),
                    Some("manual" | "auto")
                )
        }
        ContextControl::Clear => {
            value
                .get("hook_event_name")
                .and_then(serde_json::Value::as_str)
                == Some("SessionStart")
                && value.get("source").and_then(serde_json::Value::as_str) == Some("clear")
        }
    };
    if !matches_lifecycle_event {
        return Err(400);
    }
    let session_id = value.get("session_id").and_then(serde_json::Value::as_str);
    let agent_id = value.get("agent_id").and_then(serde_json::Value::as_str);
    Ok((session_id.is_some() || agent_id.is_some())
        .then(|| ConversationKey::from_headers(session_id, agent_id)))
}

fn serve_context_compact(
    stream: &mut TcpStream,
    config: &BridgeConfig,
    shutdown: &AtomicBool,
    conversations: &ConversationStore,
    log: Option<&SharedBridgeLog>,
    conversation_key: ConversationKey,
) {
    let operation = conversations.with_history(&conversation_key.id, |history| {
        let snapshot = history.snapshot();
        if snapshot.is_empty() {
            return Ok(Ok(()));
        }
        let pipeline = build_pipeline(config, log).map_err(PipelineError::Upstream);
        let result = pipeline
            .and_then(|pipeline| pipeline.compact_canonical_history(snapshot, shutdown))
            .and_then(|replacement| {
                history.replace_history(&replacement).map_err(|error| {
                    PipelineError::Translate(crate::codex_translate::TranslateError::Invalid(
                        error.to_string(),
                    ))
                })
            });
        Ok(result)
    });
    match operation {
        Ok(Ok(())) => {
            let _ = write_response(stream, 204, "application/json", b"", false);
        }
        Ok(Err(error)) => {
            let _ = write_pipeline_error(stream, &error, log);
        }
        Err(error) => {
            let failure = PipelineError::Translate(
                crate::codex_translate::TranslateError::Invalid(error.to_string()),
            );
            let _ = write_pipeline_error(stream, &failure, log);
        }
    }
}

fn serve_context_clear(
    stream: &mut TcpStream,
    conversations: &ConversationStore,
    conversation_key: ConversationKey,
) {
    conversations.clear_session(&conversation_key.session_prefix);
    let result = Ok::<(), crate::codex_history::HistoryError>(());
    match result {
        Ok(()) => {
            let _ = write_response(stream, 204, "application/json", b"", false);
        }
        Err(error) => {
            let failure = PipelineError::Translate(
                crate::codex_translate::TranslateError::Invalid(error.to_string()),
            );
            let _ = write_pipeline_error(stream, &failure, None);
        }
    }
}

/// Build the pipeline for one request.
///
/// The debug seam repoints the *upstream base URL* at a Responses-shaped fake.
/// Phase 2 used it to pass an Anthropic body through unchanged, which meant the
/// end-to-end tests proved transport and auth but nothing about translation.
fn build_pipeline(
    config: &BridgeConfig,
    log: Option<&SharedBridgeLog>,
) -> Result<Pipeline<ResolvedCredentials>, UpstreamError> {
    let credentials = match config.test_upstream_url.as_deref() {
        Some(base_url) => ResolvedCredentials::ApiKey(ApiKeyCredentials::new(
            "clud-test-upstream-key",
            Some(base_url.into()),
        )?),
        None => ResolvedCredentials::resolve_default()?,
    };
    let upstream_config = UpstreamConfig {
        first_frame_timeout: Some(config.first_frame_timeout),
        read_timeout: config.stream_idle_timeout,
        ..UpstreamConfig::default()
    };
    let mut client = UpstreamClient::new(credentials, upstream_config);
    if let Some(log) = log.cloned() {
        client = client.with_retry_observer(move |error, attempt, budget, backoff| {
            record_retry(&log, error, attempt, budget, backoff);
        });
    }
    let mut pipeline = Pipeline::new(client);
    if let Some(model) = config.default_model.clone() {
        pipeline = pipeline.with_default_model(model);
    }
    Ok(pipeline)
}

/// Serve one `POST /v1/messages`.
///
/// The status is chosen only while nothing has been written. Once the writer
/// has emitted a frame the response is committed, so a later failure is
/// reported in-band by the translator's own `error` event (already appended by
/// the pipeline) and the chunked body is simply terminated.
#[allow(clippy::too_many_arguments)]
fn serve_messages(
    stream: &mut TcpStream,
    config: &BridgeConfig,
    shutdown: &AtomicBool,
    conversations: &ConversationStore,
    body: &[u8],
    streaming: bool,
    conversation_key: &ConversationKey,
    log: Option<&SharedBridgeLog>,
) {
    let pipeline = match build_pipeline(config, log) {
        Ok(pipeline) => pipeline,
        Err(error) => {
            let failure = PipelineError::Upstream(error);
            let _ = write_pipeline_error(stream, &failure, log);
            return;
        }
    };
    let message_id = new_message_id();

    if streaming {
        let mut writer = EventStreamWriter::new(stream, config);
        let streamed = conversations.with_history(&conversation_key.id, |history| {
            let streamed = {
                let mut sink = |frame: &str| -> Result<(), UpstreamError> {
                    writer
                        .write_frame(frame)
                        .map_err(|_| UpstreamError::Downstream("client write failed"))
                };
                pipeline.stream_with_history(body, &message_id, shutdown, &mut sink, history)
            };
            if let Ok(summary) = &streamed {
                if summary.history_append_rejected && writer.started() {
                    let _ = writer.finish();
                    summary.clear_history_after_client_commit(history);
                }
            }
            Ok(streamed)
        });
        match streamed {
            Ok(Ok(summary)) => {
                if let Some(failure) = summary.in_band_failure.as_ref() {
                    log_in_band_failure(log, failure, request_phase(body), summary.request_shape);
                }
                // A quota failure delivered inside a 200 SSE stream used to
                // produce HTTP 200, no log line even under
                // `CLUD_CODEX_BRIDGE_DEBUG=1`, and an abruptly truncated turn.
                // The status is committed by now, but silence is not forced.
                if summary.terminal_account_failure {
                    let error = PipelineError::Provider(ProviderFailure {
                        kind: "billing_error".to_string(),
                        message: IN_BAND_QUOTA_MESSAGE.to_string(),
                        diagnostic: None,
                    });
                    log_pipeline_error(&error, log);
                    warn_once_on_terminal_failure(&error);
                }
                if !summary.history_append_rejected {
                    let _ = writer.finish();
                }
            }
            Ok(Err(error)) => {
                if writer.started() {
                    // Committed: the pipeline has already emitted a sanitized
                    // `error` event, so just close the body cleanly.
                    log_pipeline_error(&error, log);
                    // ...but a drained account still deserves the banner. This
                    // is the same failure as the pre-commit case; the only
                    // difference is that a frame had already gone out, which
                    // changes the status we can send and nothing else.
                    warn_once_on_terminal_failure(&error);
                    let _ = writer.finish();
                } else {
                    let _ = write_pipeline_error(stream, &error, log);
                }
            }
            Err(error) => {
                let failure = PipelineError::Translate(
                    crate::codex_translate::TranslateError::Invalid(error.to_string()),
                );
                let _ = write_pipeline_error(stream, &failure, log);
            }
        }
        return;
    }

    match conversations.with_history(&conversation_key.id, |history| {
        let completed = pipeline
            .complete_with_history(body, &message_id, shutdown, history)
            .map(|completion| {
                let rendered = serde_json::to_vec(&completion.message).unwrap_or_default();
                if write_response(stream, 200, "application/json", &rendered, false).is_ok() {
                    completion.clear_history_after_client_commit(history);
                }
            });
        Ok(completed)
    }) {
        Ok(Ok(())) => {}
        Ok(Err(error)) => {
            if let PipelineError::Provider(failure) = &error {
                if let Some(diagnostic) = &failure.diagnostic {
                    log_in_band_failure(
                        log,
                        &diagnostic.failure,
                        request_phase(body),
                        diagnostic.request_shape.clone(),
                    );
                }
            }
            let _ = write_pipeline_error(stream, &error, log);
        }
        Err(error) => {
            let failure = PipelineError::Translate(
                crate::codex_translate::TranslateError::Invalid(error.to_string()),
            );
            let _ = write_pipeline_error(stream, &failure, log);
        }
    }
}

/// Announce a terminal account failure on stderr, once per process.
///
/// Every other diagnostic the bridge produces is gated behind
/// `CLUD_CODEX_BRIDGE_DEBUG=1` or buried in the forensic log that nothing
/// reads back. A drained account is not a debugging detail: the session cannot
/// continue, no retry will fix it, and the user has to go do something in a
/// browser. It gets the one ungated line in the module.
///
/// Once per process, not per request: a failing turn can produce several of
/// these, and a banner repeated ten times is noise that trains people to
/// ignore it. Follows the `wedge_watchdog` warn-once-per-episode precedent.
fn warn_once_on_terminal_failure(error: &PipelineError) {
    static WARNED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
    if !error.is_terminal_account_failure() {
        return;
    }
    if WARNED.swap(true, std::sync::atomic::Ordering::Relaxed) {
        return;
    }
    // The leading `\x07` is a terminal bell: this is the failure most likely
    // to be waiting on a user who has looked away from an unattended run.
    eprintln!("\x07[clud] codex bridge: {}", error.client_message());
}

/// Opt-in diagnostics. The bridge answers the harness with a sanitized error,
/// which is correct but leaves an operator with no way to tell a translation
/// bug from an upstream outage. This prints the classification only -- never an
/// upstream body, which can carry account identifiers.
fn log_pipeline_error(error: &PipelineError, log: Option<&SharedBridgeLog>) {
    if let Some(log) = log {
        let mut event = serde_json::json!({
            "ts_ms": unix_ms(),
            "event": "pipeline_failure",
            "downstream_status": error.http_status(),
            "kind": pipeline_error_kind(error),
        });
        if let PipelineError::Upstream(UpstreamError::Status(failure)) = error {
            add_failure_fields(&mut event, failure);
        }
        lock_log(log).record(event);
    }
    if std::env::var_os("CLUD_CODEX_BRIDGE_DEBUG").is_some_and(|value| value == "1") {
        eprintln!(
            "[clud] codex bridge: {} -> HTTP {}",
            error,
            error.http_status()
        );
        // The classification, the correlation ids, and the scrubbed reason.
        // Without this an operator can see *that* a 502 happened but not
        // whether it came from the edge, the provider, or clud itself (#764).
        if let Some(diagnostic) = error.upstream_diagnostic() {
            eprintln!("[clud] codex bridge: upstream {diagnostic}");
        }
    }
}

fn log_in_band_failure(
    log: Option<&SharedBridgeLog>,
    failure: &InBandFailure,
    phase: &'static str,
    request_shape: serde_json::Value,
) {
    let event = serde_json::json!({
        "ts_ms": unix_ms(),
        "event": "in_band_upstream_failure",
        "upstream_status": 400,
        "category": failure.category,
        "code": failure.code,
        "request_id": failure.request_id,
        "phase": phase,
        "request_shape": request_shape,
    });
    if let Some(log) = log {
        lock_log(log).record(event.clone());
    }
    if std::env::var_os("CLUD_CODEX_BRIDGE_DEBUG").is_some_and(|value| value == "1") {
        eprintln!("[clud] codex bridge: {}", event);
    }
}

fn request_phase(body: &[u8]) -> &'static str {
    let Ok(value) = serde_json::from_slice::<serde_json::Value>(body) else {
        return "initial";
    };
    let messages = value
        .get("messages")
        .and_then(serde_json::Value::as_array)
        .map(Vec::as_slice)
        .unwrap_or_default();
    if messages
        .iter()
        .any(|message| message.get("role").and_then(serde_json::Value::as_str) == Some("assistant"))
    {
        "continuation"
    } else {
        "initial"
    }
}

fn write_pipeline_error(
    stream: &mut TcpStream,
    error: &PipelineError,
    log: Option<&SharedBridgeLog>,
) -> io::Result<()> {
    log_pipeline_error(error, log);
    warn_once_on_terminal_failure(error);
    let status = error.http_status();
    let body = serde_json::json!({
        "type": "error",
        "error": {
            "type": error_type_for(error),
            "message": error.client_message(),
        },
    });
    write_response_with(
        stream,
        status,
        "application/json",
        body.to_string().as_bytes(),
        false,
        &retry_after_header(error),
    )
}

/// The Anthropic error type, derived from the *classification* where one
/// exists and only falling back to the status otherwise.
///
/// Re-deriving it from the number alone is what kept `billing_error` from ever
/// reaching the client: a drained account and an ordinary throttle are both
/// 429, and the status has already lost the distinction the pipeline computed.
fn error_type_for(error: &PipelineError) -> &'static str {
    match error.failure_class() {
        Some(FailureClass::Exhausted) => "billing_error",
        _ => anthropic_error_type(error.http_status()),
    }
}

/// `Retry-After`, echoed to the client on a throttle or an exhaustion.
///
/// The bridge previously emitted exactly four hardcoded headers and never this
/// one, on any status -- so a client showing a reset time was reporting its own
/// separate accounting, about a different limit than the one that broke the
/// turn.
fn retry_after_header(error: &PipelineError) -> Vec<(String, String)> {
    match error.retry_after_seconds() {
        Some(seconds) => vec![("Retry-After".to_string(), seconds.to_string())],
        None => Vec::new(),
    }
}

fn lock_log(log: &SharedBridgeLog) -> std::sync::MutexGuard<'_, BridgeLog> {
    log.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
}

fn record_rejection(log: Option<&SharedBridgeLog>, status: u16, reason: &'static str) {
    if let Some(log) = log {
        lock_log(log).record(serde_json::json!({
            "ts_ms": unix_ms(),
            "event": "request_rejected",
            "downstream_status": status,
            "reason": reason,
        }));
    }
}

fn record_retry(
    log: &SharedBridgeLog,
    error: &UpstreamError,
    attempt: u32,
    budget: u32,
    backoff: Option<Duration>,
) {
    let mut event = serde_json::json!({
        "ts_ms": unix_ms(),
        "event": "upstream_attempt",
        "kind": upstream_error_kind(error),
        "attempt": attempt,
        "budget": budget,
        "decision": if backoff.is_some() { "retry" } else { "stop" },
        "backoff_ms": backoff.map(|value| value.as_millis() as u64),
    });
    if let UpstreamError::Status(failure) = error {
        add_failure_fields(&mut event, failure);
    }
    lock_log(log).record(event);
}

fn add_failure_fields(event: &mut serde_json::Value, failure: &UpstreamFailure) {
    let Some(fields) = event.as_object_mut() else {
        return;
    };
    fields.insert("upstream_status".into(), failure.status().into());
    fields.insert(
        "class".into(),
        format!("{:?}", failure.class()).to_ascii_lowercase().into(),
    );
    fields.insert("cf_ray".into(), failure.cf_ray().into());
    fields.insert("x_request_id".into(), failure.request_id().into());
    fields.insert(
        "retry_after_ms".into(),
        failure
            .retry_after()
            .map(|value| value.as_millis() as u64)
            .into(),
    );
    fields.insert(
        "resets_in_ms".into(),
        failure
            .resets_in()
            .map(|value| value.as_millis() as u64)
            .into(),
    );
    fields.insert("detail".into(), failure.detail().into());
}

fn upstream_error_kind(error: &UpstreamError) -> &'static str {
    match error {
        UpstreamError::Credentials(_) => "credentials",
        UpstreamError::Transport(_) => "transport",
        UpstreamError::CompactMalformed => "compact_malformed",
        UpstreamError::Status(_) => "status",
        UpstreamError::Timeout => "timeout",
        UpstreamError::TooLarge => "too_large",
        UpstreamError::Cancelled => "cancelled",
        UpstreamError::Downstream(_) => "downstream",
    }
}

fn pipeline_error_kind(error: &PipelineError) -> &'static str {
    match error {
        PipelineError::Translate(_) => "translate",
        PipelineError::Compaction(_) => "compaction",
        PipelineError::Provider(_) => "provider_in_band",
        PipelineError::Upstream(error) => upstream_error_kind(error),
    }
}

fn default_bridge_log_path() -> Option<std::path::PathBuf> {
    let state_dir = crate::daemon::default_state_dir().ok()?;
    Some(crate::bridge_log::session_bridge_log_path(
        &state_dir,
        std::process::id(),
        crate::process_identity::self_start_time(),
    ))
}

fn anthropic_error_type(status: u16) -> &'static str {
    match status {
        400 => "invalid_request_error",
        401 => "authentication_error",
        403 => "permission_error",
        404 => "not_found_error",
        413 => "request_too_large",
        422 => "invalid_request_error",
        429 => "rate_limit_error",
        // #764 stopped folding these into 502; they need types of their own.
        499 => "request_cancelled",
        504 => "timeout_error",
        _ => "api_error",
    }
}

/// Per-response identifier. Anthropic clients treat this as opaque; it only has
/// to be unique enough to correlate one turn.
fn new_message_id() -> String {
    let mut bytes = [0_u8; 12];
    if getrandom::fill(&mut bytes).is_err() {
        return "msg_clud_bridge".to_string();
    }
    format!(
        "msg_{}",
        base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(bytes)
    )
}

struct ParsedRequest {
    method: String,
    path: String,
    authorization: Option<String>,
    session_id: Option<String>,
    agent_id: Option<String>,
    content_length: usize,
    body_prefix: Vec<u8>,
}

fn read_headers(
    stream: &mut TcpStream,
    limit: usize,
    deadline: Instant,
    shutdown: &AtomicBool,
) -> Result<ParsedRequest, u16> {
    let mut buffer = Vec::with_capacity(limit.min(4096));
    let header_end = loop {
        if let Some(index) = find_header_end(&buffer) {
            if index + 4 > limit {
                return Err(431);
            }
            break index + 4;
        }
        if buffer.len() >= limit {
            return Err(431);
        }
        set_remaining_read_timeout(stream, deadline, shutdown)?;
        let mut chunk = [0_u8; 1024];
        match stream.read(&mut chunk) {
            Ok(0) => return Err(400),
            Ok(count) => buffer.extend_from_slice(&chunk[..count]),
            Err(error)
                if matches!(
                    error.kind(),
                    io::ErrorKind::TimedOut | io::ErrorKind::WouldBlock
                ) =>
            {
                match read_expired(deadline, shutdown) {
                    Some(status) => return Err(status),
                    None => continue,
                }
            }
            Err(_) => return Err(400),
        }
    };

    let header_text = std::str::from_utf8(&buffer[..header_end - 4]).map_err(|_| 400_u16)?;
    let mut lines = header_text.split("\r\n");
    let mut request_line = lines.next().ok_or(400_u16)?.split_whitespace();
    let method = request_line.next().ok_or(400_u16)?.to_string();
    let path = request_line.next().ok_or(400_u16)?.to_string();
    if request_line.next().ok_or(400_u16)? != "HTTP/1.1" || request_line.next().is_some() {
        return Err(400);
    }
    let mut authorization = None;
    let mut session_id = None;
    let mut agent_id = None;
    let mut content_length = 0_usize;
    for line in lines {
        let (name, value) = line.split_once(':').ok_or(400_u16)?;
        let value = value.trim();
        if name.eq_ignore_ascii_case("authorization") {
            authorization = Some(value.to_string());
        } else if name.eq_ignore_ascii_case("x-claude-code-session-id") {
            session_id = Some(value.to_string());
        } else if name.eq_ignore_ascii_case("x-claude-code-agent-id") {
            agent_id = Some(value.to_string());
        } else if name.eq_ignore_ascii_case("x-claude-code-parent-agent-id") {
            // The parent is provenance, not the child's history identity.
        } else if name.eq_ignore_ascii_case("content-length") {
            content_length = value.parse().map_err(|_| 400_u16)?;
        }
    }
    Ok(ParsedRequest {
        method,
        path,
        authorization,
        session_id,
        agent_id,
        content_length,
        body_prefix: buffer[header_end..].to_vec(),
    })
}

fn find_header_end(buffer: &[u8]) -> Option<usize> {
    buffer.windows(4).position(|window| window == b"\r\n\r\n")
}

fn read_body(
    stream: &mut TcpStream,
    mut body: Vec<u8>,
    content_length: usize,
    deadline: Instant,
    shutdown: &AtomicBool,
) -> Result<Vec<u8>, u16> {
    body.truncate(content_length);
    while body.len() < content_length {
        set_remaining_read_timeout(stream, deadline, shutdown)?;
        let remaining = content_length - body.len();
        let mut chunk = [0_u8; 4096];
        match stream.read(&mut chunk[..remaining.min(4096)]) {
            Ok(0) => return Err(400),
            Ok(count) => body.extend_from_slice(&chunk[..count]),
            Err(error)
                if matches!(
                    error.kind(),
                    io::ErrorKind::TimedOut | io::ErrorKind::WouldBlock
                ) =>
            {
                match read_expired(deadline, shutdown) {
                    Some(status) => return Err(status),
                    None => continue,
                }
            }
            Err(_) => return Err(400),
        }
    }
    Ok(body)
}

/// Arm the next read. The timeout is the smaller of the remaining phase budget
/// and [`READ_POLL`], so a caller that sees a timeout must consult the deadline
/// itself before deciding the peer is late — see [`read_expired`].
fn set_remaining_read_timeout(
    stream: &TcpStream,
    deadline: Instant,
    shutdown: &AtomicBool,
) -> Result<(), u16> {
    if shutdown.load(Ordering::Acquire) {
        return Err(ABANDON);
    }
    let remaining = deadline
        .checked_duration_since(Instant::now())
        .filter(|duration| !duration.is_zero())
        .ok_or(408_u16)?;
    stream
        .set_read_timeout(Some(remaining.min(READ_POLL)))
        .map_err(|_| 400)
}

/// Classify a read timeout: a real deadline breach is 408, a poll-interval
/// expiry is just "nothing yet, look again".
fn read_expired(deadline: Instant, shutdown: &AtomicBool) -> Option<u16> {
    if shutdown.load(Ordering::Acquire) {
        return Some(ABANDON);
    }
    (Instant::now() >= deadline).then_some(408)
}

fn write_error(stream: &mut TcpStream, status: u16) -> io::Result<()> {
    let (error_type, message) = match status {
        400 => ("invalid_request_error", "invalid request"),
        401 => ("authentication_error", "unauthorized"),
        404 => ("not_found_error", "not found"),
        408 => ("timeout_error", "request timeout"),
        413 => ("invalid_request_error", "request body too large"),
        431 => ("invalid_request_error", "request headers too large"),
        // No 502 arm: an upstream failure is written by `write_pipeline_error`,
        // which carries the classified message. This path only serves statuses
        // the connection layer itself chooses.
        _ => ("api_error", "bridge error"),
    };
    let body = format!(r#"{{"error":{{"type":"{error_type}","message":"{message}"}}}}"#);
    write_response(stream, status, "application/json", body.as_bytes(), false)
}

/// Incremental SSE writer.
///
/// `write_response` cannot serve this path: it derives `Content-Length` from a
/// fully materialised body and writes once, so a caller could only ever send a
/// stream that had already finished. Chunked transfer plus a flush per frame is
/// what lets Claude render tokens as they arrive rather than at end-of-turn.
///
/// Headers are deferred until the first frame. That is deliberate: a failure
/// before any output can still choose a status code, and only once a frame is
/// on the wire does the response become committed.
///
/// The write timeout is re-armed per frame, making it an idle timeout: total
/// response duration is unbounded, but a peer that stops reading is still cut
/// off.
struct EventStreamWriter<'a> {
    stream: &'a mut TcpStream,
    idle_timeout: Duration,
    started: bool,
    #[cfg(test)]
    frame_hold: Duration,
}

impl<'a> EventStreamWriter<'a> {
    fn new(stream: &'a mut TcpStream, config: &BridgeConfig) -> Self {
        let idle_timeout = if config.stream_idle_timeout.is_zero() {
            DEFAULT_STREAM_IDLE_TIMEOUT
        } else {
            config.stream_idle_timeout
        };
        Self {
            stream,
            idle_timeout,
            started: false,
            #[cfg(test)]
            frame_hold: config.frame_hold,
        }
    }

    fn started(&self) -> bool {
        self.started
    }

    fn write_frame(&mut self, frame: &str) -> io::Result<()> {
        // Re-arm per frame so the budget is "silence between frames", not
        // "total time to produce the response".
        self.stream.set_write_timeout(Some(self.idle_timeout))?;
        if !self.started {
            self.stream.write_all(
                b"HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nTransfer-Encoding: chunked\r\nCache-Control: no-store\r\nConnection: close\r\n\r\n",
            )?;
            self.stream.flush()?;
            self.started = true;
        }
        // Chunked framing is CRLF-delimited by specification; an LF-only
        // variant is not merely untidy, clients fail to parse it.
        write!(self.stream, "{:x}\r\n", frame.len())?;
        self.stream.write_all(frame.as_bytes())?;
        self.stream.write_all(b"\r\n")?;
        self.stream.flush()?;
        #[cfg(test)]
        if !self.frame_hold.is_zero() {
            thread::sleep(self.frame_hold);
        }
        Ok(())
    }

    fn finish(&mut self) -> io::Result<()> {
        if !self.started {
            return Ok(());
        }
        self.stream.write_all(b"0\r\n\r\n")?;
        self.stream.flush()?;
        let _ = self.stream.shutdown(Shutdown::Both);
        Ok(())
    }
}

fn write_response(
    stream: &mut TcpStream,
    status: u16,
    content_type: &str,
    body: &[u8],
    head_only: bool,
) -> io::Result<()> {
    write_response_with(stream, status, content_type, body, head_only, &[])
}

fn write_response_with(
    stream: &mut TcpStream,
    status: u16,
    content_type: &str,
    body: &[u8],
    head_only: bool,
    extra_headers: &[(String, String)],
) -> io::Result<()> {
    let reason = match status {
        200 => "OK",
        204 => "No Content",
        400 => "Bad Request",
        401 => "Unauthorized",
        404 => "Not Found",
        408 => "Request Timeout",
        413 => "Payload Too Large",
        429 => "Too Many Requests",
        431 => "Request Header Fields Too Large",
        499 => "Client Closed Request",
        502 => "Bad Gateway",
        503 => "Service Unavailable",
        504 => "Gateway Timeout",
        _ => "Error",
    };
    write!(
        stream,
        "HTTP/1.1 {status} {reason}\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\nCache-Control: no-store\r\nConnection: close\r\n",
        body.len()
    )?;
    // Header names here are ours, never echoed from upstream, so no folding or
    // injection check is needed beyond that invariant.
    for (name, value) in extra_headers {
        write!(stream, "{name}: {value}\r\n")?;
    }
    write!(stream, "\r\n")?;
    if !head_only {
        stream.write_all(body)?;
    }
    stream.flush()?;
    let _ = stream.shutdown(Shutdown::Both);
    Ok(())
}

fn constant_time_eq(left: &[u8], right: &[u8]) -> bool {
    if left.len() != right.len() {
        return false;
    }
    left.iter()
        .zip(right)
        .fold(0_u8, |difference, (a, b)| difference | (a ^ b))
        == 0
}

fn test_upstream_override_from_process() -> Option<String> {
    let integration_enabled =
        std::env::var_os("CLUD_INTEGRATION_TESTS").is_some_and(|value| value == "1");
    let value = std::env::var("CLUD_TEST_CODEX_BRIDGE_UPSTREAM_URL").ok();
    resolve_test_upstream_override(cfg!(debug_assertions), integration_enabled, value)
}

fn resolve_test_upstream_override(
    debug_assertions: bool,
    integration_enabled: bool,
    value: Option<String>,
) -> Option<String> {
    if debug_assertions && integration_enabled {
        value.filter(|url| !url.trim().is_empty())
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Read, Write};
    use std::net::{SocketAddr, TcpStream};
    use std::thread;
    use std::time::Duration;
    use sysinfo::{Pid, ProcessRefreshKind, ProcessesToUpdate, System};

    /// Read timeout for the test client.
    ///
    /// A backstop against a wedged bridge, not an assertion about latency, so
    /// it has to sit above the longest *legitimate* response. A transient
    /// upstream failure is retried with exponential backoff, and on a loaded CI
    /// runner that ladder overruns a tight budget -- which surfaces as EAGAIN
    /// from `read_to_string` and reads like a hang rather than the timing
    /// artefact it is. 10s sat under the ladder; this sits over it.
    const CLIENT_READ_TIMEOUT: Duration = Duration::from_secs(60);

    fn request(addr: SocketAddr, request: &str) -> String {
        let mut stream = TcpStream::connect(addr).expect("connect to bridge");
        stream.set_read_timeout(Some(CLIENT_READ_TIMEOUT)).unwrap();
        stream.write_all(request.as_bytes()).unwrap();
        stream.shutdown(std::net::Shutdown::Write).unwrap();
        let mut response = String::new();
        stream
            .read_to_string(&mut response)
            .expect("bridge response within the client read timeout");
        response
    }

    fn authorized(method: &str, path: &str, token: &str, body: &str) -> String {
        format!(
            "{method} {path} HTTP/1.1\r\nHost: 127.0.0.1\r\nAuthorization: Bearer {token}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
            body.len()
        )
    }

    fn authorized_with_headers(
        method: &str,
        path: &str,
        token: &str,
        body: &str,
        headers: &[(&str, &str)],
    ) -> String {
        let extra = headers
            .iter()
            .map(|(name, value)| format!("{name}: {value}\r\n"))
            .collect::<String>();
        format!(
            "{method} {path} HTTP/1.1\r\nHost: 127.0.0.1\r\nAuthorization: Bearer {token}\r\n{extra}Content-Length: {}\r\nConnection: close\r\n\r\n{body}",
            body.len()
        )
    }

    /// A Responses-shaped fake. Phase 2's seam passed an Anthropic body
    /// straight through, so the end-to-end tests proved transport and auth but
    /// nothing about translation; this one speaks the upstream protocol and
    /// records what it was actually sent.
    struct FakeResponses {
        base_url: String,
        requests: std::sync::Arc<Mutex<Vec<String>>>,
        shutdown: Arc<AtomicBool>,
        handle: Option<thread::JoinHandle<()>>,
    }

    impl FakeResponses {
        fn start() -> Self {
            Self::start_with_response(None)
        }

        fn start_with_response(scripted: Option<Vec<u8>>) -> Self {
            Self::start_with_scripted_responses(vec![scripted.clone()], scripted)
        }

        fn start_with_responses(scripted: Vec<Option<Vec<u8>>>) -> Self {
            Self::start_with_scripted_responses(scripted, None)
        }

        fn start_with_scripted_responses(
            scripted: Vec<Option<Vec<u8>>>,
            fallback_response: Option<Vec<u8>>,
        ) -> Self {
            let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
            listener.set_nonblocking(true).unwrap();
            let addr = listener.local_addr().unwrap();
            let requests = std::sync::Arc::new(Mutex::new(Vec::new()));
            let scripted = std::sync::Arc::new(Mutex::new(
                scripted
                    .into_iter()
                    .collect::<std::collections::VecDeque<Option<Vec<u8>>>>(),
            ));
            let shutdown = Arc::new(AtomicBool::new(false));
            let thread_requests = std::sync::Arc::clone(&requests);
            let thread_scripted = std::sync::Arc::clone(&scripted);
            let thread_fallback_response = fallback_response;
            let thread_shutdown = Arc::clone(&shutdown);
            let handle = thread::spawn(move || {
                while !thread_shutdown.load(Ordering::Acquire) {
                    let (mut upstream, _) = match listener.accept() {
                        Ok(pair) => pair,
                        Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                            thread::sleep(Duration::from_millis(5));
                            continue;
                        }
                        Err(_) => break,
                    };
                    upstream.set_nonblocking(false).unwrap();
                    upstream
                        .set_read_timeout(Some(Duration::from_secs(5)))
                        .unwrap();
                    let mut raw = Vec::new();
                    let mut byte = [0_u8; 1];
                    while !raw.windows(4).any(|window| window == b"\r\n\r\n") {
                        match upstream.read(&mut byte) {
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
                    let mut request_body = vec![0_u8; length];
                    if length > 0 {
                        let _ = upstream.read_exact(&mut request_body);
                    }
                    thread_requests
                        .lock()
                        .unwrap()
                        .push(format!("{head}{}", String::from_utf8_lossy(&request_body)));

                    let scripted_reply = thread_scripted
                        .lock()
                        .unwrap()
                        .pop_front()
                        .flatten()
                        .or_else(|| thread_fallback_response.clone());
                    if let Some(reply) = scripted_reply {
                        let _ = upstream.write_all(&reply);
                        let _ = upstream.flush();
                        let _ = upstream.shutdown(Shutdown::Both);
                        continue;
                    }
                    let events = concat!(
                        "event: response.created
data: {\"type\":\"response.created\"}

",
                        "event: response.output_text.delta
data: {\"type\":\"response.output_text.delta\",\"output_index\":0,\"content_index\":0,\"delta\":\"bridged \"}

",
                        "event: response.output_text.delta
data: {\"type\":\"response.output_text.delta\",\"output_index\":0,\"content_index\":0,\"delta\":\"reply\"}

",
                        "event: response.output_text.done
data: {\"type\":\"response.output_text.done\",\"output_index\":0,\"content_index\":0}

",
                        "event: response.completed
data: {\"type\":\"response.completed\",\"response\":{\"usage\":{\"input_tokens\":4,\"output_tokens\":2}}}

",
                    );
                    let reply = format!(
                        "HTTP/1.1 200 OK
Content-Type: text/event-stream
Content-Length: {}
Connection: close

{events}",
                        events.len()
                    );
                    let _ = upstream.write_all(reply.as_bytes());
                    let _ = upstream.flush();
                    let _ = upstream.shutdown(Shutdown::Both);
                }
            });
            Self {
                base_url: format!("http://{addr}"),
                requests,
                shutdown,
                handle: Some(handle),
            }
        }

        fn requests(&self) -> Vec<String> {
            self.requests.lock().unwrap().clone()
        }
    }

    impl Drop for FakeResponses {
        fn drop(&mut self) {
            self.shutdown.store(true, Ordering::Release);
            if let Some(handle) = self.handle.take() {
                let _ = handle.join();
            }
        }
    }

    fn response_with_events(events: &str) -> Vec<u8> {
        format!(
            "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{events}",
            events.len()
        )
        .into_bytes()
    }

    fn context_length_failure_response() -> Vec<u8> {
        response_with_events(
            "event: response.created\ndata: {\"type\":\"response.created\"}\n\n\
             event: response.failed\ndata: {\"type\":\"response.failed\",\"response\":{\"error\":{\"type\":\"invalid_request\",\"code\":\"context_length_exceeded\"}}}\n\n",
        )
    }

    fn context_length_status_response() -> Vec<u8> {
        let body = r#"{"error":{"type":"invalid_request","code":"context_length_exceeded","message":"too large"}}"#;
        format!(
            "HTTP/1.1 400 Bad Request\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
            body.len()
        )
        .into_bytes()
    }

    fn incomplete_response() -> Vec<u8> {
        response_with_events(
            "event: response.created\ndata: {\"type\":\"response.created\"}\n\n\
             event: response.output_text.delta\ndata: {\"type\":\"response.output_text.delta\",\"output_index\":0,\"content_index\":0,\"delta\":\"truncated\"}\n\n\
             event: response.incomplete\ndata: {\"type\":\"response.incomplete\",\"response\":{\"usage\":{\"input_tokens\":3,\"output_tokens\":1}}}\n\n",
        )
    }

    fn recovery_success_response() -> Vec<u8> {
        response_with_events(
            "event: response.created\ndata: {\"type\":\"response.created\"}\n\n\
             event: response.output_text.delta\ndata: {\"type\":\"response.output_text.delta\",\"output_index\":0,\"content_index\":0,\"delta\":\"recovered\"}\n\n\
             event: response.output_item.done\ndata: {\"type\":\"response.output_item.done\",\"item\":{\"type\":\"message\",\"id\":\"msg_recovered\",\"role\":\"assistant\",\"content\":[{\"type\":\"output_text\",\"text\":\"recovered\"}]}}\n\n\
             event: response.completed\ndata: {\"type\":\"response.completed\",\"response\":{\"usage\":{\"input_tokens\":3,\"output_tokens\":1}}}\n\n",
        )
    }

    fn function_call_response() -> Vec<u8> {
        response_with_events(
            "event: response.created\ndata: {\"type\":\"response.created\"}\n\n\
             event: response.output_item.added\ndata: {\"type\":\"response.output_item.added\",\"output_index\":0,\"item\":{\"type\":\"function_call\",\"call_id\":\"call_parent\",\"name\":\"Workflow\"}}\n\n\
             event: response.function_call_arguments.delta\ndata: {\"type\":\"response.function_call_arguments.delta\",\"output_index\":0,\"delta\":\"{}\"}\n\n\
             event: response.output_item.done\ndata: {\"type\":\"response.output_item.done\",\"output_index\":0,\"item\":{\"type\":\"function_call\",\"call_id\":\"call_parent\",\"name\":\"Workflow\",\"arguments\":\"{}\"}}\n\n\
             event: response.completed\ndata: {\"type\":\"response.completed\",\"response\":{\"usage\":{\"input_tokens\":3,\"output_tokens\":1}}}\n\n",
        )
    }

    fn compact_success_response() -> Vec<u8> {
        let body = r#"{"output":[{"type":"compaction","id":"cmp_recovered","encrypted_content":"opaque-summary"}]}"#;
        format!(
            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
            body.len()
        )
        .into_bytes()
    }

    /// Config wired to a fake upstream, so POST /v1/messages exercises the real
    /// translate -> upstream -> translate pipeline.
    fn bridged_config(upstream: &FakeResponses) -> BridgeConfig {
        BridgeConfig::default().with_test_upstream_url(Some(upstream.base_url.clone()))
    }

    /// The smallest request the translator accepts.
    const PROBE_BODY: &str =
        r#"{"model":"claude-x","messages":[{"role":"user","content":"hi"}],"stream":false}"#;
    const PROBE_STREAM_BODY: &str =
        r#"{"model":"claude-x","messages":[{"role":"user","content":"hi"}],"stream":true}"#;

    fn status(response: &str) -> u16 {
        response
            .split_whitespace()
            .nth(1)
            .expect("HTTP status")
            .parse()
            .unwrap()
    }

    /// Best-effort RSS snapshot for the opt-in bridge benchmark. It is a
    /// reported observation, not a test threshold: host allocators and test
    /// harnesses make an absolute memory limit flaky across target lanes.
    fn current_rss_bytes() -> u64 {
        let pid = Pid::from_u32(std::process::id());
        let mut system = System::new();
        system.refresh_processes_specifics(
            ProcessesToUpdate::Some(&[pid]),
            true,
            ProcessRefreshKind::nothing().with_memory(),
        );
        system.process(pid).map_or(0, |process| process.memory())
    }

    #[test]
    fn default_502_is_persisted_with_safe_diagnostics() {
        let secret = "sk-abcdefghijklmnopqrstuvwxyz0123456789SECRET";
        let body = format!(r#"{{"error":{{"message":"bad gateway {secret}"}}}}"#);
        let reply = format!(
            "HTTP/1.1 502 Bad Gateway\r\nContent-Type: application/json\r\ncf-ray: ray_772\r\nx-request-id: req_772\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
            body.len()
        )
        .into_bytes();
        let upstream = FakeResponses::start_with_response(Some(reply));
        let tmp = tempfile::tempdir().unwrap();
        let log_path = tmp.path().join("bridge.jsonl");
        let config = bridged_config(&upstream).with_log_path(log_path.clone());
        let mut bridge = BridgeHandle::start(config).unwrap();
        let bearer = bridge.bearer_token().to_string();
        let base_url = upstream.base_url.clone();
        let response = request(
            bridge.socket_addr(),
            &authorized("POST", "/v1/messages", bridge.bearer_token(), PROBE_BODY),
        );
        assert_eq!(status(&response), 502);
        bridge.shutdown().unwrap();

        let text = std::fs::read_to_string(log_path).unwrap();
        assert!(text.contains(r#""upstream_status":502"#), "{text}");
        assert!(text.contains(r#""class":"transient""#), "{text}");
        assert!(text.contains("ray_772"), "{text}");
        assert!(text.contains("req_772"), "{text}");
        for forbidden in [secret, bearer.as_str(), base_url.as_str(), "Authorization"] {
            assert!(!text.contains(forbidden), "leaked {forbidden:?}: {text}");
        }
    }

    #[test]
    fn successful_turn_creates_no_forensic_log() {
        let upstream = FakeResponses::start();
        let tmp = tempfile::tempdir().unwrap();
        let log_path = tmp.path().join("bridge.jsonl");
        let mut bridge =
            BridgeHandle::start(bridged_config(&upstream).with_log_path(log_path.clone())).unwrap();
        let response = request(
            bridge.socket_addr(),
            &authorized("POST", "/v1/messages", bridge.bearer_token(), PROBE_BODY),
        );
        assert_eq!(status(&response), 200);
        bridge.shutdown().unwrap();
        assert!(!log_path.exists());
    }

    #[test]
    fn bearer_rejection_log_never_contains_the_bearer() {
        let tmp = tempfile::tempdir().unwrap();
        let log_path = tmp.path().join("bridge.jsonl");
        let mut bridge =
            BridgeHandle::start(BridgeConfig::default().with_log_path(log_path.clone())).unwrap();
        let bearer = bridge.bearer_token().to_string();
        let response = request(
            bridge.socket_addr(),
            "POST /v1/messages HTTP/1.1\r\nHost: localhost\r\nContent-Length: 0\r\n\r\n",
        );
        assert_eq!(status(&response), 401);
        bridge.shutdown().unwrap();
        let text = std::fs::read_to_string(log_path).unwrap();
        assert!(text.contains(r#""reason":"bearer_mismatch""#));
        assert!(!text.contains(&bearer));
    }

    #[test]
    fn bridge_log_cap_is_visible_through_the_real_bridge() {
        let tmp = tempfile::tempdir().unwrap();
        let log_path = tmp.path().join("bridge.jsonl");
        let mut bridge = BridgeHandle::start(
            BridgeConfig::default()
                .with_log_path(log_path.clone())
                .with_log_max_bytes(150),
        )
        .unwrap();
        for _ in 0..10 {
            let _ = request(
                bridge.socket_addr(),
                "POST /nope HTTP/1.1\r\nHost: localhost\r\nContent-Length: 0\r\n\r\n",
            );
        }
        bridge.shutdown().unwrap();
        let text = std::fs::read_to_string(log_path).unwrap();
        assert!(text.contains(r#""event":"truncated""#), "{text}");
        for line in text.lines() {
            serde_json::from_str::<serde_json::Value>(line).unwrap();
        }
    }

    #[test]
    fn binds_ephemeral_loopback_ports_without_collision() {
        let first = BridgeHandle::start(BridgeConfig::default()).unwrap();
        let second = BridgeHandle::start(BridgeConfig::default()).unwrap();
        assert!(first.socket_addr().ip().is_loopback());
        assert!(second.socket_addr().ip().is_loopback());
        assert_ne!(first.socket_addr().port(), 0);
        assert_ne!(first.socket_addr(), second.socket_addr());
    }

    #[test]
    fn bearer_and_route_method_matrix_is_exact() {
        let upstream = FakeResponses::start();
        let bridge = BridgeHandle::start(bridged_config(&upstream)).unwrap();
        let addr = bridge.socket_addr();
        let token = bridge.bearer_token().to_owned();

        let missing = request(
            addr,
            "HEAD /v1/messages HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n",
        );
        assert_eq!(status(&missing), 401);
        let wrong = request(addr, &authorized("HEAD", "/v1/messages", "wrong", ""));
        assert_eq!(status(&wrong), 401);

        let head = request(addr, &authorized("HEAD", "/v1/messages", &token, ""));
        assert_eq!(status(&head), 200);
        assert!(head.ends_with("\r\n\r\n"), "HEAD must not return a body");

        let non_stream = request(
            addr,
            &authorized("POST", "/v1/messages", &token, PROBE_BODY),
        );
        assert_eq!(status(&non_stream), 200);
        assert!(non_stream.contains("application/json"));
        assert!(non_stream.contains("\"type\":\"message\""));
        assert!(non_stream.contains("\"text\":\"bridged reply\""));

        let stream = request(
            addr,
            &authorized("POST", "/v1/messages", &token, PROBE_STREAM_BODY),
        );
        assert_eq!(status(&stream), 200);
        assert!(stream.contains("text/event-stream"));
        assert!(stream.contains("event: message_start\ndata:"));
        assert!(stream.contains("event: message_stop"));
        assert!(!stream.contains(r#"message_start\ndata:"#));

        let count = request(
            addr,
            &authorized("POST", "/v1/messages/count_tokens", &token, "{}"),
        );
        assert_eq!(status(&count), 404);
        assert!(count.contains("not supported"));
        for (method, path) in [
            ("GET", "/v1/messages"),
            ("POST", "/unknown"),
            ("HEAD", "/unknown"),
        ] {
            let response = request(addr, &authorized(method, path, &token, ""));
            assert_eq!(status(&response), 404, "{method} {path}");
        }
    }

    #[test]
    fn transport_context_failure_compacts_and_retries_once() {
        let upstream = FakeResponses::start_with_responses(vec![
            None,
            Some(context_length_status_response()),
            Some(compact_success_response()),
            None,
        ]);
        let bridge = BridgeHandle::start(bridged_config(&upstream)).unwrap();
        let first = request(
            bridge.socket_addr(),
            &authorized("POST", "/v1/messages", bridge.bearer_token(), PROBE_BODY),
        );
        assert_eq!(status(&first), 200, "{first}");
        let continuation = r#"{"model":"claude-x","messages":[{"role":"user","content":"first"},{"role":"assistant","content":"done"},{"role":"user","content":"pending"}],"stream":false}"#;
        let recovered = request(
            bridge.socket_addr(),
            &authorized("POST", "/v1/messages", bridge.bearer_token(), continuation),
        );
        assert_eq!(status(&recovered), 200, "{recovered}");
        assert!(recovered.contains("bridged reply"), "{recovered}");
        let requests = upstream.requests();
        assert_eq!(
            requests
                .iter()
                .filter(|request| request.starts_with("POST /v1/responses/compact HTTP/1.1"))
                .count(),
            1,
            "{requests:#?}"
        );
        assert_eq!(
            requests
                .iter()
                .filter(|request| request.starts_with("POST /v1/responses HTTP/1.1"))
                .count(),
            3,
            "the transport error must be one bounded recovery cycle: {requests:#?}"
        );
    }

    #[test]
    fn authenticated_lifecycle_hook_payloads_compact_and_clear_the_bridge_session() {
        let upstream = FakeResponses::start_with_responses(vec![
            None,
            Some(compact_success_response()),
            None,
            None,
        ]);
        let bridge = BridgeHandle::start(bridged_config(&upstream)).unwrap();
        let addr = bridge.socket_addr();
        let token = bridge.bearer_token().to_owned();
        let first = request(
            addr,
            &authorized("POST", "/v1/messages", &token, PROBE_BODY),
        );
        assert_eq!(status(&first), 200, "{first}");

        let mismatched = request(
            addr,
            &authorized(
                "POST",
                "/_clud/context/compact",
                &token,
                r#"{"hook_event_name":"SessionStart","source":"clear"}"#,
            ),
        );
        assert_eq!(status(&mismatched), 400, "{mismatched}");

        let compact = request(
            addr,
            &authorized(
                "POST",
                "/_clud/context/compact",
                &token,
                r#"{"hook_event_name":"PreCompact","trigger":"manual"}"#,
            ),
        );
        assert_eq!(status(&compact), 204, "{compact}");
        let compact_requests = upstream.requests();
        assert_eq!(
            compact_requests
                .iter()
                .filter(|request| request.starts_with("POST /v1/responses/compact HTTP/1.1"))
                .count(),
            1,
            "{compact_requests:#?}"
        );

        let wrong = request(
            addr,
            &authorized("POST", "/_clud/context/clear", "wrong", ""),
        );
        assert_eq!(status(&wrong), 401, "{wrong}");
        let clear = request(
            addr,
            &authorized(
                "POST",
                "/_clud/context/clear",
                &token,
                r#"{"hook_event_name":"SessionStart","source":"clear"}"#,
            ),
        );
        assert_eq!(status(&clear), 204, "{clear}");
        let fresh = request(
            addr,
            &authorized(
                "POST",
                "/v1/messages",
                &token,
                r#"{"model":"claude-x","messages":[{"role":"user","content":"fresh"}],"stream":false}"#,
            ),
        );
        assert_eq!(status(&fresh), 200, "{fresh}");
        let requests = upstream.requests();
        let post_clear = requests
            .iter()
            .filter(|request| request.starts_with("POST /v1/responses HTTP/1.1"))
            .nth(1)
            .expect("post-clear inference request");
        assert!(post_clear.contains("fresh"), "{post_clear}");
        assert!(
            !post_clear.contains("hi"),
            "stale pre-clear input leaked: {post_clear}"
        );
    }

    #[test]
    /// Admission/concurrency behaviour moved to its own tests when queueing
    /// replaced immediate rejection; this one owns the size and time bounds.
    fn enforces_body_header_and_timeout_bounds() {
        let config = BridgeConfig {
            max_body_bytes: 64,
            max_header_bytes: 256,
            header_timeout: Duration::from_millis(100),
            max_concurrency: 1,
            ..BridgeConfig::default()
        };
        let bridge = BridgeHandle::start(config).unwrap();
        let addr = bridge.socket_addr();
        let token = bridge.bearer_token().to_owned();

        let oversized_body = request(
            addr,
            &format!(
                "POST /v1/messages HTTP/1.1\r\nAuthorization: Bearer {token}\r\nContent-Length: 65\r\nConnection: close\r\n\r\n"
            ),
        );
        assert_eq!(status(&oversized_body), 413);

        let large_header = "x".repeat(300);
        let oversized_header = request(
            addr,
            &format!("GET / HTTP/1.1\r\nX-Large: {large_header}\r\n\r\n"),
        );
        assert_eq!(status(&oversized_header), 431);

        let mut slow = TcpStream::connect(addr).unwrap();
        slow.set_read_timeout(Some(Duration::from_secs(1))).unwrap();
        slow.write_all(b"GET / HTTP/1.1\r\nHost: local").unwrap();
        thread::sleep(Duration::from_millis(175));
        let mut timeout_response = String::new();
        slow.read_to_string(&mut timeout_response).unwrap();
        assert_eq!(status(&timeout_response), 408);

        let (admitted_tx, admitted_rx) = std::sync::mpsc::sync_channel(1);
        let bridge = BridgeHandle::start(
            BridgeConfig {
                header_timeout: Duration::from_millis(150),
                ..BridgeConfig::default()
            }
            .with_admission_notifier(admitted_tx),
        )
        .unwrap();
        let mut dripping = TcpStream::connect(bridge.socket_addr()).unwrap();
        dripping
            .set_read_timeout(Some(Duration::from_secs(4)))
            .unwrap();
        dripping.write_all(b"G").unwrap();
        admitted_rx
            .recv_timeout(Duration::from_secs(30))
            .expect("slow-drip request worker admission signal");
        let mut drip_writer = dripping.try_clone().unwrap();
        let (drip_started_tx, drip_started_rx) = std::sync::mpsc::sync_channel(1);
        let writer = thread::spawn(move || {
            for (index, byte) in b"ET / HTTP/1.1\r\nHost: slow-drip"
                .iter()
                .cycle()
                .take(60)
                .enumerate()
            {
                if drip_writer.write_all(&[*byte]).is_err() {
                    break;
                }
                if index == 0 {
                    let _ = drip_started_tx.send(());
                }
                thread::sleep(Duration::from_millis(50));
            }
        });
        drip_started_rx
            .recv_timeout(Duration::from_secs(30))
            .expect("slow-drip writer start signal");
        let started = Instant::now();
        let mut absolute_timeout_response = String::new();
        let read_result = dripping.read_to_string(&mut absolute_timeout_response);
        if let Err(error) = read_result {
            assert!(matches!(
                error.kind(),
                io::ErrorKind::ConnectionAborted | io::ErrorKind::ConnectionReset
            ));
        }
        if !absolute_timeout_response.is_empty() {
            assert_eq!(status(&absolute_timeout_response), 408);
        }
        assert!(started.elapsed() < Duration::from_secs(2));
        drop(dripping);
        writer.join().unwrap();
    }

    #[test]
    fn shutdown_and_drop_are_idempotent_and_close_the_listener() {
        let (admitted_tx, admitted_rx) = std::sync::mpsc::sync_channel(1);
        let mut bridge = BridgeHandle::start(
            BridgeConfig {
                header_timeout: Duration::from_secs(10),
                ..BridgeConfig::default()
            }
            .with_admission_notifier(admitted_tx),
        )
        .unwrap();
        let addr = bridge.socket_addr();
        let mut slow_drip = TcpStream::connect(addr).unwrap();
        slow_drip
            .write_all(b"GET / HTTP/1.1\r\nHost: local")
            .unwrap();
        admitted_rx
            .recv_timeout(Duration::from_secs(30))
            .expect("request worker admission signal");
        assert_eq!(bridge.active_requests(), 1);
        let shutdown_started = Instant::now();
        bridge.shutdown().unwrap();
        assert!(shutdown_started.elapsed() < Duration::from_secs(5));
        bridge.shutdown().unwrap();
        assert!(TcpStream::connect(addr).is_err());

        let addr_after_drop = {
            let bridge = BridgeHandle::start(BridgeConfig::default()).unwrap();
            bridge.socket_addr()
        };
        assert!(TcpStream::connect(addr_after_drop).is_err());
    }

    #[test]
    fn diagnostics_redact_token_base_url_and_test_upstream() {
        let config = BridgeConfig::default().with_test_upstream_url(Some(
            "http://127.0.0.1:45678/private-test-upstream".to_string(),
        ));
        let bridge = BridgeHandle::start(config.clone()).unwrap();
        let rendered_config = format!("{config:?}");
        let rendered_handle = format!("{bridge:?}");
        for secret in [
            bridge.bearer_token(),
            bridge.base_url(),
            "http://127.0.0.1:45678/private-test-upstream",
        ] {
            assert!(!rendered_config.contains(secret));
            assert!(!rendered_handle.contains(secret));
        }
    }

    #[test]
    fn test_upstream_override_requires_debug_and_integration_gate() {
        let value = Some("http://127.0.0.1:45678".to_string());
        assert_eq!(
            resolve_test_upstream_override(true, true, value.clone()),
            value
        );
        assert_eq!(
            resolve_test_upstream_override(false, true, value.clone()),
            None
        );
        assert_eq!(resolve_test_upstream_override(true, false, value), None);
    }

    /// Split a chunked response into its decoded body. Deliberately strict:
    /// a malformed chunk header panics rather than silently returning a short
    /// body, so a framing regression fails loudly.
    fn decode_chunked_body(response: &str) -> String {
        let (headers, mut rest) = response.split_once("\r\n\r\n").expect("header terminator");
        assert!(
            headers.contains("Transfer-Encoding: chunked"),
            "streamed response must be chunked, got headers: {headers}"
        );
        assert!(
            !headers.contains("Content-Length"),
            "a streamed response cannot carry Content-Length: {headers}"
        );
        let mut body = String::new();
        loop {
            let (size_line, remainder) = rest.split_once("\r\n").expect("chunk size line");
            let size = usize::from_str_radix(size_line.trim(), 16).expect("hex chunk size");
            if size == 0 {
                break;
            }
            let (chunk, remainder) = remainder.split_at(size);
            body.push_str(chunk);
            rest = remainder
                .strip_prefix("\r\n")
                .expect("chunk data must be CRLF terminated");
        }
        body
    }

    #[test]
    fn streamed_responses_are_chunked_and_flushed_frame_by_frame() {
        let frame_hold = Duration::from_millis(120);
        let upstream = FakeResponses::start();
        let bridge =
            BridgeHandle::start(bridged_config(&upstream).with_frame_hold(frame_hold)).unwrap();
        let addr = bridge.socket_addr();
        let token = bridge.bearer_token().to_owned();

        let mut stream = TcpStream::connect(addr).unwrap();
        stream
            .set_read_timeout(Some(Duration::from_secs(10)))
            .unwrap();
        stream
            .write_all(authorized("POST", "/v1/messages", &token, PROBE_STREAM_BODY).as_bytes())
            .unwrap();

        // Read incrementally and timestamp the first frame against completion.
        // A single buffered write would make these two instants identical.
        let started = Instant::now();
        let mut raw = Vec::new();
        let mut chunk = [0_u8; 512];
        let mut first_frame_at = None;
        loop {
            let count = stream.read(&mut chunk).unwrap();
            if count == 0 {
                break;
            }
            raw.extend_from_slice(&chunk[..count]);
            if first_frame_at.is_none()
                && String::from_utf8_lossy(&raw).contains("\"type\":\"message_start\"")
            {
                first_frame_at = Some(started.elapsed());
            }
        }
        let completed_at = started.elapsed();
        let first_frame_at = first_frame_at.expect("message_start frame never arrived");

        let response = String::from_utf8(raw).expect("utf-8 response");
        assert_eq!(status(&response), 200);
        assert!(response.contains("text/event-stream"));

        // The whole point of the phase: usable output well before the end.
        assert!(
            completed_at >= first_frame_at + frame_hold * 2,
            "stream was not progressive: first frame at {first_frame_at:?}, done at {completed_at:?}"
        );

        let body = decode_chunked_body(&response);
        // Translated upstream content, not a canned fixture.
        assert!(body.contains(r#""text":"bridged "#));
        assert!(body.contains("event: content_block_stop"));
        assert!(body.starts_with("event: message_start\ndata:"));
        assert!(body.ends_with("\n\n"));
        assert!(body.contains("event: message_stop"));
    }

    #[test]
    fn first_frame_timeout_returns_a_pre_commit_gateway_error() {
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
        let upstream_url = format!("http://{}", listener.local_addr().unwrap());
        let upstream = thread::spawn(move || {
            for _ in 0..4 {
                let (mut stream, _) = listener.accept().unwrap();
                let mut request = [0_u8; 1024];
                let _ = stream.read(&mut request);
                thread::sleep(Duration::from_millis(80));
            }
        });
        let bridge = BridgeHandle::start(
            BridgeConfig {
                first_frame_timeout: Duration::from_millis(20),
                ..BridgeConfig::default()
            }
            .with_test_upstream_url(Some(upstream_url)),
        )
        .unwrap();
        let response = request(
            bridge.socket_addr(),
            &authorized("POST", "/v1/messages", bridge.bearer_token(), PROBE_BODY),
        );

        assert_eq!(status(&response), 504);
        assert!(!response.contains("text/event-stream"));
        upstream.join().unwrap();
    }

    #[test]
    fn header_body_and_stream_timeouts_are_independent() {
        // Header budget is short; body budget is long. A client that sends its
        // headers promptly and its body slowly must succeed -- under the single
        // whole-connection deadline this replaced, it returned 408.
        let upstream = FakeResponses::start();
        let bridge = BridgeHandle::start(BridgeConfig {
            header_timeout: Duration::from_millis(300),
            body_timeout: Duration::from_secs(5),
            ..bridged_config(&upstream)
        })
        .unwrap();
        let token = bridge.bearer_token().to_owned();
        let body = PROBE_BODY;

        let mut stream = TcpStream::connect(bridge.socket_addr()).unwrap();
        stream
            .set_read_timeout(Some(Duration::from_secs(10)))
            .unwrap();
        stream
            .write_all(
                format!(
                    "POST /v1/messages HTTP/1.1\r\nHost: 127.0.0.1\r\nAuthorization: Bearer {token}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                    body.len()
                )
                .as_bytes(),
            )
            .unwrap();
        stream.flush().unwrap();

        // Longer than the header budget, comfortably inside the body budget.
        thread::sleep(Duration::from_millis(600));
        stream.write_all(body.as_bytes()).unwrap();
        stream.flush().unwrap();

        let mut raw = Vec::new();
        let mut chunk = [0_u8; 512];
        loop {
            match stream.read(&mut chunk) {
                Ok(0) => break,
                Ok(count) => raw.extend_from_slice(&chunk[..count]),
                Err(error) => panic!("read failed after {} bytes: {error}", raw.len()),
            }
        }
        let response = String::from_utf8_lossy(&raw).into_owned();
        assert_eq!(
            status(&response),
            200,
            "slow body must not be charged the header budget: {response}"
        );
        assert!(response.contains("bridged reply"));
    }

    /// Regression guard: a body split across many segments must be reassembled.
    /// Accepted sockets inherit the listener's non-blocking mode on Windows,
    /// which made every read that outran the client's next segment look like a
    /// timeout and answered 408. Real Claude requests carrying a transcript or
    /// an image always span segments, so this is the shape that matters.
    #[test]
    fn bodies_split_across_segments_are_reassembled() {
        let upstream = FakeResponses::start();
        let bridge = BridgeHandle::start(bridged_config(&upstream)).unwrap();
        let token = bridge.bearer_token().to_owned();
        let filler = "x".repeat(4096);
        let body = format!(
            r#"{{"model":"claude-x","messages":[{{"role":"user","content":"hi"}}],"stream":false,"note":"{filler}"}}"#
        );

        let mut stream = TcpStream::connect(bridge.socket_addr()).unwrap();
        stream
            .set_read_timeout(Some(Duration::from_secs(10)))
            .unwrap();
        stream
            .write_all(
                format!(
                    "POST /v1/messages HTTP/1.1\r\nHost: 127.0.0.1\r\nAuthorization: Bearer {token}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                    body.len()
                )
                .as_bytes(),
            )
            .unwrap();
        stream.flush().unwrap();

        for segment in body.as_bytes().chunks(512) {
            thread::sleep(Duration::from_millis(20));
            stream.write_all(segment).unwrap();
            stream.flush().unwrap();
        }

        let mut response = String::new();
        stream.read_to_string(&mut response).unwrap();
        assert_eq!(
            status(&response),
            200,
            "segmented body must not read as a timeout: {response}"
        );
        assert!(response.contains("bridged reply"));
    }

    /// Justifies `DEFAULT_MAX_BODY_BYTES`.
    ///
    /// This is a *constructed* representative request, not captured production
    /// traffic -- stated plainly because the distinction matters. It is built
    /// from the parts a real Claude Code turn always carries: a system prompt,
    /// a set of tool definitions with JSON Schemas, a multi-turn transcript,
    /// and one screenshot. The screenshot alone is what breaks the fixture-era
    /// cap: base64 inflates bytes by 4/3, so even a modest image clears 1 MiB
    /// before any text is counted.
    #[test]
    fn a_representative_request_fits_the_body_cap() {
        let system = "You are Claude Code. ".repeat(500);
        let tools: Vec<serde_json::Value> = (0..15)
            .map(|index| {
                serde_json::json!({
                    "name": format!("tool_{index}"),
                    "description": "A tool with a realistic schema. ".repeat(20),
                    "input_schema": {
                        "type": "object",
                        "properties": {
                            "path": {"type": "string", "description": "x".repeat(200)},
                            "content": {"type": "string", "description": "y".repeat(200)},
                        },
                        "required": ["path"],
                    },
                })
            })
            .collect();
        let mut messages: Vec<serde_json::Value> = (0..20)
            .map(|index| {
                serde_json::json!({
                    "role": if index % 2 == 0 { "user" } else { "assistant" },
                    "content": format!("turn {index}: {}", "transcript text ".repeat(120)),
                })
            })
            .collect();
        // One 1-megapixel screenshot. 3 bytes per pixel, then base64's 4/3.
        let screenshot = "A".repeat(1024 * 1024 * 3 / 3 * 4);
        messages.push(serde_json::json!({
            "role": "user",
            "content": [{
                "type": "image",
                "source": {"type": "base64", "media_type": "image/png", "data": screenshot},
            }],
        }));
        let request = serde_json::json!({
            "model": "claude-sonnet-5",
            "max_tokens": 8192,
            "system": system,
            "tools": tools,
            "messages": messages,
            "stream": true,
        });
        let encoded = serde_json::to_vec(&request).unwrap();

        const FIXTURE_ERA_CAP: usize = 1024 * 1024;
        assert!(
            encoded.len() > FIXTURE_ERA_CAP,
            "the old 1 MiB cap was meant to be the thing this outgrows; got {} bytes",
            encoded.len()
        );
        assert!(
            encoded.len() < DEFAULT_MAX_BODY_BYTES,
            "representative request ({} bytes) must fit the configured cap ({DEFAULT_MAX_BODY_BYTES})",
            encoded.len()
        );
        // It must also translate: a cap that admits a request the translator
        // then refuses would not have bought anything.
        crate::codex_translate::translate_bytes(
            &encoded,
            &crate::codex_translate::TranslateOptions::default(),
        )
        .expect("a representative request must translate");
    }

    /// Justifies the admission policy: concurrent requests beyond the bound
    /// wait in the listen backlog instead of collecting a 503.
    #[test]
    fn concurrent_requests_beyond_the_bound_queue_rather_than_fail() {
        let upstream = FakeResponses::start();
        let bridge = BridgeHandle::start(BridgeConfig {
            max_concurrency: 1,
            admission_wait: Duration::from_secs(20),
            ..bridged_config(&upstream)
        })
        .unwrap();
        let addr = bridge.socket_addr();
        let token = bridge.bearer_token().to_owned();

        let handles: Vec<_> = (0..4)
            .map(|_| {
                let token = token.clone();
                thread::spawn(move || {
                    request(
                        addr,
                        &authorized("POST", "/v1/messages", &token, PROBE_BODY),
                    )
                })
            })
            .collect();

        for handle in handles {
            let response = handle.join().expect("request thread");
            assert_eq!(
                status(&response),
                200,
                "a queued request must not be answered 503: {response}"
            );
            assert!(response.contains("bridged reply"));
        }
    }

    /// Opt-in, repeatable bridge overhead report for #630. Run this locally
    /// on a quiet machine; it deliberately has no wall-clock assertion.
    #[test]
    #[ignore = "opt-in benchmark; see bench/codex_bridge/README.md"]
    fn benchmark_loopback_bridge_overhead() {
        const REQUESTS: usize = 200;
        let upstream = FakeResponses::start();
        let mut bridge = BridgeHandle::start(bridged_config(&upstream)).unwrap();
        let addr = bridge.socket_addr();
        let token = bridge.bearer_token().to_owned();
        let rss_before = current_rss_bytes();
        let started = Instant::now();

        for _ in 0..REQUESTS {
            let response = request(
                addr,
                &authorized("POST", "/v1/messages", &token, PROBE_BODY),
            );
            assert_eq!(
                status(&response),
                200,
                "benchmark request failed: {response}"
            );
        }
        bridge.shutdown().unwrap();
        let elapsed = started.elapsed();
        let rss_after = current_rss_bytes();
        println!(
            "{}",
            serde_json::json!({
                "schema": "clud.bench.codex_bridge.v1",
                "requests": REQUESTS,
                "elapsed_ms": elapsed.as_millis(),
                "requests_per_second": REQUESTS as f64 / elapsed.as_secs_f64(),
                "rss_before_bytes": rss_before,
                "rss_after_bytes": rss_after,
                "rss_growth_bytes": rss_after.saturating_sub(rss_before),
                "os": std::env::consts::OS,
                "arch": std::env::consts::ARCH,
            })
        );
    }

    /// The backstop still exists: once `admission_wait` elapses, a client gets
    /// a definite answer rather than waiting forever behind a wedged worker.
    #[test]
    fn a_saturated_bridge_still_answers_after_the_admission_wait() {
        let (admitted_tx, admitted_rx) = std::sync::mpsc::sync_channel(1);
        let bridge = BridgeHandle::start(
            BridgeConfig {
                max_concurrency: 1,
                admission_wait: Duration::from_millis(50),
                header_timeout: Duration::from_secs(5),
                ..BridgeConfig::default()
            }
            .with_request_hold(Duration::from_secs(2))
            .with_admission_notifier(admitted_tx),
        )
        .unwrap();
        let addr = bridge.socket_addr();
        let token = bridge.bearer_token().to_owned();

        let mut occupied = TcpStream::connect(addr).unwrap();
        occupied
            .write_all(authorized("HEAD", "/v1/messages", &token, "").as_bytes())
            .unwrap();
        admitted_rx
            .recv_timeout(Duration::from_secs(30))
            .expect("first request admitted");

        let started = Instant::now();
        let saturated = request(addr, &authorized("HEAD", "/v1/messages", &token, ""));
        assert_eq!(status(&saturated), 503);
        assert!(
            started.elapsed() >= Duration::from_millis(50),
            "the 503 must come only after the admission wait"
        );
        drop(occupied);
    }

    /// Regression guard: Claude Code sends `POST /v1/messages?beta=true`.
    /// Matching the raw request target 404s a valid request -- and the mock
    /// probe never caught it because it sends a bare path. Found only by
    /// running a real client against the bridge.
    #[test]
    fn a_query_string_does_not_change_the_route() {
        let upstream = FakeResponses::start();
        let bridge = BridgeHandle::start(bridged_config(&upstream)).unwrap();
        let token = bridge.bearer_token().to_owned();
        for path in [
            "/v1/messages",
            "/v1/messages?beta=true",
            "/v1/messages?beta=true&foo=bar",
        ] {
            let response = request(
                bridge.socket_addr(),
                &authorized("POST", path, &token, PROBE_BODY),
            );
            assert_eq!(status(&response), 200, "path {path}: {response}");
            assert!(response.contains("bridged reply"), "path {path}");
        }
        // Unknown routes still 404, query string or not.
        let unknown = request(
            bridge.socket_addr(),
            &authorized("POST", "/v1/unknown?beta=true", &token, PROBE_BODY),
        );
        assert_eq!(status(&unknown), 404);
    }

    #[test]
    fn the_debug_seam_speaks_the_responses_protocol_and_carries_no_downstream_secret() {
        // Phase 2's seam passed the Anthropic body through unchanged, so the
        // end-to-end tests proved transport and auth but nothing about
        // translation. Assert on what the fake actually receives, so a
        // translation regression fails here.
        let upstream = FakeResponses::start();
        let bridge = BridgeHandle::start(bridged_config(&upstream)).unwrap();
        let response = request(
            bridge.socket_addr(),
            &authorized("POST", "/v1/messages", bridge.bearer_token(), PROBE_BODY),
        );
        assert_eq!(status(&response), 200);
        assert!(response.contains(r#""text":"bridged reply""#));

        let sent = upstream.requests().remove(0);
        assert!(
            sent.starts_with("POST /v1/responses HTTP/1.1"),
            "the bridge must address the Responses endpoint: {sent}"
        );
        assert!(sent.contains("Authorization: Bearer clud-test-upstream-key"));
        // Translated shape, not a passed-through Anthropic body.
        let body = sent.split("\r\n\r\n").nth(1).expect("upstream body");
        let json: serde_json::Value = serde_json::from_str(body).expect("JSON body");
        assert_eq!(json["model"], "gpt-5.6-terra");
        assert_eq!(json["stream"], true, "upstream is always streamed");
        assert_eq!(json["input"][0]["content"][0]["type"], "input_text");
        assert!(json.get("messages").is_none(), "Anthropic shape leaked");

        // The harness's own downstream bearer must never travel upstream.
        assert!(!sent.contains(bridge.bearer_token()));
    }

    /// What a user typing `/model luna@high` actually produces on the wire.
    ///
    /// The premise this rests on is that the harness forwards the model
    /// string unvalidated behind a custom `ANTHROPIC_BASE_URL`; this test
    /// pins our half of that contract — whatever arrives in the request is
    /// parsed, expanded, and sent, with the suffix landing in
    /// `reasoning.effort` rather than in the model id.
    #[test]
    fn a_request_selecting_a_model_and_effort_reaches_upstream_expanded() {
        let upstream = FakeResponses::start();
        let bridge = BridgeHandle::start(bridged_config(&upstream)).unwrap();
        let response = request(
            bridge.socket_addr(),
            &authorized(
                "POST",
                "/v1/messages",
                bridge.bearer_token(),
                r#"{"model":"luna@high","messages":[{"role":"user","content":"hi"}],"stream":false}"#,
            ),
        );
        assert_eq!(status(&response), 200);

        let sent = upstream.requests().remove(0);
        let body = sent.split("\r\n\r\n").nth(1).expect("upstream body");
        let json: serde_json::Value = serde_json::from_str(body).expect("JSON body");
        assert_eq!(json["model"], "gpt-5.6-luna");
        assert_eq!(json["reasoning"]["effort"], "high");
        assert!(
            !body.contains("luna@high"),
            "the suffix must not travel as part of the model id: {body}"
        );
    }

    /// `/effort minimal` must fail at the bridge boundary rather than run at
    /// a different effort (#821).
    ///
    /// `minimal` is a real Responses value that gpt-5.6 does not accept, so
    /// it is the one spelling a user can plausibly type and have silently
    /// downgraded. Nothing may reach upstream, and the client must be told
    /// what is accepted.
    #[test]
    fn an_unsupported_output_config_effort_is_rejected_before_upstream() {
        let upstream = FakeResponses::start();
        let bridge = BridgeHandle::start(bridged_config(&upstream)).unwrap();
        let response = request(
            bridge.socket_addr(),
            &authorized(
                "POST",
                "/v1/messages",
                bridge.bearer_token(),
                r#"{"model":"gpt-5.6-terra","messages":[{"role":"user","content":"hi"}],"output_config":{"effort":"minimal"},"stream":false}"#,
            ),
        );

        assert_eq!(status(&response), 400);
        assert!(response.contains("minimal"), "{response}");
        assert!(response.contains("xhigh"), "{response}");
        assert!(
            upstream.requests().is_empty(),
            "a rejected effort must not be billed upstream"
        );
    }

    /// A launch-time `--model` selection is the default for requests that do
    /// not name one — which is every request the harness sends, since it
    /// sends its own `claude-*` id.
    #[test]
    fn the_launch_time_selection_becomes_the_default_for_claude_ids() {
        let upstream = FakeResponses::start();
        let bridge = BridgeHandle::start(
            bridged_config(&upstream)
                .with_default_model(Some(ModelSpec::parse("sol@xhigh").unwrap())),
        )
        .unwrap();
        let response = request(
            bridge.socket_addr(),
            &authorized("POST", "/v1/messages", bridge.bearer_token(), PROBE_BODY),
        );
        assert_eq!(status(&response), 200);

        let sent = upstream.requests().remove(0);
        let body = sent.split("\r\n\r\n").nth(1).expect("upstream body");
        let json: serde_json::Value = serde_json::from_str(body).expect("JSON body");
        assert_eq!(json["model"], "gpt-5.6-sol");
        assert_eq!(json["reasoning"]["effort"], "xhigh");
    }

    #[test]
    fn pre_output_context_failure_compacts_canonical_history_and_retries_once() {
        let upstream = FakeResponses::start_with_responses(vec![
            None,
            Some(context_length_failure_response()),
            Some(compact_success_response()),
            Some(recovery_success_response()),
        ]);
        let bridge = BridgeHandle::start(bridged_config(&upstream)).unwrap();
        let first = request(
            bridge.socket_addr(),
            &authorized(
                "POST",
                "/v1/messages",
                bridge.bearer_token(),
                PROBE_STREAM_BODY,
            ),
        );
        assert_eq!(status(&first), 200, "{first}");

        let second = request(
            bridge.socket_addr(),
            &authorized(
                "POST",
                "/v1/messages",
                bridge.bearer_token(),
                r#"{"model":"claude-x","messages":[{"role":"user","content":"first"},{"role":"assistant","content":"bridged reply"},{"role":"user","content":"pending"}],"stream":true}"#,
            ),
        );
        assert_eq!(status(&second), 200, "{second}");
        let body = decode_chunked_body(&second);
        assert_eq!(body.matches("event: message_start").count(), 1, "{body}");
        assert_eq!(body.matches("recovered").count(), 1, "{body}");
        assert!(!body.contains("context is too long"), "{body}");
        assert!(!body.contains("event: error"), "{body}");

        let requests = upstream.requests();
        assert_eq!(
            requests
                .iter()
                .filter(|request| request.starts_with("POST /v1/responses HTTP/1.1"))
                .count(),
            3,
            "one initial turn plus exactly two ordinary attempts: {requests:#?}"
        );
        assert_eq!(
            requests
                .iter()
                .filter(|request| request.starts_with("POST /v1/responses/compact HTTP/1.1"))
                .count(),
            1,
            "exactly one compact request: {requests:#?}"
        );
        let compact = requests
            .iter()
            .find(|request| request.starts_with("POST /v1/responses/compact HTTP/1.1"))
            .expect("compact request");
        let compact_body: serde_json::Value =
            serde_json::from_str(compact.split("\r\n\r\n").nth(1).expect("compact body"))
                .expect("compact JSON");
        let compact_text: Vec<_> = compact_body["input"]
            .as_array()
            .unwrap()
            .iter()
            .flat_map(|item| {
                item.pointer("/content")
                    .and_then(serde_json::Value::as_array)
                    .into_iter()
                    .flatten()
                    .filter_map(|part| part.get("text").and_then(serde_json::Value::as_str))
            })
            .collect();
        assert!(compact_text.contains(&"hi"));
        assert!(!compact_text.contains(&"pending"));
        let retry = requests
            .iter()
            .rfind(|request| request.starts_with("POST /v1/responses HTTP/1.1"))
            .expect("retry request");
        let retry_body: serde_json::Value =
            serde_json::from_str(retry.split("\r\n\r\n").nth(1).expect("retry body"))
                .expect("retry JSON");
        assert_eq!(retry_body["input"][0]["type"], "compaction");
        assert_eq!(
            retry_body["input"][0]["encrypted_content"],
            "opaque-summary"
        );
        assert_eq!(retry_body["input"][1]["content"][0]["text"], "pending");
    }

    #[test]
    fn workflow_child_does_not_replay_parent_function_call() {
        let upstream =
            FakeResponses::start_with_responses(vec![Some(function_call_response()), None]);
        let bridge = BridgeHandle::start(bridged_config(&upstream)).unwrap();
        let session = [("X-Claude-Code-Session-Id", "session-parent-child")];
        let parent = request(
            bridge.socket_addr(),
            &authorized_with_headers(
                "POST",
                "/v1/messages",
                bridge.bearer_token(),
                PROBE_BODY,
                &session,
            ),
        );
        assert_eq!(status(&parent), 200, "{parent}");

        let mut child_headers = session.to_vec();
        child_headers.push(("x-claude-code-agent-id", "agent-child"));
        let child = request(
            bridge.socket_addr(),
            &authorized_with_headers(
                "POST",
                "/v1/messages",
                bridge.bearer_token(),
                r#"{"model":"claude-x","messages":[{"role":"user","content":"child"}],"stream":false}"#,
                &child_headers,
            ),
        );
        assert_eq!(status(&child), 200, "{child}");

        let requests = upstream.requests();
        let child_body: serde_json::Value = serde_json::from_str(
            requests
                .iter()
                .filter(|request| request.starts_with("POST /v1/responses HTTP/1.1"))
                .nth(1)
                .expect("child upstream request")
                .split("\r\n\r\n")
                .nth(1)
                .expect("child request body"),
        )
        .expect("child JSON body");
        assert!(
            !child_body["input"]
                .as_array()
                .unwrap()
                .iter()
                .any(|item| item["type"] == "function_call"),
            "child replayed the parent function call: {child_body}"
        );
    }

    #[test]
    fn second_context_failure_is_terminal_and_a_later_turn_still_works() {
        let upstream = FakeResponses::start_with_responses(vec![
            None,
            Some(context_length_failure_response()),
            Some(compact_success_response()),
            Some(context_length_failure_response()),
            Some(recovery_success_response()),
        ]);
        let bridge = BridgeHandle::start(bridged_config(&upstream)).unwrap();
        let first = request(
            bridge.socket_addr(),
            &authorized(
                "POST",
                "/v1/messages",
                bridge.bearer_token(),
                PROBE_STREAM_BODY,
            ),
        );
        assert_eq!(status(&first), 200, "{first}");
        let failed = request(
            bridge.socket_addr(),
            &authorized(
                "POST",
                "/v1/messages",
                bridge.bearer_token(),
                r#"{"model":"claude-x","messages":[{"role":"user","content":"first"},{"role":"assistant","content":"bridged reply"},{"role":"user","content":"pending"}],"stream":true}"#,
            ),
        );
        assert_eq!(status(&failed), 200, "{failed}");
        let failed_body = decode_chunked_body(&failed);
        assert_eq!(
            failed_body.matches("event: message_start").count(),
            1,
            "{failed_body}"
        );
        assert_eq!(
            failed_body.matches("context is too long").count(),
            1,
            "{failed_body}"
        );
        assert!(!failed_body.contains("recovered"), "{failed_body}");

        let later = request(
            bridge.socket_addr(),
            &authorized(
                "POST",
                "/v1/messages",
                bridge.bearer_token(),
                r#"{"model":"claude-x","messages":[{"role":"user","content":"independent"}],"stream":true}"#,
            ),
        );
        assert_eq!(status(&later), 200, "{later}");
        assert!(decode_chunked_body(&later).contains("recovered"), "{later}");
        let requests = upstream.requests();
        assert_eq!(
            requests
                .iter()
                .filter(|request| request.starts_with("POST /v1/responses/compact HTTP/1.1"))
                .count(),
            1,
            "the second context failure must not compact again: {requests:#?}"
        );
        assert_eq!(
            requests
                .iter()
                .filter(|request| request.starts_with("POST /v1/responses HTTP/1.1"))
                .count(),
            4,
            "one first turn, two bounded attempts, then one independent turn: {requests:#?}"
        );
    }

    #[test]
    fn visible_context_failure_never_compacts_or_retries() {
        let visible_then_failed = response_with_events(
            "event: response.created\ndata: {\"type\":\"response.created\"}\n\n\
             event: response.output_text.delta\ndata: {\"type\":\"response.output_text.delta\",\"output_index\":0,\"content_index\":0,\"delta\":\"partial\"}\n\n\
             event: response.failed\ndata: {\"type\":\"response.failed\",\"response\":{\"error\":{\"type\":\"invalid_request\",\"code\":\"context_length_exceeded\"}}}\n\n",
        );
        let upstream = FakeResponses::start_with_responses(vec![
            None,
            Some(visible_then_failed),
            Some(recovery_success_response()),
        ]);
        let bridge = BridgeHandle::start(bridged_config(&upstream)).unwrap();
        let first = request(
            bridge.socket_addr(),
            &authorized(
                "POST",
                "/v1/messages",
                bridge.bearer_token(),
                PROBE_STREAM_BODY,
            ),
        );
        assert_eq!(status(&first), 200, "{first}");
        let second = request(
            bridge.socket_addr(),
            &authorized(
                "POST",
                "/v1/messages",
                bridge.bearer_token(),
                r#"{"model":"claude-x","messages":[{"role":"user","content":"first"},{"role":"assistant","content":"bridged reply"},{"role":"user","content":"pending"}],"stream":true}"#,
            ),
        );
        assert_eq!(status(&second), 200, "{second}");
        let body = decode_chunked_body(&second);
        assert!(body.contains("partial"), "{body}");
        assert!(body.contains("context is too long"), "{body}");
        assert!(!body.contains("recovered"), "{body}");
        let requests = upstream.requests();
        assert_eq!(
            requests
                .iter()
                .filter(|request| request.starts_with("POST /v1/responses HTTP/1.1"))
                .count(),
            2,
            "the visible turn must not retry: {requests:#?}"
        );
        assert!(
            !requests
                .iter()
                .any(|request| request.starts_with("POST /v1/responses/compact HTTP/1.1")),
            "the visible turn must not compact: {requests:#?}"
        );
    }

    /// The reported incident, reproduced at the HTTP layer.
    ///
    /// A drained account previously reached the user as
    /// `upstream provider returned status 429` with no `Retry-After`, so the
    /// only reset time on screen came from the client's own separate
    /// accounting -- about a different limit than the one that broke the turn.
    #[test]
    fn an_in_band_400_after_a_successful_turn_is_redacted_and_classified() {
        let secret = "second-turn prompt SECRET_PROMPT";
        let tool_output = "tool-output SECRET_TOOL_OUTPUT";
        let upstream_secret = "upstream-message SECRET_UPSTREAM_DETAIL";
        let failed_event = format!(
            "event: response.failed\ndata: {{\"type\":\"response.failed\",\"response\":{{\"error\":{{\"type\":\"invalid_request\",\"code\":\"context_length_exceeded\",\"message\":\"{upstream_secret}\"}},\"request_id\":\"req_second_turn\"}}}}\n\n"
        );
        let failed_reply = format!(
            "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{failed_event}",
            failed_event.len()
        )
        .into_bytes();
        let upstream = FakeResponses::start_with_responses(vec![
            None,
            Some(failed_reply.clone()),
            Some(compact_success_response()),
            Some(failed_reply),
        ]);
        let tmp = tempfile::tempdir().unwrap();
        let log_path = tmp.path().join("bridge.jsonl");
        let mut bridge =
            BridgeHandle::start(bridged_config(&upstream).with_log_path(log_path.clone())).unwrap();

        let first = request(
            bridge.socket_addr(),
            &authorized(
                "POST",
                "/v1/messages",
                bridge.bearer_token(),
                PROBE_STREAM_BODY,
            ),
        );
        assert_eq!(status(&first), 200, "{first}");
        let continuation = format!(
            r#"{{"model":"claude-x","messages":[{{"role":"user","content":"{secret}"}},{{"role":"assistant","content":"done"}},{{"role":"user","content":[{{"type":"tool_result","tool_use_id":"call_1","content":"{tool_output}"}}]}}],"stream":true}}"#
        );
        let second = request(
            bridge.socket_addr(),
            &authorized("POST", "/v1/messages", bridge.bearer_token(), &continuation),
        );
        bridge.shutdown().unwrap();

        assert_eq!(status(&second), 200, "{second}");
        assert!(second.contains("context is too long"), "{second}");
        let text = std::fs::read_to_string(log_path).unwrap();
        assert!(
            text.contains(r#""event":"in_band_upstream_failure""#),
            "{text}"
        );
        assert!(text.contains(r#""category":"context_length""#), "{text}");
        assert!(
            text.contains(r#""code":"context_length_exceeded""#),
            "{text}"
        );
        assert!(text.contains(r#""request_id":"req_second_turn""#), "{text}");
        assert!(text.contains(r#""phase":"continuation""#), "{text}");
        let event: serde_json::Value = serde_json::from_str(text.trim()).unwrap();
        assert_eq!(
            event.pointer("/request_shape/input_kinds"),
            Some(&serde_json::json!([
                "message",
                "message",
                "function_call_output"
            ]))
        );
        assert_eq!(
            event.pointer("/request_shape/has_reasoning"),
            Some(&serde_json::json!(true))
        );
        for forbidden in [secret, tool_output, upstream_secret, "Authorization"] {
            assert!(
                !second.contains(forbidden),
                "client leaked {forbidden:?}: {second}"
            );
            assert!(
                !text.contains(forbidden),
                "log leaked {forbidden:?}: {text}"
            );
        }
    }

    #[test]
    fn a_non_streaming_in_band_400_is_logged_and_sanitized() {
        let upstream_secret = "non-streaming SECRET_UPSTREAM_DETAIL";
        let failed_event = format!(
            "event: response.failed\ndata: {{\"type\":\"response.failed\",\"response\":{{\"error\":{{\"type\":\"invalid_request\",\"code\":\"cyber_policy\",\"message\":\"{upstream_secret}\"}},\"request_id\":\"req_non_streaming\"}}}}\n\n"
        );
        let failed_reply = format!(
            "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{failed_event}",
            failed_event.len()
        )
        .into_bytes();
        let upstream = FakeResponses::start_with_response(Some(failed_reply));
        let tmp = tempfile::tempdir().unwrap();
        let log_path = tmp.path().join("bridge.jsonl");
        let mut bridge =
            BridgeHandle::start(bridged_config(&upstream).with_log_path(log_path.clone())).unwrap();
        let response = request(
            bridge.socket_addr(),
            &authorized("POST", "/v1/messages", bridge.bearer_token(), PROBE_BODY),
        );
        bridge.shutdown().unwrap();

        assert_eq!(status(&response), 400, "{response}");
        assert!(response.contains("provider policy"), "{response}");
        assert!(!response.contains(upstream_secret), "{response}");
        let text = std::fs::read_to_string(log_path).unwrap();
        let event = text
            .lines()
            .map(|line| serde_json::from_str::<serde_json::Value>(line).unwrap())
            .find(|event| event["event"] == "in_band_upstream_failure")
            .expect("in-band failure event");
        assert_eq!(event["category"], "policy", "{text}");
        assert_eq!(event["code"], "cyber_policy", "{text}");
        assert_eq!(event["request_id"], "req_non_streaming", "{text}");
        assert!(!text.contains(upstream_secret), "{text}");
    }

    #[test]
    fn an_exhausted_account_answers_429_billing_error_with_a_retry_after() {
        let body = r#"{"error":{"code":"usage_limit_reached","message":"quota exhausted for acct_42","resets_in_seconds":442242}}"#;
        let reply = format!(
            "HTTP/1.1 429 Too Many Requests
Content-Type: application/json
Content-Length: {}
Connection: close

{body}",
            body.len()
        )
        .into_bytes();
        let upstream = FakeResponses::start_with_response(Some(reply));
        let mut bridge = BridgeHandle::start(bridged_config(&upstream)).unwrap();
        let response = request(
            bridge.socket_addr(),
            &authorized("POST", "/v1/messages", bridge.bearer_token(), PROBE_BODY),
        );
        bridge.shutdown().unwrap();

        assert_eq!(status(&response), 429);
        assert!(
            response.starts_with("HTTP/1.1 429 Too Many Requests"),
            "the status line had no reason phrase: {response}"
        );
        assert!(
            response.contains("Retry-After: 442242"),
            "no Retry-After header: {response}"
        );
        assert!(response.contains("billing_error"), "{response}");
        assert!(response.contains("quota exhausted"), "{response}");
        // Human-readable, not a raw second count to do arithmetic on.
        assert!(response.contains("5d 2h"), "{response}");
        // The upstream body never crosses over.
        assert!(!response.contains("acct_42"), "{response}");
        // Attempted exactly once: a multi-day exhaustion is not transient.
        assert_eq!(
            upstream.requests().len(),
            1,
            "an exhausted plan was retried"
        );
    }

    #[test]
    fn first_replayed_request_seeds_full_history_for_later_continuations() {
        let upstream = FakeResponses::start();
        let bridge = BridgeHandle::start(bridged_config(&upstream)).unwrap();
        let first_body = r#"{"model":"claude-x","messages":[{"role":"user","content":"old user"},{"role":"assistant","content":"old assistant"},{"role":"user","content":"first pending"}],"stream":false}"#;
        let first = request(
            bridge.socket_addr(),
            &authorized("POST", "/v1/messages", bridge.bearer_token(), first_body),
        );
        assert_eq!(status(&first), 200, "{first}");

        let second_body = r#"{"model":"claude-x","messages":[{"role":"user","content":"old user"},{"role":"assistant","content":"old assistant"},{"role":"user","content":"first pending"},{"role":"assistant","content":"bridged reply"},{"role":"user","content":"later pending"}],"stream":false}"#;
        let second = request(
            bridge.socket_addr(),
            &authorized("POST", "/v1/messages", bridge.bearer_token(), second_body),
        );
        assert_eq!(status(&second), 200, "{second}");

        let requests = upstream.requests();
        let body: serde_json::Value = serde_json::from_str(
            requests[1]
                .split("\r\n\r\n")
                .nth(1)
                .expect("second upstream body"),
        )
        .expect("second upstream JSON");
        let texts: Vec<_> = body["input"]
            .as_array()
            .unwrap()
            .iter()
            .flat_map(|item| item["content"].as_array().into_iter().flatten())
            .filter_map(|part| part["text"].as_str())
            .collect();
        assert_eq!(
            texts,
            vec![
                "old user",
                "old assistant",
                "first pending",
                "later pending"
            ],
            "the first request must seed its complete translated replay exactly once"
        );
    }

    #[test]
    fn continuation_merges_new_developer_instruction_without_replaying_history() {
        let upstream = FakeResponses::start();
        let bridge = BridgeHandle::start(bridged_config(&upstream)).unwrap();
        let first_body = r#"{"model":"claude-x","system":"first instruction","messages":[{"role":"user","content":"old user"}],"stream":false}"#;
        assert_eq!(
            status(&request(
                bridge.socket_addr(),
                &authorized("POST", "/v1/messages", bridge.bearer_token(), first_body),
            )),
            200
        );

        let second_body = r#"{"model":"claude-x","system":"updated instruction","messages":[{"role":"user","content":"old user"},{"role":"assistant","content":"bridged reply"},{"role":"user","content":"new pending"}],"stream":false}"#;
        assert_eq!(
            status(&request(
                bridge.socket_addr(),
                &authorized("POST", "/v1/messages", bridge.bearer_token(), second_body),
            )),
            200
        );

        let requests = upstream.requests();
        let body: serde_json::Value = serde_json::from_str(
            requests[1]
                .split("\r\n\r\n")
                .nth(1)
                .expect("second upstream body"),
        )
        .expect("second upstream JSON");
        let input = body["input"].as_array().unwrap();
        assert_eq!(
            input.len(),
            2,
            "replayed display turns must not be duplicated"
        );
        assert_eq!(body["instructions"], "updated instruction");
        assert_eq!(input[0]["content"][0]["text"], "old user");
        assert_eq!(input[1]["content"][0]["text"], "new pending");
    }

    #[test]
    fn in_band_failure_followed_by_eof_preserves_provider_classification() {
        let failed = response_with_events(
            "event: response.created\ndata: {\"type\":\"response.created\"}\n\n\
             event: response.failed\ndata: {\"type\":\"response.failed\",\"response\":{\"error\":{\"code\":\"usage_limit_reached\",\"message\":\"secret account detail\"}}}\n\n",
        );
        let upstream = FakeResponses::start_with_response(Some(failed));
        let bridge = BridgeHandle::start(bridged_config(&upstream)).unwrap();
        let response = request(
            bridge.socket_addr(),
            &authorized("POST", "/v1/messages", bridge.bearer_token(), PROBE_BODY),
        );

        assert_eq!(status(&response), 429, "{response}");
        assert!(response.contains("billing_error"), "{response}");
        assert!(response.contains("quota exhausted"), "{response}");
        assert!(!response.contains("secret account detail"), "{response}");
    }

    #[test]
    fn response_incomplete_is_a_successful_max_tokens_completion() {
        let upstream = FakeResponses::start_with_response(Some(incomplete_response()));
        let bridge = BridgeHandle::start(bridged_config(&upstream)).unwrap();
        let response = request(
            bridge.socket_addr(),
            &authorized("POST", "/v1/messages", bridge.bearer_token(), PROBE_BODY),
        );

        assert_eq!(status(&response), 200, "{response}");
        let message: serde_json::Value =
            serde_json::from_str(response.split("\r\n\r\n").nth(1).expect("response body"))
                .expect("Anthropic message JSON");
        assert_eq!(message["stop_reason"], "max_tokens");
        assert_eq!(message["content"][0]["text"], "truncated");
        assert_eq!(message["usage"]["output_tokens"], 1);
    }

    #[test]
    fn non_streaming_capacity_rejection_evicts_stale_history_after_reply() {
        let upstream = FakeResponses::start();
        let limits = HistoryLimits {
            max_conversations: 1,
            max_items_per_conversation: 1,
            max_bytes_per_conversation: 1024,
        };
        let bridge =
            BridgeHandle::start(bridged_config(&upstream).with_history_limits(limits)).unwrap();
        let first = request(
            bridge.socket_addr(),
            &authorized(
                "POST",
                "/v1/messages",
                bridge.bearer_token(),
                r#"{"model":"claude-x","messages":[{"role":"user","content":"stale first"}],"stream":false}"#,
            ),
        );
        assert_eq!(status(&first), 200, "{first}");
        let capacity_rejected = request(
            bridge.socket_addr(),
            &authorized(
                "POST",
                "/v1/messages",
                bridge.bearer_token(),
                r#"{"model":"claude-x","messages":[{"role":"user","content":"stale first"},{"role":"assistant","content":"bridged reply"},{"role":"user","content":"capacity turn"}],"stream":false}"#,
            ),
        );
        assert_eq!(status(&capacity_rejected), 200, "{capacity_rejected}");
        let fresh = request(
            bridge.socket_addr(),
            &authorized(
                "POST",
                "/v1/messages",
                bridge.bearer_token(),
                r#"{"model":"claude-x","messages":[{"role":"user","content":"fresh seed"},{"role":"assistant","content":"bridged reply"},{"role":"user","content":"later pending"}],"stream":false}"#,
            ),
        );
        assert_eq!(status(&fresh), 200, "{fresh}");

        let requests = upstream.requests();
        let body: serde_json::Value = serde_json::from_str(
            requests[2]
                .split("\r\n\r\n")
                .nth(1)
                .expect("fresh upstream body"),
        )
        .expect("fresh upstream JSON");
        let texts: Vec<_> = body["input"]
            .as_array()
            .unwrap()
            .iter()
            .flat_map(|item| item["content"].as_array().into_iter().flatten())
            .filter_map(|part| part["text"].as_str())
            .collect();
        assert_eq!(texts, vec!["fresh seed", "bridged reply", "later pending"]);
    }

    #[test]
    fn streaming_capacity_rejection_evicts_stale_history_after_reply() {
        let upstream = FakeResponses::start();
        let limits = HistoryLimits {
            max_conversations: 1,
            max_items_per_conversation: 1,
            max_bytes_per_conversation: 1024,
        };
        let bridge =
            BridgeHandle::start(bridged_config(&upstream).with_history_limits(limits)).unwrap();
        let first = request(
            bridge.socket_addr(),
            &authorized(
                "POST",
                "/v1/messages",
                bridge.bearer_token(),
                r#"{"model":"claude-x","messages":[{"role":"user","content":"stale first"}],"stream":true}"#,
            ),
        );
        assert_eq!(status(&first), 200, "{first}");
        let capacity_rejected = request(
            bridge.socket_addr(),
            &authorized(
                "POST",
                "/v1/messages",
                bridge.bearer_token(),
                r#"{"model":"claude-x","messages":[{"role":"user","content":"stale first"},{"role":"assistant","content":"bridged reply"},{"role":"user","content":"capacity turn"}],"stream":true}"#,
            ),
        );
        assert_eq!(status(&capacity_rejected), 200, "{capacity_rejected}");
        let fresh = request(
            bridge.socket_addr(),
            &authorized(
                "POST",
                "/v1/messages",
                bridge.bearer_token(),
                r#"{"model":"claude-x","messages":[{"role":"user","content":"fresh seed"},{"role":"assistant","content":"bridged reply"},{"role":"user","content":"later pending"}],"stream":true}"#,
            ),
        );
        assert_eq!(status(&fresh), 200, "{fresh}");

        let requests = upstream.requests();
        let body: serde_json::Value = serde_json::from_str(
            requests[2]
                .split("\r\n\r\n")
                .nth(1)
                .expect("fresh upstream body"),
        )
        .expect("fresh upstream JSON");
        let texts: Vec<_> = body["input"]
            .as_array()
            .unwrap()
            .iter()
            .flat_map(|item| item["content"].as_array().into_iter().flatten())
            .filter_map(|part| part["text"].as_str())
            .collect();
        assert_eq!(texts, vec!["fresh seed", "bridged reply", "later pending"]);
    }

    #[test]
    fn eof_before_response_completed_is_an_upstream_failure_and_does_not_record_history() {
        let incomplete = response_with_events(
            "event: response.created\ndata: {\"type\":\"response.created\"}\n\n\
             event: response.output_text.delta\ndata: {\"type\":\"response.output_text.delta\",\"output_index\":0,\"content_index\":0,\"delta\":\"partial\"}\n\n",
        );
        let upstream = FakeResponses::start_with_responses(vec![Some(incomplete), None]);
        let bridge = BridgeHandle::start(bridged_config(&upstream)).unwrap();
        let failed = request(
            bridge.socket_addr(),
            &authorized(
                "POST",
                "/v1/messages",
                bridge.bearer_token(),
                r#"{"model":"claude-x","messages":[{"role":"user","content":"discard me"}],"stream":false}"#,
            ),
        );
        assert_eq!(status(&failed), 502, "{failed}");
        assert!(failed.contains("upstream unreachable"), "{failed}");

        let later = request(
            bridge.socket_addr(),
            &authorized(
                "POST",
                "/v1/messages",
                bridge.bearer_token(),
                r#"{"model":"claude-x","messages":[{"role":"user","content":"keep me"}],"stream":false}"#,
            ),
        );
        assert_eq!(status(&later), 200, "{later}");
        let requests = upstream.requests();
        let body: serde_json::Value = serde_json::from_str(
            requests[1]
                .split("\r\n\r\n")
                .nth(1)
                .expect("later upstream body"),
        )
        .expect("later upstream JSON");
        assert_eq!(body["input"].as_array().unwrap().len(), 1);
        assert_eq!(body["input"][0]["content"][0]["text"], "keep me");
    }

    /// A typo must not be silently billed as the default model.
    #[test]
    fn an_unknown_alias_in_a_request_is_a_400_naming_the_valid_names() {
        let upstream = FakeResponses::start();
        let bridge = BridgeHandle::start(bridged_config(&upstream)).unwrap();
        let response = request(
            bridge.socket_addr(),
            &authorized(
                "POST",
                "/v1/messages",
                bridge.bearer_token(),
                r#"{"model":"tera","messages":[{"role":"user","content":"hi"}],"stream":false}"#,
            ),
        );
        assert_eq!(status(&response), 400);
        assert!(response.contains("terra"), "{response}");
        assert!(
            upstream.requests().is_empty(),
            "a rejected selection must not reach upstream"
        );
    }
}
