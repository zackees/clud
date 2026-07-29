#![cfg(windows)]

//! Windows integration coverage for the foreground tool-shell Job tracker.
//!
//! Raw `std::process::Command` is intentional: wrapping these fixtures in
//! `NativeProcess` would add another Job Object and mask the production
//! tracker's completion-port behavior.

use std::path::Path;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use clud::job_orphan_reaper::ForegroundJobTracker;

const HELPER_SLEEP: &str = "helper_leaked_client_sleeps";
const HELPER_NESTED: &str = "helper_spawns_nested_cmd";
const PID_PATH_ENV: &str = "CLUD_TOOL_SHELL_TEST_PID_PATH";

fn quote_powershell(path: &Path) -> String {
    format!("'{}'", path.to_string_lossy().replace('\'', "''"))
}

fn wait_for_pid_file(path: &Path) -> u32 {
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        if let Ok(text) = std::fs::read_to_string(path) {
            if let Ok(pid) = text.trim().parse() {
                return pid;
            }
        }
        assert!(
            Instant::now() < deadline,
            "timed out waiting for fixture pid at {}",
            path.display()
        );
        std::thread::sleep(Duration::from_millis(25));
    }
}

fn pid_is_alive(pid: u32) -> bool {
    use windows::Win32::Foundation::{CloseHandle, WAIT_TIMEOUT};
    use windows::Win32::System::Threading::{
        OpenProcess, WaitForSingleObject, PROCESS_QUERY_LIMITED_INFORMATION, PROCESS_SYNCHRONIZE,
    };

    let Ok(handle) = (unsafe {
        OpenProcess(
            PROCESS_QUERY_LIMITED_INFORMATION | PROCESS_SYNCHRONIZE,
            false,
            pid,
        )
    }) else {
        return false;
    };
    let alive = unsafe { WaitForSingleObject(handle, 0) } == WAIT_TIMEOUT;
    unsafe {
        let _ = CloseHandle(handle);
    }
    alive
}

fn wait_for_exit(pid: u32) {
    let deadline = Instant::now() + Duration::from_secs(10);
    while pid_is_alive(pid) {
        assert!(
            Instant::now() < deadline,
            "PID {pid} survived automatic tool-shell reap"
        );
        std::thread::sleep(Duration::from_millis(25));
    }
}

fn spawn_fake_agent(powershell: &str) -> std::process::Child {
    Command::new("cmd.exe")
        .args([
            "/d",
            "/c",
            "powershell.exe",
            "-NoLogo",
            "-NoProfile",
            "-NonInteractive",
            "-Command",
            powershell,
        ])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn fake cmd agent")
}

#[test]
fn live_tracker_reaps_direct_client_and_spares_nested_shell() {
    let tracker = ForegroundJobTracker::install().expect("install foreground Job tracker");
    let temp = tempfile::tempdir().expect("tempdir");
    let current_exe = std::env::current_exe().expect("current test exe");

    // should-reap: registered cmd agent -> PowerShell tool root -> git.exe.
    let leaked_exe = temp.path().join("git.exe");
    std::fs::copy(&current_exe, &leaked_exe).expect("copy leaked-client helper");
    let leaked_pid_path = temp.path().join("leaked.pid");
    let leaked_script = format!(
        "$p = Start-Process -FilePath {exe} \
         -ArgumentList '--ignored','--exact','{helper}' -PassThru; \
         Set-Content -LiteralPath {pid_path} -Value $p.Id",
        pid_path = quote_powershell(&leaked_pid_path),
        exe = quote_powershell(&leaked_exe),
        helper = HELPER_SLEEP,
    );
    let mut agent = spawn_fake_agent(&leaked_script);
    tracker.register_backend(agent.id(), "cmd.exe");
    assert!(agent.wait().expect("wait fake agent").success());
    let leaked_pid = wait_for_pid_file(&leaked_pid_path);
    wait_for_exit(leaked_pid);

    // must-survive: registered cmd agent -> PowerShell tool root -> new.exe
    // -> cmd.exe. The shell is below a non-shell client, so it represents an
    // intentional detachment and must not be promoted or reaped.
    let nested_exe = temp.path().join("new.exe");
    std::fs::copy(&current_exe, &nested_exe).expect("copy nested-shell helper");
    let nested_pid_path = temp.path().join("nested.pid");
    let nested_script = format!(
        "$env:{PID_PATH_ENV}={pid_path}; \
         & {exe} --ignored --exact {helper}",
        pid_path = quote_powershell(&nested_pid_path),
        exe = quote_powershell(&nested_exe),
        helper = HELPER_NESTED,
    );
    let mut agent = spawn_fake_agent(&nested_script);
    tracker.register_backend(agent.id(), "cmd.exe");
    assert!(agent.wait().expect("wait fake agent").success());
    let nested_pid = wait_for_pid_file(&nested_pid_path);
    std::thread::sleep(Duration::from_millis(500));
    assert!(
        pid_is_alive(nested_pid),
        "intentionally detached nested cmd.exe was reaped"
    );

    clud::process_tree::kill_tree(nested_pid);
    wait_for_exit(nested_pid);
}

#[test]
#[ignore = "subprocess fixture invoked by live_tracker_reaps_direct_client_and_spares_nested_shell"]
fn helper_leaked_client_sleeps() {
    let path = std::env::var_os(PID_PATH_ENV).expect("fixture pid path env");
    std::fs::write(path, std::process::id().to_string()).expect("write helper pid");
    std::thread::sleep(Duration::from_secs(30));
}

#[test]
#[ignore = "subprocess fixture invoked by live_tracker_reaps_direct_client_and_spares_nested_shell"]
#[allow(clippy::zombie_processes)] // Intentionally outlives this helper; parent test owns cleanup.
fn helper_spawns_nested_cmd() {
    let path = std::env::var_os(PID_PATH_ENV).expect("fixture pid path env");
    let child = Command::new("cmd.exe")
        .args(["/d", "/s", "/c", "ping -n 30 127.0.0.1 >NUL"])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn nested cmd fixture");
    std::fs::write(path, child.id().to_string()).expect("write nested pid");
}
