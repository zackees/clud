use std::collections::HashMap;
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Arc, Mutex};
use std::thread;

use interprocess::local_socket::traits::Listener as _;
use interprocess::local_socket::ListenerOptions;
use running_process::broker::backend_handle::DaemonProcess;
use running_process::broker::backend_sdk::{
    remove_daemon_identity_file, write_daemon_identity_file, BackendEndpointMux,
    LegacyClassification, MuxPoll,
};
use running_process::broker::builders::{CacheManifestBuilder, ServiceDefinitionBuilder};
use running_process::broker::protocol::CacheRootKind;
use running_process::broker::protocol::{encode_framed, Frame};
use running_process::broker::server::local_socket_name;
use running_process::NativeProcess;

use super::super::gc_service::RegistryMsg;
use super::endpoint::{daemon_identity_path, endpoint_for_state_dir};
use super::payload::answer_payload_frame;
use super::{
    running_process_disabled, CLUD_PAYLOAD_PROTOCOL, RUNNING_PROCESS_MIN_VERSION,
    RUNNING_PROCESS_SERVICE_NAME,
};

/// Handle to the running frame lane; cleans up the on-disk artifacts on
/// daemon shutdown. The accept thread itself is detached and blocked in
/// `accept()` — it dies with the process, which is the daemon's normal
/// exit path right after `run_daemon` returns.
pub(in crate::daemon) struct FrameLane {
    identity_path: PathBuf,
    endpoint_path: String,
}

impl FrameLane {
    pub(in crate::daemon) fn cleanup(&self) {
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
pub(in crate::daemon) fn spawn_frame_lane(
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
pub(in crate::daemon) fn install_service_definition() -> io::Result<PathBuf> {
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
pub(in crate::daemon) fn publish_cache_manifest(state_dir: &Path) -> io::Result<PathBuf> {
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

pub(super) fn start_frame_lane(
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
