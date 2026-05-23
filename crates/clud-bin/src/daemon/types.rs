use std::io;
use std::net::TcpStream;
use std::sync::mpsc;
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};

use running_process_core::pty::NativePtyProcess;
use running_process_core::NativeProcess;
use serde::{Deserialize, Serialize};
use sysinfo::Signal;

use crate::command::LaunchPlan;

use super::process_utils::signal_process_tree;

pub(super) const ENV_FEATURE_FLAG: &str = "CLUD_EXPERIMENTAL_DAEMON";
pub(super) const ENV_STATE_DIR: &str = "CLUD_DAEMON_STATE_DIR";
pub(super) const ENV_BACKLOG_BYTES: &str = "CLUD_BACKLOG_BYTES";
/// Issue #135: opt out of the always-on daemon auto-spawn. Used by both
/// the CLI flag `--no-daemon` and the `clud gc *` precondition check.
pub const ENV_NO_DAEMON: &str = "CLUD_NO_DAEMON";
pub(super) const DEFAULT_BACKLOG_LIMIT_BYTES: usize = 256 * 1024;
pub(super) const BACKGROUND_PROMPT_TIMEOUT: Duration = Duration::from_secs(5);

/// pm2-style per-session log file. Soft cap at 10 MiB; exceeding rolls the
/// current file to `<id>.log.1` (overwriting any prior backup). Keeping only
/// one backup is deliberate — clud sessions are ephemeral and the on-disk
/// footprint shouldn't grow unboundedly for a stale session nobody
/// reattaches to.
pub(super) const LOG_ROTATE_BYTES: u64 = 10 * 1024 * 1024;

