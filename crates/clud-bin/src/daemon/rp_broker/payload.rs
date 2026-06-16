use std::collections::HashMap;
use std::path::Path;
use std::sync::{mpsc, Arc, Mutex};

use running_process::broker::protocol::Frame;
use running_process::NativeProcess;

use super::super::gc_service::RegistryMsg;
use super::super::server::dispatch_daemon_request;
use super::super::types::DaemonResponse;
use super::super::wire_prost::{
    decode_daemon_request, encode_daemon_response_prost, WireFrame, CLUD_PROST_PAYLOAD_PROTOCOL,
};

pub(super) struct PayloadAnswer {
    pub(super) payload: Vec<u8>,
    pub(super) is_shutdown: bool,
}

/// Decode one clud payload frame, dispatch it, and encode the response
/// payload (prost `DaemonToClient` bytes). Decode/encode failures
/// degrade to an in-band `DaemonResponse::Error` payload so the client
/// gets a correlated reply instead of a dropped connection.
pub(super) fn answer_payload_frame(
    frame: &Frame,
    state_dir: &Path,
    workers: &Arc<Mutex<HashMap<String, Arc<NativeProcess>>>>,
    gc_tx: Option<&mpsc::Sender<RegistryMsg>>,
) -> PayloadAnswer {
    let envelope_request_id = format!("rp-{}", frame.request_id);
    let request = decode_daemon_request(&WireFrame {
        payload_protocol: CLUD_PROST_PAYLOAD_PROTOCOL,
        payload: frame.payload.clone(),
    });
    let response = match request {
        Ok(request) => dispatch_daemon_request(state_dir, workers, gc_tx, request),
        Err(err) => DaemonResponse::Error {
            message: format!("malformed clud frame payload: {err}"),
        },
    };
    let is_shutdown = matches!(response, DaemonResponse::ShutdownAck { .. });
    let payload = encode_daemon_response_prost(&response, envelope_request_id.clone())
        .map(|wire_frame| wire_frame.payload)
        .unwrap_or_else(|err| {
            let fallback = DaemonResponse::Error {
                message: format!("failed to encode daemon response: {err}"),
            };
            encode_daemon_response_prost(&fallback, envelope_request_id)
                .map(|wire_frame| wire_frame.payload)
                .unwrap_or_default()
        });
    PayloadAnswer {
        payload,
        is_shutdown,
    }
}
