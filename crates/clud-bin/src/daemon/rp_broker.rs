//! running-process broker v1 frame lane for the clud daemon.
//!
//! Consumer adoption per zackees/running-process#385 and
//! running-process `docs/INTEGRATE.md`: alongside the legacy loopback
//! TCP line wire (`server.rs`), the daemon binds a local-socket
//! endpoint (named pipe on Windows, unix socket elsewhere) served by
//! running-process's [`BackendEndpointMux`]. That endpoint answers
//! `BackendHandle` identity probes and carries clud's own
//! request/response payloads opaquely inside frozen v1 `Frame`
//! envelopes under the registered consumer payload protocol
//! [`CLUD_PAYLOAD_PROTOCOL`] (`0x7C4C`).
//!
//! Frame payloads are the exact prost messages clud already speaks on
//! its TCP wire (`proto/clud_v1.proto` `ClientToDaemon` /
//! `DaemonToClient` via `wire_prost`), so the two lanes share one
//! schema and one dispatch function
//! ([`super::server::dispatch_daemon_request`]).
//!
//! Escape hatch: `RUNNING_PROCESS_DISABLE=1` skips this lane entirely —
//! no endpoint, no identity sidecar — restoring pre-adoption behavior
//! exactly. The lane is additionally best-effort: any bind/identity
//! failure logs one note and leaves the TCP wire authoritative.

use std::collections::HashMap;
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Arc, Mutex};
use std::thread;

use interprocess::local_socket::traits::Listener as _;
use interprocess::local_socket::ListenerOptions;
use running_process::broker::adopt::{AdoptError, BrokerSession};
use running_process::broker::backend_handle::DaemonProcess;
use running_process::broker::backend_sdk::{
    read_daemon_identity_file, remove_daemon_identity_file, write_daemon_identity_file,
    BackendEndpointMux, LegacyClassification, MuxPoll,
};
use running_process::broker::builders::{CacheManifestBuilder, ServiceDefinitionBuilder};
use running_process::broker::client::{ConnectBackendRequest, RefusalKind};
use running_process::broker::doctor::default_broker_endpoint;
use running_process::broker::protocol::{encode_framed, CacheRootKind, Endpoint, Frame};
use running_process::broker::server::local_socket_name;
use running_process::NativeProcess;
use sha2::{Digest, Sha256};

use super::gc_service::RegistryMsg;
use super::server::dispatch_daemon_request;
use super::types::{DaemonRequest, DaemonResponse};
use super::wire_prost::{
    decode_daemon_request, encode_daemon_response_prost, WireFrame, CLUD_PROST_PAYLOAD_PROTOCOL,
};

running_process::register_payload_protocol! {
    /// clud daemon's opaque Frame v1 request/response lane.
    ///
    /// Registered upstream in
    /// `running-process/src/broker/protocol/registry.rs` (consumer
    /// range `0x7000..=0x7EFF`; pairwise-distinct from zccache's
    /// `0x7A63`). FROZEN — never change this value.
    pub(crate) const CLUD_PAYLOAD_PROTOCOL: u32 = 0x7C4C;
}

/// Canonical running-process escape hatch. `=1` (exact) restores
/// pre-adoption behavior: no probe endpoint, no identity sidecar, no
/// frame lane.
pub(super) const RUNNING_PROCESS_DISABLE_ENV: &str = "RUNNING_PROCESS_DISABLE";

/// Logical service name clud registers and probes under.
pub(super) const RUNNING_PROCESS_SERVICE_NAME: &str = "clud";

/// Minimum clud daemon version the broker may negotiate for this service
/// (consumer-adoption guide step 8; zackees/running-process#436). Stamped
/// onto the `ServiceDefinition` so the broker refuses pre-2.0.0 backends
/// with `RefusalKind::VersionUnsupported`.
pub(super) const RUNNING_PROCESS_MIN_VERSION: &str = "2.0.0";

/// Wire mode selector at the clud daemon boundary
/// (zackees/running-process#436, consumer-adoption-clud.md step 1).
///
/// Both arms decode into the SAME internal [`DaemonRequest`] /
/// [`DaemonResponse`] model via `wire_prost`:
/// - [`WireMode::JsonLegacy`]: clud's legacy JSON line wire over the
///   direct daemon endpoint (TCP). Selected by `RUNNING_PROCESS_DISABLE=1`.
/// - [`WireMode::ProstV1`]: running-process v1 `Frame` lane (payload
///   protocol `0x7C4C`) reached through [`BrokerSession::adopt`] /
///   Hello-skip, carrying clud prost payloads. The release default.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum WireMode {
    /// Legacy JSON over the direct daemon endpoint (escape hatch).
    JsonLegacy,
    /// running-process v1 prost frame lane via the broker session.
    ProstV1,
}

impl WireMode {
    /// Select the active wire mode from the environment. The canonical
    /// escape hatch `RUNNING_PROCESS_DISABLE=1` forces [`Self::JsonLegacy`]
    /// + the direct endpoint; everything else uses [`Self::ProstV1`].
    pub(super) fn select() -> Self {
        if running_process_disabled() {
            Self::JsonLegacy
        } else {
            Self::ProstV1
        }
    }

    /// Stable identifier reported by the daemon diagnostics CLI.
    pub(super) fn as_str(self) -> &'static str {
        match self {
            Self::JsonLegacy => "json-legacy",
            Self::ProstV1 => "prost-v1",
        }
    }
}

pub(super) fn running_process_disabled() -> bool {
    std::env::var(RUNNING_PROCESS_DISABLE_ENV)
        .map(|value| value == "1")
        .unwrap_or(false)
}

