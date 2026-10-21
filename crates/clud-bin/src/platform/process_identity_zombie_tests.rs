//! Native Linux zombie fixture for the process-identity liveness contract.
#![cfg(target_os = "linux")]

use super::*;

/// #1993: an unreaped daemon still has a PID and start time, but no
/// listener or executable code remains. Hold that exact state with
/// waitid(WNOWAIT), since NativeProcess would reap the child itself.
#[test]
fn an_unreaped_zombie_is_not_a_live_identity() {
    use std::io::Write;

    let mut child = std::process::Command::new("sh")
        .args(["-c", "read token; exit 0"])
        .stdin(std::process::Stdio::piped())
        .spawn()
        .expect("spawn child waiting for release");
    let identity = ProcessIdentity::observe(child.id()).expect("live child identity");
    child.stdin.take().unwrap().write_all(b"exit\n").unwrap();
    // SAFETY: siginfo_t may be zero-initialized, the out pointer is valid,
    // and waitid targets our own child without reaping its exit status.
    let mut info: libc::siginfo_t = unsafe { std::mem::zeroed() };
    // SAFETY: the child is ours and the initialized siginfo out pointer is valid.
    let waited = unsafe {
        libc::waitid(
            libc::P_PID,
            child.id(),
            &mut info,
            libc::WEXITED | libc::WNOWAIT,
        )
    };
    let was_live = identity.is_live();
    let observed = ProcessIdentity::observe(identity.pid);
    let mut system = System::new();
    system.refresh_processes_specifics(
        ProcessesToUpdate::Some(&[Pid::from_u32(identity.pid)]),
        true,
        ProcessRefreshKind::nothing(),
    );
    let batched = ProcessIdentity::observe_in(&system, identity.pid);
    assert!(child
        .wait()
        .expect("reap child after observations")
        .success());
    assert_eq!(waited, 0, "waitid must leave an exited child unreaped");
    assert!(
        !was_live,
        "an unreaped zombie must not be considered running"
    );
    assert_eq!(observed, None, "the direct probe must exclude a zombie");
    assert_eq!(batched, None, "the batched probe must exclude a zombie");
}
