//! Captured JSONL execution for one durable API-session generation.
//!
//! This is intentionally not an HTTP handler or lifecycle-control state
//! machine. It only launches an already canonical headless `LaunchPlan`,
//! drains it independently of consumers, and seals durable turn metadata.

use std::fs::{self, OpenOptions};
use std::io::{self, Write};
use std::path::Path;
use std::sync::Arc;
use std::thread;
use std::time::Duration;

use running_process::{NativeProcess, ProcessConfig, ReadStatus, StderrMode, StdinMode};
use serde_json::{json, Value};

use crate::command::LaunchPlan;
use crate::process_identity::ProcessIdentity;
use crate::win_creation_flags::invisible_helper_creationflags;

use super::api_sessions::{ApiSessionBackend, ApiSessionStore, ApiSessionStoreError, ApiTurnState};
use super::headless_adapter::{parse_backend_event, BackendEvent};
use super::paths::api_turn_log_path;
use super::types::unix_millis_now;

const RAW_LOG_MAX_BYTES: u64 = 1024 * 1024;
const EVENT_LINE_MAX_BYTES: usize = 8 * 1024;

#[derive(Debug)]
pub enum ApiTurnLaunchError {
    Store(ApiSessionStoreError),
    Io(io::Error),
    InvalidPlan(String),
}

impl From<ApiSessionStoreError> for ApiTurnLaunchError { fn from(value: ApiSessionStoreError) -> Self { Self::Store(value) } }
impl From<io::Error> for ApiTurnLaunchError { fn from(value: io::Error) -> Self { Self::Io(value) } }
impl std::fmt::Display for ApiTurnLaunchError { fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result { match self { Self::Store(e) => write!(f, "{e}"), Self::Io(e) => write!(f, "{e}"), Self::InvalidPlan(e) => write!(f, "{e}") } } }
impl std::error::Error for ApiTurnLaunchError {}

/// Starts a subprocess-only API turn. `plan` must be produced by the typed
/// headless adapter; this boundary rejects a missing or mismatched cwd rather
/// than inheriting the daemon's working directory.
pub fn launch_captured_turn(store: ApiSessionStore, session_id: &str, plan: LaunchPlan) -> Result<Arc<NativeProcess>, ApiTurnLaunchError> {
    let session = store.get(session_id)?;
    let plan_cwd = plan.cwd.as_ref().map(Path::new).ok_or_else(|| ApiTurnLaunchError::InvalidPlan("headless turn plan omitted cwd".to_string()))?;
    if plan_cwd != session.cwd.as_path() { return Err(ApiTurnLaunchError::InvalidPlan("headless turn plan cwd differs from persisted API session cwd".to_string())); }
    let backend = match session.backend { ApiSessionBackend::Claude => crate::backend::Backend::Claude, ApiSessionBackend::Codex => crate::backend::Backend::Codex };
    if plan.backend != backend { return Err(ApiTurnLaunchError::InvalidPlan("headless turn backend differs from logical session backend".to_string())); }
    let turn_id = format!("turn-{}-{}", session.generation.saturating_add(1), unix_millis_now());
    let turn = store.begin_turn(session_id, turn_id)?;
    let process = Arc::new(NativeProcess::new(ProcessConfig {
        command: crate::subprocess::command_spec_for_subprocess(plan.command), cwd: Some(session.cwd.clone()), env: Some(super::io_helpers::child_env()), capture: true,
        stderr_mode: StderrMode::Stdout, creationflags: invisible_helper_creationflags(), create_process_group: false, stdin_mode: StdinMode::Null, nice: None,
    }));
    if let Err(error) = process.start() { let _ = store.finish_turn(session_id, &turn.id, ApiTurnState::Failed, Some("spawn_failed".to_string())); return Err(ApiTurnLaunchError::Io(io::Error::other(error.to_string()))); }
    if let Some(pid) = process.pid() { let _ = store.set_turn_root_identity(session_id, &turn.id, ProcessIdentity::observe(pid).unwrap_or_else(|| ProcessIdentity::pid_only(pid))); }
    let drain_process = Arc::clone(&process); let drain_store = store.clone(); let drain_session = session_id.to_string(); let drain_turn = turn.id.clone();
    let log = api_turn_log_path(store.state_dir(), &drain_session, turn.generation);
    let drain_handle = thread::spawn(move || drain_jsonl(drain_process, drain_store, drain_session, drain_turn, backend, log));
    let wait_process = Arc::clone(&process); let wait_store = store; let wait_session = session_id.to_string(); let wait_turn = turn.id;
    thread::spawn(move || { let code = wait_process.wait(None).unwrap_or(1); let _ = drain_handle.join(); let state = if code == 0 { ApiTurnState::Completed } else { ApiTurnState::Failed }; let _ = wait_store.finish_turn(&wait_session, &wait_turn, state, Some(format!("exit_{code}"))); });
    Ok(process)
}