/// `<state_dir>/daemon-identity.json` — the running-process JSON
/// identity sidecar, written next to `daemon.json` at startup and
/// removed on clean shutdown. Clients read it and verify the daemon
/// with `BackendHandle::probe_with_service` before trusting it.
pub(super) fn daemon_identity_path(state_dir: &Path) -> PathBuf {
    state_dir.join("daemon-identity.json")
}

/// Stable per-state-dir token for endpoint names: first 16 hex chars of
/// SHA-256 of the state dir path as passed (not canonicalized — both
/// sides of the IPC always use the same `--state-dir` string).
fn state_dir_token(state_dir: &Path) -> String {
    let digest = Sha256::digest(state_dir.to_string_lossy().as_bytes());
    let mut token = String::with_capacity(16);
    for byte in &digest[..8] {
        token.push_str(&format!("{byte:02x}"));
    }
    token
}

/// Resolve the frame-lane endpoint for a daemon state dir.
///
/// Windows: a BARE pipe name (`Endpoint::windows_pipe` rejects the
/// `\\.\pipe\` prefix; running-process prepends it when resolving).
/// Unix: a socket path inside the state dir, falling back to a short
/// temp-dir path when the state dir would overflow `sun_path` (~104
/// bytes on macOS).
fn endpoint_for_state_dir(state_dir: &Path) -> io::Result<Endpoint> {
    let token = state_dir_token(state_dir);
    #[cfg(windows)]
    {
        Endpoint::windows_pipe(RUNNING_PROCESS_SERVICE_NAME, format!("clud-rp-{token}"))
            .map_err(|err| io::Error::new(io::ErrorKind::InvalidInput, err.to_string()))
    }
    #[cfg(not(windows))]
    {
        const SUN_PATH_BUDGET: usize = 90;
        let in_state_dir = state_dir.join("rp.sock");
        let path = if in_state_dir.as_os_str().len() <= SUN_PATH_BUDGET {
            in_state_dir
        } else {
            std::env::temp_dir().join(format!("clud-rp-{token}.sock"))
        };
        Endpoint::unix_socket(RUNNING_PROCESS_SERVICE_NAME, path.to_string_lossy())
            .map_err(|err| io::Error::new(io::ErrorKind::InvalidInput, err.to_string()))
    }
}

/// Handle to the running frame lane; cleans up the on-disk artifacts on
/// daemon shutdown. The accept thread itself is detached and blocked in
/// `accept()` — it dies with the process, which is the daemon's normal
/// exit path right after `run_daemon` returns.
pub(super) struct FrameLane {
    identity_path: PathBuf,
    endpoint_path: String,
}

impl FrameLane {
    pub(super) fn cleanup(&self) {
        remove_daemon_identity_file(&self.identity_path);
        #[cfg(not(windows))]
        {
            let _ = std::fs::remove_file(&self.endpoint_path);
        }
        #[cfg(windows)]
        {
            // Named pipes vanish with their last handle; nothing on disk.
            let _ = &self.endpoint_path;
        }
    }
}

/// Start the broker v1 frame lane, best-effort.
///
/// Returns `None` (after at most one stderr note) when the lane is
/// disabled via `RUNNING_PROCESS_DISABLE=1` or fails to come up; the
/// daemon's TCP wire keeps working either way.
pub(super) fn spawn_frame_lane(
    state_dir: &Path,
    workers: Arc<Mutex<HashMap<String, Arc<NativeProcess>>>>,
    gc_tx: Option<mpsc::Sender<RegistryMsg>>,
    shutdown_requested: Arc<AtomicBool>,
) -> Option<FrameLane> {
    if running_process_disabled() {
        return None;
    }
    // Packaged `.servicedef` (soldr#722 pattern, #385 item 4): refresh
    // clud's service definition on every daemon bringup so the broker's
    // registry always points at the binary that is actually serving.
    // Best-effort and independent of the frame lane. Skipped under
    // `cfg!(test)` — unit tests spawn `run_daemon` in-process and must
    // not register the throwaway test executable in the user's real
    // service-definition directory; `install_service_definition` itself
    // is covered by a dedicated test against a temp dir.
    if !cfg!(test) {
        if let Err(err) = install_service_definition() {
            eprintln!("[clud] note: running-process servicedef install skipped: {err}");
        }
        // Publish clud's CacheManifest (runtime/lock/config/log roots) so
        // peers can discover this daemon through the central registry
        // (#436, consumer-adoption-clud.md step 9). Best-effort and
        // independent of the frame lane; skipped under `cfg!(test)` for
        // the same reason as the servicedef install.
        if let Err(err) = publish_cache_manifest(state_dir) {
            eprintln!("[clud] note: running-process cache manifest publish skipped: {err}");
        }
    }
    match start_frame_lane(state_dir, workers, gc_tx, shutdown_requested) {
        Ok(lane) => Some(lane),
        Err(err) => {
            eprintln!("[clud] note: running-process frame lane unavailable: {err}");
            None
        }
    }
}

