#![allow(dead_code)]

use std::fmt;
use std::path::PathBuf;

use base64::{engine::general_purpose::STANDARD as BASE64_STANDARD, Engine as _};
use prost::Message;
use serde::{de::DeserializeOwned, Serialize};

use super::types::{
    CtrlCProfile, DaemonRequest, DaemonResponse, GcOp, GcReply, SessionSnapshot,
    WorkerClientMessage, WorkerLaunchSpec, WorkerServerMessage,
};

#[allow(missing_docs)]
pub(super) mod proto {
    include!(concat!(env!("OUT_DIR"), "/clud.v1.rs"));
}

/// ASCII "CLUD" in a u32. This is the clud prost payload discriminator used
/// inside the future running-process v1 Frame.payload_protocol field.
pub(super) const CLUD_PROST_PAYLOAD_PROTOCOL: u32 = 0x434c_5544;
/// ASCII "CLJS" in a u32. This names the legacy JSON payload path while the
/// migration runs with both encoders available.
pub(super) const CLUD_JSON_PAYLOAD_PROTOCOL: u32 = 0x434c_4a53;
/// Selects the daemon RPC line format. Unset or `json` keeps the legacy JSON
/// line protocol; `prost` opts into the v1 prost frame envelope.
pub(super) const ENV_DAEMON_WIRE: &str = "CLUD_DAEMON_WIRE";

const DAEMON_FRAME_LINE_PREFIX: &str = "CLUD-FRAME/1 ";

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct WireFrame {
    pub(super) payload_protocol: u32,
    pub(super) payload: Vec<u8>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum DaemonWireFormat {
    Json,
    Prost,
}

impl DaemonWireFormat {
    fn from_env_value(value: Option<&str>) -> Result<Self, WireError> {
        let Some(raw) = value else {
            return Ok(Self::Json);
        };
        let trimmed = raw.trim();
        if trimmed.is_empty() {
            return Ok(Self::Json);
        }
        match trimmed.to_ascii_lowercase().as_str() {
            "json" | "legacy" | "legacy-json" => Ok(Self::Json),
            "prost" => Ok(Self::Prost),
            _ => Err(WireError::InvalidDaemonWire(raw.to_string())),
        }
    }
}

#[derive(Debug)]
pub(super) enum WireError {
    MissingPayload(&'static str),
    UnknownPayloadProtocol(u32),
    InvalidDaemonWire(String),
    InvalidFrameLine(String),
    Json(serde_json::Error),
    Base64(base64::DecodeError),
    Decode(prost::DecodeError),
    U16OutOfRange { field: &'static str, value: u32 },
}

impl fmt::Display for WireError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::MissingPayload(kind) => write!(f, "missing {kind} prost payload"),
            Self::UnknownPayloadProtocol(protocol) => {
                write!(f, "unsupported clud payload_protocol 0x{protocol:08x}")
            }
            Self::InvalidDaemonWire(value) => {
                write!(
                    f,
                    "unsupported {ENV_DAEMON_WIRE} value {value:?}; expected json or prost"
                )
            }
            Self::InvalidFrameLine(line) => {
                write!(f, "invalid clud daemon frame line: {line}")
            }
            Self::Json(err) => write!(f, "json conversion failed: {err}"),
            Self::Base64(err) => write!(f, "frame payload base64 decode failed: {err}"),
            Self::Decode(err) => write!(f, "prost decode failed: {err}"),
            Self::U16OutOfRange { field, value } => {
                write!(f, "{field} value {value} exceeds u16::MAX")
            }
        }
    }
}

impl std::error::Error for WireError {}

impl From<serde_json::Error> for WireError {
    fn from(value: serde_json::Error) -> Self {
        Self::Json(value)
    }
}

impl From<base64::DecodeError> for WireError {
    fn from(value: base64::DecodeError) -> Self {
        Self::Base64(value)
    }
}

impl From<prost::DecodeError> for WireError {
    fn from(value: prost::DecodeError) -> Self {
        Self::Decode(value)
    }
}

