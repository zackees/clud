use super::*;

fn test_loop_repeat_24h_parses() {
    let p = plan(&["clud", "loop", "--repeat", "24h", "task"]);
    assert_eq!(
        p.repeat_schedule.as_ref().map(|s| s.interval_secs),
        Some(24 * 60 * 60)
    );
}

// ---- Scheduler: next-run computation + no-overlap invariant ----

#[test]
fn test_next_run_at_millis_basic() {
    // Run completed at t=10000 ms with a 30s interval → next run at t=40000 ms.
    assert_eq!(next_run_at_millis(10_000, 30), 40_000);
    assert_eq!(next_run_at_millis(0, 1), 1_000);
    assert_eq!(next_run_at_millis(0, 3600), 3_600_000);
}

#[test]
fn test_next_run_at_millis_long_run_pushes_schedule_out() {
    // The no-overlap invariant in numerical form: if a run that started at
    // t=0 takes 10 minutes (600_000 ms) and the interval is 1 minute
    // (60 s), the next run is scheduled at completion + interval = 660_000 ms,
    // *not* at 60_000 ms. Runs serialize; they never overlap.
    let started_at = 0u64;
    let duration_ms = 10 * 60 * 1000; // 10-minute run
    let interval_secs = 60; // 1-minute repeat
    let completed_at = started_at + duration_ms;
    let next = next_run_at_millis(completed_at, interval_secs);
    assert_eq!(
        next,
        completed_at + 60_000,
        "next run must be `interval` after completion, never overlapping the previous run"
    );
    assert!(
        next > started_at + (interval_secs * 1000),
        "long-running iteration must push the schedule past the original interval"
    );
}

#[test]
fn test_next_run_at_millis_short_run_respects_full_interval() {
    // A 1-second run with a 60-second repeat still waits the full minute
    // after completion before re-running.
    let completed_at = 1_000u64;
    let next = next_run_at_millis(completed_at, 60);
    assert_eq!(next, 61_000);
}

#[test]
fn test_next_run_at_millis_saturates_on_overflow() {
    // Pathological inputs must not panic — the daemon uses saturating
    // arithmetic so we mirror it here.
    assert_eq!(next_run_at_millis(u64::MAX, 1), u64::MAX);
    assert_eq!(next_run_at_millis(u64::MAX - 1, 3600), u64::MAX);
    assert_eq!(next_run_at_millis(0, u64::MAX), u64::MAX);
}

/// Higher-level scheduler simulation. Models the inner loop of
/// `run_repeat_worker` with synthetic clocks: the scheduler issues a
/// single run at a time, only sleeping between completions. This is a
/// pure-Rust simulation — we never spawn a real process — but it
/// exercises the same arithmetic the daemon uses.
fn simulate_repeat(start_ms: u64, run_durations_ms: &[u64], interval_secs: u64) -> Vec<(u64, u64)> {
    // Returns Vec<(start_ms, end_ms)> for each iteration.
    let mut now = start_ms;
    let mut runs = Vec::new();
    for &dur in run_durations_ms {
        let started = now;
        let ended = started + dur;
        runs.push((started, ended));
        now = next_run_at_millis(ended, interval_secs);
    }
    runs
}

#[test]
fn test_scheduler_first_run_is_immediate() {
    let runs = simulate_repeat(1_000, &[5_000], 60);
    // First run starts at start_ms exactly — no pre-sleep.
    assert_eq!(runs[0].0, 1_000);
    assert_eq!(runs[0].1, 6_000);
}

#[test]
fn test_scheduler_second_run_starts_interval_after_first_completes() {
    // Two short runs, 60s interval — second must start at first.end + 60s.
    let runs = simulate_repeat(0, &[1_000, 1_000], 60);
    assert_eq!(runs.len(), 2);
    let (first_start, first_end) = runs[0];
    let (second_start, _) = runs[1];
    assert_eq!(first_start, 0);
    assert_eq!(first_end, 1_000);
    assert_eq!(second_start, first_end + 60_000);
}

#[test]
fn test_scheduler_long_run_delays_second_run_no_overlap() {
    // First run takes 5 minutes; interval is 1 minute; second run must
    // NOT have overlapped the first.
    let interval = 60;
    let runs = simulate_repeat(0, &[5 * 60 * 1000, 1_000], interval);
    let (_first_start, first_end) = runs[0];
    let (second_start, _) = runs[1];
    assert_eq!(first_end, 5 * 60 * 1000);
    assert_eq!(
        second_start,
        first_end + interval * 1000,
        "second run start must be after first completion + interval"
    );
    assert!(
        second_start >= first_end,
        "no-overlap invariant violated: second run started before first finished"
    );
}

#[test]
fn test_scheduler_only_one_active_run_per_job() {
    // The simulation is inherently single-active by construction (each
    // iteration is processed sequentially). Assert that the runs are
    // strictly non-overlapping and strictly monotonic in time.
    let runs = simulate_repeat(0, &[100, 200, 50, 1_000], 30);
    for window in runs.windows(2) {
        let (_a_start, a_end) = window[0];
        let (b_start, _b_end) = window[1];
        assert!(
            b_start >= a_end,
            "runs overlapped: {:?} into {:?}",
            window[0],
            window[1]
        );
    }
    // And each run's own end is after its start.
    for (start, end) in runs {
        assert!(end >= start);
    }
}

#[test]
fn test_scheduler_3600s_interval_matches_1h_input() {
    // Cross-check between parse + scheduler: the seconds returned by
    // parse_repeat_interval drop directly into next_run_at_millis.
    let secs = parse_repeat_interval("1h").unwrap();
    assert_eq!(secs, 3600);
    let next = next_run_at_millis(0, secs);
    assert_eq!(next, 3_600_000);
}
