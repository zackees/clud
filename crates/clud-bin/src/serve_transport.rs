//! Socket cancellation and bounded request draining for standalone gateways.

use std::collections::HashMap;
use std::fmt;
use std::io::{self, Read, Write};
use std::net::{Shutdown, TcpStream};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

#[derive(Default)]
struct SocketState {
    closed: bool,
    next_id: usize,
    sockets: HashMap<usize, TcpStream>,
}

/// Owns duplicate handles to in-flight upstream sockets, never credentials.
#[derive(Default)]
pub(crate) struct UpstreamSockets(Mutex<SocketState>);

impl UpstreamSockets {
    fn register(self: &Arc<Self>, stream: &TcpStream) -> io::Result<SocketLease> {
        let socket = stream.try_clone()?;
        let mut state = self
            .0
            .lock()
            .map_err(|_| io::Error::other("socket registry unavailable"))?;
        if state.closed {
            let _ = socket.shutdown(Shutdown::Both);
            return Err(io::Error::new(
                io::ErrorKind::Interrupted,
                "gateway shutting down",
            ));
        }
        let id = state.next_id;
        state.next_id = state.next_id.wrapping_add(1);
        state.sockets.insert(id, socket);
        Ok(SocketLease {
            registry: Arc::clone(self),
            id,
        })
    }

    fn is_closed(&self) -> bool {
        self.0
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .closed
    }

    pub(crate) fn cancel(&self) {
        let mut state = self.0.lock().unwrap_or_else(|error| error.into_inner());
        state.closed = true;
        for socket in state.sockets.values() {
            let _ = socket.shutdown(Shutdown::Both);
        }
    }
}

struct SocketLease {
    registry: Arc<UpstreamSockets>,
    id: usize,
}

impl Drop for SocketLease {
    fn drop(&mut self) {
        let mut state = self
            .registry
            .0
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        state.sockets.remove(&self.id);
    }
}

struct TrackedTlsConnector {
    tls: Arc<dyn ureq::TlsConnector>,
    sockets: Arc<UpstreamSockets>,
}

impl ureq::TlsConnector for TrackedTlsConnector {
    fn connect(
        &self,
        dns_name: &str,
        io: Box<dyn ureq::ReadWrite>,
    ) -> Result<Box<dyn ureq::ReadWrite>, ureq::Error> {
        let socket = io
            .socket()
            .ok_or_else(|| io::Error::other("upstream socket unavailable"))?;
        let lease = self.sockets.register(socket)?;
        let cancellable = CancellableIo {
            inner: io,
            sockets: Arc::clone(&self.sockets),
        };
        let inner = self.tls.connect(dns_name, Box::new(cancellable))?;
        Ok(Box::new(TrackedStream {
            inner,
            _lease: lease,
        }))
    }
}

/// Bound raw socket operations without retrying blocking socket timeouts.
/// ureq supplies a blocking stream. Each operation temporarily uses nonblocking
/// mode; WouldBlock means no bytes moved and can safely be retried on Windows.
struct CancellableIo {
    inner: Box<dyn ureq::ReadWrite>,
    sockets: Arc<UpstreamSockets>,
}

impl fmt::Debug for CancellableIo {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("CancellableIo")
    }
}

/// Restore ureq's blocking-mode contract on success, error, or unwinding.
struct BlockingRestore<'a> {
    inner: &'a mut dyn ureq::ReadWrite,
    active: bool,
}

impl BlockingRestore<'_> {
    fn restore(&mut self) -> io::Result<()> {
        self.inner
            .socket()
            .ok_or_else(|| io::Error::other("missing socket"))?
            .set_nonblocking(false)?;
        self.active = false;
        Ok(())
    }
}

impl Drop for BlockingRestore<'_> {
    fn drop(&mut self) {
        if self.active {
            let _ = self.restore();
        }
    }
}

