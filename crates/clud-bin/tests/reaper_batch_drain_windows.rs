#![cfg(windows)]

//! Tier 2 of #706: the completion-port drain folds a burst into batches.
//!
//! **Almost all reaper coverage belongs in Tier 1** — see
//! `job_orphan_reaper::batch_tests` for the folding rule itself, which is pure
//! and asserted on every platform. What is left here is the one thing that
//! cannot be faked: a **real** Job Object completion port, with **real**
//! process churn queuing **real** notifications faster than the listener
//! drains them.
//!
//! That queuing behaviour is the entire premise of the fix. #706 measured the
//! listener taking one full `CreateToolhelp32Snapshot` over every process on
//! the host (~20 ms, 482-660 processes) *per completion-port message*, against
//! a workload spawning ~178 processes/second — ~3.6 cores of kernel time per
//! session, multiplying by concurrent sessions. Messages are now drained into
//! a batch that shares a single host enumeration.
//!
//! One test, deliberately. Anything expressible against the folding rule
//! belongs in Tier 1.
//!
//! Raw `std::process::Command` is intentional and is why this file is in
//! `ci/banned_imports.py`'s exempt set: `NativeProcess` would attach its own
//! Job Object to each child, which is precisely the containment this test
//! needs the *tracker's* job to own.

use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use clud::job_orphan_reaper::ForegroundJobTracker;

/// Processes in the burst. Large enough that notifications reliably queue
/// behind the listener's per-batch work, small enough to stay well inside the
/// suite's time budget.
const BURST: usize = 48;

fn field(lines: &[String], key: &str) -> u64 {
    let needle = format!("{key}=");
    lines
        .iter()
        .find_map(|line| {
            let rest = line.split(&needle).nth(1)?;
            let digits = rest
                .chars()
                .take_while(char::is_ascii_digit)
                .collect::<String>();
            digits.parse::<u64>().ok()
        })
        .unwrap_or_else(|| panic!("no `{key}=` in reaper measurement lines: {lines:#?}"))
}

#[test]
fn a_process_burst_shares_one_host_scan_per_batch() {
    let tracker = ForegroundJobTracker::install().expect("install foreground Job tracker");

    // Spawn concurrently and only then reap the handles: the point is to get
    // many notifications into the port while the listener is mid-batch.
    // Waiting on each child in turn would serialize the churn and defeat that.
    let mut children = Vec::with_capacity(BURST);
    for _ in 0..BURST {
        match Command::new("cmd.exe")
            .args(["/d", "/c", "exit"])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
        {
            Ok(child) => children.push(child),
            // A loaded CI box can transiently refuse a spawn; the assertions
            // below are about the ones that *did* run.
            Err(_) => break,
        }
    }
    let spawned = children.len();
    assert!(
        spawned >= 8,
        "could not spawn enough processes to exercise a burst (got {spawned})"
    );
    for mut child in children {
        let _ = child.wait();
    }

    // Let the listener finish draining. It blocks in 200 ms slices, so give it
    // several, and stop early once the tracked count has settled.
    let deadline = Instant::now() + Duration::from_secs(10);
    let mut last_tracked = u64::MAX;
    loop {
        std::thread::sleep(Duration::from_millis(300));
        let tracked = field(&tracker.finish_and_report(true), "host_scans");
        if tracked == last_tracked || Instant::now() >= deadline {
            break;
        }
        last_tracked = tracked;
    }

    let lines = tracker.finish_and_report(true);
    let host_scans = field(&lines, "host_scans");
    let peak_batch = field(&lines, "peak_batch");
    let ticks = field(&lines, "ticks");

    // Visible under `--nocapture`: this is the measurement #706 is about, and
    // it is worth being able to read it without a debugger.
    eprintln!(
        "reaper batch drain: spawned={spawned} host_scans={host_scans} \
         ticks={ticks} peak_batch={peak_batch}"
    );

    // The property #706 is actually about: a burst of N processes must not
    // cost a host enumeration per process. Each spawn raises *two*
    // completion-port messages (NEW_PROCESS + EXIT_PROCESS), so the
    // pre-fix cost of this burst was upwards of `spawned` enumerations —
    // one per NEW_PROCESS, plus one per unresolved exit.
    //
    // Deliberately not `host_scans <= ticks`: `snapshot()` counts every call
    // site, and one quiet-period tick can drive both a retry scan and a batch
    // scan. The bound that matters is against the message count, not the
    // iteration count.
    assert!(
        host_scans < spawned as u64,
        "host_scans={host_scans} for a {spawned}-process burst — the drain is \
         not amortizing enumerations across messages ({lines:#?})"
    );

    // The mechanism actually engaged: at least one drain folded more than a
    // single message. Before #706 every message paid its own full host
    // enumeration, so this was 1 by construction.
    assert!(
        peak_batch >= 2,
        "peak_batch={peak_batch} — the drain never folded two messages together, \
         so a {spawned}-process burst still cost one host scan per message ({lines:#?})"
    );
}
