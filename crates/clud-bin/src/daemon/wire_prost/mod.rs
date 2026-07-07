#![allow(dead_code)]

mod daemon_msgs;
mod error;
mod frame;
mod session;
mod worker_msgs;

use super::types::{DaemonRequest, DaemonResponse, WorkerClientMessage, WorkerServerMessage};

pub(in crate::daemon) use daemon_msgs::{
    decode_daemon_request, decode_daemon_response, encode_daemon_request_prost,
    encode_daemon_response_prost,
};
pub(in crate::daemon) use error::WireError;
pub(in crate::daemon) use frame::WireFrame;
pub(in crate::daemon) use worker_msgs::{
    decode_worker_client, decode_worker_server, encode_worker_client_prost,
    encode_worker_server_prost,
};

use frame::{decode_wire_frame_line, encode_json_line, encode_wire_frame_line};
#[cfg(test)]
use frame::{encode_legacy_json_frame, prost_frame};

#[cfg(test)]
use super::types::{
    CtrlCProfile, GcOp, GcReply, ProcRow, ProcTier, ProcTreeSnapshot, SessionSnapshot,
    WorkerLaunchSpec,
};
#[cfg(test)]
use prost::Message;
#[cfg(test)]
use std::path::PathBuf;

#[allow(missing_docs)]
pub(super) mod proto {
    include!(concat!(env!("OUT_DIR"), "/clud.v1.rs"));
}

/// ASCII "CLUD" in a u32. This is the clud prost payload discriminator used
/// inside the future running-process v1 Frame.payload_protocol field.
pub(in crate::daemon) const CLUD_PROST_PAYLOAD_PROTOCOL: u32 = 0x434c_5544;
/// ASCII "CLJS" in a u32. This names the legacy JSON payload path while the
/// migration runs with both encoders available.
pub(in crate::daemon) const CLUD_JSON_PAYLOAD_PROTOCOL: u32 = 0x434c_4a53;
/// Selects the daemon RPC line format. Unset or empty defaults to the v1
/// prost frame envelope; `json` keeps the legacy JSON line protocol available.
pub(in crate::daemon) const ENV_DAEMON_WIRE: &str = "CLUD_DAEMON_WIRE";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(in crate::daemon) enum DaemonWireFormat {
    Json,
    Prost,
}

impl DaemonWireFormat {
    fn from_env_value(value: Option<&str>) -> Result<Self, WireError> {
        let Some(raw) = value else {
            return Ok(Self::Prost);
        };
        let trimmed = raw.trim();
        if trimmed.is_empty() {
            return Ok(Self::Prost);
        }
        match trimmed.to_ascii_lowercase().as_str() {
            "json" | "legacy" | "legacy-json" => Ok(Self::Json),
            "prost" => Ok(Self::Prost),
            _ => Err(WireError::InvalidDaemonWire(raw.to_string())),
        }
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

pub(super) fn encode_worker_client_line(
    message: &WorkerClientMessage,
    format: DaemonWireFormat,
) -> Result<Vec<u8>, WireError> {
    match format {
        DaemonWireFormat::Json => encode_json_line(message),
        DaemonWireFormat::Prost => {
            let frame = encode_worker_client_prost(message)?;
            Ok(encode_wire_frame_line(&frame))
        }
    }
}

pub(super) fn decode_worker_client_line(
    line: &str,
) -> Result<(WorkerClientMessage, DaemonWireFormat), WireError> {
    if let Some(frame) = decode_wire_frame_line(line)? {
        return Ok((decode_worker_client(&frame)?, DaemonWireFormat::Prost));
    }
    Ok((serde_json::from_str(line)?, DaemonWireFormat::Json))
}

pub(super) fn encode_worker_server_line(
    message: &WorkerServerMessage,
    format: DaemonWireFormat,
) -> Result<Vec<u8>, WireError> {
    match format {
        DaemonWireFormat::Json => encode_json_line(message),
        DaemonWireFormat::Prost => {
            let frame = encode_worker_server_prost(message)?;
            Ok(encode_wire_frame_line(&frame))
        }
    }
}

pub(super) fn decode_worker_server_line(line: &str) -> Result<WorkerServerMessage, WireError> {
    if let Some(frame) = decode_wire_frame_line(line)? {
        return decode_worker_server(&frame);
    }
    Ok(serde_json::from_str(line)?)
}

#[cfg(test)]
mod tests;