/// Install/refresh `clud.servicedef` in the running-process
/// service-definition directory (`RUNNING_PROCESS_SERVICE_DEF_DIR`
/// override honored by running-process's `service_definition_dir`). The definition uses
/// SHARED_BROKER isolation, declares `min_version` 2.0.0, and points at
/// the current executable.
///
/// Built through [`ServiceDefinitionBuilder`] (#436 / #433 frozen
/// builders) so the broker boilerplate is defaulted and validated on
/// `install` instead of hand-spelled.
pub(super) fn install_service_definition() -> io::Result<PathBuf> {
    let exe = std::env::current_exe()?;
    ServiceDefinitionBuilder::shared_broker(
        RUNNING_PROCESS_SERVICE_NAME,
        exe.to_string_lossy().into_owned(),
    )
    .min_version(RUNNING_PROCESS_MIN_VERSION)
    .allow_version(env!("CARGO_PKG_VERSION"))
    .label("consumer", "clud")
    .install()
    .map_err(|err| io::Error::other(err.to_string()))
}

/// Seal and publish clud's [`CacheManifest`] into the central registry
/// (#436 / #433 builders, consumer-adoption-clud.md step 9). Records the
/// daemon's runtime, lock, config, and log roots so peers can discover
/// the cache. `RUNNING_PROCESS_MANIFEST_DIR` (honored by the running-
/// process registry helpers) redirects the registry root for tests.
///
/// The roots map clud's on-disk layout (`paths.rs`) onto the broker's
/// [`CacheRootKind`] taxonomy:
/// - runtime  → `state_dir` (`CacheRuntime`)
/// - lock     → `state_dir/daemon.lock` (`CacheLocks`)
/// - config   → `~/.clud` (`CacheConfig`)
/// - log      → `state_dir/logs` (`CacheLogs`)
pub(super) fn publish_cache_manifest(state_dir: &Path) -> io::Result<PathBuf> {
    let runtime_root = state_dir.to_string_lossy().into_owned();
    let lock_root = state_dir.join("daemon.lock").to_string_lossy().into_owned();
    let log_root = state_dir.join("logs").to_string_lossy().into_owned();
    let config_root = dirs::home_dir()
        .map(|home| home.join(".clud"))
        .unwrap_or_else(|| state_dir.to_path_buf())
        .to_string_lossy()
        .into_owned();

    CacheManifestBuilder::new(RUNNING_PROCESS_SERVICE_NAME, env!("CARGO_PKG_VERSION"))
        .broker_instance("shared")
        .root(CacheRootKind::CacheRuntime, runtime_root)
        .root(CacheRootKind::CacheLocks, lock_root)
        .root(CacheRootKind::CacheConfig, config_root)
        .root(CacheRootKind::CacheLogs, log_root)
        .publish()
        .map_err(|err| io::Error::other(err.to_string()))
}

fn start_frame_lane(
    state_dir: &Path,
    workers: Arc<Mutex<HashMap<String, Arc<NativeProcess>>>>,
    gc_tx: Option<mpsc::Sender<RegistryMsg>>,
    shutdown_requested: Arc<AtomicBool>,
) -> io::Result<FrameLane> {
    let endpoint = endpoint_for_state_dir(state_dir)?;
    let endpoint_path = endpoint.path.clone();

    #[cfg(not(windows))]
    {
        // A previous daemon that died uncleanly leaves the socket file
        // behind; binding fails with AddrInUse unless it is removed.
        let _ = std::fs::remove_file(&endpoint_path);
    }

    let name = local_socket_name(&endpoint_path)?;
    let listener = ListenerOptions::new().name(name).create_sync()?;

    let daemon = DaemonProcess::current_process(endpoint, None)
        .map_err(|err| io::Error::other(err.to_string()))?;
    let identity_path = daemon_identity_path(state_dir);
    write_daemon_identity_file(&identity_path, &daemon)?;

    let mux = Arc::new(BackendEndpointMux::new(
        daemon,
        &[CLUD_PAYLOAD_PROTOCOL],
        // This endpoint is new with the adoption — it has no legacy
        // wire. The TCP listener keeps serving the legacy line formats.
        |_buf: &[u8]| LegacyClassification::NotLegacy,
    ));

    let state_dir = state_dir.to_path_buf();
    thread::Builder::new()
        .name("clud-rp-frame-lane".to_string())
        .spawn(move || loop {
            match listener.accept() {
                Ok(stream) => {
                    if shutdown_requested.load(Ordering::SeqCst) {
                        return;
                    }
                    let mux = Arc::clone(&mux);
                    let state_dir = state_dir.clone();
                    let workers = Arc::clone(&workers);
                    let gc_tx = gc_tx.clone();
                    let shutdown_requested = Arc::clone(&shutdown_requested);
                    thread::spawn(move || {
                        let mut stream = stream;
                        let _ = serve_connection(
                            &mut stream,
                            &mux,
                            &state_dir,
                            &workers,
                            gc_tx.as_ref(),
                            &shutdown_requested,
                        );
                    });
                }
                Err(_) => {
                    if shutdown_requested.load(Ordering::SeqCst) {
                        return;
                    }
                    thread::sleep(std::time::Duration::from_millis(50));
                }
            }
        })?;

    Ok(FrameLane {
        identity_path,
        endpoint_path,
    })
}

