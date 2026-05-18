use std::fs;
use std::io::{self, Write};
use std::net::{Shutdown, TcpStream};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{mpsc, Mutex};
use std::time::{Duration, Instant};

use base64::Engine;

use crate::capture::TerminalCapture;

use super::io_helpers::write_json_file;
use super::paths::{session_log_path, session_snapshot_path};
#[cfg(test)]
use super::types::DEFAULT_BACKLOG_LIMIT_BYTES;
use super::types::{
    AttachClientResult, AttachedClient, BacklogState, SessionSnapshot, WorkerServerMessage,
    LOG_ROTATE_BYTES,
};

pub(super) struct WorkerShared {
    pub(super) state_dir: PathBuf,
    pub(super) session_id: String,
    snapshot: Mutex<SessionSnapshot>,
    pub(super) backlog: Mutex<BacklogState>,
    backlog_limit_bytes: usize,
    /// Server-side terminal emulator for PTY sessions. `Some` when the session
    /// kind is `Pty` and issue #34 attach-replay is active. The parser tracks
    /// grid + cursor + alt-screen state so a mid-session attach can emit a
    /// synthesized repaint instead of a raw byte dump, which would leave a
    /// TUI-attached client staring at a garbled frame.
    capture: Mutex<Option<TerminalCapture>>,
    /// Append-only log file for this session. Every output chunk is written
    /// here in addition to the in-memory backlog, so `clud logs <id>` can
    /// pm2-style tail / follow output that has scrolled off the in-memory
    /// backlog or from sessions that have fully exited.
    log_file: Mutex<Option<fs::File>>,
    client: Mutex<Option<AttachedClient>>,
    next_client_id: AtomicU64,
    pub(super) stop_accepting: AtomicBool,
}

impl WorkerShared {
    #[cfg(test)]
    pub(super) fn new(state_dir: PathBuf, session_id: String, snapshot: SessionSnapshot) -> Self {
        Self::new_with_backlog(state_dir, session_id, snapshot, DEFAULT_BACKLOG_LIMIT_BYTES)
    }

    pub(super) fn new_with_backlog(
        state_dir: PathBuf,
        session_id: String,
        snapshot: SessionSnapshot,
        backlog_limit_bytes: usize,
    ) -> Self {
        Self {
            state_dir,
            session_id,
            snapshot: Mutex::new(snapshot),
            backlog: Mutex::new(BacklogState::default()),
            backlog_limit_bytes: backlog_limit_bytes.max(1),
            capture: Mutex::new(None),
            log_file: Mutex::new(None),
            client: Mutex::new(None),
            next_client_id: AtomicU64::new(1),
            stop_accepting: AtomicBool::new(false),
        }
    }

    /// Open the session's log file for append. Called once during worker
    /// startup. A failure here is non-fatal: we log a warning and continue
    /// without a log file rather than killing the session.
    pub(super) fn init_log_file(&self) {
        let path = session_log_path(&self.state_dir, &self.session_id);
        if let Some(parent) = path.parent() {
            let _ = fs::create_dir_all(parent);
        }
        match fs::OpenOptions::new().create(true).append(true).open(&path) {
            Ok(file) => {
                *self.log_file.lock().expect("log_file mutex poisoned") = Some(file);
            }
            Err(err) => {
                eprintln!(
                    "[clud] warning: cannot open session log {}: {}",
                    path.display(),
                    err
                );
            }
        }
    }

    /// Append `chunk` to the session log, rotating at the soft size cap.
    fn append_log(&self, chunk: &[u8]) {
        let mut guard = self.log_file.lock().expect("log_file mutex poisoned");
        let Some(file) = guard.as_mut() else { return };
        if file.write_all(chunk).is_err() {
            return;
        }
        let _ = file.flush();
        if let Ok(meta) = file.metadata() {
            if meta.len() >= LOG_ROTATE_BYTES {
                drop(guard);
                self.rotate_log();
            }
        }
    }

