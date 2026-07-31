//! Tier 2 of #674, completing the lifecycle matrix: the **cross-platform**
//! reaper's three destructive entry points (#688).
//!
//! [`reaper_daemon_survival_windows`](./reaper_daemon_survival_windows.rs)
//! covers the Windows tool-shell reaper — the one whose blast radius is a
//! single `cmd.exe` subtree. This file covers [`clud::orphan_reaper`], which
//! runs on **every** foreground `clud` exit, on `clud slay`, and on the
//! daemon's periodic sweep, on every platform. Until #688 that reaper spared
//! by the cooperative `RUNNING_PROCESS_IS_DAEMON` marker alone, so an
//! sccache-shaped daemon survived the tool shell that started it (#681) and was
//! then killed moments later by clud's own exit.
//!
//! **Almost all reaper coverage belongs in Tier 1** — table-driven unit tests
//! over injected `ProcessFacts`, in `orphan_reaper`'s and `job_orphan_reaper`'s
//! test modules. Three tests live here, and only because what they exercise
//! cannot be faked: a **real** detached process that owns a **real** listening
//! socket, found through a **real** full-host environment scan.
//!
//! ## Why these sweeps cannot hurt the machine running them
//!
//! `clud slay` and the daemon sweep reap *every* CLUD-tagged orphan on the
//! host, which on a developer's workstation may include another session's
//! work. The tests therefore drive [`clud::orphan_reaper::reap_orphans_filtered`]
//! with an admission closure narrowed to this test's own stub PIDs. The
//! admission hook runs *before* `report_and_reap`, so the code under test —
//! scan, spare-list, classification, kill — is bit-for-bit what `clud slay`
//! executes; only the candidate set is narrowed.
//!
//! Raw `std::process::Command` is intentional and is why this file is in
//! `ci/banned_imports.py`'s exempt set: a `NativeProcess` would set the very
//! `RUNNING_PROCESS_IS_DAEMON` marker whose absence the sccache-shaped case is
//! about, and attach containment that masks the detach.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use clud::orphan_reaper::{self, ReapOpts};
use clud::reaper_facts::SpareReason;

/// Long enough for a sweep to run and for a doomed process to actually die,
/// short enough that a hang fails the test rather than the job.
const SETTLE: Duration = Duration::from_millis(400);

fn exe_name(stem: &str) -> String {
    if cfg!(windows) {
        format!("{stem}.exe")
    } else {
        stem.to_string()
    }
}

fn daemon_stub() -> PathBuf {
    // The harness binary lives in `target/<triple>/debug/deps/`; the workspace
    // binaries sit one level up.
    let mut dir = std::env::current_exe().expect("current test exe");
    dir.pop();
    if dir.ends_with("deps") {
        dir.pop();
    }
    let stub = dir.join(exe_name("daemon-stub"));
    assert!(
        stub.is_file(),
        "daemon-stub not built at {}; `soldr cargo build -p daemon-stub`",
        stub.display()
    );
    stub
}

fn wait_for_pid_file(path: &Path) -> u32 {
    let deadline = Instant::now() + Duration::from_secs(15);
    loop {
        if let Ok(text) = std::fs::read_to_string(path) {
            if let Ok(pid) = text.trim().parse::<u32>() {
                if pid != 0 {
                    return pid;
                }
            }
        }
        assert!(
            Instant::now() < deadline,
            "timed out waiting for daemon-stub pid at {}",
            path.display()
        );
        std::thread::sleep(Duration::from_millis(25));
    }
}

fn pid_is_alive(pid: u32) -> bool {
    clud::process_identity::ProcessIdentity::observe(pid).is_some()
}

/// A PID that is certainly not running: spawn something trivial, wait for it,
/// and take its number. This is what makes a tag look *abandoned* to the sweep,
/// which is the state `clud slay` and the daemon heartbeat select on.
fn a_dead_originator_pid() -> u32 {
    let mut child = trivial_command()
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn throwaway process");
    let pid = child.id();
    let _ = child.wait();
    pid
}

fn trivial_command() -> Command {
    if cfg!(windows) {
        let mut command = Command::new("cmd.exe");
        command.args(["/d", "/c", "exit"]);
        command
    } else {
        Command::new("true")
    }
}

/// Start a `daemon-stub` in the given mode, tagged as a descendant of
/// `originator`, and return the served daemon's PID.
///
/// The tag is the whole point: `RUNNING_PROCESS_ORIGINATOR=CLUD:<pid>` is
/// inherited by everything an agent spawns, and sccache-class daemons never
/// strip it, because they never call `running-process` at all. That is what
/// puts them in the reaper's candidate set with nothing protecting them.
fn start_tagged_stub(temp: &Path, label: &str, mode: &str, originator: u32) -> u32 {
    let pid_path = temp.join(format!("{label}.pid"));
    let mut launcher = Command::new(daemon_stub());
    launcher
        .arg(mode)
        .arg(&pid_path)
        .env(
            running_process::ORIGINATOR_ENV_VAR,
            format!("CLUD:{originator}"),
        )
        .env_remove(running_process::DAEMON_MARKER_ENV_VAR)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    let mut child = launcher.spawn().expect("spawn daemon-stub launcher");
    let _ = child.wait();
    wait_for_pid_file(&pid_path)
}

