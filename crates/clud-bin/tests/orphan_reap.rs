//! Integration test: `orphan_reaper::reap_orphans` actually kills a
//! CLUD-tagged process whose originator PID is no longer alive.
//!
//! Strategy: spawn `mock-agent --mock-sleep-ms 30000` with an env var of
//! `RUNNING_PROCESS_ORIGINATOR=CLUD:99999999`. PID 99999999 is overwhelmingly
//! unlikely to be a live process on the test host (and even if it is, the
//! start-time guard inside `running-process` keeps it out of the alive set).
//! That makes the spawned mock-agent an "orphan" by definition. We then
//! invoke `reap_orphans` and assert the spawned PID is gone.

use std::time::Duration;

use running_process::{CommandSpec, NativeProcess, ProcessConfig, StderrMode, StdinMode};

use clud::orphan_reaper::{reap_orphans, ReapOpts};

mod common;
use common::{mock_agent_path, wait_until};

/// Anyone-but-a-real-clud PID. Picked high enough to dodge live processes on
/// the host yet inside the legal u32 range that
/// `running-process::parse_originator_value` accepts.
const DEAD_ORIGINATOR_PID: u32 = 99_999_999;

#[test]
fn reap_orphans_kills_a_dead_originator_child() {
    let mock = mock_agent_path();

    // Inherit our env so PATH / system DLLs resolve, then override the
    // originator var to point at a PID that is not a live `clud` process.
    let mut env: Vec<(String, String)> = std::env::vars().collect();
    env.retain(|(k, _)| k != "RUNNING_PROCESS_ORIGINATOR");
    env.push((
        "RUNNING_PROCESS_ORIGINATOR".to_string(),
        format!("CLUD:{DEAD_ORIGINATOR_PID}"),
    ));

    let config = ProcessConfig {
        command: CommandSpec::Argv(vec![
            mock.to_string_lossy().into_owned(),
            "--mock-sleep-ms".to_string(),
            "30000".to_string(),
        ]),
        cwd: None,
        env: Some(env),
        capture: false,
        stderr_mode: StderrMode::Stdout,
        creationflags: None,
        create_process_group: false,
        stdin_mode: StdinMode::Null,
        nice: None,
    };
    let process = NativeProcess::new(config);
    process.start().expect("spawn mock-agent");

    let target_pid = process.pid().expect("mock-agent has a pid");

    // Wait until `find_processes_by_originator` actually observes the
    // spawned child — sysinfo's environ readback is async on some hosts.
    let observed = wait_until(Duration::from_secs(5), || {
        running_process::originator::find_processes_by_originator("CLUD")
            .iter()
            .any(|p| p.pid == target_pid)
    });
    if !observed {
        let _ = process.kill();
        let _ = process.wait(Some(Duration::from_secs(2)));
        panic!("spawned mock-agent (pid={target_pid}) was never observed as CLUD-tagged");
    }

    let outcome = reap_orphans(&ReapOpts {
        keep: false,
        quiet: true,
        explain: false,
    });
    assert!(
        outcome.found >= 1,
        "reap_orphans must have found at least our spawned child (found={})",
        outcome.found
    );
    assert!(
        outcome.reaped >= 1,
        "reap_orphans must have reaped at least our spawned child (reaped={})",
        outcome.reaped
    );

    // Confirm the child actually died. Poll its returncode rather than
    // re-scanning sysinfo: if `reap_orphans` killed the right tree, the
    // mock-agent exits within a few hundred ms.
    let died = wait_until(Duration::from_secs(5), || process.returncode().is_some());
    if !died {
        let _ = process.kill();
        let _ = process.wait(Some(Duration::from_secs(2)));
        panic!("mock-agent (pid={target_pid}) survived reap_orphans");
    }
}
