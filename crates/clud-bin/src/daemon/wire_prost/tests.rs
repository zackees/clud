use super::*;
use crate::backend::{Backend, LaunchMode};
use crate::command::LaunchPlan;
use crate::daemon::types::SessionKind;
use crate::graphics::GraphicsConfig;
use serde::Serialize;

use super::session::to_json_vec;

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
fn daemon_wire_format_env_values_default_to_prost() {
    assert_eq!(
        DaemonWireFormat::from_env_value(None).unwrap(),
        DaemonWireFormat::Prost
    );
    assert_eq!(
        DaemonWireFormat::from_env_value(Some("")).unwrap(),
        DaemonWireFormat::Prost
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

/// Mirror retired (zackees/running-process#385 item 3): encoders
/// emit ONLY the typed snapshot; `session_json` must stay empty.
#[test]
fn daemon_session_responses_encode_typed_snapshot_without_json_mirror() {
    let session = sample_snapshot();
    let cases = vec![
        DaemonResponse::Created {
            session: session.clone(),
        },
        DaemonResponse::Session {
            session: session.clone(),
        },
        DaemonResponse::Terminated {
            session: session.clone(),
        },
        DaemonResponse::Interrupted { session },
    ];

    for response in cases {
        let frame = encode_daemon_response_prost(&response, "req-typed").unwrap();
        let envelope = proto::DaemonToClient::decode(frame.payload.as_slice()).unwrap();
        let response = envelope.response.unwrap();
        match response {
            proto::daemon_to_client::Response::Created(created) => {
                assert!(created.session_json.is_empty(), "mirror is retired");
                assert_eq!(created.session.unwrap().id, "sess-test");
            }
            proto::daemon_to_client::Response::Session(session) => {
                assert!(session.session_json.is_empty(), "mirror is retired");
                assert_eq!(session.session.unwrap().id, "sess-test");
            }
            proto::daemon_to_client::Response::Terminated(terminated) => {
                assert!(terminated.session_json.is_empty(), "mirror is retired");
                assert_eq!(terminated.session.unwrap().id, "sess-test");
            }
            proto::daemon_to_client::Response::Interrupted(interrupted) => {
                assert!(interrupted.session_json.is_empty(), "mirror is retired");
                assert_eq!(interrupted.session.unwrap().id, "sess-test");
            }
            other => panic!("unexpected response payload: {other:?}"),
        }
    }
}

/// Mirror retired — see
/// [`daemon_session_responses_encode_typed_snapshot_without_json_mirror`].
#[test]
fn worker_attached_response_encodes_typed_snapshot_without_json_mirror() {
    let message = WorkerServerMessage::Attached {
        session: Box::new(sample_snapshot()),
    };
    let frame = encode_worker_server_prost(&message).unwrap();
    let envelope = proto::WorkerServerEnvelope::decode(frame.payload.as_slice()).unwrap();
    let proto::worker_server_envelope::Message::Attached(attached) = envelope.message.unwrap()
    else {
        panic!("expected attached worker payload");
    };
    assert!(attached.session_json.is_empty(), "mirror is retired");
    assert_eq!(attached.session.unwrap().id, "sess-test");
}

#[test]
fn daemon_session_responses_decode_legacy_json_only_snapshots() {
    let session = sample_snapshot();
    let session_json = to_json_vec(&session).unwrap();
    let cases = vec![
        proto::daemon_to_client::Response::Created(proto::CreatedResponse {
            session_json: session_json.clone(),
            session: None,
        }),
        proto::daemon_to_client::Response::Session(proto::SessionResponse {
            session_json: session_json.clone(),
            session: None,
        }),
        proto::daemon_to_client::Response::Terminated(proto::TerminatedResponse {
            session_json: session_json.clone(),
            session: None,
        }),
        proto::daemon_to_client::Response::Interrupted(proto::InterruptedResponse {
            session_json,
            session: None,
        }),
    ];

    for response in cases {
        let frame = prost_frame(
            proto::DaemonToClient {
                response: Some(response),
                request_id: "legacy-json-only".to_string(),
            }
            .encode_to_vec(),
        );
        let decoded = decode_daemon_response(&frame).unwrap();
        match decoded {
            DaemonResponse::Created { session }
            | DaemonResponse::Session { session }
            | DaemonResponse::Terminated { session }
            | DaemonResponse::Interrupted { session } => {
                assert_eq!(session.id, "sess-test");
                assert_eq!(session.worker_port, 9020);
            }
            other => panic!("unexpected decoded response: {other:?}"),
        }
    }
}

#[test]
fn worker_attached_response_decodes_legacy_json_only_snapshot() {
    let frame = prost_frame(
        proto::WorkerServerEnvelope {
            message: Some(proto::worker_server_envelope::Message::Attached(
                proto::WorkerAttachedResponse {
                    session_json: to_json_vec(&sample_snapshot()).unwrap(),
                    session: None,
                },
            )),
        }
        .encode_to_vec(),
    );
    let decoded = decode_worker_server(&frame).unwrap();
    let WorkerServerMessage::Attached { session } = decoded else {
        panic!("expected attached worker payload");
    };
    assert_eq!(session.id, "sess-test");
    assert_eq!(session.worker_port, 9020);
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
fn worker_line_dispatch_preserves_json_and_prost_formats() {
    let attach = WorkerClientMessage::Attach {
        terminal: None,
        rows: Some(24),
        cols: Some(80),
    };
    let json_line = encode_worker_client_line(&attach, DaemonWireFormat::Json).unwrap();
    assert!(json_line.starts_with(br#"{"op":"attach""#));
    let (decoded_attach, format) =
        decode_worker_client_line(&String::from_utf8(json_line).unwrap()).unwrap();
    assert_eq!(format, DaemonWireFormat::Json);
    assert_json_parity(&attach, &decoded_attach);

    let attached = WorkerServerMessage::Attached {
        session: Box::new(sample_snapshot()),
    };
    let prost_line = encode_worker_server_line(&attached, DaemonWireFormat::Prost).unwrap();
    assert!(prost_line.starts_with(b"CLUD-FRAME/1 434c5544 "));
    let decoded_attached =
        decode_worker_server_line(&String::from_utf8(prost_line).unwrap()).unwrap();
    assert_json_parity(&attached, &decoded_attached);
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