fn quiet() -> ReapOpts {
    ReapOpts {
        quiet: true,
        ..ReapOpts::default()
    }
}

/// #688's headline claim, against the sweep that `clud slay` and the daemon's
/// periodic heartbeat both run.
///
/// The stub reproduces the **signal shape** of sccache / `FBuildWorker` /
/// a language server: tagged, undeclared, detached on its own (`setsid` on
/// Unix, `DETACHED_PROCESS` on Windows), owning a listening socket. Depending
/// on a real sccache install would make the suite non-hermetic, and the reaper
/// keys on the shape, not the name.
///
/// The assertion is on the **reason**, not on survival alone: a sweep that
/// never saw the process at all also leaves it running, and would pass a
/// survival-only test while the protection did not exist.
#[test]
fn a_tagged_undeclared_listening_daemon_survives_the_orphan_sweep() {
    let temp = tempfile::tempdir().expect("tempdir");
    let originator = a_dead_originator_pid();
    let stub = start_tagged_stub(temp.path(), "sweep", "spawn-detached", originator);

    let outcome = orphan_reaper::reap_orphans_filtered(&quiet(), &mut |pid| pid == stub);
    std::thread::sleep(SETTLE);
    let survived = pid_is_alive(stub);
    clud::process_tree::kill_tree(stub);

    assert!(
        outcome.candidate_pids.contains(&stub),
        "the sweep never selected the stub, so this proves nothing; \
         candidates={:?}",
        outcome.candidate_pids
    );
    let reason = outcome
        .spared
        .iter()
        .find(|(pid, _)| *pid == stub)
        .map(|(_, reason)| *reason);
    assert!(
        matches!(
            reason,
            Some(SpareReason::ListeningEndpoint) | Some(SpareReason::SessionLeader)
        ),
        "an sccache-shaped daemon must be spared by an OS signal, not by luck; \
         got {reason:?}"
    );
    assert!(survived, "the sweep killed a daemon it claimed to spare");
}

/// The same claim against the *other* entry point: the scan every foreground
/// `clud` runs as it exits. It selects on "originator is me" rather than
/// "originator is dead", so it is a genuinely different candidate set and was
/// separately unprotected.
#[test]
fn a_tagged_undeclared_listening_daemon_survives_the_on_exit_scan() {
    let temp = tempfile::tempdir().expect("tempdir");
    // This test process stands in for the exiting clud, so the tag points at
    // it and only this test's own stubs can be selected.
    let me = std::process::id();
    let stub = start_tagged_stub(temp.path(), "exit", "spawn-detached", me);

    let outcome = orphan_reaper::scan_and_report(me, &quiet());
    std::thread::sleep(SETTLE);
    let survived = pid_is_alive(stub);
    clud::process_tree::kill_tree(stub);

    assert!(
        outcome.candidate_pids.contains(&stub),
        "the on-exit scan never selected the stub; candidates={:?}",
        outcome.candidate_pids
    );
    assert!(
        outcome.spared.iter().any(|(pid, _)| *pid == stub),
        "the on-exit scan reaped an sccache-shaped daemon"
    );
    assert!(survived, "the on-exit scan killed a spared daemon");
}

/// The counterweight, and the reason the signal table is ranked rather than
/// permissive. If sparing were over-broad the two tests above would pass while
/// the reaper had quietly stopped working, so a genuinely leaked orphan — same
/// tag, same dead originator, no marker, no detach, no listener — must still be
/// reaped by the same sweep.
#[test]
fn a_leaked_orphan_with_no_daemon_signal_is_still_reaped() {
    let originator = a_dead_originator_pid();
    // An ordinary long-running client: declares nothing, detaches from nothing,
    // listens on nothing.
    let mut leaked = sleeper()
        .env(
            running_process::ORIGINATOR_ENV_VAR,
            format!("CLUD:{originator}"),
        )
        .env_remove(running_process::DAEMON_MARKER_ENV_VAR)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn leaked client");
    let pid = leaked.id();
    // Let it appear in a full-host environment scan.
    std::thread::sleep(SETTLE);

    let outcome = orphan_reaper::reap_orphans_filtered(&quiet(), &mut |candidate| candidate == pid);
    assert!(
        outcome.candidate_pids.contains(&pid),
        "the sweep never selected the leaked client; candidates={:?}",
        outcome.candidate_pids
    );
    assert!(
        outcome.spared.is_empty(),
        "nothing about a leaked client deserves protection: {:?}",
        outcome.spared
    );

    let deadline = Instant::now() + Duration::from_secs(10);
    while pid_is_alive(pid) {
        if Instant::now() >= deadline {
            let _ = leaked.kill();
            panic!(
                "a leaked orphan with no daemon signal must still be reaped; \
                 over-sparing has disabled the reaper"
            );
        }
        std::thread::sleep(Duration::from_millis(25));
    }
    let _ = leaked.wait();
}

fn sleeper() -> Command {
    if cfg!(windows) {
        let mut command = Command::new("ping.exe");
        command.args(["-n", "60", "127.0.0.1"]);
        command
    } else {
        let mut command = Command::new("sleep");
        command.arg("60");
        command
    }
}