pub(super) fn daemon_wire_format_from_env() -> Result<DaemonWireFormat, WireError> {
    DaemonWireFormat::from_env_value(std::env::var(ENV_DAEMON_WIRE).ok().as_deref())
}

pub(super) fn encode_daemon_request_line(
    request: &DaemonRequest,
    format: DaemonWireFormat,
) -> Result<Vec<u8>, WireError> {
    match format {
        DaemonWireFormat::Json => encode_json_line(request),
        DaemonWireFormat::Prost => {
            let frame = encode_daemon_request_prost(request, "daemon-request")?;
            Ok(encode_wire_frame_line(&frame))
        }
    }
}

pub(super) fn decode_daemon_request_line(
    line: &str,
) -> Result<(DaemonRequest, DaemonWireFormat), WireError> {
    if let Some(frame) = decode_wire_frame_line(line)? {
        return Ok((decode_daemon_request(&frame)?, DaemonWireFormat::Prost));
    }
    Ok((serde_json::from_str(line)?, DaemonWireFormat::Json))
}

pub(super) fn encode_daemon_response_line(
    response: &DaemonResponse,
    format: DaemonWireFormat,
) -> Result<Vec<u8>, WireError> {
    match format {
        DaemonWireFormat::Json => encode_json_line(response),
        DaemonWireFormat::Prost => {
            let frame = encode_daemon_response_prost(response, "daemon-response")?;
            Ok(encode_wire_frame_line(&frame))
        }
    }
}

pub(super) fn decode_daemon_response_line(line: &str) -> Result<DaemonResponse, WireError> {
    if let Some(frame) = decode_wire_frame_line(line)? {
        return decode_daemon_response(&frame);
    }
    Ok(serde_json::from_str(line)?)
}

pub(super) fn encode_daemon_request_prost(
    request: &DaemonRequest,
    request_id: impl Into<String>,
) -> Result<WireFrame, WireError> {
    let payload = daemon_request_to_proto(request, request_id.into())?.encode_to_vec();
    Ok(prost_frame(payload))
}

pub(super) fn decode_daemon_request(frame: &WireFrame) -> Result<DaemonRequest, WireError> {
    match frame.payload_protocol {
        CLUD_PROST_PAYLOAD_PROTOCOL => {
            let proto = proto::ClientToDaemon::decode(frame.payload.as_slice())?;
            daemon_request_from_proto(proto)
        }
        CLUD_JSON_PAYLOAD_PROTOCOL => Ok(serde_json::from_slice(&frame.payload)?),
        other => Err(WireError::UnknownPayloadProtocol(other)),
    }
}

pub(super) fn encode_daemon_response_prost(
    response: &DaemonResponse,
    request_id: impl Into<String>,
) -> Result<WireFrame, WireError> {
    let payload = daemon_response_to_proto(response, request_id.into())?.encode_to_vec();
    Ok(prost_frame(payload))
}

pub(super) fn decode_daemon_response(frame: &WireFrame) -> Result<DaemonResponse, WireError> {
    match frame.payload_protocol {
        CLUD_PROST_PAYLOAD_PROTOCOL => {
            let proto = proto::DaemonToClient::decode(frame.payload.as_slice())?;
            daemon_response_from_proto(proto)
        }
        CLUD_JSON_PAYLOAD_PROTOCOL => Ok(serde_json::from_slice(&frame.payload)?),
        other => Err(WireError::UnknownPayloadProtocol(other)),
    }
}

pub(super) fn encode_worker_client_prost(
    message: &WorkerClientMessage,
) -> Result<WireFrame, WireError> {
    Ok(prost_frame(
        worker_client_to_proto(message)?.encode_to_vec(),
    ))
}

pub(super) fn decode_worker_client(frame: &WireFrame) -> Result<WorkerClientMessage, WireError> {
    match frame.payload_protocol {
        CLUD_PROST_PAYLOAD_PROTOCOL => {
            let proto = proto::WorkerClientEnvelope::decode(frame.payload.as_slice())?;
            worker_client_from_proto(proto)
        }
        CLUD_JSON_PAYLOAD_PROTOCOL => Ok(serde_json::from_slice(&frame.payload)?),
        other => Err(WireError::UnknownPayloadProtocol(other)),
    }
}

