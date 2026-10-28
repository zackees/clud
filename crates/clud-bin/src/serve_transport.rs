//! Cancellation of standalone gateway HTTPS connections during shutdown.

use std::collections::HashMap;
use std::fmt;
use std::io::{self, Read, Write};
use std::net::{Shutdown, TcpStream};
use std::sync::{Arc, Mutex, OnceLock};

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
        let inner = self.tls.connect(dns_name, io)?;
        Ok(Box::new(TrackedStream {
            inner,
            _lease: lease,
        }))
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
        let mut client = TcpStream::connect(listener.local_addr().unwrap()).unwrap();
        let (server, _) = listener.accept().unwrap();
        let sockets = Arc::new(UpstreamSockets::default());
        let lease = sockets.register(&client).unwrap();
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