    fn rotate_log(&self) {
        let primary = session_log_path(&self.state_dir, &self.session_id);
        let backup = primary.with_extension("log.1");
        // Close the current handle first so Windows lets us rename it.
        {
            let mut guard = self.log_file.lock().expect("log_file mutex poisoned");
            *guard = None;
        }
        let _ = fs::remove_file(&backup);
        if fs::rename(&primary, &backup).is_err() {
            // Rename failed (file in use, etc.) — reopen primary anyway.
        }
        if let Ok(file) = fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&primary)
        {
            *self.log_file.lock().expect("log_file mutex poisoned") = Some(file);
        }
    }

    /// Activate terminal capture for a PTY session. No-op for subprocess
    /// sessions, whose output is line-oriented and doesn't benefit from grid
    /// replay (and would pay parser cost for nothing).
    pub(super) fn init_capture(&self, rows: u16, cols: u16) {
        *self.capture.lock().expect("capture mutex poisoned") =
            Some(TerminalCapture::new(rows, cols));
    }

    pub(super) fn resize_capture(&self, rows: u16, cols: u16) {
        if let Some(capture) = self
            .capture
            .lock()
            .expect("capture mutex poisoned")
            .as_mut()
        {
            capture.resize(rows, cols);
        }
    }

    pub(super) fn snapshot(&self) -> SessionSnapshot {
        self.snapshot
            .lock()
            .expect("snapshot mutex poisoned")
            .clone()
    }

    pub(super) fn set_root_pid(&self, root_pid: Option<u32>) {
        let snapshot = {
            let mut guard = self.snapshot.lock().expect("snapshot mutex poisoned");
            guard.root_pid = root_pid;
            guard.clone()
        };
        let _ = self.persist_snapshot(&snapshot);
    }

    pub(super) fn set_exit_code(&self, exit_code: i32) {
        let snapshot = {
            let mut guard = self.snapshot.lock().expect("snapshot mutex poisoned");
            guard.exit_code = Some(exit_code);
            guard.clone()
        };
        let _ = self.persist_snapshot(&snapshot);
    }

    pub(super) fn set_background(&self, background: bool) {
        let snapshot = {
            let mut guard = self.snapshot.lock().expect("snapshot mutex poisoned");
            if guard.background == background {
                return;
            }
            guard.background = background;
            guard.clone()
        };
        let _ = self.persist_snapshot(&snapshot);
    }

    pub(super) fn set_repeat_state(&self, running: bool, next_run_at: Option<u64>) {
        let snapshot = {
            let mut guard = self.snapshot.lock().expect("snapshot mutex poisoned");
            guard.repeat_running = running;
            guard.repeat_next_run_at = next_run_at;
            guard.clone()
        };
        let _ = self.persist_snapshot(&snapshot);
    }

    pub(super) fn attach_client(&self, shutdown: TcpStream) -> Result<AttachClientResult, String> {
        // First, try to evict any dead client before checking occupancy.
        self.evict_dead_client();
        let mut guard = self.client.lock().expect("client mutex poisoned");
        if guard.is_some() {
            return Err("session already has an attached client".to_string());
        }
        let (tx, rx) = mpsc::channel();
        let client_id = self.next_client_id.fetch_add(1, Ordering::AcqRel);
        guard.replace(AttachedClient {
            id: client_id,
            sender: tx,
            shutdown,
            attached_at: Instant::now(),
        });
        drop(guard);
        self.set_background(false);
        let snapshot = self.snapshot();
        // For PTY sessions with terminal capture, emit a single synthesized
        // repaint — cells + cursor + alt-screen + mode flags — that reproduces
        // the current display on a fresh terminal. Raw backlog cannot do this
        // for TUIs because cursor moves and partial redraws stack into garbage
        // when played from the middle of a session. See issue #34.
        //
        // For subprocess (line-oriented) sessions we keep the raw-backlog
        // replay: each line is complete on its own and a history dump is
        // what a user attaching mid-run actually wants to see.
        let replay = {
            let capture = self.capture.lock().expect("capture mutex poisoned");
            if let Some(capture) = capture.as_ref() {
                vec![capture.snapshot_bytes()]
            } else {
                self.backlog
                    .lock()
                    .expect("backlog mutex poisoned")
                    .chunks
                    .iter()
                    .cloned()
                    .collect()
            }
        };
        Ok((client_id, rx, snapshot, replay))
    }

    pub(super) fn detach_client(&self, client_id: u64) {
        let mut guard = self.client.lock().expect("client mutex poisoned");
        if guard.as_ref().is_some_and(|client| client.id == client_id) {
            *guard = None;
        }
        drop(guard);
        if self.snapshot().exit_code.is_none() {
            self.set_background(true);
        }
    }

    pub(super) fn owns_client(&self, client_id: u64) -> bool {
        self.client
            .lock()
            .expect("client mutex poisoned")
            .as_ref()
            .is_some_and(|client| client.id == client_id)
    }

    pub(super) fn has_client(&self) -> bool {
        self.client.lock().expect("client mutex poisoned").is_some()
    }

    /// Check if the attached client's TCP connection is still alive.
    /// If the peer has disconnected, evict the dead client so new attaches succeed.
    pub(super) fn evict_dead_client(&self) {
        let mut guard = self.client.lock().expect("client mutex poisoned");
        let should_evict = if let Some(client) = guard.as_ref() {
            // Try a zero-byte peek to check if the connection is still alive.
            // A connection-reset or broken-pipe error means the peer is gone.
            let mut probe = [0u8; 1];
            client.shutdown.set_nonblocking(true).ok();
            let dead = match client.shutdown.peek(&mut probe) {
                // EOF means peer closed the connection
                Ok(0) => true,
                // WouldBlock means the socket is alive but has no data
                Err(ref e) if e.kind() == io::ErrorKind::WouldBlock => false,
                Err(ref e) if e.kind() == io::ErrorKind::ConnectionReset => true,
                Err(ref e) if e.kind() == io::ErrorKind::ConnectionAborted => true,
                // Unknown error: consider stale after 10s
                Err(_) => client.attached_at.elapsed() > Duration::from_secs(10),
                // Data available means socket is alive
                Ok(_) => false,
            };
            client.shutdown.set_nonblocking(false).ok();
            dead
        } else {
            false
        };
        if should_evict {
            if let Some(old) = guard.take() {
                let _ = old.shutdown.shutdown(Shutdown::Both);
            }
            drop(guard);
            if self.snapshot().exit_code.is_none() {
                self.set_background(true);
            }
        }
    }

    pub(super) fn push_output(&self, chunk: Vec<u8>) {
        {
            let mut backlog = self.backlog.lock().expect("backlog mutex poisoned");
            backlog.total_bytes += chunk.len();
            backlog.chunks.push_back(chunk.clone());
            while backlog.total_bytes > self.backlog_limit_bytes {
                if let Some(front) = backlog.chunks.pop_front() {
                    backlog.total_bytes = backlog.total_bytes.saturating_sub(front.len());
                } else {
                    break;
                }
            }
        }
        // Feed the server-side terminal emulator so the grid stays in sync
        // with what the backend is rendering. Cheap enough to do on the hot
        // path (vt100 is a streaming VTE-based parser); the expensive part,
        // snapshot synthesis, only runs on attach.
        if let Some(capture) = self
            .capture
            .lock()
            .expect("capture mutex poisoned")
            .as_mut()
        {
            capture.feed(&chunk);
        }
        // pm2-style persistent log: every byte that the session produces gets
        // appended to `<state_dir>/logs/<session_id>.log`, so `clud logs` can
        // show output that scrolled off the 256 KiB in-memory backlog.
        self.append_log(&chunk);
        self.send_to_client(WorkerServerMessage::Output {
            data_b64: base64::engine::general_purpose::STANDARD.encode(chunk),
        });
    }

    pub(super) fn broadcast_exit(&self, exit_code: i32) {
        self.set_exit_code(exit_code);
        self.stop_accepting.store(true, Ordering::Release);
        self.send_to_client(WorkerServerMessage::Exited { exit_code });
    }

    fn send_to_client(&self, message: WorkerServerMessage) {
        let sender = self
            .client
            .lock()
            .expect("client mutex poisoned")
            .as_ref()
            .map(|client| client.sender.clone());
        if let Some(sender) = sender {
            let _ = sender.send(message);
        }
    }

    fn persist_snapshot(&self, snapshot: &SessionSnapshot) -> io::Result<()> {
        write_json_file(
            &session_snapshot_path(&self.state_dir, &self.session_id),
            snapshot,
        )
    }
}