impl CancellableIo {
    fn transfer(
        &mut self,
        writing: bool,
        mut operation: impl FnMut(&mut dyn ureq::ReadWrite) -> io::Result<usize>,
    ) -> io::Result<usize> {
        const POLL: Duration = Duration::from_millis(10);
        let socket = self
            .inner
            .socket()
            .ok_or_else(|| io::Error::other("missing socket"))?;
        let original = if writing {
            socket.write_timeout()?
        } else {
            socket.read_timeout()?
        };
        let deadline = original.map(|timeout| Instant::now() + timeout);
        socket.set_nonblocking(true)?;
        let mut restore = BlockingRestore {
            inner: self.inner.as_mut(),
            active: true,
        };
        let result = loop {
            if self.sockets.is_closed() {
                break Err(io::Error::new(
                    io::ErrorKind::ConnectionAborted,
                    "gateway shutting down",
                ));
            }
            let remaining = deadline.map(|value| value.saturating_duration_since(Instant::now()));
            if remaining.is_some_and(|value| value.is_zero()) {
                break Err(io::Error::new(io::ErrorKind::TimedOut, "upstream timeout"));
            }
            match operation(&mut *restore.inner) {
                Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                    std::thread::park_timeout(remaining.unwrap_or(POLL).min(POLL));
                }
                result => break result,
            }
        };
        let restored = restore.restore();
        match result {
            Ok(count) => restored.map(|()| count),
            Err(error) => Err(error),
        }
    }
}

impl Read for CancellableIo {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        self.transfer(false, |stream| stream.read(buf))
    }
}

impl Write for CancellableIo {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.transfer(true, |stream| stream.write(buf))
    }
    fn flush(&mut self) -> io::Result<()> {
        self.inner.flush()
    }
}

impl ureq::ReadWrite for CancellableIo {
    fn socket(&self) -> Option<&TcpStream> {
        self.inner.socket()
    }
}

struct TrackedStream {
    inner: Box<dyn ureq::ReadWrite>,
    _lease: SocketLease,
}

impl fmt::Debug for TrackedStream {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("TrackedStream")
    }
}

impl Read for TrackedStream {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        self.inner.read(buf)
    }
}

impl Write for TrackedStream {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.inner.write(buf)
    }
    fn flush(&mut self) -> io::Result<()> {
        self.inner.flush()
    }
}

impl ureq::ReadWrite for TrackedStream {
    fn socket(&self) -> Option<&TcpStream> {
        self.inner.socket()
    }
}

/// Consume a small rejected request body before closing the connection, avoiding
/// a reset that can discard the 401 response on Windows. Untrusted callers get
/// at most 250ms and 64KiB of draining; no provider request or parsing occurs.
pub(crate) fn drain_rejected_body(
    stream: &mut TcpStream,
    mut remaining: usize,
    shutdown: &AtomicBool,
) -> io::Result<()> {
    if remaining == 0 || remaining > 64 * 1024 {
        return Ok(());
    }
    stream.set_nonblocking(true)?;
    let mut restore = BlockingRestore {
        inner: stream,
        active: true,
    };
    let deadline = Instant::now() + Duration::from_millis(250);
    let mut buffer = [0; 4096];
    while remaining > 0 && Instant::now() < deadline && !shutdown.load(Ordering::Acquire) {
        let count = remaining.min(buffer.len());
        match restore.inner.read(&mut buffer[..count]) {
            Ok(0) => break,
            Ok(count) => remaining -= count,
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                std::thread::park_timeout(Duration::from_millis(1));
            }
            Err(_) => break,
        }
    }
    restore.restore()
}

fn tls_config() -> Arc<ureq::rustls::ClientConfig> {
    static CONFIG: OnceLock<Arc<ureq::rustls::ClientConfig>> = OnceLock::new();
    Arc::clone(CONFIG.get_or_init(|| {
        let roots = ureq::rustls::RootCertStore {
            roots: webpki_roots::TLS_SERVER_ROOTS.to_vec(),
        };
        let config = ureq::rustls::ClientConfig::builder_with_provider(
            ureq::rustls::crypto::ring::default_provider().into(),
        )
        .with_protocol_versions(&[&ureq::rustls::version::TLS12, &ureq::rustls::version::TLS13])
        .expect("ring supports TLS 1.2 and 1.3")
        .with_root_certificates(roots)
        .with_no_client_auth();
        Arc::new(config)
    }))
}