/// Serve one accepted connection through the mux until the peer
/// disconnects: identity probes are answered by the SDK, payload frames
/// are decoded as `ClientToDaemon` prost bytes and dispatched through
/// the same [`dispatch_daemon_request`] the TCP wire uses.
///
/// Canonical accept-loop shape from running-process
/// `tests/broker/backend_sdk.rs::serve_connection` / `docs/INTEGRATE.md`.
fn serve_connection<S, F>(
    stream: &mut S,
    mux: &BackendEndpointMux<F>,
    state_dir: &Path,
    workers: &Arc<Mutex<HashMap<String, Arc<NativeProcess>>>>,
    gc_tx: Option<&mpsc::Sender<RegistryMsg>>,
    shutdown_requested: &Arc<AtomicBool>,
) -> io::Result<()>
where
    S: Read + Write,
    F: Fn(&[u8]) -> LegacyClassification,
{
    let mut buf: Vec<u8> = Vec::new();
    let mut chunk = [0u8; 4096];
    loop {
        match mux.poll(&buf).map_err(io::Error::other)? {
            MuxPoll::NeedMoreBytes => {
                let read = stream.read(&mut chunk)?;
                if read == 0 {
                    if buf.is_empty() {
                        return Ok(());
                    }
                    return Err(io::Error::new(
                        io::ErrorKind::UnexpectedEof,
                        "peer closed mid-frame",
                    ));
                }
                buf.extend_from_slice(&chunk[..read]);
            }
            MuxPoll::ProbeAnswered { reply, consumed } => {
                stream.write_all(&reply)?;
                stream.flush()?;
                buf.drain(..consumed);
            }
            MuxPoll::Payload { frame, consumed } => {
                buf.drain(..consumed);
                let response = answer_payload_frame(&frame, state_dir, workers, gc_tx);
                let is_shutdown = response.is_shutdown;
                let wire = encode_framed(&Frame::response_to(&frame, response.payload))
                    .map_err(io::Error::other)?;
                stream.write_all(&wire)?;
                stream.flush()?;
                if is_shutdown {
                    // Match the TCP lane: flag shutdown only after the
                    // ack bytes are on the wire so the requester always
                    // hears back.
                    shutdown_requested.store(true, Ordering::SeqCst);
                    return Ok(());
                }
            }
            MuxPoll::Legacy => {
                // This endpoint has no legacy wire; the detector always
                // says NotLegacy, so this verdict is unreachable.
                return Err(io::Error::other(
                    "unexpected legacy classification on frame-only endpoint",
                ));
            }
        }
    }
}

struct PayloadAnswer {
    payload: Vec<u8>,
    is_shutdown: bool,
}

/// Decode one clud payload frame, dispatch it, and encode the response
/// payload (prost `DaemonToClient` bytes). Decode/encode failures
/// degrade to an in-band `DaemonResponse::Error` payload so the client
/// gets a correlated reply instead of a dropped connection.
fn answer_payload_frame(
    frame: &Frame,
    state_dir: &Path,
    workers: &Arc<Mutex<HashMap<String, Arc<NativeProcess>>>>,
    gc_tx: Option<&mpsc::Sender<RegistryMsg>>,
) -> PayloadAnswer {
    let envelope_request_id = format!("rp-{}", frame.request_id);
    let request = decode_daemon_request(&WireFrame {
        payload_protocol: CLUD_PROST_PAYLOAD_PROTOCOL,
        payload: frame.payload.clone(),
    });
    let response = match request {
        Ok(request) => dispatch_daemon_request(state_dir, workers, gc_tx, request),
        Err(err) => DaemonResponse::Error {
            message: format!("malformed clud frame payload: {err}"),
        },
    };
    let is_shutdown = matches!(response, DaemonResponse::ShutdownAck { .. });
    let payload = encode_daemon_response_prost(&response, envelope_request_id.clone())
        .map(|wire_frame| wire_frame.payload)
        .unwrap_or_else(|err| {
            let fallback = DaemonResponse::Error {
                message: format!("failed to encode daemon response: {err}"),
            };
            encode_daemon_response_prost(&fallback, envelope_request_id)
                .map(|wire_frame| wire_frame.payload)
                .unwrap_or_default()
        });
    PayloadAnswer {
        payload,
        is_shutdown,
    }
}

/// Encode a [`DaemonRequest`] as clud frame-lane payload bytes (prost
/// `ClientToDaemon`). Client-side helper for `FrameClient::request`.
pub(super) fn encode_frame_lane_request(
    request: &DaemonRequest,
    envelope_request_id: impl Into<String>,
) -> io::Result<Vec<u8>> {
    super::wire_prost::encode_daemon_request_prost(request, envelope_request_id)
        .map(|wire_frame| wire_frame.payload)
        .map_err(|err| io::Error::new(io::ErrorKind::InvalidData, err))
}

/// Decode clud frame-lane response payload bytes (prost
/// `DaemonToClient`). Client-side helper for `FrameClient::request`.
pub(super) fn decode_frame_lane_response(payload: &[u8]) -> io::Result<DaemonResponse> {
    super::wire_prost::decode_daemon_response(&WireFrame {
        payload_protocol: CLUD_PROST_PAYLOAD_PROTOCOL,
        payload: payload.to_vec(),
    })
    .map_err(|err| io::Error::new(io::ErrorKind::InvalidData, err))
}

