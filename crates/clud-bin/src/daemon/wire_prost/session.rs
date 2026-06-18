use serde::{de::DeserializeOwned, Serialize};

use super::proto;
use super::WireError;
use crate::daemon::types::{CtrlCProfile, SessionKind, SessionSnapshot};

pub(in crate::daemon::wire_prost) fn session_to_proto(
    session: &SessionSnapshot,
) -> proto::SessionSnapshot {
    proto::SessionSnapshot {
        id: session.id.clone(),
        kind: session_kind_to_proto(&session.kind),
        backend: session.backend.clone(),
        launch_mode: session.launch_mode.clone(),
        repo_root: session.repo_root.clone(),
        command: session.command.clone(),
        cwd: session.cwd.clone(),
        name: session.name.clone(),
        created_at: session.created_at,
        detachable: Some(session.detachable),
        background: Some(session.background),
        attachable: Some(session.attachable),
        repeat_interval_secs: session.repeat_interval_secs,
        repeat_next_run_at: session.repeat_next_run_at,
        repeat_running: Some(session.repeat_running),
        daemon_pid: session.daemon_pid,
        worker_pid: session.worker_pid,
        worker_port: u32::from(session.worker_port),
        root_pid: session.root_pid,
        exit_code: session.exit_code,
        exited_at: session.exited_at,
        ctrl_c: session.ctrl_c.as_ref().map(profile_to_proto),
    }
}

pub(in crate::daemon::wire_prost) fn session_from_proto(
    session: proto::SessionSnapshot,
) -> Result<SessionSnapshot, WireError> {
    Ok(SessionSnapshot {
        id: session.id,
        kind: session_kind_from_proto(session.kind)?,
        backend: session.backend,
        launch_mode: session.launch_mode,
        repo_root: session.repo_root,
        command: session.command,
        cwd: session.cwd,
        name: session.name,
        created_at: session.created_at,
        detachable: session.detachable.unwrap_or(false),
        background: session.background.unwrap_or(false),
        attachable: session.attachable.unwrap_or(true),
        repeat_interval_secs: session.repeat_interval_secs,
        repeat_next_run_at: session.repeat_next_run_at,
        repeat_running: session.repeat_running.unwrap_or(false),
        daemon_pid: session.daemon_pid,
        worker_pid: session.worker_pid,
        worker_port: u16_field("session.worker_port", session.worker_port)?,
        root_pid: session.root_pid,
        exit_code: session.exit_code,
        exited_at: session.exited_at,
        ctrl_c: session.ctrl_c.map(profile_from_proto),
    })
}

pub(in crate::daemon::wire_prost) fn session_from_proto_or_json(
    session: Option<proto::SessionSnapshot>,
    session_json: &[u8],
) -> Result<SessionSnapshot, WireError> {
    match session {
        Some(session) => session_from_proto(session),
        None => from_json_slice::<SessionSnapshot>(session_json),
    }
}

pub(in crate::daemon::wire_prost) fn session_kind_to_proto(kind: &SessionKind) -> i32 {
    match kind {
        SessionKind::Subprocess => 1,
        SessionKind::Pty => 2,
    }
}

pub(in crate::daemon::wire_prost) fn session_kind_from_proto(
    kind: i32,
) -> Result<SessionKind, WireError> {
    match kind {
        1 => Ok(SessionKind::Subprocess),
        2 => Ok(SessionKind::Pty),
        0 => Err(WireError::MissingPayload("session kind")),
        other => Err(WireError::InvalidSessionKind(other)),
    }
}

pub(in crate::daemon::wire_prost) fn profile_to_proto(
    profile: &CtrlCProfile,
) -> proto::CtrlCProfile {
    proto::CtrlCProfile {
        cli_pid: profile.cli_pid,
        cli_observed_at_ms: profile.cli_observed_at_ms,
        cli_handoff_at_ms: profile.cli_handoff_at_ms,
        cli_return_ready_at_ms: profile.cli_return_ready_at_ms,
        cli_handoff_ms: profile.cli_handoff_ms,
        daemon_received_at_ms: profile.daemon_received_at_ms,
        daemon_kill_started_at_ms: profile.daemon_kill_started_at_ms,
        daemon_kill_finished_at_ms: profile.daemon_kill_finished_at_ms,
        daemon_kill_ms: profile.daemon_kill_ms,
        fast_path: profile.fast_path,
    }
}

pub(in crate::daemon::wire_prost) fn profile_from_proto(
    profile: proto::CtrlCProfile,
) -> CtrlCProfile {
    CtrlCProfile {
        cli_pid: profile.cli_pid,
        cli_observed_at_ms: profile.cli_observed_at_ms,
        cli_handoff_at_ms: profile.cli_handoff_at_ms,
        cli_return_ready_at_ms: profile.cli_return_ready_at_ms,
        cli_handoff_ms: profile.cli_handoff_ms,
        daemon_received_at_ms: profile.daemon_received_at_ms,
        daemon_kill_started_at_ms: profile.daemon_kill_started_at_ms,
        daemon_kill_finished_at_ms: profile.daemon_kill_finished_at_ms,
        daemon_kill_ms: profile.daemon_kill_ms,
        fast_path: profile.fast_path,
    }
}

pub(in crate::daemon::wire_prost) fn to_json_vec<T: Serialize>(
    value: &T,
) -> Result<Vec<u8>, WireError> {
    serde_json::to_vec(value).map_err(WireError::Json)
}

pub(in crate::daemon::wire_prost) fn from_json_slice<T: DeserializeOwned>(
    bytes: &[u8],
) -> Result<T, WireError> {
    serde_json::from_slice(bytes).map_err(WireError::Json)
}

pub(in crate::daemon::wire_prost) fn u16_field(
    field: &'static str,
    value: u32,
) -> Result<u16, WireError> {
    value
        .try_into()
        .map_err(|_| WireError::U16OutOfRange { field, value })
}