pub(super) fn encode_worker_server_prost(
    message: &WorkerServerMessage,
) -> Result<WireFrame, WireError> {
    Ok(prost_frame(
        worker_server_to_proto(message)?.encode_to_vec(),
    ))
}

pub(super) fn decode_worker_server(frame: &WireFrame) -> Result<WorkerServerMessage, WireError> {
    match frame.payload_protocol {
        CLUD_PROST_PAYLOAD_PROTOCOL => {
            let proto = proto::WorkerServerEnvelope::decode(frame.payload.as_slice())?;
            worker_server_from_proto(proto)
        }
        CLUD_JSON_PAYLOAD_PROTOCOL => Ok(serde_json::from_slice(&frame.payload)?),
        other => Err(WireError::UnknownPayloadProtocol(other)),
    }
}

#[cfg(test)]
fn encode_legacy_json_frame<T: Serialize>(value: &T) -> Result<WireFrame, WireError> {
    Ok(WireFrame {
        payload_protocol: CLUD_JSON_PAYLOAD_PROTOCOL,
        payload: serde_json::to_vec(value)?,
    })
}

fn prost_frame(payload: Vec<u8>) -> WireFrame {
    WireFrame {
        payload_protocol: CLUD_PROST_PAYLOAD_PROTOCOL,
        payload,
    }
}

fn encode_json_line<T: Serialize>(value: &T) -> Result<Vec<u8>, WireError> {
    let mut bytes = serde_json::to_vec(value)?;
    bytes.push(b'\n');
    Ok(bytes)
}

fn encode_wire_frame_line(frame: &WireFrame) -> Vec<u8> {
    let payload = BASE64_STANDARD.encode(&frame.payload);
    format!(
        "{DAEMON_FRAME_LINE_PREFIX}{:08x} {payload}\n",
        frame.payload_protocol
    )
    .into_bytes()
}

fn decode_wire_frame_line(line: &str) -> Result<Option<WireFrame>, WireError> {
    let trimmed = line.trim_end_matches('\n').trim_end_matches('\r');
    let Some(rest) = trimmed.strip_prefix(DAEMON_FRAME_LINE_PREFIX) else {
        return Ok(None);
    };
    let Some((protocol_hex, payload_b64)) = rest.split_once(' ') else {
        return Err(WireError::InvalidFrameLine(
            "missing protocol or payload".to_string(),
        ));
    };
    if protocol_hex.len() != 8 || payload_b64.is_empty() {
        return Err(WireError::InvalidFrameLine(
            "expected 8-digit protocol and non-empty payload".to_string(),
        ));
    }
    let payload_protocol = u32::from_str_radix(protocol_hex, 16).map_err(|_| {
        WireError::InvalidFrameLine(format!("invalid payload protocol {protocol_hex:?}"))
    })?;
    let payload = BASE64_STANDARD.decode(payload_b64)?;
    Ok(Some(WireFrame {
        payload_protocol,
        payload,
    }))
}

fn daemon_request_to_proto(
    request: &DaemonRequest,
    request_id: String,
) -> Result<proto::ClientToDaemon, WireError> {
    use proto::client_to_daemon::Request;
    let request = match request {
        DaemonRequest::Create { spec } => Request::Create(proto::CreateRequest {
            spec_json: to_json_vec(spec.as_ref())?,
        }),
        DaemonRequest::Session { session_id } => Request::Session(proto::SessionRequest {
            session_id: session_id.clone(),
        }),
        DaemonRequest::ListLiveCwds => Request::ListLiveCwds(proto::ListLiveCwdsRequest {}),
        DaemonRequest::Terminate { session_id } => Request::Terminate(proto::TerminateRequest {
            session_id: session_id.clone(),
        }),
        DaemonRequest::Interrupt {
            session_id,
            profile,
        } => Request::Interrupt(proto::InterruptRequest {
            session_id: session_id.clone(),
            profile: Some(profile_to_proto(profile)),
        }),
        DaemonRequest::AdoptKill { pids, reason } => Request::AdoptKill(proto::AdoptKillRequest {
            pids: pids.clone(),
            reason: reason.clone(),
        }),
        DaemonRequest::Gc { payload } => Request::Gc(proto::GcRequest {
            payload_json: to_json_vec(payload)?,
        }),
        DaemonRequest::Shutdown => Request::Shutdown(proto::ShutdownRequest {}),
    };
    Ok(proto::ClientToDaemon {
        request: Some(request),
        request_id,
    })
}