/// One request/response round trip over the running-process frame lane
/// (`WireMode::ProstV1`).
///
/// Client-side fast path for `send_daemon_request`: selects the wire mode
/// ([`WireMode::select`]), reads the daemon identity sidecar, and adopts
/// the broker session through [`BrokerSession::adopt`] (#436 /
/// consumer-adoption-clud.md step 6). Adoption performs the Hello
/// handshake (`service_name = "clud"`, protocol min/max = 1,
/// `client_lib_name = "running-process"`, `wanted_version` = clud daemon
/// version) when a real broker is reached, and Hello-skips straight to
/// the sidecar's backend endpoint otherwise (`cached_backend_endpoint`
/// is set and `wanted_version == self_version` by construction). It then
/// exchanges one [`CLUD_PAYLOAD_PROTOCOL`] frame.
///
/// Returns `None` on ANY miss — `WireMode::JsonLegacy` (disable hatch),
/// missing sidecar, broker refusal, connect/encode/decode failure — so
/// the caller falls back to the legacy TCP wire, which stays the
/// authoritative path. Broker refusals are classified through
/// [`RefusalKind`] before degrading so the cause is logged, not
/// swallowed silently.
pub(super) fn try_send_via_frame_lane(
    state_dir: &Path,
    request: &DaemonRequest,
) -> Option<DaemonResponse> {
    if WireMode::select() == WireMode::JsonLegacy {
        return None;
    }
    let identity = read_daemon_identity_file(&daemon_identity_path(state_dir))?;
    let version = env!("CARGO_PKG_VERSION");
    // A broker endpoint is required by the request shape but only ever
    // dialed when the Hello-skip connect misses; an underivable default
    // is fine to substitute with a never-listening name.
    let broker_endpoint =
        default_broker_endpoint().unwrap_or_else(|_| String::from("clud-rp-no-broker"));
    let mut connect = ConnectBackendRequest::new(
        &broker_endpoint,
        RUNNING_PROCESS_SERVICE_NAME,
        version,
        version,
    );
    connect.client_lib_name = "running-process";
    connect.cached_backend_endpoint = Some(&identity.ipc_endpoint.path);

    let mut session = match BrokerSession::adopt(connect) {
        Ok(session) => session,
        Err(err) => {
            log_adopt_miss(&err);
            return None;
        }
    };
    let payload = encode_frame_lane_request(request, format!("cli-{}", std::process::id())).ok()?;
    let reply = session.request(CLUD_PAYLOAD_PROTOCOL, payload).ok()?;
    decode_frame_lane_response(&reply.payload).ok()
}

