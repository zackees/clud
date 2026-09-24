use std::fs;
use std::io::Write;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

#[test]
fn version_probe_writes_smoke_marker() {
    let marker = std::env::temp_dir().join(format!(
        "mock-agent-version-{}-{}.txt",
        std::process::id(),
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock")
            .as_nanos()
    ));
    let output = Command::new(mock_agent_binary())
        .arg("--version")
        .env("CLUD_KITTY_SMOKE_VERSION_MARKER", &marker)
        .output()
        .expect("run mock version probe");
    assert!(output.status.success());
    assert_eq!(
        fs::read_to_string(&marker).expect("read version marker"),
        "mock-agent --version"
    );
    fs::remove_file(marker).expect("remove version marker");
}

#[test]
fn readiness_report_is_atomic_and_precedes_release_and_final_report() {
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("clock")
        .as_nanos();
    let dir = std::env::temp_dir().join(format!("mock-agent-ready-{}-{nonce}", std::process::id()));
    fs::create_dir(&dir).expect("create isolated test directory");
    let ready = dir.join("ready.json");
    let release = dir.join("release.txt");
    let report = dir.join("report.json");

    let mut child = Command::new(mock_agent_binary())
        .args([
            "--mock-ready-file",
            ready.to_str().expect("ready path"),
            "--mock-wait-for-file",
            release.to_str().expect("release path"),
            "--mock-report-file",
            report.to_str().expect("report path"),
            "--mock-exit-code",
            "23",
        ])
        .env("CLUD_KITTY_TERM", "1")
        .env("WEZTERM_PANE", "7")
        .env("WEZTERM_UNIX_SOCKET", "C:/test/gui-sock-42")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .spawn()
        .expect("spawn mock agent");

    let deadline = Instant::now() + Duration::from_secs(5);
    while !ready.is_file() && Instant::now() < deadline {
        assert!(
            child.try_wait().expect("poll child").is_none(),
            "child exited before ready"
        );
        thread::sleep(Duration::from_millis(10));
    }
    assert!(ready.is_file(), "mock agent did not write readiness report");
    let value: serde_json::Value =
        serde_json::from_slice(&fs::read(&ready).expect("read complete ready JSON"))
            .expect("parse complete ready JSON");
    assert_eq!(value["env"]["CLUD_KITTY_TERM"], "1");
    assert_eq!(value["env"]["WEZTERM_PANE"], "7");
    assert_eq!(value["env"]["WEZTERM_UNIX_SOCKET"], "C:/test/gui-sock-42");
    assert!(!report.exists(), "final report must wait for release");
    assert!(
        child.try_wait().expect("poll child").is_none(),
        "child exited before release"
    );

    fs::write(&release, "release").expect("release mock agent");
    assert_eq!(child.wait().expect("wait for child").code(), Some(23));
    assert!(report.is_file(), "final report missing after release");
    fs::remove_dir_all(&dir).expect("remove isolated test directory");
}

#[test]
fn ansi_after_wait_emits_only_after_release() {
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("clock")
        .as_nanos();
    let dir = std::env::temp_dir().join(format!("mock-agent-ansi-{}-{nonce}", std::process::id()));
    fs::create_dir(&dir).expect("create isolated test directory");
    let initial = dir.join("initial.bin");
    let after = dir.join("after.bin");
    let ready = dir.join("ready.json");
    let release = dir.join("release.txt");
    fs::write(&initial, b"MAIN\x1b[?1049hALT").expect("write initial script");
    fs::write(&after, b"\x1b[?1049lRESTORED").expect("write release script");

    let mut child = Command::new(mock_agent_binary())
        .args([
            "--mock-ansi-script",
            initial.to_str().expect("initial path"),
            "--mock-ansi-after-wait",
            after.to_str().expect("after path"),
            "--mock-ready-file",
            ready.to_str().expect("ready path"),
            "--mock-wait-for-file",
            release.to_str().expect("release path"),
        ])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .spawn()
        .expect("spawn mock agent");
    let deadline = Instant::now() + Duration::from_secs(5);
    while !ready.is_file() && Instant::now() < deadline {
        assert!(child.try_wait().expect("poll child").is_none());
        thread::sleep(Duration::from_millis(10));
    }
    assert!(ready.is_file(), "mock agent did not reach first ANSI stage");
    assert!(!release.exists());
    fs::write(&release, "release").expect("release mock agent");
    let output = child.wait_with_output().expect("collect output");
    assert!(output.status.success());
    assert!(
        output
            .stdout
            .starts_with(b"MAIN\x1b[?1049hALT\x1b[?1049lRESTORED"),
        "initial and release scripts must remain ordered before the JSON report"
    );
    fs::remove_dir_all(&dir).expect("remove isolated test directory");
}

#[test]
fn stdin_ready_marker_is_atomic_and_precedes_input_capture() {
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("clock")
        .as_nanos();
    let dir = std::env::temp_dir().join(format!("mock-agent-stdin-{}-{nonce}", std::process::id()));
    fs::create_dir(&dir).expect("create isolated test directory");
    let ready = dir.join("stdin-ready.json");
    let report = dir.join("report.json");
    let mut child = Command::new(mock_agent_binary())
        .args([
            "--mock-stdin-ready-file",
            ready.to_str().expect("ready path"),
            "--mock-read-stdin-ms",
            "500",
            "--mock-report-file",
            report.to_str().expect("report path"),
        ])
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .spawn()
        .expect("spawn mock agent");
    let deadline = Instant::now() + Duration::from_secs(5);
    while !ready.is_file() && Instant::now() < deadline {
        assert!(child.try_wait().expect("poll child").is_none());
        thread::sleep(Duration::from_millis(10));
    }
    let value: serde_json::Value = serde_json::from_slice(&fs::read(&ready).expect("ready file"))
        .expect("atomic complete stdin readiness report");
    assert_eq!(value["stdin_raw_ready"], true);
    child
        .stdin
        .take()
        .expect("stdin pipe")
        .write_all(b"stdin-after-ready")
        .expect("write input after readiness");
    assert!(child.wait().expect("wait for mock agent").success());
    let value: serde_json::Value =
        serde_json::from_slice(&fs::read(&report).expect("report")).expect("valid report");
    assert_eq!(value["stdin"], "stdin-after-ready");
    fs::remove_dir_all(&dir).expect("remove isolated test directory");
}

fn mock_agent_binary() -> PathBuf {
    if let Some(dir) = std::env::var_os("CLUD_TEST_BIN_DIR") {
        let candidate = PathBuf::from(dir).join(exe_file_name("mock-agent"));
        if candidate.is_file() {
            return candidate;
        }
    }
    if let Some(compiled) = option_env!("CARGO_BIN_EXE_mock-agent") {
        return PathBuf::from(compiled);
    }
    let mut dir = std::env::current_exe().expect("current test exe");
    dir.pop();
    if dir.ends_with("deps") {
        dir.pop();
    }
    dir.join(exe_file_name("mock-agent"))
}

fn exe_file_name(name: &str) -> String {
    let ext = if cfg!(windows) { ".exe" } else { "" };
    format!("{name}{ext}")
}
