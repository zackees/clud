use std::path::PathBuf;

use prost::Message;

use super::frame::prost_frame;
use super::proto;
use super::session::{
    from_json_slice, profile_from_proto, profile_to_proto, session_from_proto_or_json,
    session_to_proto, to_json_vec,
};
use super::{WireError, WireFrame, CLUD_JSON_PAYLOAD_PROTOCOL, CLUD_PROST_PAYLOAD_PROTOCOL};
use crate::daemon::types::{DaemonRequest, DaemonResponse, GcOp, GcReply, WorkerLaunchSpec};

pub(in crate::daemon) fn encode_daemon_request_prost(
    request: &DaemonRequest,
    request_id: impl Into<String>,
) -> Result<WireFrame, WireError> {
    let payload = daemon_request_to_proto(request, request_id.into())?.encode_to_vec();
    Ok(prost_frame(payload))
}

pub(in crate::daemon) fn decode_daemon_request(
    frame: &WireFrame,
) -> Result<DaemonRequest, WireError> {
    match frame.payload_protocol {
        CLUD_PROST_PAYLOAD_PROTOCOL => {
            let proto = proto::ClientToDaemon::decode(frame.payload.as_slice())?;
            daemon_request_from_proto(proto)
        }
        CLUD_JSON_PAYLOAD_PROTOCOL => Ok(serde_json::from_slice(&frame.payload)?),
        other => Err(WireError::UnknownPayloadProtocol(other)),
    }
}

pub(in crate::daemon) fn encode_daemon_response_prost(
    response: &DaemonResponse,
    request_id: impl Into<String>,
) -> Result<WireFrame, WireError> {
    let payload = daemon_response_to_proto(response, request_id.into())?.encode_to_vec();
    Ok(prost_frame(payload))
}

pub(in crate::daemon) fn decode_daemon_response(
    frame: &WireFrame,
) -> Result<DaemonResponse, WireError> {
    match frame.payload_protocol {
        CLUD_PROST_PAYLOAD_PROTOCOL => {
            let proto = proto::DaemonToClient::decode(frame.payload.as_slice())?;
            daemon_response_from_proto(proto)
        }
        CLUD_JSON_PAYLOAD_PROTOCOL => Ok(serde_json::from_slice(&frame.payload)?),
        other => Err(WireError::UnknownPayloadProtocol(other)),
    }
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
        DaemonRequest::ReapOrphans => Request::ReapOrphans(proto::ReapOrphansRequest {}),
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
        Request::ReapOrphans(_) => Ok(DaemonRequest::ReapOrphans),
    }
}

fn daemon_response_to_proto(
    response: &DaemonResponse,
    request_id: String,
) -> Result<proto::DaemonToClient, WireError> {
    use proto::daemon_to_client::Response;
    // `session_json` mirror RETIRED (zackees/running-process#385 item 3):
    // encoders emit only the typed `SessionSnapshot`. No published clud
    // ever shipped a decoder that required the mirror (the prost wire
    // itself postdates the last JSON-only release), and `ensure_daemon`
    // (#192) restarts version-mismatched daemons, so client and daemon
    // are always the same build. Decoders keep the JSON fallback in
    // `session_from_proto_or_json` as a defensive measure only.
    let response = match response {
        DaemonResponse::Created { session } => Response::Created(proto::CreatedResponse {
            session_json: Vec::new(),
            session: Some(session_to_proto(session)),
        }),
        DaemonResponse::Session { session } => Response::Session(proto::SessionResponse {
            session_json: Vec::new(),
            session: Some(session_to_proto(session)),
        }),
        DaemonResponse::LiveCwds { paths } => Response::LiveCwds(proto::LiveCwdsResponse {
            paths: paths
                .iter()
                .map(|path| path.to_string_lossy().to_string())
                .collect(),
        }),
        DaemonResponse::Terminated { session } => Response::Terminated(proto::TerminatedResponse {
            session_json: Vec::new(),
            session: Some(session_to_proto(session)),
        }),
        DaemonResponse::Interrupted { session } => {
            Response::Interrupted(proto::InterruptedResponse {
                session_json: Vec::new(),
                session: Some(session_to_proto(session)),
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
        DaemonResponse::ReapOrphansAck { found, reaped } => {
            Response::ReapOrphansAck(proto::ReapOrphansAckResponse {
                found: *found,
                reaped: *reaped,
            })
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
            session: session_from_proto_or_json(created.session, &created.session_json)?,
        }),
        Response::Session(session) => Ok(DaemonResponse::Session {
            session: session_from_proto_or_json(session.session, &session.session_json)?,
        }),
        Response::LiveCwds(live) => Ok(DaemonResponse::LiveCwds {
            paths: live.paths.into_iter().map(PathBuf::from).collect(),
        }),
        Response::Terminated(terminated) => Ok(DaemonResponse::Terminated {
            session: session_from_proto_or_json(terminated.session, &terminated.session_json)?,
        }),
        Response::Interrupted(interrupted) => Ok(DaemonResponse::Interrupted {
            session: session_from_proto_or_json(interrupted.session, &interrupted.session_json)?,
        }),
        Response::AdoptKillAck(ack) => Ok(DaemonResponse::AdoptKillAck {
            accepted: ack.accepted as usize,
        }),
        Response::Gc(gc) => Ok(DaemonResponse::Gc {
            reply: from_json_slice::<GcReply>(&gc.reply_json)?,
        }),
        Response::ShutdownAck(ack) => Ok(DaemonResponse::ShutdownAck { pid: ack.pid }),
        Response::ReapOrphansAck(ack) => Ok(DaemonResponse::ReapOrphansAck {
            found: ack.found,
            reaped: ack.reaped,
        }),
        Response::Error(error) => Ok(DaemonResponse::Error {
            message: error.message,
        }),
    }
}