#[cfg(test)]
mod tests {
    //! Tests that the terminal-capture layer is wired correctly through
    //! `WorkerShared`. Complements the pure-unit tests in `capture.rs`: those
    //! prove the parser can round-trip any given byte stream; these prove the
    //! daemon calls `init_capture`, `feed`, `resize`, and `snapshot_bytes` at
    //! the right points so an attach actually delivers the current screen.
    use super::*;
    use crate::daemon::types::SessionKind;
    use std::net::TcpListener;
    use std::path::Path;
    use std::sync::Arc;
    use std::thread;
    use tempfile::TempDir;

    fn loopback_pair() -> (TcpStream, TcpStream) {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind loopback");
        let port = listener.local_addr().expect("local_addr").port();
        let client = TcpStream::connect(("127.0.0.1", port)).expect("connect");
        let (server, _) = listener.accept().expect("accept");
        (client, server)
    }

    fn test_shared(state_dir: &Path, kind: SessionKind) -> Arc<WorkerShared> {
        let snap = SessionSnapshot {
            id: "test-session".into(),
            kind,
            cwd: None,
            name: None,
            created_at: Some(0),
            detachable: true,
            background: false,
            attachable: true,
            repeat_interval_secs: None,
            repeat_next_run_at: None,
            repeat_running: false,
            daemon_pid: 0,
            worker_pid: 0,
            worker_port: 0,
            root_pid: None,
            exit_code: None,
        };
        Arc::new(WorkerShared::new(
            state_dir.to_path_buf(),
            "test-session".into(),
            snap,
        ))
    }