fn drain_jsonl(process: Arc<NativeProcess>, store: ApiSessionStore, session_id: String, turn_id: String, backend: crate::backend::Backend, log_path: std::path::PathBuf) {
    loop { match process.read_combined(Some(Duration::from_millis(100))) {
        ReadStatus::Line(event) => observe_line(&store, &session_id, &turn_id, backend, &event.line, &log_path),
        ReadStatus::Timeout => if process.returncode().is_some() { break; }, ReadStatus::Eof => break,
    }}
}

fn observe_line(store: &ApiSessionStore, session_id: &str, turn_id: &str, backend: crate::backend::Backend, bytes: &[u8], log_path: &Path) {
    append_raw_line(log_path, bytes);
    let line = String::from_utf8_lossy(bytes).to_string();
    let raw = truncate(&line); let _ = store.append_event(session_id, Some(turn_id.to_string()), "raw_jsonl".to_string(), json!({"line": raw}));
    match parse_backend_event(backend, &line) {
        BackendEvent::ProviderSessionId(id) => { let _ = store.set_provider_session_id(session_id, id.clone()); let _ = store.append_event(session_id, Some(turn_id.to_string()), "provider_identity".to_string(), json!({"provider_session_id": id})); }
        BackendEvent::Opaque(value) => { let _ = store.append_event(session_id, Some(turn_id.to_string()), "backend_event".to_string(), bounded_value(value)); }
        BackendEvent::Malformed { line, error } => { let _ = store.append_event(session_id, Some(turn_id.to_string()), "backend_malformed".to_string(), json!({"line": truncate(&line), "error": error})); }
    }
}

fn append_raw_line(path: &Path, bytes: &[u8]) { if let Some(parent) = path.parent() { let _ = fs::create_dir_all(parent); } if fs::metadata(path).map(|m| m.len()).unwrap_or(0) > RAW_LOG_MAX_BYTES { let _ = fs::rename(path, path.with_extension("jsonl.1")); } if let Ok(mut file) = OpenOptions::new().create(true).append(true).open(path) { let _ = file.write_all(bytes); let _ = file.write_all(b"\n"); } }
fn truncate(value: &str) -> String { value.chars().take(EVENT_LINE_MAX_BYTES).collect() }
fn bounded_value(value: Value) -> Value { if serde_json::to_vec(&value).map(|b| b.len()).unwrap_or(usize::MAX) <= EVENT_LINE_MAX_BYTES { value } else { json!({"truncated": true}) } }

#[cfg(test)]
mod tests {
    use super::*; use crate::daemon::api_sessions::{CreateApiSession, ResolvedApiSessionSettings}; use tempfile::TempDir;
    fn store() -> (TempDir, ApiSessionStore, String) { let temp = TempDir::new().unwrap(); let store = ApiSessionStore::new(temp.path()); let record = store.create(CreateApiSession { backend: ApiSessionBackend::Claude, cwd: temp.path().to_path_buf(), name: None, resolved_settings: ResolvedApiSessionSettings { model: None, safe: true, model_provider: None, harness: None, routing_mode: None } }).unwrap(); (temp, store, record.id) }
    #[test] fn observes_identity_opaque_and_malformed_without_unbounded_events() { let (temp, store, id) = store(); let turn = store.begin_turn(&id, "turn-a".to_string()).unwrap(); let log = api_turn_log_path(temp.path(), &id, turn.generation); observe_line(&store, &id, &turn.id, crate::backend::Backend::Claude, br#"{"type":"system","subtype":"init","session_id":"provider-a"}"#, &log); observe_line(&store, &id, &turn.id, crate::backend::Backend::Claude, br#"{"type":"future"}"#, &log); observe_line(&store, &id, &turn.id, crate::backend::Backend::Claude, b"not json", &log); let record = store.get(&id).unwrap(); assert_eq!(record.provider_session_id.as_deref(), Some("provider-a")); assert_eq!(record.events.len(), 6); assert!(log.exists()); }
}