fn daemon_request_from_proto(proto: proto::ClientToDaemon) -> Result<DaemonRequest, WireError> {
    use proto::client_to_daemon::Request;
    match proto
        .request
        .ok_or(WireError::MissingPayload("daemon request"))?
    {
        Request::Create(create) => Ok(DaemonRequest::Create {
            spec: Box::new(from_json_slice::<WorkerLaunchSpec>(&create.spec_json)?),
        }),
        Request::Session(session) => Ok(DaemonRequest::Session {
            session_id: session.session_id,
        }),
        Request::ListLiveCwds(_) => Ok(DaemonRequest::ListLiveCwds),
        Request::Terminate(terminate) => Ok(DaemonRequest::Terminate {
            session_id: terminate.session_id,
        }),
        Request::Interrupt(interrupt) => Ok(DaemonRequest::Interrupt {
            session_id: interrupt.session_id,
            profile: profile_from_proto(
                interrupt
                    .profile
                    .ok_or(WireError::MissingPayload("ctrl-c profile"))?,
            ),
        }),
        Request::AdoptKill(adopt) => Ok(DaemonRequest::AdoptKill {
            pids: adopt.pids,
            reason: adopt.reason,
        }),
        Request::Gc(gc) => Ok(DaemonRequest::Gc {
            payload: from_json_slice::<GcOp>(&gc.payload_json)?,
        }),
        Request::Shutdown(_) => Ok(DaemonRequest::Shutdown),
    }
}

fn daemon_response_to_proto(
    response: &DaemonResponse,
    request_id: String,
) -> Result<proto::DaemonToClient, WireError> {
    use proto::daemon_to_client::Response;
    let response = match response {
        DaemonResponse::Created { session } => Response::Created(proto::CreatedResponse {
            session_json: to_json_vec(session)?,
        }),
        DaemonResponse::Session { session } => Response::Session(proto::SessionResponse {
            session_json: to_json_vec(session)?,
        }),
        DaemonResponse::LiveCwds { paths } => Response::LiveCwds(proto::LiveCwdsResponse {
            paths: paths
                .iter()
                .map(|path| path.to_string_lossy().to_string())
                .collect(),
        }),
        DaemonResponse::Terminated { session } => Response::Terminated(proto::TerminatedResponse {
            session_json: to_json_vec(session)?,
        }),
        DaemonResponse::Interrupted { session } => {
            Response::Interrupted(proto::InterruptedResponse {
                session_json: to_json_vec(session)?,
            })
        }
        DaemonResponse::AdoptKillAck { accepted } => {
            Response::AdoptKillAck(proto::AdoptKillAckResponse {
                accepted: *accepted as u32,
            })
        }
        DaemonResponse::Gc { reply } => Response::Gc(proto::GcResponse {
            reply_json: to_json_vec(reply)?,
        }),
        DaemonResponse::ShutdownAck { pid } => {
            Response::ShutdownAck(proto::ShutdownAckResponse { pid: *pid })
        }
        DaemonResponse::Error { message } => Response::Error(proto::ErrorResponse {
            message: message.clone(),
        }),
    };
    Ok(proto::DaemonToClient {
        response: Some(response),
        request_id,
    })
}

