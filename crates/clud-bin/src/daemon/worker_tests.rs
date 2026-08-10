use super::*;
use crate::backend::{
    Backend, HarnessSelection, LaunchMode, ModelProvider, PreferenceSource, RoutingMode,
};
use crate::command::LaunchPlan;
use crate::daemon::wire_prost::{
    decode_worker_server_line, encode_worker_client_line, DaemonWireFormat,
};
use std::io::BufRead;
use std::sync::Arc;
use std::time::Instant;
use tempfile::TempDir;

fn cross_route_plan() -> LaunchPlan {
    LaunchPlan {
        command: vec!["claude".to_string()],
        iterations: 1,
        backend: Backend::Claude,
        routing_mode: RoutingMode::Direct,
        model_provider: Some(ModelProvider::Codex),
        requested_harness: Some(HarnessSelection::Claude),
        effective_harness: Some(Backend::Claude),
        provider_source: Some(PreferenceSource::Cli),
        harness_source: Some(PreferenceSource::Cli),
        launch_mode: LaunchMode::Subprocess,
        cwd: None,
        graphics: GraphicsConfig::default(),
        repeat_schedule: None,
        task_summary: None,
        loop_markers: None,
        stream_json_progress: false,
        codex_model: None,
        model_selection: None,
    }
}

#[test]
fn worker_cross_route_runtime_exposes_bridge_only_to_the_child_environment() {
    let runtime = start_worker_runtime(&cross_route_plan()).unwrap();
    let env = runtime.env();
    assert!(env.iter().any(|(key, _)| key == "ANTHROPIC_BASE_URL"));
    assert!(env.iter().any(|(key, _)| key == "ANTHROPIC_AUTH_TOKEN"));
    assert!(!env.iter().any(|(key, _)| key == "ANTHROPIC_API_KEY"));
}

fn test_shared(tmp: &TempDir) -> Arc<WorkerShared> {
    let snapshot = SessionSnapshot {
        id: "worker-wire-test".to_string(),
        kind: SessionKind::Subprocess,
        backend: None,
        launch_mode: None,
        repo_root: None,
        command: Vec::new(),
        cwd: None,
        name: Some("worker wire test".to_string()),
        created_at: Some(1),
        detachable: true,
        background: true,
        attachable: true,
        repeat_interval_secs: None,
        repeat_next_run_at: None,
        repeat_running: false,
        daemon_pid: std::process::id(),
        worker_pid: std::process::id(),
        worker_port: 0,
        root_pid: None,
        daemon_pid_start: 0,
        worker_pid_start: 0,
        root_pid_start: 0,
        exit_code: None,
        exited_at: None,
        ctrl_c: None,
    };
    Arc::new(WorkerShared::new(
        tmp.path().to_path_buf(),
        "worker-wire-test".to_string(),
        snapshot,
    ))
}

fn test_runtime() -> SessionRuntime {
    SessionRuntime::Subprocess(Arc::new(NativeProcess::new(ProcessConfig {
        command: subprocess::command_spec_for_subprocess(vec![
            "__clud_unstarted_test_process__".to_string()
        ]),
        cwd: None,
        env: None,
        capture: false,
        stderr_mode: StderrMode::Stdout,
        creationflags: None,
        create_process_group: false,
        stdin_mode: StdinMode::Null,
        nice: None,
    })))
}

fn attach_message() -> WorkerClientMessage {
    WorkerClientMessage::Attach {
        terminal: None,
        rows: Some(24),
        cols: Some(80),
    }
}

fn write_client_message(
    stream: &mut TcpStream,
    message: &WorkerClientMessage,
    format: DaemonWireFormat,
) {
    let bytes = encode_worker_client_line(message, format).unwrap();
    stream.write_all(&bytes).unwrap();
    stream.flush().unwrap();
}

fn spawn_worker_handler(
    shared: Arc<WorkerShared>,
) -> (TcpStream, thread::JoinHandle<io::Result<()>>) {
    let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
    let port = listener.local_addr().unwrap().port();
    let handle = thread::spawn(move || {
        let (stream, _) = listener.accept().unwrap();
        handle_worker_client(
            stream,
            &shared,
            &test_runtime(),
            &GraphicsConfig::default(),
            SessionKind::Subprocess,
            24,
            80,
        )
    });
    let stream = TcpStream::connect(("127.0.0.1", port)).unwrap();
    stream
        .set_read_timeout(Some(Duration::from_secs(2)))
        .unwrap();
    (stream, handle)
}

fn read_server_message(reader: &mut BufReader<TcpStream>) -> (String, WorkerServerMessage) {
    let mut line = String::new();
    reader.read_line(&mut line).unwrap();
    let message = decode_worker_server_line(&line).unwrap();
    (line, message)
}

#[test]
fn worker_attach_live_path_preserves_json_wire() {
    let tmp = TempDir::new().unwrap();
    let shared = test_shared(&tmp);
    let (mut stream, handle) = spawn_worker_handler(shared);

    write_client_message(&mut stream, &attach_message(), DaemonWireFormat::Json);
    let mut reader = BufReader::new(stream.try_clone().unwrap());
    let (line, message) = read_server_message(&mut reader);

    assert!(line.starts_with(r#"{"op":"attached""#), "{line:?}");
    assert!(matches!(message, WorkerServerMessage::Attached { .. }));
    drop(reader);
    drop(stream);
    assert!(handle.join().unwrap().is_ok());
}

#[test]
fn worker_attach_live_path_accepts_prost_messages() {
    let tmp = TempDir::new().unwrap();
    let shared = test_shared(&tmp);
    let (mut stream, handle) = spawn_worker_handler(Arc::clone(&shared));

    write_client_message(&mut stream, &attach_message(), DaemonWireFormat::Prost);
    let mut reader = BufReader::new(stream.try_clone().unwrap());
    let (line, message) = read_server_message(&mut reader);

    assert!(line.starts_with("CLUD-FRAME/1 434c5544 "), "{line:?}");
    assert!(matches!(message, WorkerServerMessage::Attached { .. }));

    write_client_message(
        &mut stream,
        &WorkerClientMessage::Input {
            data_b64: base64::engine::general_purpose::STANDARD.encode(b"abc"),
            submit: false,
        },
        DaemonWireFormat::Prost,
    );
    write_client_message(
        &mut stream,
        &WorkerClientMessage::Resize {
            rows: 30,
            cols: 100,
        },
        DaemonWireFormat::Prost,
    );
    write_client_message(
        &mut stream,
        &WorkerClientMessage::Interrupt {
            profile: Some(CtrlCProfile {
                cli_pid: Some(77),
                fast_path: true,
                ..CtrlCProfile::default()
            }),
        },
        DaemonWireFormat::Prost,
    );

    let deadline = Instant::now() + Duration::from_secs(2);
    loop {
        let snapshot = shared.snapshot();
        if snapshot
            .ctrl_c
            .as_ref()
            .is_some_and(|profile| profile.cli_pid == Some(77) && profile.fast_path)
        {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "prost interrupt frame was not decoded by the worker attach loop"
        );
        thread::sleep(Duration::from_millis(25));
    }

    drop(reader);
    drop(stream);
    assert!(handle.join().unwrap().is_ok());
}