// Debug-only loopback seam: tests can exercise the HTTPS connector's socket
// ownership against a plaintext fixture without embedding a private test key.
#[cfg(debug_assertions)]
struct LoopbackFixtureConnector;

#[cfg(debug_assertions)]
impl ureq::TlsConnector for LoopbackFixtureConnector {
    fn connect(
        &self,
        _dns_name: &str,
        io: Box<dyn ureq::ReadWrite>,
    ) -> Result<Box<dyn ureq::ReadWrite>, ureq::Error> {
        let peer = io
            .socket()
            .ok_or_else(|| io::Error::other("fixture socket unavailable"))?
            .peer_addr()?;
        if !peer.ip().is_loopback() {
            return Err(io::Error::other("fixture requires a loopback peer").into());
        }
        Ok(io)
    }
}

fn connector() -> Arc<dyn ureq::TlsConnector> {
    #[cfg(debug_assertions)]
    if crate::provider_auth::test_vault_active()
        && std::env::var_os("CLUD_INTEGRATION_TESTS").is_some_and(|value| value == "1")
        && std::env::var_os("CLUD_TEST_GATEWAY_PLAINTEXT_TLS").is_some_and(|value| value == "1")
    {
        return Arc::new(LoopbackFixtureConnector);
    }
    Arc::new(tls_config())
}