fn daemon_response_from_proto(proto: proto::DaemonToClient) -> Result<DaemonResponse, WireError> {
    use proto::daemon_to_client::Response;
    match proto
        .response
        .ok_or(WireError::MissingPayload("daemon response"))?
    {
        Response::Created(created) => Ok(DaemonResponse::Created {
            session: from_json_slice::<SessionSnapshot>(&created.session_json)?,
        }),
        Response::Session(session) => Ok(DaemonResponse::Session {
            session: from_json_slice::<SessionSnapshot>(&session.session_json)?,
        }),
        Response::LiveCwds(live) => Ok(DaemonResponse::LiveCwds {
            paths: live.paths.into_iter().map(PathBuf::from).collect(),
        }),
        Response::Terminated(terminated) => Ok(DaemonResponse::Terminated {
            session: from_json_slice::<SessionSnapshot>(&terminated.session_json)?,
        }),
        Response::Interrupted(interrupted) => Ok(DaemonResponse::Interrupted {
            session: from_json_slice::<SessionSnapshot>(&interrupted.session_json)?,
        }),
        Response::AdoptKillAck(ack) => Ok(DaemonResponse::AdoptKillAck {
            accepted: ack.accepted as usize,
        }),
        Response::Gc(gc) => Ok(DaemonResponse::Gc {
            reply: from_json_slice::<GcReply>(&gc.reply_json)?,
        }),
        Response::ShutdownAck(ack) => Ok(DaemonResponse::ShutdownAck { pid: ack.pid }),
        Response::Error(error) => Ok(DaemonResponse::Error {
            message: error.message,
        }),
    }
}

fn worker_client_to_proto(
    message: &WorkerClientMessage,
) -> Result<proto::WorkerClientEnvelope, WireError> {
    use proto::worker_client_envelope::Message;
    let message = match message {
        WorkerClientMessage::Attach {
            terminal,
            rows,
            cols,
        } => Message::Attach(proto::WorkerAttachRequest {
            terminal_json: terminal.as_ref().map(to_json_vec).transpose()?,
            rows: rows.map(u32::from),
            cols: cols.map(u32::from),
        }),
        WorkerClientMessage::Input { data_b64, submit } => {
            Message::Input(proto::WorkerInputRequest {
                data_b64: data_b64.clone(),
                submit: *submit,
            })
        }
        WorkerClientMessage::Resize { rows, cols } => Message::Resize(proto::WorkerResizeRequest {
            rows: u32::from(*rows),
            cols: u32::from(*cols),
        }),
        WorkerClientMessage::Interrupt { profile } => {
            Message::Interrupt(proto::WorkerInterruptRequest {
                profile: profile.as_ref().map(profile_to_proto),
            })
        }
    };
    Ok(proto::WorkerClientEnvelope {
        message: Some(message),
    })
}

fn worker_client_from_proto(
    proto: proto::WorkerClientEnvelope,
) -> Result<WorkerClientMessage, WireError> {
    use proto::worker_client_envelope::Message;
    match proto
        .message
        .ok_or(WireError::MissingPayload("worker client message"))?
    {
        Message::Attach(attach) => Ok(WorkerClientMessage::Attach {
            terminal: attach
                .terminal_json
                .as_deref()
                .map(from_json_slice)
                .transpose()?,
            rows: attach
                .rows
                .map(|value| u16_field("attach.rows", value))
                .transpose()?,
            cols: attach
                .cols
                .map(|value| u16_field("attach.cols", value))
                .transpose()?,
        }),
        Message::Input(input) => Ok(WorkerClientMessage::Input {
            data_b64: input.data_b64,
            submit: input.submit,
        }),
        Message::Resize(resize) => Ok(WorkerClientMessage::Resize {
            rows: u16_field("resize.rows", resize.rows)?,
            cols: u16_field("resize.cols", resize.cols)?,
        }),
        Message::Interrupt(interrupt) => Ok(WorkerClientMessage::Interrupt {
            profile: interrupt.profile.map(profile_from_proto),
        }),
    }
}