    fn shared_with_log(tmp: &TempDir, id: &str) -> Arc<WorkerShared> {
        let snap = SessionSnapshot {
            id: id.into(),
            kind: SessionKind::Subprocess,
            cwd: None,
            name: None,
            created_at: Some(0),
            detachable: true,
            background: false,
            attachable: true,
            repeat_interval_secs: None,
            repeat_next_run_at: None,
            repeat_running: false,
            daemon_pid: 0,
            worker_pid: 0,
            worker_port: 0,
            root_pid: None,
            exit_code: None,
        };
        let shared = Arc::new(WorkerShared::new(tmp.path().to_path_buf(), id.into(), snap));
        shared.init_log_file();
        shared
    }

    #[test]
    fn pty_attach_returns_single_synthesized_snapshot() {
        let tmp = TempDir::new().expect("tempdir");
        let shared = test_shared(tmp.path(), SessionKind::Pty);
        shared.init_capture(24, 80);
        shared.push_output(b"\x1b[1;1HHEADER\x1b[5;1HFOOTER".to_vec());

        let (_client, server) = loopback_pair();
        let (_id, _rx, _snap, replay) = shared.attach_client(server).expect("attach");

        assert_eq!(
            replay.len(),
            1,
            "PTY attach must deliver exactly one synthesized snapshot chunk"
        );
        let mut p = vt100::Parser::new(24, 80, 0);
        p.process(&replay[0]);
        let contents = p.screen().contents();
        assert!(contents.contains("HEADER"), "HEADER missing from replay");
        assert!(contents.contains("FOOTER"), "FOOTER missing from replay");
    }

    #[test]
    fn subprocess_attach_returns_raw_backlog_unchanged() {
        // Back-compat guarantee: subprocess sessions are line-oriented, users
        // attaching mid-run want history, not a repaint. `init_capture` is
        // never called in that code path, so `attach_client` must fall back
        // to the raw-chunk replay that pre-dated issue #34.
        let tmp = TempDir::new().expect("tempdir");
        let shared = test_shared(tmp.path(), SessionKind::Subprocess);
        shared.push_output(b"line1\n".to_vec());
        shared.push_output(b"line2\n".to_vec());

        let (_client, server) = loopback_pair();
        let (_id, _rx, _snap, replay) = shared.attach_client(server).expect("attach");
        assert_eq!(replay, vec![b"line1\n".to_vec(), b"line2\n".to_vec()]);
    }

    #[test]
    fn reattach_after_detach_delivers_current_frame() {
        // Output arriving *between* detach and reattach must still make it
        // into the second snapshot — the capture keeps feeding even with no
        // client connected.
        let tmp = TempDir::new().expect("tempdir");
        let shared = test_shared(tmp.path(), SessionKind::Pty);
        shared.init_capture(24, 80);
        shared.push_output(b"\x1b[1;1HBEFORE".to_vec());

        let (_c1, s1) = loopback_pair();
        let (cid1, _, _, _) = shared.attach_client(s1).expect("first attach");
        shared.detach_client(cid1);

        shared.push_output(b"\x1b[2;1HAFTER".to_vec());

        let (_c2, s2) = loopback_pair();
        let (_, _, _, replay) = shared.attach_client(s2).expect("second attach");
        let mut p = vt100::Parser::new(24, 80, 0);
        p.process(&replay[0]);
        let c = p.screen().contents();
        assert!(c.contains("BEFORE"), "pre-detach content missing: {:?}", c);
        assert!(c.contains("AFTER"), "post-detach content missing: {:?}", c);
    }