pub(super) fn default_attachable() -> bool {
    true
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(super) enum SessionKind {
    Subprocess,
    Pty,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(super) struct DaemonInfo {
    pub(super) pid: u32,
    pub(super) port: u16,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(super) struct SessionSnapshot {
    pub(super) id: String,
    pub(super) kind: SessionKind,
    #[serde(default)]
    pub(super) cwd: Option<String>,
    #[serde(default)]
    pub(super) name: Option<String>,
    #[serde(default)]
    pub(super) created_at: Option<u64>,
    #[serde(default)]
    pub(super) detachable: bool,
    #[serde(default)]
    pub(super) background: bool,
    #[serde(default = "default_attachable")]
    pub(super) attachable: bool,
    #[serde(default)]
    pub(super) repeat_interval_secs: Option<u64>,
    #[serde(default)]
    pub(super) repeat_next_run_at: Option<u64>,
    #[serde(default)]
    pub(super) repeat_running: bool,
    pub(super) daemon_pid: u32,
    pub(super) worker_pid: u32,
    pub(super) worker_port: u16,
    pub(super) root_pid: Option<u32>,
    pub(super) exit_code: Option<i32>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(super) struct WorkerLaunchSpec {
    pub(super) plan: LaunchPlan,
    pub(super) kind: SessionKind,
    #[serde(default)]
    pub(super) name: Option<String>,
    #[serde(default)]
    pub(super) detachable: bool,
    #[serde(default)]
    pub(super) background_on_launch: bool,
    #[serde(default = "default_attachable")]
    pub(super) attachable: bool,
    pub(super) rows: u16,
    pub(super) cols: u16,
    #[serde(default)]
    pub(super) repeat_interval_secs: Option<u64>,
    #[serde(default)]
    pub(super) repeat_run_command: Option<Vec<String>>,
    /// In-memory attach-replay backlog cap. `None` uses the compiled default
    /// (`DEFAULT_BACKLOG_LIMIT_BYTES`). Optional for wire compatibility with
    /// spec files written by older clud versions.
    #[serde(default)]
    pub(super) backlog_bytes: Option<usize>,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
pub(super) enum DaemonRequest {
    Create {
        spec: Box<WorkerLaunchSpec>,
    },
    Session {
        session_id: String,
    },
    Terminate {
        session_id: String,
    },
    /// Issue #135: GC ops served by the registry worker thread (see
    /// `gc_service.rs`). Carry the original `gc.*` op inside a single
    /// enum variant so the wire format and the registry-worker dispatch
    /// share one definition. (Field is `payload` rather than `op` so it
    /// doesn't collide with the outer `#[serde(tag = "op")]`.)
    Gc {
        payload: GcOp,
    },
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
pub(super) enum DaemonResponse {
    Created { session: SessionSnapshot },
    Session { session: SessionSnapshot },
    Terminated { session: SessionSnapshot },
    Gc { reply: GcReply },
    Error { message: String },
}

/// Issue #135: payload carried by `DaemonRequest::Gc`. Identical in
/// shape to the standalone `gc_daemon` protocol it replaces; only the
/// outer envelope changed.
#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "gc_op", rename_all = "snake_case")]
pub(crate) enum GcOp {
    List {
        #[serde(default)]
        kind: Option<String>,
    },
    Purge {
        /// Duration string (e.g. `"7d"`) or `None` to purge ALL non-live-locked entries.
        #[serde(default)]
        duration: Option<String>,
        #[serde(default)]
        kind: Option<String>,
        #[serde(default)]
        dry_run: bool,
    },
    Reconcile {
        repo_root: String,
    },
    Insert {
        kind: String,
        path: String,
        #[serde(default)]
        repo_root: Option<String>,
        #[serde(default)]
        branch: Option<String>,
        #[serde(default)]
        agent_id: Option<String>,
        #[serde(default)]
        created_unix: Option<i64>,
    },
}

/// Issue #135: payload carried by `DaemonResponse::Gc`. Mirrors what the
/// registry worker emits.
#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "gc_op", rename_all = "snake_case")]
pub(crate) enum GcReply {
    ListOk { rows: Vec<ListRow> },
    PurgeOk { removed: usize, skipped: usize },
    ReconcileOk { inserted: usize },
    InsertOk,
    Error { message: String },
}

/// Row shape returned by `gc.list`. Stable JSON schema for the CLI; the
/// `clud gc list --json` output is this struct serialized as an array.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ListRow {
    pub id: i64,
    pub kind: String,
    pub path: String,
    pub repo_root: Option<String>,
    pub branch: Option<String>,
    pub agent_id: Option<String>,
    pub created_unix: i64,
    pub live_locked: bool,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
pub(super) enum WorkerClientMessage {
    Attach,
    Input { data_b64: String, submit: bool },
    Resize { rows: u16, cols: u16 },
    Interrupt,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
pub(super) enum WorkerServerMessage {
    Attached { session: SessionSnapshot },
    Output { data_b64: String },
    Exited { exit_code: i32 },
    Error { message: String },
}

#[derive(Clone)]
pub(super) enum SessionRuntime {
    Subprocess(Arc<NativeProcess>),
    Pty(Arc<NativePtyProcess>),
}

impl SessionRuntime {
    pub(super) fn root_pid(&self) -> Option<u32> {
        match self {
            Self::Subprocess(process) => process.pid(),
            Self::Pty(process) => process.pid().ok().flatten(),
        }
    }

    pub(super) fn interrupt(&self) {
        match self {
            Self::Subprocess(process) => {
                let _ = process.kill();
            }
            Self::Pty(process) => {
                let _ = process.send_interrupt_impl();
            }
        }
    }

    pub(super) fn write(&self, data: &[u8], submit: bool) {
        if let Self::Pty(process) = self {
            let _ = process.write_impl(data, submit);
        }
    }

    pub(super) fn resize(&self, rows: u16, cols: u16) {
        if let Self::Pty(process) = self {
            let _ = process.resize_impl(rows, cols);
        }
    }

    pub(super) fn cleanup_tree(&self) {
        if let Some(pid) = self.root_pid() {
            signal_process_tree(pid, Signal::Term);
            thread::sleep(Duration::from_millis(150));
            signal_process_tree(pid, Signal::Kill);
        }
        match self {
            Self::Subprocess(process) => {
                let _ = process.kill();
            }
            Self::Pty(process) => {
                let _ = process.terminate_tree_impl();
                thread::sleep(Duration::from_millis(150));
                let _ = process.kill_tree_impl();
                let _ = process.close_impl();
            }
        }
    }
}

pub(super) type AttachClientResult = (
    u64,
    mpsc::Receiver<WorkerServerMessage>,
    SessionSnapshot,
    Vec<Vec<u8>>,
);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum LocalAttachResult {
    Completed(i32),
    InterruptRequested,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum BackgroundPromptDecision {
    ContinueInBackground,
    EndSession,
}

pub(super) struct RawTerminalGuard;

impl RawTerminalGuard {
    pub(super) fn enter() -> io::Result<Self> {
        crossterm::terminal::enable_raw_mode()?;
        Ok(Self)
    }
}

impl Drop for RawTerminalGuard {
    fn drop(&mut self) {
        let _ = crossterm::terminal::disable_raw_mode();
    }
}

#[derive(Debug, PartialEq, Eq)]
pub(super) enum KeyAction {
    Forward(Vec<u8>),
    Interrupt,
    /// F3 went down. In centralized mode the attach pump fires
    /// `InteractiveHooks::on_f3_press`; in local-PTY mode the runner's
    /// raw pump observes the matching byte sequence directly. Both paths
    /// converge on `VoiceMode` so hold-to-record behaves the same way.
    F3Press,
    /// F3 came back up — emitted only by terminals that report key
    /// releases (kitty protocol; Windows ConPTY only when keyboard
    /// enhancement flags negotiate). Other terminals stop recording via
    /// VAD auto-stop in `VoiceMode::on_tick`.
    F3Release,
    Ignore,
}

pub(super) struct AttachedClient {
    pub(super) id: u64,
    pub(super) sender: mpsc::Sender<WorkerServerMessage>,
    pub(super) shutdown: TcpStream,
    pub(super) attached_at: Instant,
}

#[derive(Default)]
pub(super) struct BacklogState {
    pub(super) chunks: std::collections::VecDeque<Vec<u8>>,
    pub(super) total_bytes: usize,
}