fn worker_server_to_proto(
    message: &WorkerServerMessage,
) -> Result<proto::WorkerServerEnvelope, WireError> {
    use proto::worker_server_envelope::Message;
    let message = match message {
        WorkerServerMessage::Attached { session } => {
            Message::Attached(proto::WorkerAttachedResponse {
                session_json: to_json_vec(session.as_ref())?,
            })
        }
        WorkerServerMessage::Output { data_b64 } => Message::Output(proto::WorkerOutputResponse {
            data_b64: data_b64.clone(),
        }),
        WorkerServerMessage::Exited { exit_code } => Message::Exited(proto::WorkerExitedResponse {
            exit_code: *exit_code,
        }),
        WorkerServerMessage::Error { message } => Message::Error(proto::WorkerErrorResponse {
            message: message.clone(),
        }),
    };
    Ok(proto::WorkerServerEnvelope {
        message: Some(message),
    })
}

fn worker_server_from_proto(
    proto: proto::WorkerServerEnvelope,
) -> Result<WorkerServerMessage, WireError> {
    use proto::worker_server_envelope::Message;
    match proto
        .message
        .ok_or(WireError::MissingPayload("worker server message"))?
    {
        Message::Attached(attached) => Ok(WorkerServerMessage::Attached {
            session: Box::new(from_json_slice::<SessionSnapshot>(&attached.session_json)?),
        }),
        Message::Output(output) => Ok(WorkerServerMessage::Output {
            data_b64: output.data_b64,
        }),
        Message::Exited(exited) => Ok(WorkerServerMessage::Exited {
            exit_code: exited.exit_code,
        }),
        Message::Error(error) => Ok(WorkerServerMessage::Error {
            message: error.message,
        }),
    }
}

