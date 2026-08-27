//! Serialized lifecycle controls for durable API sessions (#1043).
//! Legacy attach Ctrl-C and `DaemonRequest::Interrupt` do not use this module.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use running_process::NativeProcess;
use sysinfo::Signal;

use crate::command::LaunchPlan;

use super::api_sessions::{ApiSessionState, ApiSessionStore, ApiTurnState};
use super::api_turn_controller::launch_captured_turn;
use super::process_utils::signal_process_tree_as;

pub const DEFAULT_INTERRUPT_GRACE: Duration = Duration::from_secs(5);

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LifecycleReply { Started { generation: u64, turn_id: String }, Replayed { turn_id: String }, SessionBusy, Interrupted { forced: bool }, Terminated }
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LifecycleError { NotFound, Terminated, ResumeUnavailable, IdempotencyConflict, Launch(String) }

#[derive(Clone)]
pub struct ApiSessionLifecycle { store: ApiSessionStore, active: Arc<Mutex<HashMap<String, Arc<NativeProcess>>>>, grace: Duration }

impl ApiSessionLifecycle {
    pub fn new(store: ApiSessionStore) -> Self { Self { store, active: Arc::new(Mutex::new(HashMap::new())), grace: DEFAULT_INTERRUPT_GRACE } }
    pub fn submit(&self, session_id: &str, plan: LaunchPlan, key: Option<String>, fingerprint: String, replace: bool) -> Result<LifecycleReply, LifecycleError> {
        let session = self.store.get(session_id).map_err(|_| LifecycleError::NotFound)?;
        if session.state == ApiSessionState::Terminated { return Err(LifecycleError::Terminated); }
        if let Some(key) = key.as_ref() { if let Some(old) = session.idempotency.iter().find(|old| old.key == *key) { return if old.request_fingerprint == fingerprint { Ok(LifecycleReply::Replayed { turn_id: old.turn_id.clone() }) } else { Err(LifecycleError::IdempotencyConflict) }; } }
        if let Some(process) = self.active.lock().unwrap_or_else(|p| p.into_inner()).remove(session_id) {
            if !replace { self.active.lock().unwrap_or_else(|p| p.into_inner()).insert(session_id.to_string(), process); return Ok(LifecycleReply::SessionBusy); }
            self.stop(session_id, &process);
        } else if matches!(session.state, ApiSessionState::Running | ApiSessionState::Interrupting) { return Ok(LifecycleReply::SessionBusy); }
        let process = launch_captured_turn(self.store.clone(), session_id, plan).map_err(|e| LifecycleError::Launch(e.to_string()))?;
        let current = self.store.get(session_id).map_err(|_| LifecycleError::NotFound)?;
        let turn_id = current.current_turn_id.clone().ok_or_else(|| LifecycleError::Launch("turn not persisted".to_string()))?;
        if let Some(key) = key { self.store.remember_idempotency(session_id, key, turn_id.clone(), fingerprint).map_err(|_| LifecycleError::IdempotencyConflict)?; }
        self.active.lock().unwrap_or_else(|p| p.into_inner()).insert(session_id.to_string(), process);
        Ok(LifecycleReply::Started { generation: current.generation, turn_id })
    }
    pub fn interrupt(&self, session_id: &str) -> Result<LifecycleReply, LifecycleError> { let process = self.active.lock().unwrap_or_else(|p| p.into_inner()).remove(session_id).ok_or(LifecycleError::NotFound)?; Ok(LifecycleReply::Interrupted { forced: self.stop(session_id, &process) }) }
    pub fn kill(&self, session_id: &str) -> Result<LifecycleReply, LifecycleError> { let record = self.store.get(session_id).map_err(|_| LifecycleError::NotFound)?; if let Some(process) = self.active.lock().unwrap_or_else(|p| p.into_inner()).remove(session_id) { let _ = process.kill(); } if let Some(turn) = record.current_turn_id { let _ = self.store.finish_turn(session_id, &turn, ApiTurnState::Killed, Some("terminal_kill".to_string())); } let _ = self.store.terminate(session_id); Ok(LifecycleReply::Terminated) }
    fn stop(&self, session_id: &str, process: &Arc<NativeProcess>) -> bool {
        let record = match self.store.get(session_id) { Ok(record) => record, Err(_) => return true };
        let _ = self.store.append_event(session_id, record.current_turn_id.clone(), "interrupt_requested".to_string(), serde_json::json!({"grace_ms": self.grace.as_millis()}));
        if let Some(identity) = record.turns.last().and_then(|turn| turn.root_identity) { signal_process_tree_as(&identity, Signal::Interrupt); }
        let started = Instant::now(); while process.returncode().is_none() && started.elapsed() < self.grace { thread::sleep(Duration::from_millis(25)); }
        let forced = process.returncode().is_none(); if forced { let _ = process.kill(); }
        if let Some(turn) = record.current_turn_id { let disposition = if forced { "forced_interrupt" } else { "graceful_interrupt" }; let _ = self.store.finish_turn(session_id, &turn, ApiTurnState::Interrupted, Some(disposition.to_string())); }
        forced
    }
}
