use prost::Message;

use super::frame::prost_frame;
use super::proto;
use super::session::{
    from_json_slice, profile_from_proto, profile_to_proto, session_from_proto_or_json,
    session_to_proto, to_json_vec, u16_field,
};
use super::{WireError, WireFrame, CLUD_JSON_PAYLOAD_PROTOCOL, CLUD_PROST_PAYLOAD_PROTOCOL};
use crate::daemon::types::{WorkerClientMessage, WorkerServerMessage};

pub(in crate::daemon) fn encode_worker_client_prost(
    message: &WorkerClientMessage,
) -> Result<WireFrame, WireError> {
    Ok(prost_frame(
        worker_client_to_proto(message)?.encode_to_vec(),
    ))
}

pub(in crate::daemon) fn decode_worker_client(
    frame: &WireFrame,
) -> Result<WorkerClientMessage, WireError> {
    match frame.payload_protocol {
        CLUD_PROST_PAYLOAD_PROTOCOL => {
            let proto = proto::WorkerClientEnvelope::decode(frame.payload.as_slice())?;
            worker_client_from_proto(proto)
        }
        CLUD_JSON_PAYLOAD_PROTOCOL => Ok(serde_json::from_slice(&frame.payload)?),
        other => Err(WireError::UnknownPayloadProtocol(other)),
    }
}

pub(in crate::daemon) fn encode_worker_server_prost(
    message: &WorkerServerMessage,
) -> Result<WireFrame, WireError> {
    Ok(prost_frame(
        worker_server_to_proto(message)?.encode_to_vec(),
    ))
}

pub(in crate::daemon) fn decode_worker_server(
    frame: &WireFrame,
) -> Result<WorkerServerMessage, WireError> {
    match frame.payload_protocol {
        CLUD_PROST_PAYLOAD_PROTOCOL => {
            let proto = proto::WorkerServerEnvelope::decode(frame.payload.as_slice())?;
            worker_server_from_proto(proto)
        }
        CLUD_JSON_PAYLOAD_PROTOCOL => Ok(serde_json::from_slice(&frame.payload)?),
        other => Err(WireError::UnknownPayloadProtocol(other)),
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
            // Mirror retired — see `daemon_response_to_proto`.
            Message::Attached(proto::WorkerAttachedResponse {
                session_json: Vec::new(),
                session: Some(session_to_proto(session.as_ref())),
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
            session: Box::new(session_from_proto_or_json(
                attached.session,
                &attached.session_json,
            )?),
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
