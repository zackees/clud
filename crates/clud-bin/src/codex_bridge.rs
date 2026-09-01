//! Authenticated loopback bridge used by Codex-provider launches through the
//! Claude harness (issue #626).

use crate::backend::ModelProvider;
use crate::bridge_log::{unix_ms, BridgeLog};
use crate::codex_history::{ConversationKey, ConversationRoute, ConversationStore, HistoryLimits};
use crate::codex_model::ModelSpec;
use crate::codex_pipeline::{Pipeline, PipelineError, ProviderFailure};
use crate::codex_sse::InBandFailure;
use crate::codex_upstream::{
    ApiKeyCredentials, FailureClass, ResolvedCredentials, UpstreamClient, UpstreamConfig,
    UpstreamError, UpstreamFailure,
};
use crate::failover::{FailoverLadder, FailoverRung};
use crate::provider_catalog;
use crate::route_health::{RouteLedger, RouteState, RouteVerdict};
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
/// Excess connections wait in the listen backlog until a worker becomes
/// available or the bridge shuts down. This keeps local contention out of the
/// harness-visible API surface without accepting or buffering their bodies.
const DEFAULT_MAX_CONCURRENCY: usize = 1;
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

const ANTHROPIC_MESSAGES_BASE_URL: &str = "https://api.anthropic.com";
const DEEPSEEK_ANTHROPIC_BASE_URL: &str = "https://api.deepseek.com/anthropic";
/// OpenRouter's native Anthropic Messages endpoint. Note the missing `/v1`:
/// the path is appended by the proxy, and `https://openrouter.ai/api/v1` is a
/// different (OpenAI-shaped) surface.
const OPENROUTER_ANTHROPIC_BASE_URL: &str = "https://openrouter.ai/api";
pub const UNIFIED_GATEWAY_TOKEN_HEADER: &str = "X-Clud-Gateway-Token";

/// A launch-scoped multiplexer configuration. Secret material is intentionally
/// opaque in Debug output and never reaches launch plans or daemon wire state.
#[derive(Clone)]
pub struct UnifiedGatewayConfig {
    deepseek_api_key: Option<String>,
    openrouter_api_key: Option<String>,
    codex_available: bool,
    anthropic_base_url: String,
    deepseek_base_url: String,
    openrouter_base_url: String,
    /// Ordered fallback routes. Empty by default, so a launch that did not ask
    /// for failover keeps exactly today's behavior and today's spend.
    failover: FailoverLadder,
    /// Which routes can serve right now. Shared across connections because a
    /// route drained on one turn must stay drained for the next one, and
    /// launch-scoped because a wedged account in this session must never
    /// suppress a route in another.
    route_ledger: Arc<Mutex<RouteLedger>>,
}

impl UnifiedGatewayConfig {
    pub fn new(deepseek_api_key: Option<String>, codex_available: bool) -> Self {
        Self {
            deepseek_api_key,
            openrouter_api_key: None,
            codex_available,
            anthropic_base_url: ANTHROPIC_MESSAGES_BASE_URL.to_string(),
            deepseek_base_url: DEEPSEEK_ANTHROPIC_BASE_URL.to_string(),
            openrouter_base_url: OPENROUTER_ANTHROPIC_BASE_URL.to_string(),
            failover: FailoverLadder::default(),
            route_ledger: Arc::new(Mutex::new(RouteLedger::new())),
        }
    }

    /// OpenRouter's key, when the launch has one. Absent leaves the route out
    /// of discovery entirely rather than advertising a row that cannot serve.
    pub fn with_openrouter(mut self, api_key: Option<String>) -> Self {
        self.openrouter_api_key = api_key;
        self
    }

    pub fn with_failover(mut self, failover: FailoverLadder) -> Self {
        self.failover = failover;
        self
    }

    #[cfg(test)]
    fn with_upstreams(mut self, anthropic_base_url: String, deepseek_base_url: String) -> Self {
        self.anthropic_base_url = anthropic_base_url;
        self.deepseek_base_url = deepseek_base_url;
        self
    }

    #[cfg(test)]
    fn with_openrouter_upstream(mut self, base_url: String) -> Self {
        self.openrouter_base_url = base_url;
        self
    }

    /// Record a verdict against a route and report the state it leaves behind.
    /// A poisoned ledger degrades to "no health known" rather than killing the
    /// turn: failover is a recovery mechanism and must not become a new way to
    /// fail.
    fn record_route(&self, route: ConversationRoute, verdict: RouteVerdict) -> Option<RouteState> {
        let mut ledger = self.route_ledger.lock().ok()?;
        Some(ledger.record(route, verdict, Instant::now()))
    }

    fn record_route_success(&self, route: ConversationRoute) {
        if let Ok(mut ledger) = self.route_ledger.lock() {
            ledger.record_success(route);
        }
    }

    /// The next rung below `after` that the ledger says can serve.
    fn next_rung(&self, after: Option<&str>, current: ConversationRoute) -> Option<FailoverRung> {
        let ledger = self.route_ledger.lock().ok()?;
        self.failover
            .next_available_excluding(after, Some(current), &ledger, Instant::now())
            .cloned()
    }
}

impl fmt::Debug for UnifiedGatewayConfig {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("UnifiedGatewayConfig")
            .field("deepseek_configured", &self.deepseek_api_key.is_some())
            .field("openrouter_configured", &self.openrouter_api_key.is_some())
            .field("codex_available", &self.codex_available)
            .field("failover_rungs", &self.failover.rungs().len())
            .finish()
    }
}

