use std::io;
use std::path::{Path, PathBuf};

use running_process::broker::protocol::Endpoint;
use sha2::{Digest, Sha256};

use super::RUNNING_PROCESS_SERVICE_NAME;

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
pub(super) fn endpoint_for_state_dir(state_dir: &Path) -> io::Result<Endpoint> {
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