fn profile_to_proto(profile: &CtrlCProfile) -> proto::CtrlCProfile {
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

fn profile_from_proto(profile: proto::CtrlCProfile) -> CtrlCProfile {
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

fn to_json_vec<T: Serialize>(value: &T) -> Result<Vec<u8>, WireError> {
    serde_json::to_vec(value).map_err(WireError::Json)
}

fn from_json_slice<T: DeserializeOwned>(bytes: &[u8]) -> Result<T, WireError> {
    serde_json::from_slice(bytes).map_err(WireError::Json)
}

fn u16_field(field: &'static str, value: u32) -> Result<u16, WireError> {
    value
        .try_into()
        .map_err(|_| WireError::U16OutOfRange { field, value })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::backend::{Backend, LaunchMode};
    use crate::command::LaunchPlan;
    use crate::daemon::types::SessionKind;
    use crate::graphics::GraphicsConfig;

    fn sample_launch_spec() -> WorkerLaunchSpec {
        WorkerLaunchSpec {
            plan: LaunchPlan {
                command: vec!["codex".to_string(), "exec".to_string()],
                iterations: 1,
                backend: Backend::Codex,
                launch_mode: LaunchMode::Subprocess,
                cwd: Some("C:/work/repo".to_string()),
                graphics: GraphicsConfig::default(),
                repeat_schedule: None,
                task_summary: Some("wire test".to_string()),
                loop_markers: None,
                stream_json_progress: false,
            },
            kind: SessionKind::Subprocess,
            name: Some("sample".to_string()),
            detachable: true,
            background_on_launch: false,
            attachable: true,
            rows: 24,
            cols: 80,
            repeat_interval_secs: None,
            repeat_run_command: None,
            backlog_bytes: Some(256 * 1024),
            transcript_path: None,
        }
    }

    fn sample_snapshot() -> SessionSnapshot {
        SessionSnapshot {
            id: "sess-test".to_string(),
            kind: SessionKind::Subprocess,
            cwd: Some("C:/work/repo".to_string()),
            name: Some("sample".to_string()),
            created_at: Some(42),
            detachable: true,
            background: true,
            attachable: true,
            repeat_interval_secs: None,
            repeat_next_run_at: None,
            repeat_running: false,
            daemon_pid: 100,
            worker_pid: 101,
            worker_port: 9020,
            root_pid: Some(102),
            exit_code: None,
            ctrl_c: Some(sample_profile()),
        }
    }

    fn sample_profile() -> CtrlCProfile {
        CtrlCProfile {
            cli_pid: Some(10),
            cli_observed_at_ms: Some(20),
            cli_handoff_at_ms: Some(30),
            cli_return_ready_at_ms: Some(31),
            cli_handoff_ms: Some(1),
            daemon_received_at_ms: Some(35),
            daemon_kill_started_at_ms: Some(36),
            daemon_kill_finished_at_ms: Some(37),
            daemon_kill_ms: Some(1),
            fast_path: true,
        }
    }

    fn assert_json_parity<T>(original: &T, decoded: &T)
    where
        T: Serialize,
    {
        let original_json = serde_json::to_value(original).unwrap();
        let decoded_json = serde_json::to_value(decoded).unwrap();
        assert_eq!(decoded_json, original_json);
    }

    #[test]
    fn payload_protocol_constants_are_ascii_discriminators() {
        assert_eq!(CLUD_PROST_PAYLOAD_PROTOCOL.to_be_bytes(), *b"CLUD");
        assert_eq!(CLUD_JSON_PAYLOAD_PROTOCOL.to_be_bytes(), *b"CLJS");
    }

    #[test]
    fn daemon_wire_format_env_values_default_to_json() {
        assert_eq!(
            DaemonWireFormat::from_env_value(None).unwrap(),
            DaemonWireFormat::Json
        );
        assert_eq!(
            DaemonWireFormat::from_env_value(Some("")).unwrap(),
            DaemonWireFormat::Json
        );
        assert_eq!(
            DaemonWireFormat::from_env_value(Some("legacy-json")).unwrap(),
            DaemonWireFormat::Json
        );
        assert_eq!(
            DaemonWireFormat::from_env_value(Some("prost")).unwrap(),
            DaemonWireFormat::Prost
        );
        assert!(matches!(
            DaemonWireFormat::from_env_value(Some("bincode")),
            Err(WireError::InvalidDaemonWire(_))
        ));
    }

    #[test]
    fn daemon_request_line_json_preserves_legacy_shape() {
        let request = DaemonRequest::Shutdown;
        let line = encode_daemon_request_line(&request, DaemonWireFormat::Json).unwrap();
        assert!(line.starts_with(br#"{"op":"shutdown"}"#));

        let line = String::from_utf8(line).unwrap();
        let (decoded, format) = decode_daemon_request_line(&line).unwrap();
        assert_eq!(format, DaemonWireFormat::Json);
        assert_json_parity(&request, &decoded);
    }

    #[test]
    fn daemon_request_line_prost_carries_frame_envelope() {
        let request = DaemonRequest::Shutdown;
        let line = encode_daemon_request_line(&request, DaemonWireFormat::Prost).unwrap();
        let line = String::from_utf8(line).unwrap();
        assert!(line.starts_with("CLUD-FRAME/1 434c5544 "));

        let (decoded, format) = decode_daemon_request_line(&line).unwrap();
        assert_eq!(format, DaemonWireFormat::Prost);
        assert_json_parity(&request, &decoded);
    }

    #[test]
    fn daemon_response_line_prost_roundtrips() {
        let response = DaemonResponse::ShutdownAck { pid: 1234 };
        let line = encode_daemon_response_line(&response, DaemonWireFormat::Prost).unwrap();
        let decoded = decode_daemon_response_line(&String::from_utf8(line).unwrap()).unwrap();
        assert_json_parity(&response, &decoded);
    }

    #[test]
    fn daemon_request_prost_roundtrips_json_shapes() {
        let cases = vec![
            DaemonRequest::Create {
                spec: Box::new(sample_launch_spec()),
            },
            DaemonRequest::Session {
                session_id: "sess-test".to_string(),
            },
            DaemonRequest::ListLiveCwds,
            DaemonRequest::Terminate {
                session_id: "sess-test".to_string(),
            },
            DaemonRequest::Interrupt {
                session_id: "sess-test".to_string(),
                profile: sample_profile(),
            },
            DaemonRequest::AdoptKill {
                pids: vec![1, 2],
                reason: Some("ctrl_c_handoff".to_string()),
            },
            DaemonRequest::Gc {
                payload: GcOp::List {
                    kind: Some("worktree".to_string()),
                },
            },
            DaemonRequest::Shutdown,
        ];

        for request in cases {
            let frame = encode_daemon_request_prost(&request, "req-1").unwrap();
            assert_eq!(frame.payload_protocol, CLUD_PROST_PAYLOAD_PROTOCOL);
            let decoded = decode_daemon_request(&frame).unwrap();
            assert_json_parity(&request, &decoded);
        }
    }

    #[test]
    fn daemon_response_prost_roundtrips_json_shapes() {
        let session = sample_snapshot();
        let cases = vec![
            DaemonResponse::Created {
                session: session.clone(),
            },
            DaemonResponse::Session {
                session: session.clone(),
            },
            DaemonResponse::LiveCwds {
                paths: vec![PathBuf::from("C:/work/repo"), PathBuf::from("D:/other")],
            },
            DaemonResponse::Terminated {
                session: session.clone(),
            },
            DaemonResponse::Interrupted { session },
            DaemonResponse::AdoptKillAck { accepted: 2 },
            DaemonResponse::Gc {
                reply: GcReply::ListOk { rows: Vec::new() },
            },
            DaemonResponse::ShutdownAck { pid: 1234 },
            DaemonResponse::Error {
                message: "failed".to_string(),
            },
        ];

        for response in cases {
            let frame = encode_daemon_response_prost(&response, "req-1").unwrap();
            assert_eq!(frame.payload_protocol, CLUD_PROST_PAYLOAD_PROTOCOL);
            let decoded = decode_daemon_response(&frame).unwrap();
            assert_json_parity(&response, &decoded);
        }
    }

    #[test]
    fn worker_messages_prost_roundtrip_json_shapes() {
        let client_cases = vec![
            WorkerClientMessage::Attach {
                terminal: None,
                rows: Some(24),
                cols: Some(80),
            },
            WorkerClientMessage::Input {
                data_b64: "YWJj".to_string(),
                submit: true,
            },
            WorkerClientMessage::Resize {
                rows: 30,
                cols: 120,
            },
            WorkerClientMessage::Interrupt {
                profile: Some(sample_profile()),
            },
        ];

        for message in client_cases {
            let frame = encode_worker_client_prost(&message).unwrap();
            let decoded = decode_worker_client(&frame).unwrap();
            assert_json_parity(&message, &decoded);
        }

        let server_cases = vec![
            WorkerServerMessage::Attached {
                session: Box::new(sample_snapshot()),
            },
            WorkerServerMessage::Output {
                data_b64: "YWJj".to_string(),
            },
            WorkerServerMessage::Exited { exit_code: 130 },
            WorkerServerMessage::Error {
                message: "bad attach".to_string(),
            },
        ];

        for message in server_cases {
            let frame = encode_worker_server_prost(&message).unwrap();
            let decoded = decode_worker_server(&frame).unwrap();
            assert_json_parity(&message, &decoded);
        }
    }

    #[test]
    fn dispatcher_accepts_legacy_json_frames() {
        let request = DaemonRequest::Shutdown;
        let frame = encode_legacy_json_frame(&request).unwrap();
        let decoded = decode_daemon_request(&frame).unwrap();
        assert_json_parity(&request, &decoded);
    }

    #[test]
    fn dispatcher_rejects_unknown_payload_protocol() {
        let err = decode_daemon_request(&WireFrame {
            payload_protocol: 0xFEED_BEEF,
            payload: Vec::new(),
        })
        .unwrap_err();
        assert!(matches!(
            err,
            WireError::UnknownPayloadProtocol(0xFEED_BEEF)
        ));
    }
}