#[derive(Clone, Debug)]
enum GatewayMode {
    Codex,
    Unified(UnifiedGatewayConfig),
}

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
    /// Default model+effort selection, from `--model` on the launch. `None`
    /// keeps the built-in default. A request that names its own model still
    /// wins over this.
    default_model: Option<ModelSpec>,
    gateway_mode: GatewayMode,
    history_limits: HistoryLimits,
    log_path: Option<std::path::PathBuf>,
    log_max_bytes: usize,
    test_upstream_url: Option<String>,
    /// How many `POST /v1/messages` turns the harness has asked this bridge to
    /// serve. Shared with [`BridgeHandle`] so a launch that exited without ever
    /// asking can be told apart from one that asked and was refused (#998).
    turn_requests: Arc<AtomicUsize>,
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
            default_model: None,
            gateway_mode: GatewayMode::Codex,
            history_limits: HistoryLimits::default(),
            log_path: default_bridge_log_path(),
            log_max_bytes: crate::bridge_log::DEFAULT_MAX_BYTES,
            test_upstream_url: test_upstream_override_from_process(),
            turn_requests: Arc::new(AtomicUsize::new(0)),
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

    /// Turn the existing launch-scoped bridge into the unified multiplexer.
    /// This remains an in-process foreground listener; it is never a daemon.
    pub fn with_unified_gateway(mut self, config: UnifiedGatewayConfig) -> Self {
        self.gateway_mode = GatewayMode::Unified(config);
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
            .field(
                "default_model",
                &self.default_model.as_ref().map(ModelSpec::display),
            )
            .field("gateway_mode", &self.gateway_mode)
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
    /// Descriptor-backed provider credentials could not be read at the
    /// child-spawn boundary. The error is provider-neutral and secret-free.
    AnthropicCompatCredentials,
    /// Direct Codex-via-Claude admission failed before the listener or child
    /// exists. The renderer is shared by foreground and daemon launches.
    CodexBridgeCredentials(crate::codex_upstream::CodexBridgeCredentialError),
    /// The caller has explicitly disabled the discovery request unified mode
    /// needs, so launching would silently present a misleading picker.
    DiscoveryDisabled,
    /// The launch declared a failover rung the gateway cannot route. Carries
    /// the ladder's own message, which names the offending rung.
    Failover(String),
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
            Self::AnthropicCompatCredentials => formatter.write_str(
                "provider credentials are unavailable at the child-spawn boundary",
            ),
            Self::CodexBridgeCredentials(error) => write!(formatter, "{error}"),
            Self::DiscoveryDisabled => formatter.write_str(
                "this Claude gateway requires model discovery, but CLAUDE_CODE_DISABLE_NONESSENTIAL_TRAFFIC is enabled",
            ),
            Self::Failover(error) => write!(formatter, "--failover: {error}"),
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
    turn_requests: Arc<AtomicUsize>,
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
        let turn_requests = Arc::clone(&config.turn_requests);
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
            turn_requests,
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

    /// How many `POST /v1/messages` turns the harness asked this bridge for.
    /// Zero on a launch that never reached the gateway -- see
    /// `launch_log::silent_bridge_reason` (#998).
    pub(crate) fn turn_requests(&self) -> usize {
        self.turn_requests.load(Ordering::Acquire)
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
                if log.has_notable_records() {
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
    // the kernel's listen backlog instead of being answered 503. The active
    // worker still has first-frame and stream-idle timeouts; healthy streaming
    // work can run for hours, so queue age must not be a local failure policy.
    let mut full_since: Option<Instant> = None;
    while !shutdown.load(Ordering::Acquire) {
        workers.retain(|worker| !worker.is_finished());
        let limit = config.max_concurrency.max(1);
        if active.load(Ordering::Acquire) >= limit {
            if full_since.is_none() {
                full_since = Some(Instant::now());
            }
            thread::sleep(ACCEPT_POLL);
            continue;
        } else {
            // Reserve before accepting.  The check above is only a hint: an
            // old worker can exit (or a new worker can reserve) between it and
            // this point. Reserving first means no accepted socket is ever
            // discarded because admission raced after `accept`.
            if !reserve_worker(&active, limit) {
                continue;
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
                        active.fetch_sub(1, Ordering::AcqRel);
                        continue;
                    }
                    // `full_since` begins at saturation, before the OS exposes
                    // a readable pending socket, so `wait_ms` is a secret-free
                    // aggregate upper bound rather than a per-client identity.
                    // Only emit events now that an actual queued socket has
                    // been accepted; a lone occupied worker produces neither.
                    if let Some(wait) = full_since.take().map(|started| started.elapsed()) {
                        record_admission_queued(log.as_ref());
                        record_admission_acquired(log.as_ref(), wait);
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
                    active.fetch_sub(1, Ordering::AcqRel);
                    // There was no pending socket, so any saturation interval
                    // was not an admission queue and must not reach the log.
                    full_since = None;
                    thread::sleep(ACCEPT_POLL);
                }
                Err(_) => {
                    active.fetch_sub(1, Ordering::AcqRel);
                    break;
                }
            }
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
    if !request_is_authenticated(&parsed, bearer_token, &config.gateway_mode) {
        let reason = match config.gateway_mode {
            GatewayMode::Codex => "bearer_mismatch",
            GatewayMode::Unified(_) => "gateway_token_mismatch",
        };
        record_rejection(log, 401, reason);
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
        ("POST", "/_clud/context/compact-finished") => {
            let body_deadline = Instant::now() + config.body_timeout;
            match read_context_control_body(
                &mut stream,
                parsed.body_prefix,
                parsed.content_length,
                body_deadline,
                shutdown,
                ContextControl::CompactFinished,
            ) {
                Ok(body_key) => serve_context_compact_finished(
                    &mut stream,
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
        ("GET", "/v1/models") => match &config.gateway_mode {
            GatewayMode::Codex => serve_codex_catalog(&mut stream, log),
            GatewayMode::Unified(_) => serve_unified_catalog(&mut stream, config),
        },
        ("GET", "/_clud/route/status") => match &config.gateway_mode {
            GatewayMode::Unified(unified) => serve_route_status(&mut stream, unified),
            GatewayMode::Codex => {
                record_rejection(log, 404, "route_status_unsupported");
                let _ = write_error(&mut stream, 404);
            }
        },
        ("POST", "/_clud/route/clear") => {
            let body_deadline = Instant::now() + config.body_timeout;
            match read_body(
                &mut stream,
                parsed.body_prefix,
                parsed.content_length,
                body_deadline,
                shutdown,
            ) {
                Ok(body) => match &config.gateway_mode {
                    GatewayMode::Unified(unified) => serve_route_clear(&mut stream, unified, &body),
                    GatewayMode::Codex => {
                        record_rejection(log, 404, "route_clear_unsupported");
                        let _ = write_error(&mut stream, 404);
                    }
                },
                Err(ABANDON) => {}
                Err(status) => {
                    record_rejection(log, status, "route_control_body");
                    let _ = write_error(&mut stream, status);
                }
            }
        }
        ("HEAD", "/v1/messages") => {
            let _ = write_response(&mut stream, 200, "application/json", b"", true);
        }
        ("POST", "/v1/messages/count_tokens") => {
            if !matches!(config.gateway_mode, GatewayMode::Unified(_)) {
                record_rejection(log, 404, "token_counting_unsupported");
                let _ = write_response(
                    &mut stream,
                    404,
                    "application/json",
                    br#"{"error":{"type":"not_found_error","message":"token counting is not supported by the Codex bridge"}}"#,
                    false,
                );
                return;
            }
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
            serve_unified_count_tokens(
                &mut stream,
                config,
                shutdown,
                conversations,
                &body,
                &conversation_key,
                &parsed.headers,
            );
        }
        ("POST", "/v1/messages") => {
            // Counted before the body is read, so a turn the bridge later
            // refuses still counts as "the harness talked to us" (#998). The
            // discovery route's own refusals are the bridge log's story; the
            // classification this feeds is only for total silence.
            config.turn_requests.fetch_add(1, Ordering::Release);
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
            if matches!(config.gateway_mode, GatewayMode::Unified(_)) {
                serve_unified_messages(
                    &mut stream,
                    config,
                    shutdown,
                    conversations,
                    &body,
                    streaming,
                    &conversation_key,
                    &parsed.headers,
                    log,
                );
            } else {
                serve_codex_discovery_messages(
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

/// `GET /_clud/route/status` -- the configured ladder and each route's health.
///
/// Sits beside `/_clud/context/*` and carries the same gateway-token
/// requirement. Deliberately names only the public route, its cost owner, and a
/// reset clock: never a credential, base URL, prompt, or response body.
fn serve_route_status(stream: &mut TcpStream, unified: &UnifiedGatewayConfig) {
    let now = Instant::now();
    let ladder: Vec<serde_json::Value> = unified
        .failover
        .rungs()
        .iter()
        .map(|rung| {
            let state = unified
                .route_ledger
                .lock()
                .map(|ledger| ledger.state(rung.route, now))
                .unwrap_or(RouteState::Available);
            serde_json::json!({
                "spec": rung.spec,
                "route": rung.route.as_str(),
                "cost": rung.cost.as_str(),
                "withheld_for_consent": !unified.failover.allows_metered()
                    && rung.cost == crate::failover::CostOwner::Metered,
                "state": route_state_json(state),
            })
        })
        .collect();
    let routes: Vec<serde_json::Value> = unified
        .route_ledger
        .lock()
        .map(|ledger| {
            ledger
                .snapshot(now)
                .into_iter()
                .map(|(route, state)| {
                    serde_json::json!({
                        "route": route.as_str(),
                        "state": route_state_json(state),
                    })
                })
                .collect()
        })
        .unwrap_or_default();
    let body = serde_json::json!({ "ladder": ladder, "routes": routes }).to_string();
    let _ = write_response(stream, 200, "application/json", body.as_bytes(), false);
}

fn route_state_json(state: RouteState) -> serde_json::Value {
    match state {
        RouteState::Available => serde_json::json!({"status": "available"}),
        RouteState::Cooling { remaining, reason } => serde_json::json!({
            "status": "cooling",
            "reason": reason,
            "retry_in_seconds": remaining.as_secs(),
        }),
        RouteState::Down { reason } => serde_json::json!({
            "status": "down",
            "reason": reason,
        }),
    }
}

/// `POST /_clud/route/clear` -- forget one route's health.
///
/// The escape hatch for a wedged session: a route marked drained has no clock,
/// so after the operator tops up a balance or replaces a key nothing else can
/// bring it back.
fn serve_route_clear(stream: &mut TcpStream, unified: &UnifiedGatewayConfig, body: &[u8]) {
    let requested = serde_json::from_slice::<serde_json::Value>(body)
        .ok()
        .and_then(|value| {
            value
                .get("route")
                .and_then(serde_json::Value::as_str)
                .map(str::to_string)
        });
    let route = match requested.as_deref() {
        Some("claude") => Some(ConversationRoute::Claude),
        Some("codex") => Some(ConversationRoute::Codex),
        Some("deepseek") => Some(ConversationRoute::DeepSeek),
        Some("openrouter") => Some(ConversationRoute::OpenRouter),
        _ => None,
    };
    let Some(route) = route else {
        let _ = write_response(
            stream,
            400,
            "application/json",
            br#"{"error":{"type":"invalid_request_error","message":"route must be one of claude, codex, deepseek, openrouter"}}"#,
            false,
        );
        return;
    };
    if let Ok(mut ledger) = unified.route_ledger.lock() {
        ledger.clear(route);
    }
    let body = serde_json::json!({"cleared": route.as_str()}).to_string();
    let _ = write_response(stream, 200, "application/json", body.as_bytes(), false);
}

fn serve_unified_catalog(stream: &mut TcpStream, config: &BridgeConfig) {
    let GatewayMode::Unified(unified) = &config.gateway_mode else {
        unreachable!("catalog route is guarded by the unified mode match arm");
    };
    let data = provider_catalog::MODELS
        .iter()
        .filter(|entry| match entry.provider {
            ModelProvider::Codex => unified.codex_available,
            ModelProvider::DeepSeek => unified.deepseek_api_key.is_some(),
            // Phase 4 of #937 wires Kimi's unified route; until then it is
            // never advertised, so a direct `--kimi` launch is unaffected.
            ModelProvider::Kimi => false,
            ModelProvider::OpenRouter => unified.openrouter_api_key.is_some(),
            ModelProvider::Claude => false,
        })
        .filter_map(|entry| {
            entry.discovery_id.map(|id| {
                serde_json::json!({
                    "id": id,
                    "display_name": entry.display_name,
                    "type": "model",
                })
            })
        })
        .collect::<Vec<_>>();
    let body = serde_json::json!({"data": data, "has_more": false}).to_string();
    let _ = write_response(stream, 200, "application/json", body.as_bytes(), false);
}

fn serve_codex_catalog(stream: &mut TcpStream, log: Option<&SharedBridgeLog>) {
    let advertised = provider_catalog::MODELS
        .iter()
        .filter(|entry| entry.provider == ModelProvider::Codex)
        .filter_map(|entry| entry.discovery_id.map(|id| (id, entry.display_name)))
        .collect::<Vec<_>>();
    record_catalog_advertised(
        log,
        &advertised.iter().map(|(id, _)| *id).collect::<Vec<_>>(),
    );
    let data = advertised
        .iter()
        .map(|(id, display_name)| {
            serde_json::json!({
                "id": id,
                "display_name": display_name,
                "type": "model",
            })
        })
        .collect::<Vec<_>>();
    let body = serde_json::json!({"data": data, "has_more": false}).to_string();
    let _ = write_response(stream, 200, "application/json", body.as_bytes(), false);
}

fn request_is_authenticated(request: &ParsedRequest, token: &str, mode: &GatewayMode) -> bool {
    let provided = match mode {
        GatewayMode::Codex => request.authorization.as_deref(),
        GatewayMode::Unified(_) => request.header(UNIFIED_GATEWAY_TOKEN_HEADER),
    };
    let expected = match mode {
        GatewayMode::Codex => format!("Bearer {token}"),
        GatewayMode::Unified(_) => token.to_string(),
    };
    provided.is_some_and(|provided| constant_time_eq(provided.as_bytes(), expected.as_bytes()))
}

#[derive(Clone, Copy)]
enum ContextControl {
    Compact,
    CompactFinished,
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
        ContextControl::CompactFinished => {
            value
                .get("hook_event_name")
                .and_then(serde_json::Value::as_str)
                == Some("SessionStart")
                && value.get("source").and_then(serde_json::Value::as_str) == Some("compact")
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
            history.begin_harness_compaction_fallback();
            return Ok(Ok(()));
        }
        // Claude may start compaction while a tool batch is outstanding, and
        // some credential routes may not expose `/responses/compact`. Neither
        // condition may block Claude's own compaction lifecycle. Fall back to
        // a two-phase clear: allow the compaction inference to replay the old
        // transcript, then discard that temporary replay when
        // `SessionStart(compact)` confirms Claude installed its summary.
        let result = crate::codex_pipeline::validate_canonical_history(&snapshot)
            .and_then(|()| build_pipeline(config, log).map_err(PipelineError::Upstream))
            .and_then(|pipeline| pipeline.compact_canonical_history(snapshot, shutdown))
            .and_then(|replacement| {
                history
                    .install_provider_compaction(&replacement)
                    .map_err(|error| {
                        PipelineError::Translate(crate::codex_translate::TranslateError::Invalid(
                            error.to_string(),
                        ))
                    })
            });
        if let Err(error) = result {
            log_context_compact_fallback(&error, &conversation_key, log);
            history.begin_harness_compaction_fallback();
        }
        Ok(Ok(()))
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

fn serve_context_compact_finished(
    stream: &mut TcpStream,
    conversations: &ConversationStore,
    log: Option<&SharedBridgeLog>,
    conversation_key: ConversationKey,
) {
    let operation = conversations.with_history(&conversation_key.id, |history| {
        Ok(history.finish_harness_compaction_fallback())
    });
    match operation {
        Ok(reset) => {
            if reset {
                if let Some(log) = log {
                    lock_log(log).record(serde_json::json!({
                        "ts_ms": unix_ms(),
                        "event": "context_compact_fallback_finished",
                        "conversation_scope": conversation_key.scope(),
                    }));
                }
            }
            let _ = write_response(stream, 204, "application/json", b"", false);
        }
        Err(error) => {
            let failure = PipelineError::Translate(
                crate::codex_translate::TranslateError::Invalid(error.to_string()),
            );
            let _ = write_pipeline_error(stream, &failure, log);
        }
    }
}

fn log_context_compact_fallback(
    error: &PipelineError,
    conversation_key: &ConversationKey,
    log: Option<&SharedBridgeLog>,
) {
    let Some(log) = log else {
        return;
    };
    let mut event = serde_json::json!({
        "ts_ms": unix_ms(),
        "event": "context_compact_fallback",
        "conversation_scope": conversation_key.scope(),
        "kind": pipeline_error_kind(error),
    });
    if let PipelineError::ContinuationInvariant(failure) = error {
        event["unmatched_call_count"] = failure.unmatched_call_count.into();
        event["input_count"] = failure.input_count.into();
    }
    if let PipelineError::Upstream(UpstreamError::Status(failure)) = error {
        add_failure_fields(&mut event, failure);
    }
    lock_log(log).record(event);
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

/// Route one unified request before the legacy Codex translator sees it.
/// Synthetic IDs are resolved here, never by the legacy `claude*` fallback.
#[allow(clippy::too_many_arguments)]
fn serve_unified_messages(
    stream: &mut TcpStream,
    config: &BridgeConfig,
    shutdown: &AtomicBool,
    conversations: &ConversationStore,
    body: &[u8],
    streaming: bool,
    conversation_key: &ConversationKey,
    headers: &[(String, String)],
    log: Option<&SharedBridgeLog>,
) {
    let GatewayMode::Unified(unified) = &config.gateway_mode else {
        unreachable!("unified request dispatch is guarded by the caller");
    };
    let mut request: serde_json::Value = match serde_json::from_slice(body) {
        Ok(request) => request,
        Err(_) => {
            let _ = write_error(stream, 400);
            return;
        }
    };
    let model = match request.get("model").and_then(serde_json::Value::as_str) {
        Some(model) => model,
        None => {
            let _ = write_error(stream, 400);
            return;
        }
    };
    let exact_catalog = provider_catalog::model_by_discovery_id(model);
    // A Codex discovery selection may retain the bridge's legacy
    // `<model>@<effort>` override. Resolve the reserved base ID before the
    // translator sees its `claude` substring, then carry the suffix onto the
    // reviewed wire model so `effort_for` remains the sole precedence owner.
    let mut codex_effort_suffix = None;
    let catalog = exact_catalog
        .or_else(|| {
            let (base, effort) = model.rsplit_once('@')?;
            let entry = provider_catalog::model_by_discovery_id(base).or_else(|| {
                provider_catalog::non_claude_model_by_any_id(base)
                    .filter(|entry| entry.provider == ModelProvider::Codex)
            })?;
            (entry.provider == ModelProvider::Codex).then(|| {
                codex_effort_suffix = Some(effort);
                entry
            })
        })
        // A persisted or continued session can still name a known provider by
        // wire ID or CLI alias instead of its discovery ID. Resolve those
        // through the shared catalog so they route to their own provider
        // rather than leaking to Anthropic as an "ordinary Claude" model.
        .or_else(|| provider_catalog::non_claude_model_by_any_id(model));
    if model.starts_with("clud-claude-") && catalog.is_none() {
        let ids = unified_catalog_ids(unified);
        let body = serde_json::json!({
            "type": "error",
            "error": {
                "type": "invalid_request_error",
                "message": format!("unknown clud gateway model; available IDs: {}", ids.join(", ")),
            }
        })
        .to_string();
        let _ = write_response(stream, 400, "application/json", body.as_bytes(), false);
        return;
    }
    // What one descent step will send. `model` is `None` only for an ordinary
    // Claude ID, which stays byte-for-byte caller owned.
    struct Attempt {
        provider: ModelProvider,
        route: ConversationRoute,
        model: Option<String>,
        spec: Option<String>,
    }

    let mut attempt = match catalog {
        None => Attempt {
            provider: ModelProvider::Claude,
            route: ConversationRoute::Claude,
            model: None,
            spec: None,
        },
        Some(entry) => Attempt {
            provider: entry.provider,
            route: match entry.provider {
                ModelProvider::Codex => ConversationRoute::Codex,
                ModelProvider::DeepSeek => ConversationRoute::DeepSeek,
                ModelProvider::OpenRouter => ConversationRoute::OpenRouter,
                _ => ConversationRoute::Claude,
            },
            model: Some(codex_effort_suffix.map_or_else(
                || entry.wire_id.to_string(),
                |effort| format!("{}@{effort}", entry.wire_id),
            )),
            spec: None,
        },
    };

    // Descend the ladder. Probing is enabled only while a further rung exists,
    // so a launch that configured no failover takes exactly today's path and
    // never buffers a byte it would not have buffered before.
    loop {
        let payload = match &attempt.model {
            Some(model) => {
                request["model"] = serde_json::Value::String(model.clone());
                serde_json::to_vec(&request).unwrap_or_default()
            }
            None => body.to_vec(),
        };
        let fallback = unified.next_rung(attempt.spec.as_deref(), attempt.route);
        let probe = fallback.is_some();

        let outcome = match attempt.provider {
            ModelProvider::Claude => serve_unified_anthropic_proxy(
                stream,
                conversations,
                conversation_key,
                ConversationRoute::Claude,
                &unified.anthropic_base_url,
                "/v1/messages",
                &payload,
                headers,
                None,
                config.stream_idle_timeout,
                shutdown,
                probe,
            ),
            ModelProvider::DeepSeek if unified.deepseek_api_key.is_some() => {
                serve_unified_anthropic_proxy(
                    stream,
                    conversations,
                    conversation_key,
                    ConversationRoute::DeepSeek,
                    &unified.deepseek_base_url,
                    "/v1/messages",
                    &payload,
                    headers,
                    unified.deepseek_api_key.as_deref(),
                    config.stream_idle_timeout,
                    shutdown,
                    probe,
                )
            }
            ModelProvider::OpenRouter if unified.openrouter_api_key.is_some() => {
                serve_unified_anthropic_proxy(
                    stream,
                    conversations,
                    conversation_key,
                    ConversationRoute::OpenRouter,
                    &unified.openrouter_base_url,
                    "/v1/messages",
                    &payload,
                    headers,
                    unified.openrouter_api_key.as_deref(),
                    config.stream_idle_timeout,
                    shutdown,
                    probe,
                )
            }
            ModelProvider::Codex if unified.codex_available => {
                // Codex is a valid destination but never a probe source: its
                // pipeline commits through a different path, so a descent that
                // lands here stops here.
                serve_messages(
                    stream,
                    config,
                    shutdown,
                    conversations,
                    &payload,
                    streaming,
                    conversation_key,
                    Some(ConversationRoute::Codex),
                    log,
                );
                ProxyOutcome::local(200)
            }
            _ => {
                // Defense in depth: unavailable routes are omitted from
                // discovery, but a stale picker must not reach any paid model.
                let body = serde_json::json!({
                    "type": "error",
                    "error": {
                        "type": "invalid_request_error",
                        "message": "the selected provider is not configured; run `clud auth status`",
                    }
                })
                .to_string();
                let _ = write_response(stream, 400, "application/json", body.as_bytes(), false);
                ProxyOutcome::local(400)
            }
        };

        match outcome {
            ProxyOutcome::Committed { status } => {
                // Only a served turn is evidence the route recovered. A
                // committed *failure* -- a malformed request, an upstream
                // outage -- says nothing about the route, so it must not clear
                // a cooldown the ledger is still serving.
                if status < 400 {
                    unified.record_route_success(attempt.route);
                }
                return;
            }
            ProxyOutcome::Declined(verdict) => {
                let state = unified.record_route(attempt.route, verdict);
                // Recomputed after recording, so the route that just declined
                // is skipped on its own merits rather than by position.
                let Some(rung) = unified.next_rung(attempt.spec.as_deref(), attempt.route) else {
                    // Nothing left to try. Re-issue against the same route with
                    // probing off so the client receives the real upstream
                    // status instead of a silent hang.
                    warn_route_exhausted(attempt.route, verdict, state, None);
                    attempt.spec = None;
                    return finish_without_failover(
                        stream,
                        config,
                        unified,
                        conversations,
                        conversation_key,
                        headers,
                        &payload,
                        attempt.route,
                        shutdown,
                    );
                };
                warn_route_exhausted(attempt.route, verdict, state, Some(&rung));
                attempt = Attempt {
                    provider: rung.provider,
                    route: rung.route,
                    model: Some(rung.wire_id.clone()),
                    spec: Some(rung.spec.clone()),
                };
            }
        }
    }
}

/// One sanitized line per transition. Names the route, the reason, and the
/// rung taken -- never a credential, prompt, or response body.
fn warn_route_exhausted(
    route: ConversationRoute,
    verdict: RouteVerdict,
    state: Option<RouteState>,
    taken: Option<&FailoverRung>,
) {
    let clock = match state {
        Some(RouteState::Cooling { remaining, .. }) => {
            format!(" (retry in {}s)", remaining.as_secs())
        }
        _ => String::new(),
    };
    match taken {
        Some(rung) => eprintln!(
            "[clud] route: {} {}{} -> continuing on {}",
            route.as_str(),
            verdict.reason(),
            clock,
            rung.label(),
        ),
        None => eprintln!(
            "[clud] route: {} {}{}; no configured fallback is available",
            route.as_str(),
            verdict.reason(),
            clock,
        ),
    }
}

/// Replay the last attempt with probing disabled so the client sees the real
/// upstream response. Only reached once the ladder is spent.
#[allow(clippy::too_many_arguments)]
fn finish_without_failover(
    stream: &mut TcpStream,
    config: &BridgeConfig,
    unified: &UnifiedGatewayConfig,
    conversations: &ConversationStore,
    conversation_key: &ConversationKey,
    headers: &[(String, String)],
    payload: &[u8],
    route: ConversationRoute,
    shutdown: &AtomicBool,
) {
    let (base_url, key) = match route {
        ConversationRoute::DeepSeek => (
            unified.deepseek_base_url.as_str(),
            unified.deepseek_api_key.as_deref(),
        ),
        ConversationRoute::OpenRouter => (
            unified.openrouter_base_url.as_str(),
            unified.openrouter_api_key.as_deref(),
        ),
        _ => (unified.anthropic_base_url.as_str(), None),
    };
    serve_unified_anthropic_proxy(
        stream,
        conversations,
        conversation_key,
        route,
        base_url,
        "/v1/messages",
        payload,
        headers,
        key,
        config.stream_idle_timeout,
        shutdown,
        false,
    );
}

#[allow(clippy::too_many_arguments)]
fn serve_unified_anthropic_proxy(
    stream: &mut TcpStream,
    conversations: &ConversationStore,
    conversation_key: &ConversationKey,
    route: ConversationRoute,
    base_url: &str,
    path: &str,
    body: &[u8],
    headers: &[(String, String)],
    injected_api_key: Option<&str>,
    idle_timeout: Duration,
    shutdown: &AtomicBool,
    probe: bool,
) -> ProxyOutcome {
    let mut outcome = ProxyOutcome::local(500);
    let routed = conversations.with_history(&conversation_key.id, |history| {
        // Crossing a provider boundary starts a new route epoch, which is
        // exactly what a failover is: a provider switch nobody typed.
        history.enter_route(route);
        outcome = serve_anthropic_proxy(
            stream,
            AnthropicProxyTarget {
                base_url,
                path,
                injected_api_key,
            },
            body,
            headers,
            idle_timeout,
            shutdown,
            probe,
        );
        Ok(())
    });
    if let Err(error) = routed {
        let failure = PipelineError::Translate(crate::codex_translate::TranslateError::Invalid(
            error.to_string(),
        ));
        let _ = write_pipeline_error(stream, &failure, None);
        return ProxyOutcome::local(500);
    }
    outcome
}

/// Unified token counting has one Anthropic-compatible contract: ordinary
/// Claude model IDs proxy upstream, while synthetic provider routes return an
/// explicit 404 so Claude Code falls back to its documented local estimation.
#[allow(clippy::too_many_arguments)]
fn serve_unified_count_tokens(
    stream: &mut TcpStream,
    config: &BridgeConfig,
    shutdown: &AtomicBool,
    conversations: &ConversationStore,
    body: &[u8],
    conversation_key: &ConversationKey,
    headers: &[(String, String)],
) {
    let GatewayMode::Unified(unified) = &config.gateway_mode else {
        unreachable!("unified token-count dispatch is guarded by the caller");
    };
    let request: serde_json::Value = match serde_json::from_slice(body) {
        Ok(request) => request,
        Err(_) => {
            let _ = write_error(stream, 400);
            return;
        }
    };
    let Some(model) = request.get("model").and_then(serde_json::Value::as_str) else {
        let _ = write_error(stream, 400);
        return;
    };
    let catalog = provider_catalog::model_by_discovery_id(model)
        .or_else(|| provider_catalog::non_claude_model_by_any_id(model));
    if model.starts_with("clud-claude-") && catalog.is_none() {
        let _ = write_error(stream, 400);
        return;
    }
    if catalog.is_some() {
        let response = br#"{"error":{"type":"not_found_error","message":"token counting is not supported for this unified provider route"}}"#;
        let _ = write_response(stream, 404, "application/json", response, false);
        return;
    }
    // Token counting is never failed over: it is a cheap advisory call whose
    // answer is provider-specific, so a count from a fallback route would be
    // wrong rather than merely late.
    serve_unified_anthropic_proxy(
        stream,
        conversations,
        conversation_key,
        ConversationRoute::Claude,
        &unified.anthropic_base_url,
        "/v1/messages/count_tokens",
        body,
        headers,
        None,
        config.stream_idle_timeout,
        shutdown,
        false,
    );
}

fn unified_catalog_ids(config: &UnifiedGatewayConfig) -> Vec<&'static str> {
    provider_catalog::MODELS
        .iter()
        .filter(|entry| match entry.provider {
            ModelProvider::Codex => config.codex_available,
            ModelProvider::DeepSeek => config.deepseek_api_key.is_some(),
            // Phase 4 of #937 wires Kimi's unified route.
            ModelProvider::Kimi => false,
            ModelProvider::OpenRouter => config.openrouter_api_key.is_some(),
            ModelProvider::Claude => false,
        })
        .filter_map(|entry| entry.discovery_id)
        .collect()
}

/// Proxy an Anthropic-compatible Messages response without buffering its body.
/// The caller's Claude credential is retained only for native Claude requests;
/// DeepSeek injects its vault credential and never receives caller credentials.
struct AnthropicProxyTarget<'a> {
    base_url: &'a str,
    path: &'a str,
    injected_api_key: Option<&'a str>,
}

/// What one proxy attempt did to the client connection.
///
/// The distinction is the whole basis of failover: until a byte has been
/// written the status is still ours to choose, so a route-terminal failure can
/// be replayed elsewhere and the client never learns a provider declined.
/// Afterwards the status is committed and the only honest move is to finish
/// the turn.
#[derive(Debug)]
enum ProxyOutcome {
    /// Bytes reached the client. Nothing may be retried. Carries the status so
    /// the caller can tell a served turn from a committed failure: only the
    /// former is evidence that the route recovered.
    Committed { status: u16 },
    /// Nothing was written. The route declined with this verdict.
    Declined(RouteVerdict),
}

impl ProxyOutcome {
    /// A status the gateway produced itself, with no upstream verdict behind
    /// it. Treated as a committed failure so it never clears a cooldown.
    const fn local(status: u16) -> Self {
        Self::Committed { status }
    }
}

/// Bounded prefix read from a failing upstream so it can be classified.
///
/// Large enough for any provider error envelope, small enough that a
/// misbehaving upstream cannot make the gateway buffer a response — the
/// success path still streams without buffering.
const FAILURE_PREFIX_BYTES: usize = 8 * 1024;

/// Statuses worth reading a body for before committing. Everything else is
/// either a success or a failure whose meaning the status already fixes, and
/// buffering those would only delay the client.
fn may_be_route_terminal(status: u16) -> bool {
    matches!(status, 401 | 402 | 403 | 429)
}

fn serve_anthropic_proxy(
    stream: &mut TcpStream,
    target: AnthropicProxyTarget<'_>,
    body: &[u8],
    headers: &[(String, String)],
    idle_timeout: Duration,
    shutdown: &AtomicBool,
    probe: bool,
) -> ProxyOutcome {
    let mut request = ureq::post(&format!(
        "{}{}",
        target.base_url.trim_end_matches('/'),
        target.path
    ))
    .set("Content-Type", "application/json");
    for (name, value) in headers {
        let forwarded = name.eq_ignore_ascii_case("authorization")
            || name.eq_ignore_ascii_case("x-api-key")
            || name.eq_ignore_ascii_case("anthropic-version")
            || name.to_ascii_lowercase().starts_with("anthropic-")
            || name.eq_ignore_ascii_case("accept");
        let hop_by_hop = name.eq_ignore_ascii_case("host")
            || name.eq_ignore_ascii_case("connection")
            || name.eq_ignore_ascii_case("content-length")
            || name.eq_ignore_ascii_case(UNIFIED_GATEWAY_TOKEN_HEADER);
        let caller_credential =
            name.eq_ignore_ascii_case("authorization") || name.eq_ignore_ascii_case("x-api-key");
        if forwarded && !hop_by_hop && (target.injected_api_key.is_none() || !caller_credential) {
            request = request.set(name, value);
        }
    }
    if let Some(api_key) = target.injected_api_key {
        request = request.set("Authorization", &format!("Bearer {api_key}"));
    }
    let response = match request.timeout(idle_timeout).send_bytes(body) {
        Ok(response) => response,
        Err(ureq::Error::Status(_, response)) => response,
        Err(ureq::Error::Transport(_)) => {
            let _ = write_response(
                stream,
                502,
                "application/json",
                br#"{"error":{"type":"api_error","message":"gateway upstream unavailable"}}"#,
                false,
            );
            return ProxyOutcome::local(502);
        }
    };
    let status = response.status();
    let content_type = response
        .header("content-type")
        .unwrap_or("application/json")
        .to_string();
    let retry_after = response.header("retry-after").map(str::to_string);
    let request_id = response.header("request-id").map(str::to_string);
    // Captured before `into_reader` consumes the response, so a probe can
    // classify without a second round trip.
    let probe_headers: Vec<(String, String)> = if probe && may_be_route_terminal(status) {
        ["retry-after", "x-request-id", "cf-ray"]
            .into_iter()
            .filter_map(|name| {
                response
                    .header(name)
                    .map(|value| (name.to_string(), value.to_string()))
            })
            .collect()
    } else {
        Vec::new()
    };
    let mut reader = response.into_reader();

    // Pre-commit probe. Nothing has been written yet, so if this route is spent
    // the caller can replay the identical body elsewhere and the client never
    // sees that a provider declined. A prefix that does not read as
    // route-terminal falls through and is re-emitted below, so the probe costs
    // the client nothing.
    let mut prefix = Vec::new();
    if probe && may_be_route_terminal(status) {
        let mut window = [0_u8; 1024];
        while prefix.len() < FAILURE_PREFIX_BYTES {
            match reader.read(&mut window) {
                Ok(0) | Err(_) => break,
                Ok(count) => prefix.extend_from_slice(&window[..count]),
            }
        }
        let text = String::from_utf8_lossy(&prefix);
        let failure = UpstreamFailure::from_parts(
            status,
            |name| {
                probe_headers
                    .iter()
                    .find(|(header, _)| header.eq_ignore_ascii_case(name))
                    .map(|(_, value)| value.clone())
            },
            &text,
            crate::route_health::MAX_COOLDOWN,
        );
        let verdict = RouteVerdict::from_failure(&failure);
        if verdict.fails_over() {
            return ProxyOutcome::Declined(verdict);
        }
    }

    let _ = stream.set_write_timeout(Some(idle_timeout));
    let _ = write!(
        stream,
        "HTTP/1.1 {status} Upstream\r\nContent-Type: {content_type}\r\nTransfer-Encoding: chunked\r\nCache-Control: no-store\r\nConnection: close\r\n"
    );
    if let Some(retry_after) = retry_after {
        let _ = write!(stream, "Retry-After: {retry_after}\r\n");
    }
    if let Some(request_id) = request_id {
        let _ = write!(stream, "request-id: {request_id}\r\n");
    }
    if stream.write_all(b"\r\n").is_err() || stream.flush().is_err() {
        return ProxyOutcome::Committed { status };
    }
    // Anything the probe already consumed has to go out first, or the client
    // would receive a truncated error envelope.
    if !prefix.is_empty()
        && (write!(stream, "{:x}\r\n", prefix.len())
            .and_then(|()| stream.write_all(&prefix))
            .and_then(|()| stream.write_all(b"\r\n"))
            .and_then(|()| stream.flush()))
        .is_err()
    {
        return ProxyOutcome::Committed { status };
    }
    let mut chunk = [0_u8; 8192];
    loop {
        if shutdown.load(Ordering::Relaxed) {
            break;
        }
        match reader.read(&mut chunk) {
            Ok(0) | Err(_) => break,
            Ok(count) => {
                if write!(stream, "{count:x}\r\n")
                    .and_then(|()| stream.write_all(&chunk[..count]))
                    .and_then(|()| stream.write_all(b"\r\n"))
                    .and_then(|()| stream.flush())
                    .is_err()
                {
                    break;
                }
            }
        }
    }
    let _ = stream.write_all(b"0\r\n\r\n");
    let _ = stream.flush();
    let _ = stream.shutdown(Shutdown::Both);
    ProxyOutcome::Committed { status }
}

#[allow(clippy::too_many_arguments)]
fn serve_codex_discovery_messages(
    stream: &mut TcpStream,
    config: &BridgeConfig,
    shutdown: &AtomicBool,
    conversations: &ConversationStore,
    body: &[u8],
    streaming: bool,
    conversation_key: &ConversationKey,
    log: Option<&SharedBridgeLog>,
) {
    let mut request: serde_json::Value = match serde_json::from_slice(body) {
        Ok(request) => request,
        Err(_) => {
            let _ = write_error(stream, 400);
            return;
        }
    };
    let Some(model) = request.get("model").and_then(serde_json::Value::as_str) else {
        let _ = write_error(stream, 400);
        return;
    };
    let (base, effort) = model
        .rsplit_once('@')
        .map_or((model, None), |(base, effort)| (base, Some(effort)));
    // A persisted or continued session can name a Codex row by wire ID or CLI
    // alias instead of its discovery ID, exactly as on the unified route.
    let known = provider_catalog::model_by_discovery_id(base)
        .or_else(|| provider_catalog::non_claude_model_by_any_id(base));
    let discovered = known.filter(|entry| entry.provider == ModelProvider::Codex);
    // Claude Code merges this gateway's rows with its own built-in catalog, so
    // IDs the gateway never advertised do arrive here. Only a `claude*` ID is
    // caller-owned -- `codex_translate::resolve_selection` maps those onto the
    // reviewed default. Anything else the bridge cannot resolve is refused
    // here: forwarding it unrewritten would translate an unknown ID to the
    // Codex upstream, which answers with an error about a model the user never
    // knowingly sent there (#997).
    if discovered.is_none() && !base.to_ascii_lowercase().starts_with("claude") {
        let ids = provider_catalog::models_for_provider(ModelProvider::Codex)
            .filter_map(|entry| entry.discovery_id)
            .collect::<Vec<_>>()
            .join(", ");
        // Two different failures land here and they need different answers
        // (#1000): a model clud has never heard of is the caller's to fix,
        // while a model clud does know but this bridge does not serve is a
        // clud-side limit -- saying so stops the user debugging their own
        // model choice.
        let (message, reason) = if known.is_some() {
            (
                format!(
                    "clud knows the model '{base}', but this Codex gateway is not \
                     serving it; available IDs: {ids}"
                ),
                "model_not_served_here",
            )
        } else {
            (
                format!("unknown clud Codex model '{base}'; available IDs: {ids}"),
                "unknown_model",
            )
        };
        // #999: the terminal names the rejected model and the log did not, so a
        // session wedged by a model selection left no evidence behind. The two
        // cases stay as distinguishable here as they are to the user.
        record_model_rejection(log, 400, reason, base);
        let body = serde_json::json!({
            "type": "error",
            "error": {
                "type": "invalid_request_error",
                "message": message,
            }
        })
        .to_string();
        let _ = write_response(stream, 400, "application/json", body.as_bytes(), false);
        return;
    }
    if let Some(entry) = discovered {
        let wire_model = effort.map_or_else(
            || entry.wire_id.to_string(),
            |effort| format!("{}@{effort}", entry.wire_id),
        );
        request["model"] = serde_json::Value::String(wire_model);
    }
    let rewritten = serde_json::to_vec(&request).unwrap_or_default();
    serve_messages(
        stream,
        config,
        shutdown,
        conversations,
        &rewritten,
        streaming,
        conversation_key,
        None,
        log,
    );
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
    unified_route: Option<ConversationRoute>,
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
            if let Some(route) = unified_route {
                history.enter_route(route);
            }
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
                if summary.orphaned_outputs_repaired > 0 {
                    log_orphaned_outputs_repaired(log, summary.orphaned_outputs_repaired);
                }
                if summary.pending_outputs_recovered > 0 {
                    log_pending_outputs_recovered(
                        log,
                        conversation_key,
                        summary.pending_outputs_recovered,
                    );
                }
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
                log_continuation_invariant(&error, conversation_key, log);
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
        if let Some(route) = unified_route {
            history.enter_route(route);
        }
        let completed = pipeline
            .complete_with_history(body, &message_id, shutdown, history)
            .map(|completion| {
                if completion.pending_outputs_recovered > 0 {
                    log_pending_outputs_recovered(
                        log,
                        conversation_key,
                        completion.pending_outputs_recovered,
                    );
                }
                let rendered = serde_json::to_vec(&completion.message).unwrap_or_default();
                if write_response(stream, 200, "application/json", &rendered, false).is_ok() {
                    completion.clear_history_after_client_commit(history);
                }
            });
        Ok(completed)
    }) {
        Ok(Ok(())) => {}
        Ok(Err(error)) => {
            log_continuation_invariant(&error, conversation_key, log);
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

fn log_continuation_invariant(
    error: &PipelineError,
    conversation_key: &ConversationKey,
    log: Option<&SharedBridgeLog>,
) {
    let PipelineError::ContinuationInvariant(failure) = error else {
        return;
    };
    let event = serde_json::json!({
        "ts_ms": unix_ms(),
        "event": "continuation_invariant_failure",
        "downstream_status": 400,
        "conversation_scope": conversation_key.scope(),
        "input_count": failure.input_count,
        "input_kinds": failure.input_kinds,
        "function_call_count": failure.function_call_count,
        "function_call_output_count": failure.function_call_output_count,
        "unmatched_call_count": failure.unmatched_call_count,
        "source": failure.source,
    });
    if let Some(log) = log {
        lock_log(log).record(event.clone());
    }
    if std::env::var_os("CLUD_CODEX_BRIDGE_DEBUG").is_some_and(|value| value == "1") {
        eprintln!("[clud] codex bridge: {event}");
    }
}

/// Record that a turn rewrote orphaned tool results to keep the Responses
/// API from rejecting it outright. Counts only — no call ids, no outputs.
///
/// Worth logging because the untreated form of this is a *permanent* 400
/// that wedges a session until the process restarts, and the bridge log is
/// what identified it.
fn log_orphaned_outputs_repaired(log: Option<&SharedBridgeLog>, repaired: usize) {
    let event = serde_json::json!({
        "ts_ms": unix_ms(),
        "event": "orphaned_outputs_repaired",
        "repaired_count": repaired,
        "reason": "history_compaction_elided_the_originating_call",
    });
    if let Some(log) = log {
        lock_log(log).record(event.clone());
    }
    if std::env::var_os("CLUD_CODEX_BRIDGE_DEBUG").is_some_and(|value| value == "1") {
        eprintln!("[clud] codex bridge: {event}");
    }
}

/// Record that canonical continuation assembly found real, previously omitted
/// tool outputs in Claude's full replay. Counts and scope only: the identifiers
/// and output payloads remain request-local.
fn log_pending_outputs_recovered(
    log: Option<&SharedBridgeLog>,
    conversation_key: &ConversationKey,
    recovered: usize,
) {
    let event = serde_json::json!({
        "ts_ms": unix_ms(),
        "event": "pending_outputs_recovered",
        "conversation_scope": conversation_key.scope(),
        "recovered_count": recovered,
    });
    if let Some(log) = log {
        lock_log(log).record(event.clone());
    }
    if std::env::var_os("CLUD_CODEX_BRIDGE_DEBUG").is_some_and(|value| value == "1") {
        eprintln!("[clud] codex bridge: {event}");
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

/// Record one contiguous interval in which new sockets remain in the kernel
/// listener backlog. No request-derived field enters this event.
fn record_admission_queued(log: Option<&SharedBridgeLog>) {
    if let Some(log) = log {
        lock_log(log).record_ambient(serde_json::json!({
            "ts_ms": unix_ms(),
            "event": "admission_queued",
        }));
    }
}

/// The matching admission event is deliberately distinct from
/// `upstream_attempt`: queue time is local scheduling, not a retried provider
/// request. `wait_ms` is an aggregate backlog interval and contains no client
/// identity, header, token, or request content.
fn record_admission_acquired(log: Option<&SharedBridgeLog>, wait: Duration) {
    if let Some(log) = log {
        lock_log(log).record_ambient(serde_json::json!({
            "ts_ms": unix_ms(),
            "event": "admission_acquired",
            "wait_ms": wait.as_millis() as u64,
        }));
    }
}

/// Longest model ID persisted. The field comes from a request body capped only
/// at `DEFAULT_MAX_BODY_BYTES`, and a single ~1 MiB value would exhaust the
/// log's budget and silence every failure after it -- the incident the log
/// exists for. No real ID is close to this.
const MAX_LOGGED_MODEL_CHARS: usize = 128;

/// A rejection whose cause is the model the caller asked for, recorded with
/// that ID (#999). `reason` alone cannot say *which* selection wedged the
/// session, and the model ID is the one request-body field worth persisting.
fn record_model_rejection(
    log: Option<&SharedBridgeLog>,
    status: u16,
    reason: &'static str,
    model: &str,
) {
    if let Some(log) = log {
        let model = model
            .chars()
            .take(MAX_LOGGED_MODEL_CHARS)
            .collect::<String>();
        lock_log(log).record(serde_json::json!({
            "ts_ms": unix_ms(),
            "event": "request_rejected",
            "downstream_status": status,
            "reason": reason,
            "model": model,
        }));
    }
}

/// The model IDs a `GET /v1/models` advertised (#999).
///
/// Not a failure, so it widens the log's contract deliberately: when a model
/// selection wedges a session this is often the only surviving evidence of
/// what the bridge offered, and a refusal is only readable against it. Recorded
/// as ambient context so it does not, on its own, claim an operator's attention
/// through the shutdown hint.
fn record_catalog_advertised(log: Option<&SharedBridgeLog>, model_ids: &[&str]) {
    if let Some(log) = log {
        lock_log(log).record_ambient(serde_json::json!({
            "ts_ms": unix_ms(),
            "event": "catalog_advertised",
            "model_ids": model_ids,
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
        PipelineError::ContinuationInvariant(_) => "continuation_invariant",
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
    headers: Vec<(String, String)>,
    authorization: Option<String>,
    session_id: Option<String>,
    agent_id: Option<String>,
    content_length: usize,
    body_prefix: Vec<u8>,
}

impl ParsedRequest {
    fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(candidate, _)| candidate.eq_ignore_ascii_case(name))
            .map(|(_, value)| value.as_str())
    }
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
    let mut headers = Vec::new();
    let mut authorization = None;
    let mut session_id = None;
    let mut agent_id = None;
    let mut content_length = 0_usize;
    for line in lines {
        let (name, value) = line.split_once(':').ok_or(400_u16)?;
        let value = value.trim();
        if value.contains(['\r', '\n']) {
            return Err(400);
        }
        headers.push((name.to_string(), value.to_string()));
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
        headers,
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

    fn not_found_response() -> Vec<u8> {
        let body = r#"{"error":{"type":"not_found_error","message":"not found"}}"#;
        format!(
            "HTTP/1.1 404 Not Found\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
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

    fn visible_transport_failure_response() -> Vec<u8> {
        let events = "event: response.created\ndata: {\"type\":\"response.created\"}\n\n\
                      event: response.output_text.delta\ndata: {\"type\":\"response.output_text.delta\",\"output_index\":0,\"content_index\":0,\"delta\":\"partial\"}\n\n";
        format!(
            "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{events}",
            events.len() + 1024
        )
        .into_bytes()
    }

    fn failed_turn_replay_body(mut suffix: Vec<serde_json::Value>) -> String {
        let mut messages = vec![
            serde_json::json!({"role": "user", "content": "hi"}),
            serde_json::json!({
                "role": "assistant",
                "content": [{
                    "type": "tool_use",
                    "id": "call_parent",
                    "name": "Workflow",
                    "input": {},
                }],
            }),
            serde_json::json!({
                "role": "user",
                "content": [{
                    "type": "tool_result",
                    "tool_use_id": "call_parent",
                    "content": "result-parent",
                }],
            }),
        ];
        messages.append(&mut suffix);
        serde_json::json!({
            "model": "claude-x",
            "messages": messages,
            "stream": true,
        })
        .to_string()
    }

    fn parallel_function_call_response(count: usize) -> Vec<u8> {
        let mut events =
            String::from("event: response.created\ndata: {\"type\":\"response.created\"}\n\n");
        for index in 0..count {
            events.push_str(&format!(
                "event: response.output_item.added\ndata: {{\"type\":\"response.output_item.added\",\"output_index\":{index},\"item\":{{\"type\":\"function_call\",\"call_id\":\"call_{index}\",\"name\":\"Bash\"}}}}\n\n\
                 event: response.function_call_arguments.delta\ndata: {{\"type\":\"response.function_call_arguments.delta\",\"output_index\":{index},\"delta\":\"{{}}\"}}\n\n\
                 event: response.output_item.done\ndata: {{\"type\":\"response.output_item.done\",\"output_index\":{index},\"item\":{{\"type\":\"function_call\",\"call_id\":\"call_{index}\",\"name\":\"Bash\",\"arguments\":\"{{}}\"}}}}\n\n"
            ));
        }
        events.push_str(
            "event: response.completed\ndata: {\"type\":\"response.completed\",\"response\":{\"usage\":{\"input_tokens\":3,\"output_tokens\":8}}}\n\n",
        );
        response_with_events(&events)
    }

    fn split_parallel_result_body(count: usize, include_every_result: bool) -> String {
        let calls = (0..count)
            .map(|index| {
                serde_json::json!({
                    "type": "tool_use",
                    "id": format!("call_{index}"),
                    "name": "Bash",
                    "input": {},
                })
            })
            .collect::<Vec<_>>();
        let mut messages = vec![
            serde_json::json!({"role": "user", "content": "inspect"}),
            serde_json::json!({"role": "assistant", "content": calls}),
        ];
        if include_every_result {
            messages.extend((0..count).map(|index| {
                serde_json::json!({
                    "role": "user",
                    "content": [{
                        "type": "tool_result",
                        "tool_use_id": format!("call_{index}"),
                        "content": format!("result-{index}"),
                    }],
                })
            }));
        } else {
            messages.push(serde_json::json!({"role": "user", "content": "missing results"}));
        }
        serde_json::json!({
            "model": "claude-x",
            "messages": messages,
            "stream": false,
        })
        .to_string()
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

    fn unified_config(
        anthropic: &FakeResponses,
        codex: &FakeResponses,
        deepseek: &FakeResponses,
    ) -> BridgeConfig {
        BridgeConfig::default()
            .with_test_upstream_url(Some(codex.base_url.clone()))
            .with_unified_gateway(
                UnifiedGatewayConfig::new(Some("deepseek-route-canary".to_string()), true)
                    .with_upstreams(anthropic.base_url.clone(), deepseek.base_url.clone()),
            )
    }

    fn unified_request(
        bridge: &BridgeHandle,
        body: &str,
        session: &str,
        agent: Option<&str>,
    ) -> String {
        let mut headers = vec![
            (UNIFIED_GATEWAY_TOKEN_HEADER, bridge.bearer_token()),
            ("x-api-key", "claude-api-key-canary"),
            ("anthropic-version", "2023-06-01"),
            ("anthropic-beta", "oauth-2025-04-20,fixture-capability"),
            ("x-claude-code-session-id", session),
        ];
        if let Some(agent) = agent {
            headers.push(("x-claude-code-agent-id", agent));
        }
        request(
            bridge.socket_addr(),
            &authorized_with_headers(
                "POST",
                "/v1/messages?beta=true",
                "claude-oauth-canary",
                body,
                &headers,
            ),
        )
    }

    fn captured_body(raw_request: &str) -> &str {
        raw_request
            .split_once("\r\n\r\n")
            .map(|(_, body)| body)
            .expect("captured HTTP body")
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

    /// Issue #955: a future Codex row must become both discoverable and
    /// routable without adding another bridge-specific model list.
    #[test]
    fn direct_codex_catalog_advertises_and_routes_every_registered_row() {
        let upstream = FakeResponses::start();
        let bridge = BridgeHandle::start(bridged_config(&upstream)).unwrap();
        let token = bridge.bearer_token().to_owned();
        let catalog_response = request(
            bridge.socket_addr(),
            &authorized("GET", "/v1/models?limit=1000", &token, ""),
        );
        assert_eq!(status(&catalog_response), 200, "{catalog_response}");
        let catalog: serde_json::Value = serde_json::from_str(
            catalog_response
                .split("\r\n\r\n")
                .nth(1)
                .expect("catalog response body"),
        )
        .unwrap();
        let ids = catalog["data"]
            .as_array()
            .unwrap()
            .iter()
            .map(|entry| entry["id"].as_str().unwrap())
            .collect::<Vec<_>>();
        let registered = provider_catalog::models_for_provider(ModelProvider::Codex)
            .map(|entry| {
                (
                    entry.discovery_id.expect("Codex discovery id"),
                    entry.wire_id,
                )
            })
            .collect::<Vec<_>>();
        assert_eq!(
            ids,
            registered
                .iter()
                .map(|(discovery_id, _)| *discovery_id)
                .collect::<Vec<_>>()
        );

        for (discovery_id, wire_id) in &registered {
            let body = PROBE_BODY.replace("claude-x", &format!("{discovery_id}@high"));
            let response = request(
                bridge.socket_addr(),
                &authorized("POST", "/v1/messages", &token, &body),
            );
            assert_eq!(status(&response), 200, "{discovery_id}: {response}");
            let requests = upstream.requests();
            let sent: serde_json::Value =
                serde_json::from_str(captured_body(requests.last().unwrap())).unwrap();
            assert_eq!(sent["model"], *wire_id);
            assert_eq!(sent["reasoning"]["effort"], "high");
        }

        let unknown = PROBE_BODY.replace("claude-x", "clud-claude-codex-nova");
        let response = request(
            bridge.socket_addr(),
            &authorized("POST", "/v1/messages", &token, &unknown),
        );
        assert_eq!(status(&response), 400, "{response}");
        assert_eq!(upstream.requests().len(), registered.len());
    }

    /// Issue #997: only a `claude*` ID is caller-owned on this route. Any
    /// other ID the bridge cannot resolve must be refused here rather than
    /// translated to the Codex upstream, which would answer with an error
    /// about a model the user never knowingly sent there.
    ///
    /// Issue #1000: the two refusals must not read alike. A model clud has
    /// never heard of is the caller's to fix; a model clud does know but this
    /// gateway does not serve is a clud-side limit and must say so.
    #[test]
    fn an_unresolvable_non_claude_model_is_refused_before_the_translator() {
        let upstream = FakeResponses::start();
        let bridge = BridgeHandle::start(bridged_config(&upstream)).unwrap();
        let body = PROBE_BODY.replace("claude-x", "gpt-6-nonexistent");
        let response = request(
            bridge.socket_addr(),
            &authorized("POST", "/v1/messages", bridge.bearer_token(), &body),
        );
        assert_eq!(status(&response), 400, "{response}");
        assert!(response.contains("invalid_request_error"), "{response}");
        assert!(
            response.contains("unknown clud Codex model 'gpt-6-nonexistent'"),
            "{response}"
        );
        assert!(!response.contains("clud knows the model"), "{response}");

        // Case 2: a real catalog row, served by another provider's route.
        let known = PROBE_BODY.replace("claude-x", "clud-claude-kimi-k3");
        let refused = request(
            bridge.socket_addr(),
            &authorized("POST", "/v1/messages", bridge.bearer_token(), &known),
        );
        assert_eq!(status(&refused), 400, "{refused}");
        assert!(
            refused.contains("clud knows the model 'clud-claude-kimi-k3'"),
            "{refused}"
        );
        assert!(!refused.contains("unknown clud Codex model"), "{refused}");
        // Both refusals still say what this gateway can serve.
        for body in [&response, &refused] {
            assert!(body.contains("clud-claude-codex-terra"), "{body}");
        }
        assert!(
            upstream.requests().is_empty(),
            "a refused model must not reach the Codex upstream"
        );
    }

    /// Issue #999: a session wedged by a model selection left nothing in the
    /// bridge log. The advertised set is the evidence that survives when
    /// nothing else does.
    #[test]
    fn a_catalog_fetch_records_the_advertised_model_ids() {
        let upstream = FakeResponses::start();
        let tmp = tempfile::tempdir().unwrap();
        let log_path = tmp.path().join("bridge.jsonl");
        let mut bridge =
            BridgeHandle::start(bridged_config(&upstream).with_log_path(log_path.clone())).unwrap();
        let bearer = bridge.bearer_token().to_string();
        let response = request(
            bridge.socket_addr(),
            &authorized("GET", "/v1/models?limit=1000", &bearer, ""),
        );
        assert_eq!(status(&response), 200, "{response}");
        bridge.shutdown().unwrap();

        let text = std::fs::read_to_string(log_path).unwrap();
        assert!(text.contains(r#""event":"catalog_advertised""#), "{text}");
        assert!(text.contains("clud-claude-codex-terra"), "{text}");
        assert!(!text.contains(&bearer), "{text}");
        // Ambient context only: the shutdown hint stays a signal that this
        // launch left something worth reading, not a footer on every session.
        let log = bridge.log.as_ref().expect("configured log");
        assert!(!lock_log(log).has_notable_records());
    }

    /// Issue #999: the refusal added by #1005 named the model to the user but
    /// not to the log, and #1009's two cases must stay distinguishable there.
    #[test]
    fn a_refused_model_is_recorded_with_its_id_and_case() {
        let upstream = FakeResponses::start();
        let tmp = tempfile::tempdir().unwrap();
        let log_path = tmp.path().join("bridge.jsonl");
        let mut bridge =
            BridgeHandle::start(bridged_config(&upstream).with_log_path(log_path.clone())).unwrap();
        let bearer = bridge.bearer_token().to_string();
        let oversized = "x".repeat(5000);
        for model in ["gpt-6-nonexistent", "clud-claude-kimi-k3", &oversized] {
            let body = PROBE_BODY.replace("claude-x", model);
            let refused = request(
                bridge.socket_addr(),
                &authorized("POST", "/v1/messages", &bearer, &body),
            );
            assert_eq!(status(&refused), 400, "{refused}");
        }
        bridge.shutdown().unwrap();

        let text = std::fs::read_to_string(log_path).unwrap();
        // Serialized field order is serde_json's (sorted): model, then reason.
        assert!(
            text.contains(r#""model":"gpt-6-nonexistent","reason":"unknown_model""#),
            "{text}"
        );
        assert!(
            text.contains(r#""model":"clud-claude-kimi-k3","reason":"model_not_served_here""#),
            "{text}"
        );
        // The model field comes from a 32 MiB-capped body: one oversized value
        // would otherwise exhaust the 1 MiB budget and silence every later
        // failure, so it is truncated before it reaches the log.
        assert!(text.contains(&"x".repeat(MAX_LOGGED_MODEL_CHARS)), "{text}");
        assert!(
            !text.contains(&"x".repeat(MAX_LOGGED_MODEL_CHARS + 1)),
            "{text}"
        );
        assert!(!text.contains(&bearer), "{text}");
    }

    #[test]
    fn unified_catalog_requires_its_custom_token_and_omits_unavailable_routes() {
        let fake = FakeResponses::start();
        let config = BridgeConfig::default().with_unified_gateway(
            UnifiedGatewayConfig::new(Some("deepseek-test-secret".to_string()), true)
                .with_upstreams(fake.base_url.clone(), fake.base_url.clone()),
        );
        let bridge = BridgeHandle::start(config).unwrap();
        let addr = bridge.socket_addr();
        let missing = request(
            addr,
            &authorized("GET", "/v1/models?limit=1000", bridge.bearer_token(), ""),
        );
        assert_eq!(status(&missing), 401, "bearer auth is never unified auth");
        let response = request(
            addr,
            &authorized_with_headers(
                "GET",
                "/v1/models?limit=1000",
                "native-claude-credential",
                "",
                &[(UNIFIED_GATEWAY_TOKEN_HEADER, bridge.bearer_token())],
            ),
        );
        assert_eq!(status(&response), 200, "{response}");
        let catalog: serde_json::Value = serde_json::from_str(
            response
                .split("\r\n\r\n")
                .nth(1)
                .expect("catalog response body"),
        )
        .unwrap();
        let rows = catalog["data"].as_array().unwrap();
        let ids = rows
            .iter()
            .map(|row| row["id"].as_str().unwrap())
            .collect::<Vec<_>>();
        assert_eq!(
            ids,
            vec![
                "clud-claude-codex-sol",
                "clud-claude-codex-terra",
                "clud-claude-codex-luna",
                "clud-claude-deepseek-v4-pro-0813",
                "clud-claude-deepseek-v4-flash",
            ],
            "catalog ordering is part of the picker contract"
        );
        assert_eq!(
            rows.iter()
                .map(|row| row["display_name"].as_str().unwrap())
                .collect::<Vec<_>>(),
            vec![
                "Codex Sol (OpenAI)",
                "Codex Terra (OpenAI)",
                "Codex Luna (OpenAI)",
                "DeepSeek V4 Pro 0813",
                "DeepSeek V4 Flash",
            ]
        );
        for secret in [bridge.bearer_token(), "deepseek-test-secret"] {
            assert!(!format!("{bridge:?}").contains(secret));
            assert!(!response.contains(secret));
        }

        let unavailable = BridgeHandle::start(
            BridgeConfig::default().with_unified_gateway(UnifiedGatewayConfig::new(None, false)),
        )
        .unwrap();
        let response = request(
            unavailable.socket_addr(),
            &authorized_with_headers(
                "GET",
                "/v1/models?limit=1000",
                "native-claude-credential",
                "",
                &[(UNIFIED_GATEWAY_TOKEN_HEADER, unavailable.bearer_token())],
            ),
        );
        assert_eq!(status(&response), 200, "{response}");
        assert!(response.contains(r#""data":[]"#), "{response}");
    }

    fn unified_message_request(
        bridge: &BridgeHandle,
        model: &str,
        session_id: &str,
        caller_credential: &str,
    ) -> String {
        let body = serde_json::json!({
            "model": model,
            "messages": [{"role": "user", "content": format!("route {model}")}],
            "stream": false,
        })
        .to_string();
        authorized_with_headers(
            "POST",
            "/v1/messages",
            caller_credential,
            &body,
            &[
                (UNIFIED_GATEWAY_TOKEN_HEADER, bridge.bearer_token()),
                ("X-Claude-Code-Session-Id", session_id),
                ("X-Api-Key", "native-claude-api-key"),
                ("Anthropic-Version", "2023-06-01"),
                ("Anthropic-Beta", "context-1m-2025-08-07"),
            ],
        )
    }

    #[test]
    fn unified_routes_all_five_ids_with_provider_credential_isolation() {
        let codex = FakeResponses::start();
        let claude = FakeResponses::start();
        let deepseek = FakeResponses::start();
        let config = BridgeConfig::default()
            .with_test_upstream_url(Some(codex.base_url.clone()))
            .with_unified_gateway(
                UnifiedGatewayConfig::new(Some("deepseek-vault-canary".to_string()), true)
                    .with_upstreams(claude.base_url.clone(), deepseek.base_url.clone()),
            );
        let bridge = BridgeHandle::start(config).unwrap();

        let native = request(
            bridge.socket_addr(),
            &unified_message_request(
                &bridge,
                "claude-opus-4-1",
                "unified-native",
                "native-claude-oauth-canary",
            ),
        );
        assert_eq!(status(&native), 200, "{native}");

        for model in [
            "clud-claude-codex-sol",
            "clud-claude-codex-terra",
            "clud-claude-codex-luna",
            "clud-claude-deepseek-v4-pro-0813",
            "clud-claude-deepseek-v4-flash",
        ] {
            let response = request(
                bridge.socket_addr(),
                &unified_message_request(
                    &bridge,
                    model,
                    "unified-synthetic",
                    "native-claude-oauth-canary",
                ),
            );
            assert_eq!(status(&response), 200, "{model}: {response}");
        }

        let claude_request = claude.requests().pop().unwrap();
        assert!(claude_request.starts_with("POST /v1/messages "));
        assert!(claude_request.contains("claude-opus-4-1"));
        assert!(claude_request.contains("Bearer native-claude-oauth-canary"));
        assert!(claude_request.contains("native-claude-api-key"));
        assert!(claude_request.contains("Anthropic-Beta: context-1m-2025-08-07"));
        assert!(!claude_request.contains(bridge.bearer_token()));
        assert!(!claude_request.contains("deepseek-vault-canary"));
        assert!(!claude_request.contains("clud-test-upstream-key"));

        let codex_requests = codex.requests();
        assert_eq!(codex_requests.len(), 3);
        for (raw, wire_id) in
            codex_requests
                .iter()
                .zip(["gpt-5.6-sol", "gpt-5.6-terra", "gpt-5.6-luna"])
        {
            assert!(raw.starts_with("POST /v1/responses "));
            assert!(raw.contains(wire_id), "{raw}");
            assert!(raw.contains("Bearer clud-test-upstream-key"));
            for forbidden in [
                bridge.bearer_token(),
                "native-claude-oauth-canary",
                "native-claude-api-key",
                "deepseek-vault-canary",
            ] {
                assert!(!raw.contains(forbidden), "Codex leaked {forbidden}: {raw}");
            }
        }

        let deepseek_requests = deepseek.requests();
        assert_eq!(deepseek_requests.len(), 2);
        for (raw, wire_id) in deepseek_requests
            .iter()
            .zip(["deepseek-v4-pro[1m]", "deepseek-v4-flash"])
        {
            assert!(raw.starts_with("POST /v1/messages "));
            assert!(raw.contains(wire_id), "{raw}");
            assert!(raw.contains("Bearer deepseek-vault-canary"));
            assert!(raw.contains("Anthropic-Version: 2023-06-01"));
            assert!(raw.contains("Anthropic-Beta: context-1m-2025-08-07"));
            for forbidden in [
                bridge.bearer_token(),
                "native-claude-oauth-canary",
                "native-claude-api-key",
                "clud-test-upstream-key",
            ] {
                assert!(
                    !raw.contains(forbidden),
                    "DeepSeek leaked {forbidden}: {raw}"
                );
            }
        }

        let before = codex.requests().len() + claude.requests().len() + deepseek.requests().len();
        let unknown = request(
            bridge.socket_addr(),
            &unified_message_request(
                &bridge,
                "clud-claude-unknown",
                "unified-unknown",
                "native-claude-oauth-canary",
            ),
        );
        assert_eq!(status(&unknown), 400, "{unknown}");
        assert_eq!(
            codex.requests().len() + claude.requests().len() + deepseek.requests().len(),
            before,
            "reserved unknown IDs must fail before any upstream request"
        );
    }

    /// Build a raw upstream HTTP response for a canary to replay verbatim.
    fn raw_response(status: u16, reason: &str, body: &str) -> Vec<u8> {
        format!(
            "HTTP/1.1 {status} {reason}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
            body.len()
        )
        .into_bytes()
    }

    /// The whole point of #968: a spent route is replayed onto the next rung
    /// *before* a byte reaches the client, so the client sees one ordinary 200
    /// and never learns a provider declined.
    #[test]
    fn an_exhausted_route_is_replayed_onto_the_next_rung_before_the_client_sees_anything() {
        let claude = FakeResponses::start();
        let deepseek = FakeResponses::start_with_responses(vec![Some(raw_response(
            429,
            "Too Many Requests",
            r#"{"error":{"message":"Rate limit exceeded: free-models-per-day-stealth"}}"#,
        ))]);
        let config = BridgeConfig::default().with_unified_gateway(
            UnifiedGatewayConfig::new(Some("deepseek-vault-canary".to_string()), false)
                .with_upstreams(claude.base_url.clone(), deepseek.base_url.clone())
                .with_failover(FailoverLadder::parse("claude-opus-4-1", true).unwrap()),
        );
        let bridge = BridgeHandle::start(config).unwrap();

        let response = request(
            bridge.socket_addr(),
            &unified_message_request(
                &bridge,
                "clud-claude-deepseek-v4-flash",
                "failover-session",
                "native-claude-oauth-canary",
            ),
        );

        assert_eq!(
            status(&response),
            200,
            "the client must never see the exhausted route's 429: {response}"
        );
        assert!(
            !response.contains("free-models-per-day"),
            "no part of the declined response may reach the client: {response}"
        );

        let deepseek_requests = deepseek.requests();
        assert_eq!(
            deepseek_requests.len(),
            1,
            "the spent route must be tried exactly once"
        );

        let replayed = claude.requests().pop().expect("the rung was taken");
        assert!(
            replayed.contains("claude-opus-4-1"),
            "the replay must carry the rung's model: {replayed}"
        );
        assert!(
            replayed.contains("route clud-claude-deepseek-v4-flash"),
            "the replay must carry the caller's original transcript: {replayed}"
        );
        assert!(
            !replayed.contains("deepseek-vault-canary"),
            "credentials must not cross a provider boundary: {replayed}"
        );
    }

    /// The reported wedge, reproduced and fixed (#968).
    ///
    /// A session on OpenRouter's free daily tier exhausted its quota and had no
    /// in-session escape: `/model` moves the model ID, not the upstream. Routed
    /// through the gateway, the same exhaustion lands on the Claude rung and
    /// the turn completes.
    #[test]
    fn an_exhausted_openrouter_route_continues_on_claude_without_the_client_noticing() {
        let claude = FakeResponses::start();
        let openrouter = FakeResponses::start_with_responses(vec![Some(raw_response(
            429,
            "Too Many Requests",
            r#"{"error":{"message":"Rate limit exceeded: free-models-per-day-stealth"}}"#,
        ))]);
        let config = BridgeConfig::default().with_unified_gateway(
            UnifiedGatewayConfig::new(None, false)
                .with_openrouter(Some("openrouter-vault-canary".to_string()))
                .with_upstreams(claude.base_url.clone(), String::new())
                .with_openrouter_upstream(openrouter.base_url.clone())
                .with_failover(FailoverLadder::parse("claude-opus-4-1", true).unwrap()),
        );
        let bridge = BridgeHandle::start(config).unwrap();

        let response = request(
            bridge.socket_addr(),
            &unified_message_request(
                &bridge,
                "clud-claude-openrouter-sonnet",
                "openrouter-session",
                "native-claude-oauth-canary",
            ),
        );

        assert_eq!(status(&response), 200, "{response}");
        let sent = openrouter.requests().pop().expect("openrouter was tried");
        assert!(
            sent.contains("~anthropic/claude-sonnet-latest"),
            "the reviewed OpenRouter wire ID must be used: {sent}"
        );
        assert!(
            sent.contains("Bearer openrouter-vault-canary"),
            "OpenRouter must receive its own credential: {sent}"
        );

        let rescued = claude.requests().pop().expect("the Claude rung was taken");
        assert!(rescued.contains("claude-opus-4-1"), "{rescued}");
        assert!(
            !rescued.contains("openrouter-vault-canary"),
            "credentials must not cross a provider boundary: {rescued}"
        );
    }

    /// The operator surface: what the ladder is, what each route's health is,
    /// and the escape hatch for a clock-less failure.
    #[test]
    fn route_status_reports_health_and_clear_restores_a_drained_route() {
        let claude = FakeResponses::start();
        let deepseek = FakeResponses::start_with_responses(vec![Some(raw_response(
            402,
            "Payment Required",
            r#"{"error":{"message":"This request requires more credits"}}"#,
        ))]);
        let config = BridgeConfig::default().with_unified_gateway(
            UnifiedGatewayConfig::new(Some("deepseek-vault-canary".to_string()), false)
                .with_upstreams(claude.base_url.clone(), deepseek.base_url.clone())
                .with_failover(
                    FailoverLadder::parse("claude-opus-4-1,deepseek-v4-flash", false).unwrap(),
                ),
        );
        let bridge = BridgeHandle::start(config).unwrap();
        let addr = bridge.socket_addr();
        let token = bridge.bearer_token().to_owned();

        let status_request = || {
            request(
                addr,
                &authorized_with_headers(
                    "GET",
                    "/_clud/route/status",
                    "native-claude-oauth-canary",
                    "",
                    &[(UNIFIED_GATEWAY_TOKEN_HEADER, &token)],
                ),
            )
        };

        let before = status_request();
        assert_eq!(status(&before), 200, "{before}");
        assert!(
            before.contains("\"cost\":\"subscription\""),
            "the ladder must name who pays: {before}"
        );
        assert!(
            before.contains("\"withheld_for_consent\":true"),
            "a metered rung without consent must say so: {before}"
        );

        // Drain DeepSeek, then confirm the ledger reports it with no clock.
        let drained = request(
            addr,
            &unified_message_request(
                &bridge,
                "clud-claude-deepseek-v4-flash",
                "status-session",
                "native-claude-oauth-canary",
            ),
        );
        assert_eq!(
            status(&drained),
            200,
            "the Claude rung serves it: {drained}"
        );

        let after = status_request();
        assert!(
            after.contains("\"status\":\"down\"") && after.contains("\"reason\":\"drained\""),
            "a spent balance must be reported as down, not cooling: {after}"
        );

        let cleared = request(
            addr,
            &authorized_with_headers(
                "POST",
                "/_clud/route/clear",
                "native-claude-oauth-canary",
                r#"{"route":"deepseek"}"#,
                &[(UNIFIED_GATEWAY_TOKEN_HEADER, &token)],
            ),
        );
        assert_eq!(status(&cleared), 200, "{cleared}");
        let restored = status_request();
        assert!(
            !restored.contains("\"reason\":\"drained\""),
            "clearing must restore the route a top-up fixed: {restored}"
        );
    }

    /// `route_health` asserts this at the unit level; this pins the gateway
    /// wiring to the same contract. A committed *failure* says nothing about
    /// the route, so it must not clear a cooldown the ledger is still serving.
    #[test]
    fn a_committed_failure_does_not_clear_a_cooling_route() {
        let claude = FakeResponses::start();
        let deepseek = FakeResponses::start_with_responses(vec![
            // First: drains the route and is failed over.
            Some(raw_response(
                402,
                "Payment Required",
                r#"{"error":{"message":"This request requires more credits"}}"#,
            )),
            // Second: a malformed request, committed straight through.
            Some(raw_response(
                400,
                "Bad Request",
                r#"{"error":{"type":"invalid_request_error","message":"bad shape"}}"#,
            )),
        ]);
        let config = BridgeConfig::default().with_unified_gateway(
            UnifiedGatewayConfig::new(Some("deepseek-vault-canary".to_string()), false)
                .with_upstreams(claude.base_url.clone(), deepseek.base_url.clone())
                .with_failover(FailoverLadder::parse("claude-opus-4-1", true).unwrap()),
        );
        let bridge = BridgeHandle::start(config).unwrap();
        let addr = bridge.socket_addr();
        let token = bridge.bearer_token().to_owned();

        for session in ["drain", "fatal"] {
            let _ = request(
                addr,
                &unified_message_request(
                    &bridge,
                    "clud-claude-deepseek-v4-flash",
                    session,
                    "native-claude-oauth-canary",
                ),
            );
        }

        let status_body = request(
            addr,
            &authorized_with_headers(
                "GET",
                "/_clud/route/status",
                "native-claude-oauth-canary",
                "",
                &[(UNIFIED_GATEWAY_TOKEN_HEADER, &token)],
            ),
        );
        assert!(
            status_body.contains("\"reason\":\"drained\""),
            "the 400 must not have resurrected the drained route: {status_body}"
        );
    }

    /// The control surface is gateway-token gated exactly like `/v1/messages`.
    #[test]
    fn route_status_requires_the_gateway_token() {
        let claude = FakeResponses::start();
        let config = BridgeConfig::default().with_unified_gateway(
            UnifiedGatewayConfig::new(None, false)
                .with_upstreams(claude.base_url.clone(), String::new()),
        );
        let bridge = BridgeHandle::start(config).unwrap();
        let denied = request(
            bridge.socket_addr(),
            &authorized_with_headers(
                "GET",
                "/_clud/route/status",
                "native-claude-oauth-canary",
                "",
                &[(UNIFIED_GATEWAY_TOKEN_HEADER, "wrong-token")],
            ),
        );
        assert_eq!(status(&denied), 401, "{denied}");
    }

    /// A malformed request fails identically everywhere, so replaying it would
    /// only spend a second account to reproduce the same error.
    #[test]
    fn a_request_fatal_failure_never_descends_the_ladder() {
        let claude = FakeResponses::start();
        let deepseek = FakeResponses::start_with_responses(vec![Some(raw_response(
            400,
            "Bad Request",
            r#"{"error":{"type":"invalid_request_error","message":"bad shape"}}"#,
        ))]);
        let config = BridgeConfig::default().with_unified_gateway(
            UnifiedGatewayConfig::new(Some("deepseek-vault-canary".to_string()), false)
                .with_upstreams(claude.base_url.clone(), deepseek.base_url.clone())
                .with_failover(FailoverLadder::parse("claude-opus-4-1", true).unwrap()),
        );
        let bridge = BridgeHandle::start(config).unwrap();

        let response = request(
            bridge.socket_addr(),
            &unified_message_request(
                &bridge,
                "clud-claude-deepseek-v4-flash",
                "fatal-session",
                "native-claude-oauth-canary",
            ),
        );

        assert_eq!(status(&response), 400, "{response}");
        assert!(
            claude.requests().is_empty(),
            "a 400 must not be replayed onto a second account"
        );
    }

    /// With no ladder configured the gateway takes exactly today's path: the
    /// upstream status reaches the client unchanged and nothing is buffered.
    #[test]
    fn without_a_ladder_the_upstream_status_reaches_the_client_unchanged() {
        let claude = FakeResponses::start();
        let deepseek = FakeResponses::start_with_responses(vec![Some(raw_response(
            429,
            "Too Many Requests",
            r#"{"error":{"message":"Rate limit exceeded: free-models-per-day-stealth"}}"#,
        ))]);
        let config = BridgeConfig::default().with_unified_gateway(
            UnifiedGatewayConfig::new(Some("deepseek-vault-canary".to_string()), false)
                .with_upstreams(claude.base_url.clone(), deepseek.base_url.clone()),
        );
        let bridge = BridgeHandle::start(config).unwrap();

        let response = request(
            bridge.socket_addr(),
            &unified_message_request(
                &bridge,
                "clud-claude-deepseek-v4-flash",
                "no-ladder-session",
                "native-claude-oauth-canary",
            ),
        );

        assert_eq!(status(&response), 429, "{response}");
        assert!(
            claude.requests().is_empty(),
            "an unconfigured ladder must never reach another provider"
        );
    }

    #[test]
    fn unified_wire_ids_route_to_their_own_provider_not_anthropic() {
        let codex = FakeResponses::start();
        let claude = FakeResponses::start();
        let deepseek = FakeResponses::start();
        let config = BridgeConfig::default()
            .with_test_upstream_url(Some(codex.base_url.clone()))
            .with_unified_gateway(
                UnifiedGatewayConfig::new(Some("deepseek-vault-canary".to_string()), true)
                    .with_upstreams(claude.base_url.clone(), deepseek.base_url.clone()),
            );
        let bridge = BridgeHandle::start(config).unwrap();

        // A persisted or continued session can carry provider wire IDs
        // instead of discovery IDs. Each must reach its own upstream.
        let deepseek_wire = request(
            bridge.socket_addr(),
            &unified_message_request(
                &bridge,
                "deepseek-v4-pro[1m]",
                "unified-wire-deepseek",
                "native-claude-oauth-canary",
            ),
        );
        assert_eq!(status(&deepseek_wire), 200, "{deepseek_wire}");
        let deepseek_request = deepseek.requests().pop().unwrap();
        assert!(deepseek_request.starts_with("POST /v1/messages "));
        assert!(deepseek_request.contains("deepseek-v4-pro[1m]"));
        assert!(deepseek_request.contains("Bearer deepseek-vault-canary"));
        assert!(!deepseek_request.contains("native-claude-oauth-canary"));

        let codex_wire = request(
            bridge.socket_addr(),
            &unified_message_request(
                &bridge,
                "gpt-5.6-terra",
                "unified-wire-codex",
                "native-claude-oauth-canary",
            ),
        );
        assert_eq!(status(&codex_wire), 200, "{codex_wire}");
        let codex_request = codex.requests().pop().unwrap();
        assert!(codex_request.starts_with("POST /v1/responses "));
        assert!(codex_request.contains("gpt-5.6-terra"));
        assert!(codex_request.contains("Bearer clud-test-upstream-key"));
        assert!(!codex_request.contains("native-claude-oauth-canary"));

        assert!(
            claude.requests().is_empty(),
            "a known provider wire ID must never reach Anthropic"
        );
    }

    #[test]
    fn unified_native_count_tokens_is_proxied_with_claude_auth() {
        let body = r#"{"input_tokens":7}"#;
        let reply = format!(
            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
            body.len()
        )
        .into_bytes();
        let claude = FakeResponses::start_with_response(Some(reply));
        let other = FakeResponses::start();
        let config = BridgeConfig::default().with_unified_gateway(
            UnifiedGatewayConfig::new(None, false)
                .with_upstreams(claude.base_url.clone(), other.base_url.clone()),
        );
        let bridge = BridgeHandle::start(config).unwrap();
        let request_body =
            r#"{"model":"claude-opus-4-1","messages":[{"role":"user","content":"measure"}]}"#;
        let response = request(
            bridge.socket_addr(),
            &authorized_with_headers(
                "POST",
                "/v1/messages/count_tokens",
                "native-count-token-canary",
                request_body,
                &[
                    (UNIFIED_GATEWAY_TOKEN_HEADER, bridge.bearer_token()),
                    ("Anthropic-Version", "2023-06-01"),
                ],
            ),
        );
        assert_eq!(status(&response), 200, "{response}");
        assert!(response.contains(r#"{"input_tokens":7}"#));
        let upstream = claude.requests().pop().unwrap();
        assert!(upstream.starts_with("POST /v1/messages/count_tokens "));
        assert!(upstream.contains("Bearer native-count-token-canary"));
        assert!(!upstream.contains(bridge.bearer_token()));

        for synthetic in [
            serde_json::json!({
                "model": "clud-claude-codex-sol",
                "messages": [{"role": "user", "content": "measure"}],
            })
            .to_string(),
            // A persisted session may name the provider wire ID instead of
            // its discovery ID; it must not be proxied to Anthropic.
            serde_json::json!({
                "model": "deepseek-v4-pro[1m]",
                "messages": [{"role": "user", "content": "measure"}],
            })
            .to_string(),
        ] {
            let unsupported = request(
                bridge.socket_addr(),
                &authorized_with_headers(
                    "POST",
                    "/v1/messages/count_tokens",
                    "native-count-token-canary",
                    &synthetic,
                    &[(UNIFIED_GATEWAY_TOKEN_HEADER, bridge.bearer_token())],
                ),
            );
            assert_eq!(status(&unsupported), 404, "{unsupported}");
        }
        assert_eq!(
            claude.requests().len(),
            1,
            "only the native Claude count request may reach Anthropic"
        );
    }

    #[test]
    fn unified_provider_switches_reseed_codex_without_private_output_items() {
        let codex =
            FakeResponses::start_with_responses(vec![Some(recovery_success_response()), None]);
        let claude = FakeResponses::start();
        let deepseek = FakeResponses::start();
        let config = BridgeConfig::default()
            .with_test_upstream_url(Some(codex.base_url.clone()))
            .with_unified_gateway(
                UnifiedGatewayConfig::new(Some("deepseek-route-key".to_string()), true)
                    .with_upstreams(claude.base_url.clone(), deepseek.base_url.clone()),
            );
        let bridge = BridgeHandle::start(config).unwrap();
        let session = "provider-switch-session";
        let turn = |model: &str, messages: serde_json::Value| {
            let body = serde_json::json!({
                "model": model,
                "messages": messages,
                "stream": false,
            })
            .to_string();
            request(
                bridge.socket_addr(),
                &authorized_with_headers(
                    "POST",
                    "/v1/messages",
                    "native-route-credential",
                    &body,
                    &[
                        (UNIFIED_GATEWAY_TOKEN_HEADER, bridge.bearer_token()),
                        ("X-Claude-Code-Session-Id", session),
                        ("Anthropic-Version", "2023-06-01"),
                    ],
                ),
            )
        };

        let first_claude = turn(
            "claude-opus-4-1",
            serde_json::json!([{"role": "user", "content": "first claude prompt"}]),
        );
        assert_eq!(status(&first_claude), 200, "{first_claude}");
        let first_codex = turn(
            "clud-claude-codex-sol",
            serde_json::json!([
                {"role": "user", "content": "first claude prompt"},
                {"role": "assistant", "content": "first claude visible answer"},
                {"role": "user", "content": "first codex prompt"}
            ]),
        );
        assert_eq!(status(&first_codex), 200, "{first_codex}");
        let switched_deepseek = turn(
            "clud-claude-deepseek-v4-flash",
            serde_json::json!([
                {"role": "user", "content": "first claude prompt"},
                {"role": "assistant", "content": "first claude visible answer"},
                {"role": "user", "content": "first codex prompt"},
                {"role": "assistant", "content": "recovered"},
                {"role": "user", "content": "deepseek prompt"}
            ]),
        );
        assert_eq!(status(&switched_deepseek), 200, "{switched_deepseek}");
        let switched_claude = turn(
            "claude-opus-4-1",
            serde_json::json!([
                {"role": "user", "content": "first claude prompt"},
                {"role": "assistant", "content": "first claude visible answer"},
                {"role": "user", "content": "first codex prompt"},
                {"role": "assistant", "content": "recovered"},
                {"role": "user", "content": "deepseek prompt"},
                {"role": "assistant", "content": "deepseek visible answer"},
                {"role": "user", "content": "second claude prompt"}
            ]),
        );
        assert_eq!(status(&switched_claude), 200, "{switched_claude}");
        let final_codex = turn(
            "clud-claude-codex-terra",
            serde_json::json!([
                {"role": "user", "content": "first claude prompt"},
                {"role": "assistant", "content": "first claude visible answer"},
                {"role": "user", "content": "first codex prompt"},
                {"role": "assistant", "content": "recovered"},
                {"role": "user", "content": "deepseek prompt"},
                {"role": "assistant", "content": "deepseek visible answer"},
                {"role": "user", "content": "second claude prompt"},
                {"role": "assistant", "content": "second claude visible answer"},
                {"role": "user", "content": "final codex prompt"}
            ]),
        );
        assert_eq!(status(&final_codex), 200, "{final_codex}");

        let requests = codex.requests();
        assert_eq!(requests.len(), 2);
        let final_request = &requests[1];
        assert!(final_request.contains("first claude prompt"));
        assert!(final_request.contains("deepseek visible answer"));
        assert!(final_request.contains("second claude visible answer"));
        assert!(final_request.contains("final codex prompt"));
        assert!(
            !final_request.contains("msg_recovered"),
            "provider-private Codex output IDs crossed a route epoch: {final_request}"
        );
    }

    #[test]
    fn unified_native_claude_preserves_effort_payload_and_required_headers_byte_for_byte() {
        let anthropic = FakeResponses::start();
        let codex = FakeResponses::start();
        let deepseek = FakeResponses::start();
        let bridge = BridgeHandle::start(unified_config(&anthropic, &codex, &deepseek)).unwrap();
        let body = r#"{ "model":"claude-opus-4-8", "messages":[{"role":"user","content":"fixture prompt"}], "thinking":{"type":"adaptive","fixture":{"keep":true}}, "output_config":{"effort":"xhigh","format":{"type":"json_schema"}}, "stream":false }"#;

        let response = unified_request(&bridge, body, "native-byte-session", None);
        assert_eq!(status(&response), 200, "{response}");
        let requests = anthropic.requests();
        assert_eq!(requests.len(), 1);
        assert_eq!(captured_body(&requests[0]), body);
        let sent = requests[0].to_ascii_lowercase();
        for required in [
            "authorization: bearer claude-oauth-canary",
            "x-api-key: claude-api-key-canary",
            "anthropic-version: 2023-06-01",
            "anthropic-beta: oauth-2025-04-20,fixture-capability",
        ] {
            assert!(sent.contains(required), "missing {required}: {sent}");
        }
        for forbidden in [bridge.bearer_token(), "deepseek-route-canary"] {
            assert!(!requests[0].contains(forbidden), "leaked {forbidden}");
        }
        assert!(codex.requests().is_empty());
        assert!(deepseek.requests().is_empty());
    }

    #[test]
    fn unified_codex_models_and_efforts_reach_the_exact_responses_fields() {
        let anthropic = FakeResponses::start();
        let codex = FakeResponses::start();
        let deepseek = FakeResponses::start();
        let bridge = BridgeHandle::start(unified_config(&anthropic, &codex, &deepseek)).unwrap();
        let models =
            provider_catalog::models_for_provider(ModelProvider::Codex).collect::<Vec<_>>();
        let mut expected = Vec::new();
        for (model_index, model) in models.iter().enumerate() {
            let discovery = model.discovery_id.expect("Codex discovery id");
            for (effort_index, effort) in model.supported_efforts.iter().enumerate() {
                let body = serde_json::json!({
                    "model": discovery,
                    "messages": [{"role": "user", "content": "route matrix"}],
                    "thinking": {"type": "adaptive"},
                    "output_config": {"effort": effort.as_str()},
                    "stream": false,
                })
                .to_string();
                let session = format!("codex-{model_index}-{effort_index}");
                let response = unified_request(&bridge, &body, &session, None);
                assert_eq!(status(&response), 200, "{discovery} {effort}: {response}");
                expected.push((model.wire_id, effort.as_str()));
            }
        }

        let suffix = r#"{"model":"clud-claude-codex-terra@max","messages":[{"role":"user","content":"suffix wins"}],"thinking":{"type":"enabled","budget_tokens":9000},"output_config":{"effort":"low"},"stream":false}"#;
        assert_eq!(
            status(&unified_request(&bridge, suffix, "codex-suffix", None)),
            200
        );
        expected.push(("gpt-5.6-terra", "max"));

        let thinking = r#"{"model":"clud-claude-codex-luna","messages":[{"role":"user","content":"thinking fallback"}],"thinking":{"type":"enabled","budget_tokens":9000},"stream":false}"#;
        assert_eq!(
            status(&unified_request(&bridge, thinking, "codex-thinking", None)),
            200
        );
        expected.push(("gpt-5.6-luna", "high"));

        let catalog_default = r#"{"model":"clud-claude-codex-sol","messages":[{"role":"user","content":"default fallback"}],"thinking":{"type":"adaptive"},"stream":false}"#;
        assert_eq!(
            status(&unified_request(
                &bridge,
                catalog_default,
                "codex-default",
                None
            )),
            200
        );
        expected.push(("gpt-5.6-sol", "low"));

        let requests = codex.requests();
        assert_eq!(requests.len(), expected.len());
        for (request, (model, effort)) in requests.iter().zip(expected) {
            let body: serde_json::Value =
                serde_json::from_str(captured_body(request)).expect("Responses JSON");
            assert_eq!(body["model"], model);
            assert_eq!(body["reasoning"]["effort"], effort);
            assert!(!request.contains("claude-oauth-canary"));
            assert!(!request.contains("claude-api-key-canary"));
            assert!(!request.contains("deepseek-route-canary"));
        }

        let before_rejection = codex.requests().len();
        for rejected in ["minimal", "ultra"] {
            let body = serde_json::json!({
                "model": "clud-claude-codex-terra",
                "messages": [{"role": "user", "content": "reject locally"}],
                "output_config": {"effort": rejected},
                "stream": false,
            })
            .to_string();
            let response =
                unified_request(&bridge, &body, &format!("codex-reject-{rejected}"), None);
            assert_eq!(status(&response), 400, "{response}");
            assert!(response.contains(rejected), "{response}");
            assert!(response.contains("xhigh"), "{response}");
        }
        assert_eq!(
            codex.requests().len(),
            before_rejection,
            "a rejected effort must make zero upstream requests"
        );
        assert!(anthropic.requests().is_empty());
        assert!(deepseek.requests().is_empty());
    }

    #[test]
    fn unified_deepseek_preserves_effort_for_both_models_without_codex_validation() {
        fn provider_effective_effort(incoming: &str) -> Option<&'static str> {
            match incoming {
                "low" | "medium" | "high" => Some("high"),
                "xhigh" | "max" => Some("max"),
                _ => None,
            }
        }

        let anthropic = FakeResponses::start();
        let codex = FakeResponses::start();
        let deepseek = FakeResponses::start();
        let bridge = BridgeHandle::start(unified_config(&anthropic, &codex, &deepseek)).unwrap();
        let models = [
            ("clud-claude-deepseek-v4-pro-0813", "deepseek-v4-pro[1m]"),
            ("clud-claude-deepseek-v4-flash", "deepseek-v4-flash"),
        ];
        // DeepSeek, not clud, owns this compatibility mapping. Assert both the
        // unchanged gateway payload and the provider-documented effective
        // level so this acceptance matrix cannot silently drift.
        let efforts = [
            ("low", "high"),
            ("medium", "high"),
            ("high", "high"),
            ("xhigh", "max"),
            ("max", "max"),
        ];
        for (model_index, (discovery, wire)) in models.iter().enumerate() {
            for (effort_index, (incoming, effective)) in efforts.iter().enumerate() {
                let body = serde_json::json!({
                    "model": discovery,
                    "messages": [{"role": "user", "content": "deepseek matrix"}],
                    "thinking": {"type": "adaptive", "fixture": {"keep": true}},
                    "output_config": {"effort": incoming, "fixture": {"keep": true}},
                    "stream": false,
                })
                .to_string();
                let response = unified_request(
                    &bridge,
                    &body,
                    &format!("deepseek-{model_index}-{effort_index}"),
                    None,
                );
                assert_eq!(status(&response), 200, "{discovery} {incoming}: {response}");
                let requests = deepseek.requests();
                let sent = requests.last().expect("DeepSeek request");
                let sent_body: serde_json::Value =
                    serde_json::from_str(captured_body(sent)).expect("Anthropic JSON");
                assert_eq!(sent_body["model"], *wire);
                assert_eq!(
                    sent_body["thinking"],
                    serde_json::json!({
                        "type": "adaptive",
                        "fixture": {"keep": true},
                    })
                );
                assert_eq!(
                    sent_body["output_config"],
                    serde_json::json!({
                        "effort": incoming,
                        "fixture": {"keep": true},
                    })
                );
                assert_eq!(
                    provider_effective_effort(
                        sent_body["output_config"]["effort"]
                            .as_str()
                            .expect("DeepSeek effort string")
                    ),
                    Some(*effective),
                    "provider mapping for {incoming}"
                );
                assert!(sent
                    .to_ascii_lowercase()
                    .contains("authorization: bearer deepseek-route-canary"));
                assert!(!sent.contains("claude-oauth-canary"));
                assert!(!sent.contains("claude-api-key-canary"));
                assert!(!sent.contains(bridge.bearer_token()));
            }
        }

        let future = r#"{"model":"clud-claude-deepseek-v4-flash","messages":[{"role":"user","content":"future effort"}],"output_config":{"effort":"future-provider-level"},"stream":false}"#;
        assert_eq!(
            status(&unified_request(&bridge, future, "deepseek-future", None)),
            200
        );
        let requests = deepseek.requests();
        let body: serde_json::Value =
            serde_json::from_str(captured_body(requests.last().unwrap())).expect("Anthropic JSON");
        assert_eq!(body["output_config"]["effort"], "future-provider-level");
        assert!(anthropic.requests().is_empty());
        assert!(codex.requests().is_empty());
    }

    #[test]
    fn unified_provider_switch_reseeds_codex_and_keeps_main_and_agent_efforts_independent() {
        let anthropic = FakeResponses::start();
        let codex = FakeResponses::start();
        let deepseek = FakeResponses::start();
        let bridge = BridgeHandle::start(unified_config(&anthropic, &codex, &deepseek)).unwrap();
        let session = "switch-session";
        let turn = |model: &str, messages: serde_json::Value, effort: &str| {
            serde_json::json!({
                "model": model,
                "messages": messages,
                "thinking": {"type": "adaptive"},
                "output_config": {"effort": effort},
                "stream": false,
            })
            .to_string()
        };

        let claude_first = turn(
            "claude-opus-4-8",
            serde_json::json!([{"role": "user", "content": "claude first"}]),
            "high",
        );
        assert_eq!(
            status(&unified_request(&bridge, &claude_first, session, None)),
            200
        );
        let codex_first = turn(
            "clud-claude-codex-terra",
            serde_json::json!([
                {"role": "user", "content": "claude first"},
                {"role": "assistant", "content": "claude reply"},
                {"role": "user", "content": "codex first"}
            ]),
            "high",
        );
        assert_eq!(
            status(&unified_request(&bridge, &codex_first, session, None)),
            200
        );
        let deepseek_turn = turn(
            "clud-claude-deepseek-v4-pro-0813",
            serde_json::json!([
                {"role": "user", "content": "claude first"},
                {"role": "assistant", "content": "claude reply"},
                {"role": "user", "content": "codex first"},
                {"role": "assistant", "content": "bridged reply"},
                {"role": "user", "content": "deepseek turn"}
            ]),
            "high",
        );
        assert_eq!(
            status(&unified_request(&bridge, &deepseek_turn, session, None)),
            200
        );
        let claude_return = turn(
            "claude-opus-4-8",
            serde_json::json!([
                {"role": "user", "content": "claude first"},
                {"role": "assistant", "content": "claude reply"},
                {"role": "user", "content": "codex first"},
                {"role": "assistant", "content": "bridged reply"},
                {"role": "user", "content": "deepseek turn"},
                {"role": "assistant", "content": "deepseek reply"},
                {"role": "user", "content": "claude return"}
            ]),
            "high",
        );
        assert_eq!(
            status(&unified_request(&bridge, &claude_return, session, None)),
            200
        );
        let codex_return = turn(
            "clud-claude-codex-luna",
            serde_json::json!([
                {"role": "user", "content": "claude first"},
                {"role": "assistant", "content": "claude reply"},
                {"role": "user", "content": "codex first"},
                {"role": "assistant", "content": "bridged reply"},
                {"role": "user", "content": "deepseek turn"},
                {"role": "assistant", "content": "deepseek reply"},
                {"role": "user", "content": "claude return"},
                {"role": "assistant", "content": "claude return reply"},
                {"role": "user", "content": "codex return"}
            ]),
            "high",
        );
        assert_eq!(
            status(&unified_request(&bridge, &codex_return, session, None)),
            200
        );

        let agent = turn(
            "clud-claude-deepseek-v4-flash",
            serde_json::json!([{"role": "user", "content": "agent override"}]),
            "xhigh",
        );
        assert_eq!(
            status(&unified_request(
                &bridge,
                &agent,
                session,
                Some("child-agent")
            )),
            200
        );

        let codex_requests = codex.requests();
        let reseeded: serde_json::Value = serde_json::from_str(captured_body(
            codex_requests.last().expect("returning Codex request"),
        ))
        .expect("Responses JSON");
        assert_eq!(reseeded["model"], "gpt-5.6-luna");
        assert_eq!(reseeded["reasoning"]["effort"], "high");
        let text = reseeded["input"].to_string();
        for visible in [
            "claude first",
            "claude reply",
            "codex first",
            "deepseek turn",
            "deepseek reply",
            "claude return",
            "claude return reply",
            "codex return",
        ] {
            assert!(text.contains(visible), "missing {visible}: {text}");
        }
        let deepseek_requests = deepseek.requests();
        let agent_body: serde_json::Value = serde_json::from_str(captured_body(
            deepseek_requests.last().expect("agent DeepSeek request"),
        ))
        .expect("Anthropic JSON");
        assert_eq!(agent_body["output_config"]["effort"], "xhigh");
        for request in anthropic.requests() {
            let body: serde_json::Value =
                serde_json::from_str(captured_body(&request)).expect("Anthropic JSON");
            assert_eq!(body["output_config"]["effort"], "high");
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
    fn automatic_compaction_fallback_resets_rehydrated_tool_history() {
        let upstream = FakeResponses::start_with_responses(vec![
            Some(parallel_function_call_response(3)),
            None,
            None,
        ]);
        let bridge = BridgeHandle::start(bridged_config(&upstream)).unwrap();
        let addr = bridge.socket_addr();
        let token = bridge.bearer_token().to_owned();
        let headers = [("X-Claude-Code-Session-Id", "session-auto-compact")];
        let first = request(
            addr,
            &authorized_with_headers("POST", "/v1/messages", &token, PROBE_STREAM_BODY, &headers),
        );
        assert_eq!(status(&first), 200, "{first}");

        let compact = request(
            addr,
            &authorized(
                "POST",
                "/_clud/context/compact",
                &token,
                r#"{"hook_event_name":"PreCompact","trigger":"auto","session_id":"session-auto-compact"}"#,
            ),
        );
        assert_eq!(status(&compact), 204, "{compact}");
        assert!(
            !upstream
                .requests()
                .iter()
                .any(|request| request.starts_with("POST /v1/responses/compact HTTP/1.1")),
            "an incomplete continuation must be discarded locally, not sent upstream"
        );

        let mut replay: serde_json::Value =
            serde_json::from_str(&split_parallel_result_body(3, true)).unwrap();
        replay["messages"]
            .as_array_mut()
            .unwrap()
            .push(serde_json::json!({"role": "user", "content": "compact this context"}));
        let replay = replay.to_string();
        let compaction_inference = request(
            addr,
            &authorized_with_headers("POST", "/v1/messages", &token, &replay, &headers),
        );
        assert_eq!(status(&compaction_inference), 200, "{compaction_inference}");

        let finished = request(
            addr,
            &authorized(
                "POST",
                "/_clud/context/compact-finished",
                &token,
                r#"{"hook_event_name":"SessionStart","source":"compact","session_id":"session-auto-compact"}"#,
            ),
        );
        assert_eq!(status(&finished), 204, "{finished}");

        let compacted_turn = request(
            addr,
            &authorized_with_headers(
                "POST",
                "/v1/messages",
                &token,
                r#"{"model":"claude-x","messages":[{"role":"user","content":"COMPACTED SUMMARY"},{"role":"user","content":"after compact"}],"stream":false}"#,
                &headers,
            ),
        );
        assert_eq!(status(&compacted_turn), 200, "{compacted_turn}");
        let requests = upstream.requests();
        let compaction_request = requests
            .iter()
            .filter(|request| request.starts_with("POST /v1/responses HTTP/1.1"))
            .nth(1)
            .expect("Claude compaction inference");
        assert!(
            compaction_request.contains("call_0"),
            "{compaction_request}"
        );
        assert!(
            compaction_request.contains("result-0"),
            "{compaction_request}"
        );
        let fresh_request = requests
            .iter()
            .filter(|request| request.starts_with("POST /v1/responses HTTP/1.1"))
            .nth(2)
            .expect("post-compaction inference");
        assert!(
            fresh_request.contains("COMPACTED SUMMARY"),
            "{fresh_request}"
        );
        assert!(
            !fresh_request.contains("call_0"),
            "the post-compaction reset must not retain the old tool replay: {fresh_request}"
        );
    }

    #[test]
    fn automatic_compaction_fallback_handles_unavailable_endpoint() {
        let upstream =
            FakeResponses::start_with_responses(vec![None, Some(not_found_response()), None, None]);
        let bridge = BridgeHandle::start(bridged_config(&upstream)).unwrap();
        let addr = bridge.socket_addr();
        let token = bridge.bearer_token().to_owned();
        let headers = [("X-Claude-Code-Session-Id", "session-compact-404")];
        let first = request(
            addr,
            &authorized_with_headers("POST", "/v1/messages", &token, PROBE_BODY, &headers),
        );
        assert_eq!(status(&first), 200, "{first}");

        let compact = request(
            addr,
            &authorized(
                "POST",
                "/_clud/context/compact",
                &token,
                r#"{"hook_event_name":"PreCompact","trigger":"auto","session_id":"session-compact-404"}"#,
            ),
        );
        assert_eq!(status(&compact), 204, "{compact}");

        let compaction_inference = request(
            addr,
            &authorized_with_headers(
                "POST",
                "/v1/messages",
                &token,
                r#"{"model":"claude-x","messages":[{"role":"user","content":"hi"},{"role":"assistant","content":"bridged reply"},{"role":"user","content":"compact this context"}],"stream":false}"#,
                &headers,
            ),
        );
        assert_eq!(status(&compaction_inference), 200, "{compaction_inference}");
        let finished = request(
            addr,
            &authorized(
                "POST",
                "/_clud/context/compact-finished",
                &token,
                r#"{"hook_event_name":"SessionStart","source":"compact","session_id":"session-compact-404"}"#,
            ),
        );
        assert_eq!(status(&finished), 204, "{finished}");
        let after = request(
            addr,
            &authorized_with_headers(
                "POST",
                "/v1/messages",
                &token,
                r#"{"model":"claude-x","messages":[{"role":"user","content":"SUMMARY AFTER 404"}],"stream":false}"#,
                &headers,
            ),
        );
        assert_eq!(status(&after), 200, "{after}");
        let requests = upstream.requests();
        assert_eq!(
            requests
                .iter()
                .filter(|request| request.starts_with("POST /v1/responses/compact HTTP/1.1"))
                .count(),
            1
        );
        let final_request = requests.last().expect("post-compaction inference");
        assert!(
            final_request.contains("SUMMARY AFTER 404"),
            "{final_request}"
        );
        assert!(!final_request.contains("bridged reply"), "{final_request}");
    }

    #[test]
    fn automatic_compaction_fallback_resets_an_initially_empty_history() {
        let upstream = FakeResponses::start_with_responses(vec![None, None]);
        let bridge = BridgeHandle::start(bridged_config(&upstream)).unwrap();
        let addr = bridge.socket_addr();
        let token = bridge.bearer_token().to_owned();
        let headers = [("X-Claude-Code-Session-Id", "session-empty-compact")];

        let compact = request(
            addr,
            &authorized(
                "POST",
                "/_clud/context/compact",
                &token,
                r#"{"hook_event_name":"PreCompact","trigger":"auto","session_id":"session-empty-compact"}"#,
            ),
        );
        assert_eq!(status(&compact), 204, "{compact}");
        let compaction_inference = request(
            addr,
            &authorized_with_headers(
                "POST",
                "/v1/messages",
                &token,
                r#"{"model":"claude-x","messages":[{"role":"user","content":"FULL PRECOMPACTION HISTORY"},{"role":"user","content":"compact this context"}],"stream":false}"#,
                &headers,
            ),
        );
        assert_eq!(status(&compaction_inference), 200, "{compaction_inference}");
        let finished = request(
            addr,
            &authorized(
                "POST",
                "/_clud/context/compact-finished",
                &token,
                r#"{"hook_event_name":"SessionStart","source":"compact","session_id":"session-empty-compact"}"#,
            ),
        );
        assert_eq!(status(&finished), 204, "{finished}");
        let after = request(
            addr,
            &authorized_with_headers(
                "POST",
                "/v1/messages",
                &token,
                r#"{"model":"claude-x","messages":[{"role":"user","content":"SUMMARY FROM EMPTY"}],"stream":false}"#,
                &headers,
            ),
        );
        assert_eq!(status(&after), 200, "{after}");
        let final_request = upstream
            .requests()
            .pop()
            .expect("post-compaction inference");
        assert!(
            final_request.contains("SUMMARY FROM EMPTY"),
            "{final_request}"
        );
        assert!(
            !final_request.contains("FULL PRECOMPACTION HISTORY"),
            "{final_request}"
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
        // This connection remains in the kernel backlog: the sole worker is
        // blocked on the slow header above and must not create a second worker.
        let mut queued = TcpStream::connect(addr).unwrap();
        queued
            .set_read_timeout(Some(Duration::from_secs(2)))
            .unwrap();
        queued
            .write_all(b"GET / HTTP/1.1\r\nHost: queued\r\n\r\n")
            .unwrap();
        thread::sleep(Duration::from_millis(50));
        assert_eq!(bridge.active_requests(), 1);
        let shutdown_started = Instant::now();
        bridge.shutdown().unwrap();
        assert!(shutdown_started.elapsed() < Duration::from_secs(5));
        let mut byte = [0_u8; 1];
        let queued_result = queued.read(&mut byte);
        assert!(
            matches!(queued_result, Ok(0))
                || queued_result.as_ref().is_err_and(|error| {
                    matches!(
                        error.kind(),
                        io::ErrorKind::ConnectionAborted
                            | io::ErrorKind::ConnectionReset
                            | io::ErrorKind::NotConnected
                    )
                }),
            "listener shutdown must tear down a queued socket: {queued_result:?}"
        );
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
        let bridge = BridgeHandle::start(
            BridgeConfig {
                max_concurrency: 1,
                ..bridged_config(&upstream)
            }
            .with_request_hold(Duration::from_millis(100)),
        )
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
        assert_eq!(
            upstream
                .requests()
                .iter()
                .filter(|request| request.starts_with("POST /v1/responses HTTP/1.1"))
                .count(),
            4,
            "each queued original must reach upstream once"
        );
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

    /// #1054 RED: local admission saturation is not an API error.  A second
    /// socket stays in the kernel backlog past the old finite budget, then its
    /// original request is admitted exactly once when the sole worker exits.
    #[test]
    fn a_saturated_bridge_admits_the_original_request_after_the_old_budget() {
        let (admitted_tx, admitted_rx) = std::sync::mpsc::sync_channel(1);
        let upstream = FakeResponses::start();
        let log_dir = tempfile::tempdir().unwrap();
        let log_path = log_dir.path().join("bridge.jsonl");
        // Keep the first worker alive past the former admission deadline.
        // The second request must remain unaccepted rather than collecting a
        // synthetic 503, then make one normal upstream call after admission.
        let bridge = BridgeHandle::start(
            BridgeConfig {
                max_concurrency: 1,
                header_timeout: Duration::from_secs(5),
                ..bridged_config(&upstream)
            }
            .with_request_hold(Duration::from_secs(2))
            .with_admission_notifier(admitted_tx)
            .with_log_path(log_path.clone()),
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

        let queued_token = token.clone();
        let queued = thread::spawn(move || {
            request(
                addr,
                &authorized("POST", "/v1/messages", &queued_token, PROBE_BODY),
            )
        });
        thread::sleep(Duration::from_millis(500));
        assert!(
            !queued.is_finished(),
            "the queued socket must outlive the old admission budget"
        );
        let response = queued.join().expect("queued request thread");
        assert_eq!(status(&response), 200, "queued request: {response}");
        assert!(!response.contains("bridge busy"));
        assert_eq!(
            upstream
                .requests()
                .iter()
                .filter(|request| request.starts_with("POST /v1/responses HTTP/1.1"))
                .count(),
            1,
            "the queued original must make exactly one upstream request"
        );
        drop(occupied);
        let mut bridge = bridge;
        bridge.shutdown().unwrap();
        let log = std::fs::read_to_string(log_path).unwrap_or_default();
        assert!(log.contains("admission_queued"), "{log}");
        assert!(log.contains("admission_acquired"), "{log}");
        assert!(log.contains("wait_ms"), "{log}");
    }

    /// Saturation alone is not an admission queue.  The listener must have an
    /// actual later socket before the aggregate queue observability is emitted.
    #[test]
    fn a_lone_held_request_emits_no_admission_events() {
        let log_dir = tempfile::tempdir().unwrap();
        let log_path = log_dir.path().join("bridge.jsonl");
        let upstream = FakeResponses::start();
        let mut bridge = BridgeHandle::start(
            bridged_config(&upstream)
                .with_request_hold(Duration::from_millis(250))
                .with_log_path(log_path.clone()),
        )
        .unwrap();
        let response = request(
            bridge.socket_addr(),
            &authorized("POST", "/v1/messages", bridge.bearer_token(), PROBE_BODY),
        );
        assert_eq!(status(&response), 200, "{response}");
        bridge.shutdown().unwrap();
        let log = std::fs::read_to_string(log_path).unwrap_or_default();
        assert!(!log.contains("admission_queued"), "{log}");
        assert!(!log.contains("admission_acquired"), "{log}");
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
    /// pins our half of that contract — a catalog-known id (anything else is
    /// refused up front since #1005) is parsed, expanded, and sent, with the
    /// suffix landing in `reasoning.effort` rather than in the model id.
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
    fn agent_continuation_preserves_eight_split_parallel_tool_results() {
        let upstream = FakeResponses::start_with_responses(vec![
            Some(parallel_function_call_response(8)),
            None,
        ]);
        let bridge = BridgeHandle::start(bridged_config(&upstream)).unwrap();
        let headers = [
            ("X-Claude-Code-Session-Id", "session-parallel"),
            ("x-claude-code-agent-id", "agent-parallel"),
        ];
        let first = request(
            bridge.socket_addr(),
            &authorized_with_headers(
                "POST",
                "/v1/messages",
                bridge.bearer_token(),
                PROBE_BODY,
                &headers,
            ),
        );
        assert_eq!(status(&first), 200, "{first}");

        let continuation = split_parallel_result_body(8, true);
        let second = request(
            bridge.socket_addr(),
            &authorized_with_headers(
                "POST",
                "/v1/messages",
                bridge.bearer_token(),
                &continuation,
                &headers,
            ),
        );
        assert_eq!(status(&second), 200, "{second}");

        let requests = upstream.requests();
        let body: serde_json::Value = serde_json::from_str(
            requests
                .iter()
                .filter(|request| request.starts_with("POST /v1/responses HTTP/1.1"))
                .nth(1)
                .expect("continuation upstream request")
                .split("\r\n\r\n")
                .nth(1)
                .expect("continuation request body"),
        )
        .expect("continuation JSON");
        let input = body["input"].as_array().unwrap();
        let calls = input
            .iter()
            .filter(|item| item["type"] == "function_call")
            .collect::<Vec<_>>();
        let outputs = input
            .iter()
            .filter(|item| item["type"] == "function_call_output")
            .collect::<Vec<_>>();
        assert_eq!(calls.len(), 8, "all calls must remain canonical: {body}");
        assert_eq!(outputs.len(), 8, "all results must reach upstream: {body}");
        for index in 0..8 {
            assert_eq!(calls[index]["call_id"], format!("call_{index}"));
            assert_eq!(outputs[index]["call_id"], format!("call_{index}"));
        }
    }

    /// Regression for #960: a failed turn does not commit its pending tool
    /// result, and Claude records a later assistant error before replaying the
    /// conversation. The bridge must recover the real result from that full
    /// replay rather than wedging on the final-assistant suffix boundary.
    #[test]
    fn failed_stream_recovers_pending_output_from_full_replay() {
        let failed_assistants = [
            (
                "synthetic-error",
                serde_json::json!({
                    "role": "assistant",
                    "content": "API Error: Server error mid-response.",
                }),
            ),
            (
                "partial-tool-use",
                serde_json::json!({
                    "role": "assistant",
                    "content": [{
                        "type": "tool_use",
                        "id": "call_partial",
                        "name": "SendMessage",
                        "input": {},
                    }],
                }),
            ),
        ];

        for (case, failed_assistant) in failed_assistants {
            let upstream = FakeResponses::start_with_responses(vec![
                Some(function_call_response()),
                Some(visible_transport_failure_response()),
                None,
                None,
            ]);
            let tmp = tempfile::tempdir().unwrap();
            let log_path = tmp.path().join("bridge.jsonl");
            let mut bridge =
                BridgeHandle::start(bridged_config(&upstream).with_log_path(log_path.clone()))
                    .unwrap();
            let headers = [("X-Claude-Code-Session-Id", "secret-session-960")];

            let first = request(
                bridge.socket_addr(),
                &authorized_with_headers(
                    "POST",
                    "/v1/messages",
                    bridge.bearer_token(),
                    PROBE_STREAM_BODY,
                    &headers,
                ),
            );
            assert_eq!(status(&first), 200, "{case}: {first}");

            let failed = request(
                bridge.socket_addr(),
                &authorized_with_headers(
                    "POST",
                    "/v1/messages",
                    bridge.bearer_token(),
                    &failed_turn_replay_body(Vec::new()),
                    &headers,
                ),
            );
            assert_eq!(status(&failed), 200, "{case}: {failed}");
            assert!(failed.contains("partial"), "{case}: {failed}");
            assert!(failed.contains("event: error"), "{case}: {failed}");
            assert_eq!(
                upstream
                    .requests()
                    .iter()
                    .filter(|request| request.starts_with("POST /v1/responses HTTP/1.1"))
                    .count(),
                2,
                "{case}: a visible failed turn must not be retried"
            );

            let recovered = request(
                bridge.socket_addr(),
                &authorized_with_headers(
                    "POST",
                    "/v1/messages",
                    bridge.bearer_token(),
                    &failed_turn_replay_body(vec![
                        failed_assistant.clone(),
                        serde_json::json!({"role": "user", "content": "continue"}),
                    ]),
                    &headers,
                ),
            );
            assert_eq!(status(&recovered), 200, "{case}: {recovered}");

            let later = request(
                bridge.socket_addr(),
                &authorized_with_headers(
                    "POST",
                    "/v1/messages",
                    bridge.bearer_token(),
                    &failed_turn_replay_body(vec![
                        failed_assistant,
                        serde_json::json!({"role": "user", "content": "continue"}),
                        serde_json::json!({"role": "assistant", "content": "bridged reply"}),
                        serde_json::json!({"role": "user", "content": "still usable"}),
                    ]),
                    &headers,
                ),
            );
            assert_eq!(status(&later), 200, "{case}: {later}");

            let requests = upstream
                .requests()
                .into_iter()
                .filter(|request| request.starts_with("POST /v1/responses HTTP/1.1"))
                .collect::<Vec<_>>();
            assert_eq!(requests.len(), 4, "{case}: recovery did not reach upstream");
            for request in &requests[2..] {
                let body: serde_json::Value =
                    serde_json::from_str(request.split_once("\r\n\r\n").expect("upstream body").1)
                        .expect("upstream JSON");
                let outputs = body["input"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .filter(|item| {
                        item["type"] == "function_call_output" && item["call_id"] == "call_parent"
                    })
                    .count();
                assert_eq!(outputs, 1, "{case}: recovered output duplicated: {body}");
            }

            bridge.shutdown().unwrap();
            let log = std::fs::read_to_string(log_path).unwrap();
            assert!(
                log.contains(r#""event":"pending_outputs_recovered""#),
                "{case}: {log}"
            );
            assert!(log.contains(r#""recovered_count":1"#), "{case}: {log}");
            assert!(
                log.contains(r#""conversation_scope":"main""#),
                "{case}: {log}"
            );
            for forbidden in ["call_parent", "result-parent", "secret-session-960"] {
                assert!(
                    !log.contains(forbidden),
                    "{case}: leaked {forbidden}: {log}"
                );
            }
        }
    }

    /// A harness transcript whose compaction dropped the assistant `tool_use`
    /// but kept the user `tool_result` — the shape that wedged production.
    fn orphaned_tool_result_body() -> String {
        serde_json::json!({
            "model": "claude-x",
            "messages": [
                {"role": "user", "content": "carry on"},
                {"role": "user", "content": [{
                    "type": "tool_result",
                    "tool_use_id": "call_0",
                    "content": "result-0",
                }]},
            ],
            "stream": false,
        })
        .to_string()
    }

    /// End-to-end reproduction of the `/loop` wedge, through the real bridge.
    ///
    /// Sequence, exactly as the fastled session ran it:
    ///   1. a turn that leaves a tool batch outstanding (canonical history now
    ///      holds a `function_call` with no result);
    ///   2. `POST /_clud/context/compact` — `validate_canonical_history` fails
    ///      on that outstanding call, so the bridge takes the documented
    ///      fallback and `clear_items()` the history;
    ///   3. the harness sends its own compacted transcript, in which the
    ///      assistant `tool_use` is gone but the user `tool_result` remains.
    ///
    /// Before the fix, step 3 reached upstream verbatim and earned a permanent
    /// `invalid_request_error: No tool call found for function call output`,
    /// which then repeated on every scheduled firing.
    #[test]
    fn compaction_fallback_then_orphaned_result_still_reaches_upstream_valid() {
        let upstream = FakeResponses::start_with_responses(vec![
            Some(parallel_function_call_response(2)),
            None,
        ]);
        let mut bridge = BridgeHandle::start(bridged_config(&upstream)).unwrap();

        // 1. Upstream answers with a parallel tool batch, so canonical
        //    history now holds `function_call`s whose results have not
        //    arrived — the "tool batch outstanding" state the bridge's own
        //    comment calls out.
        let first = request(
            bridge.socket_addr(),
            &authorized("POST", "/v1/messages", bridge.bearer_token(), PROBE_BODY),
        );
        assert_eq!(status(&first), 200, "{first}");

        // 2. Compaction with the batch outstanding -> documented fallback,
        //    which clears the canonical history.
        let compacted = request(
            bridge.socket_addr(),
            &authorized(
                "POST",
                "/_clud/context/compact",
                bridge.bearer_token(),
                r#"{"hook_event_name":"PreCompact","trigger":"auto"}"#,
            ),
        );
        assert_eq!(
            status(&compacted),
            204,
            "the fallback must not block the harness's compaction lifecycle: {compacted}"
        );

        // 3. The harness's own compacted transcript, orphaned.
        let after = request(
            bridge.socket_addr(),
            &authorized(
                "POST",
                "/v1/messages",
                bridge.bearer_token(),
                &orphaned_tool_result_body(),
            ),
        );
        assert_ne!(
            status(&after),
            400,
            "an orphaned tool result must not wedge the session: {after}"
        );

        // The decisive assertion: whatever we sent upstream carried no
        // function_call_output without a matching function_call.
        let sent = upstream.requests();
        let last = sent.last().expect("a request must have reached upstream");
        let body: serde_json::Value =
            serde_json::from_str(last.split("\r\n\r\n").nth(1).expect("request body"))
                .expect("upstream body is json");
        let input = body["input"].as_array().cloned().unwrap_or_default();
        let calls: std::collections::HashSet<&str> = input
            .iter()
            .filter(|item| item["type"] == "function_call")
            .filter_map(|item| item["call_id"].as_str())
            .collect();
        let orphans = input
            .iter()
            .filter(|item| item["type"] == "function_call_output")
            .filter(|item| {
                item["call_id"]
                    .as_str()
                    .is_none_or(|id| !calls.contains(id))
            })
            .count();
        assert_eq!(orphans, 0, "orphaned tool result reached upstream: {body}");
        // And the result itself was preserved rather than dropped.
        assert!(
            last.contains("result-0"),
            "the tool output must survive the repair: {last}"
        );
        let _ = bridge.shutdown();
    }

    #[test]
    fn unmatched_parallel_call_is_rejected_locally_with_safe_diagnostics() {
        let upstream = FakeResponses::start_with_responses(vec![
            Some(parallel_function_call_response(2)),
            None,
        ]);
        let tmp = tempfile::tempdir().unwrap();
        let log_path = tmp.path().join("bridge.jsonl");
        let mut bridge =
            BridgeHandle::start(bridged_config(&upstream).with_log_path(log_path.clone())).unwrap();
        let headers = [
            ("X-Claude-Code-Session-Id", "secret-session-id"),
            ("x-claude-code-agent-id", "secret-agent-id"),
        ];
        let first = request(
            bridge.socket_addr(),
            &authorized_with_headers(
                "POST",
                "/v1/messages",
                bridge.bearer_token(),
                PROBE_BODY,
                &headers,
            ),
        );
        assert_eq!(status(&first), 200, "{first}");

        let malformed = split_parallel_result_body(2, false);
        let second = request(
            bridge.socket_addr(),
            &authorized_with_headers(
                "POST",
                "/v1/messages",
                bridge.bearer_token(),
                &malformed,
                &headers,
            ),
        );
        assert_eq!(status(&second), 400, "{second}");
        bridge.shutdown().unwrap();

        let upstream_requests = upstream
            .requests()
            .into_iter()
            .filter(|request| request.starts_with("POST /v1/responses HTTP/1.1"))
            .count();
        assert_eq!(
            upstream_requests, 1,
            "invalid continuation reached upstream"
        );
        let log = std::fs::read_to_string(log_path).unwrap();
        assert!(
            log.contains(r#""event":"continuation_invariant_failure""#),
            "{log}"
        );
        assert!(log.contains(r#""unmatched_call_count":2"#), "{log}");
        assert!(log.contains(r#""conversation_scope":"agent""#), "{log}");
        for forbidden in [
            "call_0",
            "call_1",
            "secret-session-id",
            "secret-agent-id",
            "missing results",
        ] {
            assert!(!log.contains(forbidden), "leaked {forbidden:?}: {log}");
        }
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
        // The scenario is "an in-band 400 *after a successful turn*", so the
        // log legitimately holds one line per turn. Parsing the whole file as
        // a single JSON value fails with `trailing characters, line: 2`.
        // Select the event by name, the way
        // `a_non_streaming_in_band_400_is_logged_and_sanitized` below already
        // does against the same log.
        let event = text
            .lines()
            .map(|line| serde_json::from_str::<serde_json::Value>(line).unwrap())
            .find(|event| event["event"] == "in_band_upstream_failure")
            .expect("in-band failure event");
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
    ///
    /// Since #1005 the bridge refuses `tera` itself, so the translator's
    /// `SelectionError::UnknownAlias` text is no longer reachable from this
    /// route -- `codex_model::tests::an_unknown_alias_names_the_valid_ones`
    /// keeps that assertion. What this test pins is the bridge's own case-1
    /// wording: the typo is named, and the servable IDs are listed.
    #[test]
    fn a_typo_of_a_model_alias_is_a_400_naming_the_servable_ids() {
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
        assert!(
            response.contains("unknown clud Codex model 'tera'"),
            "{response}"
        );
        assert!(response.contains("clud-claude-codex-terra"), "{response}");
        assert!(!response.contains("clud knows the model"), "{response}");
        assert!(
            upstream.requests().is_empty(),
            "a rejected selection must not reach upstream"
        );
    }
}
