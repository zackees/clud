//! The tool-shell probe on Windows: a PowerShell tool call leaks a client,
//! and the probe reports whether clud's tool-shell reaper removed it.

use std::path::Path;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

pub fn run_tool_shell_probe(exe: &str, report_path: &Path) {
    use sysinfo::{Pid, ProcessRefreshKind, ProcessesToUpdate, System};

    let pid_path = report_path.with_extension("pid");
    let quote = |value: &str| format!("'{}'", value.replace('\'', "''"));
    let script = format!(
        "$p = Start-Process -FilePath {} \
         -ArgumentList '--mock-helper-role','tool-leak','--mock-sleep-ms','30000' -PassThru; \
         Set-Content -LiteralPath {} -Value $p.Id",
        quote(exe),
        quote(&pid_path.to_string_lossy()),
    );
    let status = Command::new("powershell.exe")
        .args([
            "-NoLogo",
            "-NoProfile",
            "-NonInteractive",
            "-Command",
            &script,
        ])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .expect("spawn tool PowerShell");
    assert!(status.success(), "tool PowerShell failed: {status}");

    let pid: u32 = std::fs::read_to_string(&pid_path)
        .expect("read leaked-client pid")
        .trim()
        .parse()
        .expect("parse leaked-client pid");
    let deadline = Instant::now() + Duration::from_secs(10);
    let mut system = System::new();
    let reaped = loop {
        system.refresh_processes_specifics(
            ProcessesToUpdate::Some(&[Pid::from_u32(pid)]),
            true,
            ProcessRefreshKind::nothing(),
        );
        if system.process(Pid::from_u32(pid)).is_none() {
            break true;
        }
        if Instant::now() >= deadline {
            if let Some(process) = system.process(Pid::from_u32(pid)) {
                let _ = process.kill();
            }
            break false;
        }
        std::thread::sleep(Duration::from_millis(25));
    };
    let report = serde_json::json!({
        "tool_shell_probe": {
            "client_pid": pid,
            "reaped": reaped,
        }
    });
    if let Some(parent) = report_path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    std::fs::write(report_path, report.to_string()).expect("write tool-shell probe report");
    println!("{report}");
    std::process::exit(if reaped { 0 } else { 1 });
}
