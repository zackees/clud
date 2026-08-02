//! Authenticated loopback bridge used by Codex-provider launches through the
//! Claude harness (issue #626).

use base64::Engine as _;
use std::collections::HashMap;
use std::fmt;
use std::io::{self, Read, Write};
use std::net::{IpAddr, Ipv4Addr, Shutdown, TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

const DEFAULT_MAX_BODY_BYTES: usize = 1024 * 1024;
const DEFAULT_MAX_HEADER_BYTES: usize = 32 * 1024;
const DEFAULT_IO_TIMEOUT: Duration = Duration::from_secs(5);
const DEFAULT_MAX_CONCURRENCY: usize = 4;
const ACCEPT_POLL: Duration = Duration::from_millis(5);

type ActiveConnections = Arc<Mutex<HashMap<usize, TcpStream>>>;

/// Resource and test-seam policy for one bridge launch.
#[derive(Clone)]
pub struct BridgeConfig {
    pub max_body_bytes: usize,
    pub max_header_bytes: usize,
    pub io_timeout: Duration,
    pub max_concurrency: usize,
    test_upstream_url: Option<String>,
    #[cfg(test)]
    request_hold: Duration,
    #[cfg(test)]
    admission_notifier: Option<std::sync::mpsc::SyncSender<()>>,
}

impl Default for BridgeConfig {
    fn default() -> Self {
        Self {
            max_body_bytes: DEFAULT_MAX_BODY_BYTES,
            max_header_bytes: DEFAULT_MAX_HEADER_BYTES,
            io_timeout: DEFAULT_IO_TIMEOUT,
            max_concurrency: DEFAULT_MAX_CONCURRENCY,
            test_upstream_url: test_upstream_override_from_process(),
            #[cfg(test)]
            request_hold: Duration::ZERO,
            #[cfg(test)]
            admission_notifier: None,
        }
    }
}

impl BridgeConfig {
    #[cfg(test)]
    fn with_test_upstream_url(mut self, url: Option<String>) -> Self {
        self.test_upstream_url = url;
        self
    }

    #[cfg(test)]
    fn with_request_hold(mut self, hold: Duration) -> Self {
        self.request_hold = hold;
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
            .field("io_timeout", &self.io_timeout)
            .field("max_concurrency", &self.max_concurrency)
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
    connections: ActiveConnections,
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
        let connections = Arc::new(Mutex::new(HashMap::new()));
        let (ready_tx, ready_rx) = std::sync::mpsc::sync_channel(0);
        let thread_shutdown = Arc::clone(&shutdown);
        let thread_active = Arc::clone(&active);
        let thread_connections = Arc::clone(&connections);
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
                    thread_connections,
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
            connections,
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
        if let Some(join) = self.join.take() {
            join.join().map_err(|_| BridgeError::Join)?;
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

fn serve(
    listener: TcpListener,
    config: BridgeConfig,
    bearer_token: String,
    shutdown: Arc<AtomicBool>,
    active: Arc<AtomicUsize>,
    connections: ActiveConnections,
) {
    let mut workers = Vec::<JoinHandle<()>>::new();
    let mut next_worker_id = 0_usize;
    while !shutdown.load(Ordering::Acquire) {
        workers.retain(|worker| !worker.is_finished());
        match listener.accept() {
            Ok((stream, _peer)) => {
                if !reserve_worker(&active, config.max_concurrency.max(1)) {
                    reject_busy(stream, config.io_timeout);
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
                let worker_connections = Arc::clone(&connections);
                let worker_config = config.clone();
                let worker_token = bearer_token.clone();
                match thread::Builder::new()
                    .name("clud-codex-bridge-request".to_string())
                    .spawn(move || {
                        let _guard = ActiveWorker {
                            active: worker_active,
                            connections: worker_connections,
                            worker_id,
                        };
                        handle_connection(stream, &worker_config, &worker_token);
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

fn reject_busy(mut stream: TcpStream, timeout: Duration) {
    // Consume the already-buffered request headers before closing. On Windows,
    // closing a TCP socket with unread inbound bytes sends RST and can discard
    // the 503 that was just written.
    let drain_timeout = timeout.min(Duration::from_millis(100));
    let deadline = Instant::now() + drain_timeout;
    let _ = read_headers(&mut stream, DEFAULT_MAX_HEADER_BYTES, deadline);
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

fn handle_connection(mut stream: TcpStream, config: &BridgeConfig, bearer_token: &str) {
    #[cfg(test)]
    if let Some(notifier) = &config.admission_notifier {
        let _ = notifier.try_send(());
    }
    #[cfg(test)]
    if !config.request_hold.is_zero() {
        thread::sleep(config.request_hold);
    }
    let deadline = Instant::now() + config.io_timeout;
    if stream.set_write_timeout(Some(config.io_timeout)).is_err() {
        return;
    }

    let parsed = match read_headers(&mut stream, config.max_header_bytes, deadline) {
        Ok(parsed) => parsed,
        Err(status) => {
            let _ = write_error(&mut stream, status);
            return;
        }
    };

    if parsed.content_length > config.max_body_bytes {
        let _ = write_error(&mut stream, 413);
        return;
    }
    let expected = format!("Bearer {bearer_token}");
    if !parsed
        .authorization
        .as_deref()
        .is_some_and(|provided| constant_time_eq(provided.as_bytes(), expected.as_bytes()))
    {
        let _ = write_error(&mut stream, 401);
        return;
    }

    match (parsed.method.as_str(), parsed.path.as_str()) {
        ("HEAD", "/v1/messages") => {
            let _ = write_response(&mut stream, 200, "application/json", b"", true);
        }
        ("POST", "/v1/messages/count_tokens") => {
            let _ = write_response(
                &mut stream,
                404,
                "application/json",
                br#"{"error":{"type":"not_found_error","message":"count_tokens unsupported in bridge phase 2"}}"#,
                false,
            );
        }
        ("POST", "/v1/messages") => {
            let body = match read_body(
                &mut stream,
                parsed.body_prefix,
                parsed.content_length,
                deadline,
            ) {
                Ok(body) => body,
                Err(status) => {
                    let _ = write_error(&mut stream, status);
                    return;
                }
            };
            let json: serde_json::Value = match serde_json::from_slice(&body) {
                Ok(json) => json,
                Err(_) => {
                    let _ = write_error(&mut stream, 400);
                    return;
                }
            };
            if let Some(upstream_url) = config.test_upstream_url.as_deref() {
                forward_test_upstream(&mut stream, config, upstream_url, &body);
            } else if json.get("stream").and_then(serde_json::Value::as_bool) == Some(true) {
                let _ = write_response(
                    &mut stream,
                    200,
                    "text/event-stream",
                    fixture_sse().as_bytes(),
                    false,
                );
            } else {
                let _ = write_response(
                    &mut stream,
                    200,
                    "application/json",
                    fixture_message().as_bytes(),
                    false,
                );
            }
        }
        _ => {
            let _ = write_error(&mut stream, 404);
        }
    }
}

struct ParsedRequest {
    method: String,
    path: String,
    authorization: Option<String>,
    content_length: usize,
    body_prefix: Vec<u8>,
}

fn read_headers(
    stream: &mut TcpStream,
    limit: usize,
    deadline: Instant,
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
        set_remaining_read_timeout(stream, deadline)?;
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
                return Err(408);
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
    let mut content_length = 0_usize;
    for line in lines {
        let (name, value) = line.split_once(':').ok_or(400_u16)?;
        let value = value.trim();
        if name.eq_ignore_ascii_case("authorization") {
            authorization = Some(value.to_string());
        } else if name.eq_ignore_ascii_case("content-length") {
            content_length = value.parse().map_err(|_| 400_u16)?;
        }
    }
    Ok(ParsedRequest {
        method,
        path,
        authorization,
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
) -> Result<Vec<u8>, u16> {
    body.truncate(content_length);
    while body.len() < content_length {
        set_remaining_read_timeout(stream, deadline)?;
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
                return Err(408);
            }
            Err(_) => return Err(400),
        }
    }
    Ok(body)
}

fn set_remaining_read_timeout(stream: &TcpStream, deadline: Instant) -> Result<(), u16> {
    let remaining = deadline
        .checked_duration_since(Instant::now())
        .filter(|duration| !duration.is_zero())
        .ok_or(408_u16)?;
    stream.set_read_timeout(Some(remaining)).map_err(|_| 400)
}

fn forward_test_upstream(
    stream: &mut TcpStream,
    config: &BridgeConfig,
    upstream_url: &str,
    body: &[u8],
) {
    let messages_url = if upstream_url.trim_end_matches('/').ends_with("/v1/messages") {
        upstream_url.trim_end_matches('/').to_string()
    } else {
        format!("{}/v1/messages", upstream_url.trim_end_matches('/'))
    };
    let agent = ureq::AgentBuilder::new().timeout(config.io_timeout).build();
    let response = match agent
        .post(&messages_url)
        .set("Content-Type", "application/json")
        .send_bytes(body)
    {
        Ok(response) => response,
        Err(ureq::Error::Status(_, response)) => response,
        Err(ureq::Error::Transport(_)) => {
            let _ = write_error(stream, 502);
            return;
        }
    };
    let status = response.status();
    let content_type = response
        .header("Content-Type")
        .unwrap_or("application/json")
        .to_string();
    let mut upstream_body = Vec::new();
    if response
        .into_reader()
        .take(config.max_body_bytes.saturating_add(1) as u64)
        .read_to_end(&mut upstream_body)
        .is_err()
        || upstream_body.len() > config.max_body_bytes
    {
        let _ = write_error(stream, 502);
        return;
    }
    let _ = write_response(stream, status, &content_type, &upstream_body, false);
}

fn write_error(stream: &mut TcpStream, status: u16) -> io::Result<()> {
    let (error_type, message) = match status {
        400 => ("invalid_request_error", "invalid request"),
        401 => ("authentication_error", "unauthorized"),
        404 => ("not_found_error", "not found"),
        408 => ("timeout_error", "request timeout"),
        413 => ("invalid_request_error", "request body too large"),
        431 => ("invalid_request_error", "request headers too large"),
        502 => ("api_error", "test upstream unavailable"),
        _ => ("api_error", "bridge error"),
    };
    let body = format!(r#"{{"error":{{"type":"{error_type}","message":"{message}"}}}}"#);
    write_response(stream, status, "application/json", body.as_bytes(), false)
}

fn write_response(
    stream: &mut TcpStream,
    status: u16,
    content_type: &str,
    body: &[u8],
    head_only: bool,
) -> io::Result<()> {
    let reason = match status {
        200 => "OK",
        400 => "Bad Request",
        401 => "Unauthorized",
        404 => "Not Found",
        408 => "Request Timeout",
        413 => "Payload Too Large",
        431 => "Request Header Fields Too Large",
        502 => "Bad Gateway",
        503 => "Service Unavailable",
        _ => "Error",
    };
    write!(
        stream,
        "HTTP/1.1 {status} {reason}\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\nCache-Control: no-store\r\nConnection: close\r\n\r\n",
        body.len()
    )?;
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

fn fixture_message() -> String {
    serde_json::json!({
        "id": "msg_clud_fixture",
        "type": "message",
        "role": "assistant",
        "model": "clud-bridge-fixture",
        "content": [{"type": "text", "text": "clud bridge fixture"}],
        "stop_reason": "end_turn",
        "stop_sequence": null,
        "usage": {"input_tokens": 0, "output_tokens": 4}
    })
    .to_string()
}

fn fixture_sse() -> String {
    concat!(
        "event: message_start\n",
        r#"data: {"type":"message_start","message":{"id":"msg_clud_fixture","type":"message","role":"assistant","model":"clud-bridge-fixture","content":[],"stop_reason":null,"stop_sequence":null,"usage":{"input_tokens":0,"output_tokens":0}}}"#,
        "\n\n",
        "event: content_block_start\n",
        r#"data: {"type":"content_block_start","index":0,"content_block":{"type":"text","text":""}}"#,
        "\n\n",
        "event: content_block_delta\n",
        r#"data: {"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"clud bridge fixture"}}"#,
        "\n\n",
        "event: content_block_stop\n",
        r#"data: {"type":"content_block_stop","index":0}"#,
        "\n\n",
        "event: message_delta\n",
        r#"data: {"type":"message_delta","delta":{"stop_reason":"end_turn","stop_sequence":null},"usage":{"output_tokens":4}}"#,
        "\n\n",
        "event: message_stop\n",
        r#"data: {"type":"message_stop"}"#,
        "\n\n",
    )
    .to_owned()
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

    fn request(addr: SocketAddr, request: &str) -> String {
        let mut stream = TcpStream::connect(addr).expect("connect to bridge");
        stream
            .set_read_timeout(Some(Duration::from_secs(2)))
            .unwrap();
        stream.write_all(request.as_bytes()).unwrap();
        stream.shutdown(std::net::Shutdown::Write).unwrap();
        let mut response = String::new();
        stream.read_to_string(&mut response).unwrap();
        response
    }

    fn authorized(method: &str, path: &str, token: &str, body: &str) -> String {
        format!(
            "{method} {path} HTTP/1.1\r\nHost: 127.0.0.1\r\nAuthorization: Bearer {token}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
            body.len()
        )
    }

    fn status(response: &str) -> u16 {
        response
            .split_whitespace()
            .nth(1)
            .expect("HTTP status")
            .parse()
            .unwrap()
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
        let bridge = BridgeHandle::start(BridgeConfig::default()).unwrap();
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
            &authorized(
                "POST",
                "/v1/messages",
                &token,
                r#"{"model":"fixture","messages":[],"stream":false}"#,
            ),
        );
        assert_eq!(status(&non_stream), 200);
        assert!(non_stream.contains("application/json"));
        assert!(non_stream.contains("\"type\":\"message\""));
        assert!(non_stream.contains("\"text\":\"clud bridge fixture\""));

        let stream = request(
            addr,
            &authorized(
                "POST",
                "/v1/messages",
                &token,
                r#"{"model":"fixture","messages":[],"stream":true}"#,
            ),
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
        assert!(count.contains("unsupported"));
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
    fn enforces_body_header_timeout_and_concurrency_bounds() {
        let config = BridgeConfig {
            max_body_bytes: 64,
            max_header_bytes: 256,
            io_timeout: Duration::from_millis(100),
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
                io_timeout: Duration::from_millis(150),
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

        drop(bridge);
        let bridge = BridgeHandle::start(
            BridgeConfig {
                max_concurrency: 1,
                io_timeout: Duration::from_secs(2),
                ..BridgeConfig::default()
            }
            .with_request_hold(Duration::from_secs(1)),
        )
        .unwrap();
        let addr = bridge.socket_addr();
        let token = bridge.bearer_token().to_owned();
        let mut occupied = TcpStream::connect(addr).unwrap();
        occupied
            .write_all(authorized("HEAD", "/v1/messages", &token, "").as_bytes())
            .unwrap();
        // A heavily loaded Windows host can take over a second to schedule a
        // freshly created listener thread. Observe admission rather than
        // assuming a wall-clock sleep is enough.
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        while bridge.active_requests() != 1 && std::time::Instant::now() < deadline {
            thread::sleep(Duration::from_millis(5));
        }
        assert_eq!(
            bridge.active_requests(),
            1,
            "blocking request was not admitted"
        );
        let saturated = request(addr, &authorized("HEAD", "/v1/messages", &token, ""));
        assert_eq!(status(&saturated), 503);
        drop(occupied);
    }

    #[test]
    fn shutdown_and_drop_are_idempotent_and_close_the_listener() {
        let (admitted_tx, admitted_rx) = std::sync::mpsc::sync_channel(1);
        let mut bridge = BridgeHandle::start(
            BridgeConfig {
                io_timeout: Duration::from_secs(10),
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

    #[test]
    fn gated_test_upstream_is_used_for_message_requests() {
        let upstream = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
        let upstream_addr = upstream.local_addr().unwrap();
        let worker = thread::spawn(move || {
            let (mut stream, _) = upstream.accept().unwrap();
            stream
                .set_read_timeout(Some(Duration::from_secs(2)))
                .unwrap();
            let mut request = Vec::new();
            let mut chunk = [0_u8; 1024];
            while !request.ends_with(b"}") {
                let count = stream.read(&mut chunk).unwrap();
                assert_ne!(count, 0);
                request.extend_from_slice(&chunk[..count]);
            }
            stream
                .write_all(
                    b"HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: 27\r\nConnection: close\r\n\r\n{\"test_upstream\":\"reached\"}",
                )
                .unwrap();
            String::from_utf8(request).unwrap()
        });
        let bridge = BridgeHandle::start(
            BridgeConfig::default().with_test_upstream_url(Some(format!("http://{upstream_addr}"))),
        )
        .unwrap();
        let response = request(
            bridge.socket_addr(),
            &authorized(
                "POST",
                "/v1/messages",
                bridge.bearer_token(),
                r#"{"stream":false}"#,
            ),
        );
        assert_eq!(status(&response), 200);
        assert!(response.contains(r#"{"test_upstream":"reached"}"#));
        let upstream_request = worker.join().unwrap();
        assert!(upstream_request.starts_with("POST /v1/messages HTTP/1.1"));
        assert!(!upstream_request.contains(bridge.bearer_token()));
    }
}