/// Classify a [`BrokerSession::adopt`] failure for one diagnostic line
/// before falling back to the TCP wire (#436 step 6, typed `Refused`).
///
/// `BrokerDisabled` is the escape hatch, not a failure, so it stays
/// silent. A broker refusal is reported through [`RefusalKind`] rather
/// than string-matched; everything else (unreachable broker, dial/IO
/// error) is a plain miss and also stays quiet — the TCP fallback covers
/// it on every launch.
fn log_adopt_miss(err: &AdoptError) {
    if let AdoptError::Connect(connect) = err {
        match connect.refusal_kind() {
            Some(RefusalKind::VersionUnsupported) => eprintln!(
                "[clud] note: broker refused clud (version unsupported); upgrade running-process. Falling back to TCP."
            ),
            Some(RefusalKind::VersionBlocked) => eprintln!(
                "[clud] note: broker refused clud (daemon version blocked). Falling back to TCP."
            ),
            Some(RefusalKind::ServiceUnknown) => eprintln!(
                "[clud] note: broker has no clud.servicedef (service unknown). Falling back to TCP."
            ),
            Some(RefusalKind::RateLimited) => eprintln!(
                "[clud] note: broker rate-limited clud. Falling back to TCP."
            ),
            Some(RefusalKind::ShuttingDown) => eprintln!(
                "[clud] note: broker is shutting down. Falling back to TCP."
            ),
            Some(RefusalKind::Other(code)) => {
                eprintln!("[clud] note: broker refused clud (code {code:?}). Falling back to TCP.")
            }
            // None => not a refusal (broker unreachable / dial error);
            // the TCP fallback handles it on every launch, so stay quiet.
            None => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::super::io_helpers::read_json_file;
    use super::super::paths::daemon_info_path;
    use super::super::server::run_daemon;
    use super::super::types::DaemonInfo;
    use super::*;
    use running_process::broker::backend_handle::BackendHandle;
    use running_process::broker::backend_sdk::{read_daemon_identity_file, FrameClient};
    use std::net::TcpStream;
    use std::time::{Duration, Instant};

    /// Serializes the env mutations in this module AND pins the
    /// RUNNING_PROCESS_DISABLE state each test depends on.
    struct EnvGuard {
        priors: Vec<(&'static str, Option<String>)>,
        _lock: std::sync::MutexGuard<'static, ()>,
    }

    impl EnvGuard {
        fn lock() -> std::sync::MutexGuard<'static, ()> {
            static LOCK: std::sync::OnceLock<std::sync::Mutex<()>> = std::sync::OnceLock::new();
            LOCK.get_or_init(|| std::sync::Mutex::new(()))
                .lock()
                .unwrap_or_else(|poison| poison.into_inner())
        }

        fn set(key: &'static str, value: &str) -> Self {
            Self::apply(vec![(key, Some(value.to_string()))])
        }

        fn unset(key: &'static str) -> Self {
            Self::apply(vec![(key, None)])
        }

        /// Set/unset several variables under ONE lock acquisition (the
        /// mutex is not reentrant — nesting two guards would deadlock).
        fn apply(vars: Vec<(&'static str, Option<String>)>) -> Self {
            let lock = Self::lock();
            let priors = vars
                .into_iter()
                .map(|(key, value)| {
                    let prior = std::env::var(key).ok();
                    match value {
                        Some(value) => std::env::set_var(key, value),
                        None => std::env::remove_var(key),
                    }
                    (key, prior)
                })
                .collect();
            Self {
                priors,
                _lock: lock,
            }
        }
    }

    impl Drop for EnvGuard {
        fn drop(&mut self) {
            for (key, prior) in self.priors.drain(..) {
                match prior {
                    Some(value) => std::env::set_var(key, value),
                    None => std::env::remove_var(key),
                }
            }
        }
    }

    fn wait_for_daemon_ready(state_dir: &Path) -> DaemonInfo {
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            if let Ok(info) = read_json_file::<DaemonInfo>(&daemon_info_path(state_dir)) {
                if TcpStream::connect(("127.0.0.1", info.port)).is_ok() {
                    return info;
                }
            }
            assert!(
                Instant::now() < deadline,
                "timed out waiting for daemon startup"
            );
            thread::sleep(Duration::from_millis(25));
        }
    }

    fn wait_for_identity_sidecar(state_dir: &Path) -> DaemonProcess {
        let path = daemon_identity_path(state_dir);
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            if let Some(identity) = read_daemon_identity_file(&path) {
                return identity;
            }
            assert!(
                Instant::now() < deadline,
                "timed out waiting for daemon identity sidecar"
            );
            thread::sleep(Duration::from_millis(25));
        }
    }

    /// FROZEN golden bytes for clud's Frame v1 wire (#385 adoption).
    ///
    /// `encode_framed(Frame::request(0x7C4C, <prost ClientToDaemon
    /// shutdown, request_id "golden">).with_request_id(1))`. The outer
    /// layout is `[u8 framing_version=1][u32 LE body_len][prost Frame]`.
    /// If this test ever fails, the wire drifted — fix the code, never
    /// the constant.
    #[test]
    fn golden_bytes_pin_clud_frame_v1_request_wire() {
        let payload = encode_frame_lane_request(&DaemonRequest::Shutdown, "golden")
            .expect("encode shutdown payload");
        let frame = Frame::request(CLUD_PAYLOAD_PROTOCOL, payload).with_request_id(1);
        let wire = encode_framed(&frame).expect("encode framed");

        let expected_hex = "0115000000080118ccf801220b4200a20606676f6c64656e2801";
        let got_hex: String = wire.iter().map(|byte| format!("{byte:02x}")).collect();
        assert_eq!(
            got_hex, expected_hex,
            "clud Frame v1 golden bytes drifted; the wire is frozen-forever"
        );
    }

    #[test]
    fn payload_protocol_is_frozen_registered_consumer_id() {
        assert_eq!(CLUD_PAYLOAD_PROTOCOL, 0x7C4C);
        assert_ne!(
            CLUD_PAYLOAD_PROTOCOL, 0x7A63,
            "must not collide with zccache"
        );
    }

    /// End-to-end: a real daemon answers a `BackendHandle` identity
    /// probe via the sidecar, serves clud payload frames through
    /// `FrameClient`, and shuts down via the frame lane.
    #[test]
    fn frame_lane_serves_probe_and_clud_requests_end_to_end() {
        let _env = EnvGuard::unset(RUNNING_PROCESS_DISABLE_ENV);
        let tmp = tempfile::tempdir().unwrap();
        let state_dir = tmp.path().to_path_buf();
        let daemon_state_dir = state_dir.clone();
        let daemon_thread = thread::spawn(move || run_daemon(&daemon_state_dir));

        wait_for_daemon_ready(&state_dir);
        let identity = wait_for_identity_sidecar(&state_dir);

        let handle = BackendHandle::probe_with_service(
            RUNNING_PROCESS_SERVICE_NAME,
            env!("CARGO_PKG_VERSION"),
            &identity.ipc_endpoint,
            &identity,
        )
        .expect("daemon must answer the identity probe");
        assert!(handle.is_alive());

        let mut client =
            FrameClient::connect(&identity.ipc_endpoint).expect("connect frame client");
        let payload = encode_frame_lane_request(&DaemonRequest::ListLiveCwds, "rt-1")
            .expect("encode request");
        let response_frame = client
            .request(CLUD_PAYLOAD_PROTOCOL, payload)
            .expect("frame round trip");
        let response =
            decode_frame_lane_response(&response_frame.payload).expect("decode response");
        assert!(matches!(response, DaemonResponse::LiveCwds { .. }));

        let payload =
            encode_frame_lane_request(&DaemonRequest::Shutdown, "rt-2").expect("encode shutdown");
        let response_frame = client
            .request(CLUD_PAYLOAD_PROTOCOL, payload)
            .expect("shutdown round trip");
        let response =
            decode_frame_lane_response(&response_frame.payload).expect("decode shutdown");
        assert!(matches!(
            response,
            DaemonResponse::ShutdownAck { pid } if pid == std::process::id()
        ));
        drop(client);

        assert_eq!(daemon_thread.join().unwrap(), 0);
        assert!(
            read_daemon_identity_file(&daemon_identity_path(&state_dir)).is_none(),
            "daemon should remove the identity sidecar during shutdown"
        );
    }

    /// RUNNING_PROCESS_DISABLE=1 must restore pre-adoption behavior:
    /// no identity sidecar, no frame endpoint, TCP wire untouched.
    #[test]
    fn disable_env_skips_frame_lane_entirely() {
        let _env = EnvGuard::set(RUNNING_PROCESS_DISABLE_ENV, "1");
        let tmp = tempfile::tempdir().unwrap();
        let state_dir = tmp.path().to_path_buf();
        let daemon_state_dir = state_dir.clone();
        let daemon_thread = thread::spawn(move || run_daemon(&daemon_state_dir));

        let info = wait_for_daemon_ready(&state_dir);
        assert!(
            !daemon_identity_path(&state_dir).exists(),
            "disabled lane must not write an identity sidecar"
        );

        // TCP wire still works and shuts the daemon down.
        let mut stream = TcpStream::connect(("127.0.0.1", info.port)).unwrap();
        use std::io::{BufRead, BufReader, Write as _};
        stream.write_all(b"{\"op\":\"shutdown\"}\n").unwrap();
        stream.flush().unwrap();
        let mut reader = BufReader::new(stream);
        let mut line = String::new();
        reader.read_line(&mut line).unwrap();
        assert!(line.contains("shutdown_ack"));
        assert_eq!(daemon_thread.join().unwrap(), 0);
    }

    /// Client wiring (#385 item 1): `send_daemon_request` uses the
    /// frame lane when the sidecar is present; `try_send_via_frame_lane`
    /// honors the disable hatch and degrades to `None` (TCP fallback)
    /// when the sidecar is missing.
    #[test]
    fn client_round_trips_via_frame_lane_and_falls_back_to_tcp() {
        let tmp = tempfile::tempdir().unwrap();
        let state_dir = tmp.path().to_path_buf();
        let daemon_state_dir = state_dir.clone();
        let daemon_thread;
        {
            let _env = EnvGuard::unset(RUNNING_PROCESS_DISABLE_ENV);
            daemon_thread = thread::spawn(move || run_daemon(&daemon_state_dir));
            wait_for_daemon_ready(&state_dir);
            wait_for_identity_sidecar(&state_dir);

            // Direct frame-lane round trip.
            let response = try_send_via_frame_lane(&state_dir, &DaemonRequest::ListLiveCwds)
                .expect("frame lane must answer while the sidecar is live");
            assert!(matches!(response, DaemonResponse::LiveCwds { .. }));

            // The public client entry point goes through the same lane.
            let response =
                super::super::client::send_daemon_request(&state_dir, &DaemonRequest::ListLiveCwds)
                    .expect("send_daemon_request");
            assert!(matches!(response, DaemonResponse::LiveCwds { .. }));
        }

        {
            // Disable hatch bypasses the lane entirely...
            let _env = EnvGuard::set(RUNNING_PROCESS_DISABLE_ENV, "1");
            assert!(
                try_send_via_frame_lane(&state_dir, &DaemonRequest::ListLiveCwds).is_none(),
                "disable hatch must bypass the frame lane"
            );
            // ...while the legacy TCP wire keeps working underneath.
            let response =
                super::super::client::send_daemon_request(&state_dir, &DaemonRequest::ListLiveCwds)
                    .expect("TCP fallback under the disable hatch");
            assert!(matches!(response, DaemonResponse::LiveCwds { .. }));
        }

        let _env = EnvGuard::unset(RUNNING_PROCESS_DISABLE_ENV);
        // A missing sidecar degrades to None (caller falls back to TCP).
        std::fs::remove_file(daemon_identity_path(&state_dir)).unwrap();
        assert!(
            try_send_via_frame_lane(&state_dir, &DaemonRequest::ListLiveCwds).is_none(),
            "missing sidecar must miss the frame lane"
        );
        let response =
            super::super::client::send_daemon_request(&state_dir, &DaemonRequest::Shutdown)
                .expect("shutdown via TCP fallback");
        assert!(matches!(response, DaemonResponse::ShutdownAck { .. }));
        assert_eq!(daemon_thread.join().unwrap(), 0);
    }

    /// The `RUNNING_PROCESS_FAKE_BACKEND` seam redirects the client to
    /// the seam endpoint even when the cached sidecar endpoint is bogus.
    #[test]
    fn fake_backend_seam_overrides_cached_endpoint() {
        let tmp = tempfile::tempdir().unwrap();
        let state_dir = tmp.path().to_path_buf();
        let daemon_state_dir = state_dir.clone();
        let daemon_thread;
        let real_endpoint_path;
        {
            let _env = EnvGuard::unset(RUNNING_PROCESS_DISABLE_ENV);
            daemon_thread = thread::spawn(move || run_daemon(&daemon_state_dir));
            wait_for_daemon_ready(&state_dir);
            let identity = wait_for_identity_sidecar(&state_dir);
            real_endpoint_path = identity.ipc_endpoint.path.clone();

            // Poison the sidecar's endpoint so a Hello-skip dial would miss.
            let mut poisoned = identity.clone();
            poisoned.ipc_endpoint.path = format!("{real_endpoint_path}-bogus");
            write_daemon_identity_file(&daemon_identity_path(&state_dir), &poisoned)
                .expect("rewrite sidecar");
        }

        {
            let _env = EnvGuard::apply(vec![
                (RUNNING_PROCESS_DISABLE_ENV, None),
                ("RUNNING_PROCESS_FAKE_BACKEND", Some(real_endpoint_path)),
            ]);
            let response = try_send_via_frame_lane(&state_dir, &DaemonRequest::ListLiveCwds)
                .expect("seam must route around the poisoned cached endpoint");
            assert!(matches!(response, DaemonResponse::LiveCwds { .. }));
            let response = try_send_via_frame_lane(&state_dir, &DaemonRequest::Shutdown)
                .expect("shutdown via seam");
            assert!(matches!(response, DaemonResponse::ShutdownAck { .. }));
        }
        assert_eq!(daemon_thread.join().unwrap(), 0);
    }

    /// `.servicedef` packaging (#385 item 4): the install helper writes
    /// a valid SHARED_BROKER definition for service "clud" pointing at
    /// the current executable, into the (env-overridden) directory.
    #[test]
    fn install_service_definition_writes_valid_shared_broker_servicedef() {
        use prost::Message as _;
        use running_process::broker::protocol::{BrokerIsolation, ServiceDefinition};
        use running_process::broker::server::validate_service_definition_for_service;

        let tmp = tempfile::tempdir().unwrap();
        let _env = EnvGuard::set(
            "RUNNING_PROCESS_SERVICE_DEF_DIR",
            tmp.path().to_str().unwrap(),
        );
        let path = install_service_definition().expect("install servicedef");
        assert_eq!(path, tmp.path().join("clud.servicedef"));

        let bytes = std::fs::read(&path).unwrap();
        let definition = ServiceDefinition::decode(bytes.as_slice()).unwrap();
        assert_eq!(definition.service_name, RUNNING_PROCESS_SERVICE_NAME);
        assert_eq!(definition.isolation, BrokerIsolation::SharedBroker as i32);
        assert_eq!(
            std::path::PathBuf::from(&definition.binary_path),
            std::env::current_exe().unwrap()
        );
        // #436 step 8: SHARED_BROKER + min_version "2.0.0" + consumer label.
        assert_eq!(definition.min_version, RUNNING_PROCESS_MIN_VERSION);
        assert_eq!(definition.min_version, "2.0.0");
        assert_eq!(
            definition.labels.get("consumer").map(String::as_str),
            Some("clud")
        );
        validate_service_definition_for_service(&definition, RUNNING_PROCESS_SERVICE_NAME)
            .expect("definition must validate");

        // Idempotent refresh: a second install overwrites in place.
        let again = install_service_definition().expect("reinstall servicedef");
        assert_eq!(again, path);
    }

    /// #436 step 1: the daemon-boundary wire selector maps the canonical
    /// escape hatch. `RUNNING_PROCESS_DISABLE=1` → json-legacy; anything
    /// else → prost-v1.
    #[test]
    fn wire_mode_select_maps_the_disable_hatch() {
        let _env = EnvGuard::set(RUNNING_PROCESS_DISABLE_ENV, "1");
        assert_eq!(WireMode::select(), WireMode::JsonLegacy);
        assert_eq!(WireMode::JsonLegacy.as_str(), "json-legacy");
        drop(_env);

        let _env = EnvGuard::unset(RUNNING_PROCESS_DISABLE_ENV);
        assert_eq!(WireMode::select(), WireMode::ProstV1);
        assert_eq!(WireMode::ProstV1.as_str(), "prost-v1");
        drop(_env);

        // A non-`1` value does NOT engage the hatch — prost-v1 stays the
        // default (matches `running_process_disabled` exact-match rule).
        let _env = EnvGuard::set(RUNNING_PROCESS_DISABLE_ENV, "true");
        assert_eq!(WireMode::select(), WireMode::ProstV1);
    }

    /// #436 step 9: `publish_cache_manifest` seals a manifest recording
    /// clud's runtime, lock, config, and log roots and writes it to the
    /// (env-redirected) central registry; it round-trips through
    /// `read_manifest`.
    #[test]
    fn publish_cache_manifest_records_clud_roots_and_round_trips() {
        use running_process::broker::manifest::read_manifest;
        use running_process::broker::protocol::{CacheManifest, CacheRootKind};

        let tmp = tempfile::tempdir().unwrap();
        let registry = tmp.path().join("manifests");
        let state_dir = tmp.path().join("state");
        let _env = EnvGuard::set("RUNNING_PROCESS_MANIFEST_DIR", registry.to_str().unwrap());

        let path = publish_cache_manifest(&state_dir).expect("publish manifest");
        assert!(path.exists(), "manifest must be written to the registry");

        let manifest: CacheManifest = read_manifest(&path).expect("read manifest back");
        assert_eq!(manifest.service_name, RUNNING_PROCESS_SERVICE_NAME);
        assert_eq!(manifest.service_version, env!("CARGO_PKG_VERSION"));
        assert_eq!(manifest.broker_instance, "shared");

        // Exactly the four declared roots, mapped onto the broker taxonomy.
        let kinds: Vec<i32> = manifest.roots.iter().map(|root| root.kind).collect();
        assert!(kinds.contains(&(CacheRootKind::CacheRuntime as i32)));
        assert!(kinds.contains(&(CacheRootKind::CacheLocks as i32)));
        assert!(kinds.contains(&(CacheRootKind::CacheConfig as i32)));
        assert!(kinds.contains(&(CacheRootKind::CacheLogs as i32)));
        assert_eq!(manifest.roots.len(), 4);

        let runtime = manifest
            .roots
            .iter()
            .find(|root| root.kind == CacheRootKind::CacheRuntime as i32)
            .expect("runtime root present");
        assert_eq!(
            std::path::PathBuf::from(&runtime.path),
            state_dir,
            "runtime root must be the daemon state dir"
        );
        let log = manifest
            .roots
            .iter()
            .find(|root| root.kind == CacheRootKind::CacheLogs as i32)
            .expect("log root present");
        assert_eq!(
            std::path::PathBuf::from(&log.path),
            state_dir.join("logs"),
            "log root must be <state_dir>/logs"
        );

        // self_sha256 is sealed (non-empty) by the builder.
        assert!(
            !manifest.self_sha256.is_empty(),
            "manifest digest must be sealed"
        );
    }

    #[test]
    fn running_process_disabled_requires_exact_value() {
        let _env = EnvGuard::set(RUNNING_PROCESS_DISABLE_ENV, "1");
        assert!(running_process_disabled());
        drop(_env);
        let _env = EnvGuard::set(RUNNING_PROCESS_DISABLE_ENV, "true");
        assert!(!running_process_disabled());
        drop(_env);
        let _env = EnvGuard::unset(RUNNING_PROCESS_DISABLE_ENV);
        assert!(!running_process_disabled());
    }
}