/// Use the same root set as ureq while retaining a cancellation handle.
pub(crate) fn tracked_agent(
    builder: ureq::AgentBuilder,
    sockets: &Arc<UpstreamSockets>,
) -> ureq::Agent {
    builder
        // Provider credentials must never follow a redirect to another origin.
        .redirects(0)
        .tls_connector(Arc::new(TrackedTlsConnector {
            tls: connector(),
            sockets: Arc::clone(sockets),
        }))
        .build()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::{Ipv4Addr, TcpListener};
    use std::sync::mpsc;
    use std::time::Duration;

    #[cfg(debug_assertions)]
    #[derive(Debug, Default)]
    struct SocketlessFixture(std::io::Cursor<Vec<u8>>);

    #[cfg(debug_assertions)]
    impl Read for SocketlessFixture {
        fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
            self.0.read(buf)
        }
    }

    #[cfg(debug_assertions)]
    impl Write for SocketlessFixture {
        fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
            self.0.write(buf)
        }
        fn flush(&mut self) -> io::Result<()> {
            self.0.flush()
        }
    }

    #[cfg(debug_assertions)]
    impl ureq::ReadWrite for SocketlessFixture {
        fn socket(&self) -> Option<&TcpStream> {
            None
        }
    }

    #[cfg(debug_assertions)]
    #[test]
    fn plaintext_fixture_rejects_an_unverifiable_peer() {
        let result = ureq::TlsConnector::connect(
            &LoopbackFixtureConnector,
            "localhost",
            Box::new(SocketlessFixture::default()),
        );
        assert!(result.is_err());
    }

    struct PassthroughConnector;

    impl ureq::TlsConnector for PassthroughConnector {
        fn connect(
            &self,
            _dns_name: &str,
            io: Box<dyn ureq::ReadWrite>,
        ) -> Result<Box<dyn ureq::ReadWrite>, ureq::Error> {
            Ok(io)
        }
    }

    struct BlockingConnector(mpsc::Sender<()>);

    impl ureq::TlsConnector for BlockingConnector {
        fn connect(
            &self,
            _dns_name: &str,
            mut io: Box<dyn ureq::ReadWrite>,
        ) -> Result<Box<dyn ureq::ReadWrite>, ureq::Error> {
            self.0.send(()).unwrap();
            let mut byte = [0_u8; 1];
            let _ = io.read(&mut byte);
            Err(io::Error::other("fixture handshake stopped").into())
        }
    }

    #[test]
    fn cancellation_interrupts_a_stalled_tls_handshake() {
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
        let client = TcpStream::connect(listener.local_addr().unwrap()).unwrap();
        let (_server, _) = listener.accept().unwrap();
        let sockets = Arc::new(UpstreamSockets::default());
        let (started_tx, started_rx) = mpsc::channel();
        let connector = TrackedTlsConnector {
            tls: Arc::new(BlockingConnector(started_tx)),
            sockets: Arc::clone(&sockets),
        };
        let (done_tx, done_rx) = mpsc::channel();
        let worker = std::thread::spawn(move || {
            let result = ureq::TlsConnector::connect(&connector, "localhost", Box::new(client));
            done_tx.send(result.is_err()).unwrap();
        });
        started_rx.recv_timeout(Duration::from_secs(2)).unwrap();
        sockets.cancel();
        assert!(done_rx.recv_timeout(Duration::from_secs(2)).unwrap());
        worker.join().unwrap();
        assert!(sockets.0.lock().unwrap().sockets.is_empty());
    }

    #[test]
    fn rejected_request_drain_is_bounded_when_the_body_never_arrives() {
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
        let mut client = TcpStream::connect(listener.local_addr().unwrap()).unwrap();
        let (_server, _) = listener.accept().unwrap();
        let started = Instant::now();
        drain_rejected_body(&mut client, 100, &AtomicBool::new(false)).unwrap();
        assert!(started.elapsed() < Duration::from_secs(1));
    }

    #[test]
    fn cancellation_poll_preserves_the_original_read_deadline_and_timeout() {
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
        let client = TcpStream::connect(listener.local_addr().unwrap()).unwrap();
        let (_server, _) = listener.accept().unwrap();
        let timeout = Duration::from_millis(250);
        client.set_read_timeout(Some(timeout)).unwrap();
        let mut stream = CancellableIo {
            inner: Box::new(client),
            sockets: Arc::new(UpstreamSockets::default()),
        };
        let started = Instant::now();
        let error = stream.read(&mut [0_u8; 1]).unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::TimedOut);
        assert!(started.elapsed() >= timeout);
        assert_eq!(
            stream.inner.socket().unwrap().read_timeout().unwrap(),
            Some(timeout)
        );
    }

    #[test]
    fn delayed_data_survives_polling_and_restores_blocking_mode() {
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
        let client = TcpStream::connect(listener.local_addr().unwrap()).unwrap();
        client
            .set_read_timeout(Some(Duration::from_secs(2)))
            .unwrap();
        let (mut peer, _) = listener.accept().unwrap();
        let (finished_tx, finished_rx) = mpsc::channel();
        let server = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(100));
            peer.write_all(b"late").unwrap();
            finished_rx.recv_timeout(Duration::from_secs(5)).unwrap();
        });
        let mut stream = CancellableIo {
            inner: Box::new(client),
            sockets: Arc::new(UpstreamSockets::default()),
        };
        let mut bytes = [0; 4];
        stream.read_exact(&mut bytes).unwrap();
        assert_eq!(&bytes, b"late");
        stream
            .inner
            .socket()
            .unwrap()
            .set_read_timeout(Some(Duration::from_millis(100)))
            .unwrap();
        let started = Instant::now();
        assert!(stream.inner.read(&mut [0; 1]).is_err());
        assert!(started.elapsed() >= Duration::from_millis(80));
        finished_tx.send(()).unwrap();
        server.join().unwrap();
    }

    #[test]
    fn writes_under_backpressure_preserve_every_byte() {
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
        let client = TcpStream::connect(listener.local_addr().unwrap()).unwrap();
        client
            .set_write_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        let (mut peer, _) = listener.accept().unwrap();
        peer.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
        let server = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(100));
            let mut bytes = Vec::new();
            peer.read_to_end(&mut bytes).unwrap();
            bytes
        });
        let payload = vec![0x5a; 8 * 1024 * 1024];
        let mut stream = CancellableIo {
            inner: Box::new(client),
            sockets: Arc::new(UpstreamSockets::default()),
        };
        stream.write_all(&payload).unwrap();
        drop(stream);
        assert_eq!(server.join().unwrap(), payload);
    }

    #[test]
    fn tracked_connector_requires_a_valid_tls_handshake() {
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
        let address = listener.local_addr().unwrap();
        let (observed_tx, observed_rx) = mpsc::channel();
        let server = std::thread::spawn(move || {
            let (mut socket, _) = listener.accept().unwrap();
            socket
                .set_read_timeout(Some(Duration::from_secs(2)))
                .unwrap();
            let mut bytes = [0_u8; 4096];
            let count = socket.read(&mut bytes).unwrap();
            observed_tx.send(bytes[..count].to_vec()).unwrap();
            let _ = socket.write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 0\r\n\r\n");
        });
        let sockets = Arc::new(UpstreamSockets::default());
        let agent = ureq::AgentBuilder::new()
            .try_proxy_from_env(false)
            .timeout(Duration::from_secs(3))
            .tls_connector(Arc::new(TrackedTlsConnector {
                tls: Arc::new(tls_config()),
                sockets: Arc::clone(&sockets),
            }))
            .build();
        let result = agent
            .get(&format!("https://{address}/v1/models"))
            .set("x-api-key", "tls-secret-canary")
            .call();
        assert!(result.is_err());
        let observed = observed_rx.recv_timeout(Duration::from_secs(2)).unwrap();
        assert_eq!(observed.first(), Some(&0x16));
        assert!(!observed
            .windows(b"tls-secret-canary".len())
            .any(|part| part == b"tls-secret-canary"));
        server.join().unwrap();
        assert!(sockets.0.lock().unwrap().sockets.is_empty());
    }

    #[test]
    fn tracked_connector_closes_its_stream_and_releases_the_lease() {
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
        let client = TcpStream::connect(listener.local_addr().unwrap()).unwrap();
        let (_server, _) = listener.accept().unwrap();
        let sockets = Arc::new(UpstreamSockets::default());
        let connector = TrackedTlsConnector {
            tls: Arc::new(PassthroughConnector),
            sockets: Arc::clone(&sockets),
        };
        let mut stream =
            ureq::TlsConnector::connect(&connector, "localhost", Box::new(client)).unwrap();
        assert_eq!(sockets.0.lock().unwrap().sockets.len(), 1);
        let (done_tx, done_rx) = mpsc::channel();
        let worker = std::thread::spawn(move || {
            let mut byte = [0_u8; 1];
            let result = stream.read(&mut byte);
            drop(stream);
            done_tx.send(result).unwrap();
        });
        sockets.cancel();
        let result = done_rx.recv_timeout(Duration::from_secs(2)).unwrap();
        assert!(!matches!(result, Ok(count) if count > 0));
        worker.join().unwrap();
        assert!(sockets.0.lock().unwrap().sockets.is_empty());
    }

    #[test]
    fn cancellation_unblocks_an_upstream_read_and_rejects_late_connections() {
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
        let client = TcpStream::connect(listener.local_addr().unwrap()).unwrap();
        let (server, _) = listener.accept().unwrap();
        let sockets = Arc::new(UpstreamSockets::default());
        let lease = sockets.register(&client).unwrap();
        let mut client = CancellableIo {
            inner: Box::new(client),
            sockets: Arc::clone(&sockets),
        };
        let (started_tx, started_rx) = mpsc::channel();
        let (done_tx, done_rx) = mpsc::channel();
        let worker = std::thread::spawn(move || {
            started_tx.send(()).unwrap();
            let mut byte = [0_u8; 1];
            let result = client.read(&mut byte);
            done_tx.send(result).unwrap();
        });
        started_rx.recv_timeout(Duration::from_secs(2)).unwrap();
        sockets.cancel();
        let result = done_rx.recv_timeout(Duration::from_secs(2)).unwrap();
        assert!(!matches!(result, Ok(count) if count > 0));
        worker.join().unwrap();
        drop(lease);
        assert!(sockets.register(&server).is_err());
        assert!(sockets.0.lock().unwrap().sockets.is_empty());
    }
}
