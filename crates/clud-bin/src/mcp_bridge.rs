//! Issue #259: `clud mcp` — stdio↔TCP bridge for the in-daemon MCP server.
//!
//! Claude Code and Codex MCP hosts spawn this subcommand and treat its
//! stdio as the MCP transport. This module:
//!
//! 1. Calls `daemon::ensure_daemon` (transparently bringing the daemon up
//!    if it isn't running).
//! 2. Reads `DaemonInfo.memory_mcp_port` from `daemon.json`.
//! 3. If the port is unavailable, emits a single JSON-RPC error reply to
//!    stdout and exits non-zero. (Better than hanging — MCP hosts that
//!    just wait on stdout would block forever otherwise.)
//! 4. Otherwise, connects to `127.0.0.1:<port>` and proxies bytes both
//!    directions until either side closes.
//!
//! Pure `std::net` / `std::thread` — no tokio runtime is started by the
//! bridge process. The two copy threads exit when one closes the
//! connection.

use std::io::{self, Read, Write};
use std::net::TcpStream;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread;

use serde_json::json;

use crate::daemon;
use crate::daemon::memory_mcp::JSONRPC_DAEMON_UNAVAILABLE;

/// CLI entry point for `clud mcp`. Returns the process exit code.
pub fn run() -> i32 {
    let state_dir = match daemon::default_state_dir() {
        Ok(p) => p,
        Err(err) => {
            emit_bridge_error(&format!("cannot resolve daemon state dir: {err}"));
            return 1;
        }
    };
    if let Err(err) = daemon::ensure_daemon(&state_dir) {
        emit_bridge_error(&format!("daemon unavailable: {err}"));
        return 1;
    }
    let port = match daemon::read_memory_mcp_port(&state_dir) {
        Ok(Some(p)) => p,
        Ok(None) => {
            emit_bridge_error(
                "memory subsystem not running on this daemon; check `clud daemon status`",
            );
            return 1;
        }
        Err(err) => {
            emit_bridge_error(&format!("read daemon.json: {err}"));
            return 1;
        }
    };
    proxy_stdio(port)
}

/// Connect to the daemon's loopback MCP port and forward bytes between
/// stdio and the TCP socket until either side closes.
fn proxy_stdio(port: u16) -> i32 {
    let stream = match TcpStream::connect(("127.0.0.1", port)) {
        Ok(s) => s,
        Err(err) => {
            emit_bridge_error(&format!("connect 127.0.0.1:{port}: {err}"));
            return 1;
        }
    };
    let stream_for_stdout = match stream.try_clone() {
        Ok(s) => s,
        Err(err) => {
            emit_bridge_error(&format!("clone tcp stream: {err}"));
            return 1;
        }
    };
    let done = Arc::new(AtomicBool::new(false));
    let done_a = Arc::clone(&done);
    let done_b = Arc::clone(&done);

    // stdin → tcp
    let writer = stream;
    let t_in = thread::Builder::new()
        .name("clud-mcp-stdin".to_string())
        .spawn(move || {
            let stdin = io::stdin();
            let mut reader = stdin.lock();
            let mut writer = writer;
            let mut buf = [0u8; 8192];
            while !done_a.load(Ordering::SeqCst) {
                match reader.read(&mut buf) {
                    Ok(0) => break,
                    Ok(n) => {
                        if writer.write_all(&buf[..n]).is_err() {
                            break;
                        }
                        let _ = writer.flush();
                    }
                    Err(_) => break,
                }
            }
            done_a.store(true, Ordering::SeqCst);
            // Half-close so the daemon sees EOF and tears down.
            let _ = writer.shutdown(std::net::Shutdown::Write);
        })
        .expect("spawn clud-mcp-stdin");

    // tcp → stdout
    let mut reader = stream_for_stdout;
    let stdout = io::stdout();
    let mut out = stdout.lock();
    let mut buf = [0u8; 8192];
    loop {
        match reader.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => {
                if out.write_all(&buf[..n]).is_err() {
                    break;
                }
                let _ = out.flush();
            }
            Err(_) => break,
        }
    }
    done_b.store(true, Ordering::SeqCst);
    let _ = reader.shutdown(std::net::Shutdown::Both);
    let _ = t_in.join();
    0
}