    #[test]
    fn live_attached_client_blocks_second_attach() {
        let tmp = TempDir::new().expect("tempdir");
        let shared = test_shared(tmp.path(), SessionKind::Subprocess);

        let (_client1, server1) = loopback_pair();
        let (_cid1, _, _, _) = shared.attach_client(server1).expect("first attach");
        thread::sleep(Duration::from_millis(1050));

        let (_client2, server2) = loopback_pair();
        let err = shared.attach_client(server2).expect_err("second attach");
        assert_eq!(err, "session already has an attached client");
    }

    #[test]
    fn dead_attached_client_is_evicted_on_next_attach() {
        let tmp = TempDir::new().expect("tempdir");
        let shared = test_shared(tmp.path(), SessionKind::Subprocess);

        let (client1, server1) = loopback_pair();
        let (_cid1, _, _, _) = shared.attach_client(server1).expect("first attach");
        drop(client1);
        thread::sleep(Duration::from_millis(50));

        let (_client2, server2) = loopback_pair();
        shared.attach_client(server2).expect("reattach");
    }

    #[test]
    fn resize_capture_takes_effect_for_subsequent_paint() {
        // Pre-resize the parser grid isn't wide enough to hold col 100.
        // After resize, a paint at col 100 lands correctly and the attach
        // replay reflects it.
        let tmp = TempDir::new().expect("tempdir");
        let shared = test_shared(tmp.path(), SessionKind::Pty);
        shared.init_capture(24, 80);
        shared.resize_capture(40, 120);
        shared.push_output(b"\x1b[1;100HEDGE".to_vec());

        let (_c, s) = loopback_pair();
        let (_, _, _, replay) = shared.attach_client(s).expect("attach");

        let mut p = vt100::Parser::new(40, 120, 0);
        p.process(&replay[0]);
        let cell = p.screen().cell(0, 99).expect("cell 0,99 in 120-col grid");
        assert_eq!(
            cell.contents(),
            "E",
            "'E' of EDGE should land at col 99 after resize"
        );
    }

    #[test]
    fn push_output_appends_to_log_file() {
        let tmp = TempDir::new().unwrap();
        let shared = shared_with_log(&tmp, "s1");
        shared.push_output(b"line one\n".to_vec());
        shared.push_output(b"line two\n".to_vec());
        // Drop so the file flushes / releases on Windows.
        drop(shared);

        let path = session_log_path(tmp.path(), "s1");
        let contents = fs::read(&path).expect("read log");
        assert_eq!(contents, b"line one\nline two\n");
    }

    #[test]
    fn rotation_moves_oversize_log_to_backup() {
        let tmp = TempDir::new().unwrap();
        let shared = shared_with_log(&tmp, "s2");
        let chunk = vec![b'x'; (LOG_ROTATE_BYTES / 4) as usize];
        // Push enough chunks to exceed the rotate threshold.
        for _ in 0..6 {
            shared.push_output(chunk.clone());
        }
        drop(shared);

        let primary = session_log_path(tmp.path(), "s2");
        let backup = primary.with_extension("log.1");
        assert!(
            backup.exists(),
            "rotation should have produced a .log.1 backup"
        );
        // Primary may exist (post-rotation reopened) but be smaller than the cap.
        if primary.exists() {
            let len = fs::metadata(&primary).unwrap().len();
            assert!(len < LOG_ROTATE_BYTES, "primary grew past the rotation cap");
        }
    }

    #[test]
    fn worker_shared_honors_backlog_override() {
        // With a 10-byte cap, pushing three 5-byte chunks should evict the
        // oldest so only the most-recent chunks remain resident.
        let tmp = TempDir::new().unwrap();
        let snap = SessionSnapshot {
            id: "s".into(),
            kind: SessionKind::Subprocess,
            cwd: None,
            name: None,
            created_at: Some(0),
            detachable: false,
            background: false,
            attachable: true,
            repeat_interval_secs: None,
            repeat_next_run_at: None,
            repeat_running: false,
            daemon_pid: 0,
            worker_pid: 0,
            worker_port: 0,
            root_pid: None,
            exit_code: None,
        };
        let shared = Arc::new(WorkerShared::new_with_backlog(
            tmp.path().to_path_buf(),
            "s".into(),
            snap,
            10,
        ));
        shared.push_output(b"AAAAA".to_vec());
        shared.push_output(b"BBBBB".to_vec());
        shared.push_output(b"CCCCC".to_vec());
        let backlog = shared.backlog.lock().unwrap();
        assert!(
            backlog.total_bytes <= 10,
            "backlog exceeded cap: {} > 10",
            backlog.total_bytes
        );
    }
}
