use std::fs;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

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