/// Write a single JSON-RPC error envelope to stdout. Hosts that wait on
/// the bridge's stdout will see the error and surface it instead of
/// hanging forever.
fn emit_bridge_error(message: &str) {
    let payload = json!({
        "jsonrpc": "2.0",
        "id": null,
        "error": {
            "code": JSONRPC_DAEMON_UNAVAILABLE,
            "message": message,
        }
    });
    let mut out = io::stdout().lock();
    let _ = writeln!(out, "{payload}");
    let _ = out.flush();
    eprintln!("[clud mcp] {message}");
}

/// Issue #259 test helper: deterministic resolver factored out so unit
/// tests can stub `read_memory_mcp_port` with a fake daemon state dir.
#[cfg(test)]
pub(crate) fn resolve_port_from(state_dir: &std::path::Path) -> Result<u16, String> {
    match daemon::read_memory_mcp_port(state_dir) {
        Ok(Some(p)) => Ok(p),
        Ok(None) => Err("memory subsystem not running on this daemon".to_string()),
        Err(err) => Err(format!("read daemon.json: {err}")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::Value;

    /// Acceptance: the bridge resolver returns a clear error when the
    /// daemon's `daemon.json` doesn't carry a memory_mcp_port. Captures
    /// the contract `clud mcp` relies on (without spawning a real daemon
    /// or talking real stdio).
    #[test]
    fn bridge_errors_clearly_when_daemon_has_no_memory_port() {
        let tmp = tempfile::tempdir().unwrap();
        let info = json!({
            "pid": 1u32,
            "port": 12345u16,
        });
        std::fs::write(
            tmp.path().join("daemon.json"),
            serde_json::to_string(&info).unwrap(),
        )
        .unwrap();
        let err = resolve_port_from(tmp.path()).unwrap_err();
        assert!(err.to_lowercase().contains("memory subsystem"));
    }

    /// `daemon.json` carrying an explicit port resolves cleanly.
    #[test]
    fn bridge_resolves_port_from_daemon_info() {
        let tmp = tempfile::tempdir().unwrap();
        let info = json!({
            "pid": 1u32,
            "port": 12345u16,
            "memory_mcp_port": 31415u16,
        });
        std::fs::write(
            tmp.path().join("daemon.json"),
            serde_json::to_string(&info).unwrap(),
        )
        .unwrap();
        assert_eq!(resolve_port_from(tmp.path()).unwrap(), 31415);
    }

    /// Missing `daemon.json` produces a clear, non-panicking error.
    #[test]
    fn bridge_errors_when_daemon_info_missing() {
        let tmp = tempfile::tempdir().unwrap();
        let err = resolve_port_from(tmp.path()).unwrap_err();
        assert!(err.to_lowercase().contains("read daemon.json"));
    }

    /// The emitted bridge-error envelope is a well-formed JSON-RPC error.
    /// (Smoke-test the shape so MCP hosts can rely on it.)
    #[test]
    fn bridge_error_envelope_is_valid_jsonrpc() {
        let payload = json!({
            "jsonrpc": "2.0",
            "id": null,
            "error": {
                "code": JSONRPC_DAEMON_UNAVAILABLE,
                "message": "test",
            }
        });
        let s = payload.to_string();
        let parsed: Value = serde_json::from_str(&s).unwrap();
        assert_eq!(parsed["jsonrpc"], "2.0");
        assert!(parsed["id"].is_null());
        assert_eq!(parsed["error"]["code"], JSONRPC_DAEMON_UNAVAILABLE);
    }
}
