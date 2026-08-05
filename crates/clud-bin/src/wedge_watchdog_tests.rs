use super::*;

fn hot_quiet_sample(wall: Duration) -> Sample {
    Sample {
        hottest_thread_user_delta: wall,
        wall_delta: wall,
        io_write_delta: 0,
    }
}

fn cfg() -> WedgeDetectorCfg {
    WedgeDetectorCfg {
        user_pct_threshold: 0.90,
        io_epsilon_bytes: 4096,
        required_streak: 3,
    }
}

// ── spin + no output -> Wedged after N windows ──────────────────────

#[test]
fn spin_with_no_output_reaches_wedged_after_required_streak() {
    let mut detector = WedgeDetector::new(cfg());
    let window = Duration::from_secs(10);

    // First two qualifying windows: Suspect, not yet Wedged.
    for i in 1..3 {
        let state = detector.observe(hot_quiet_sample(window));
        assert_eq!(state, WedgeState::Suspect { streak: i });
    }
    // Third qualifying window (== required_streak) fires Wedged.
    let state = detector.observe(hot_quiet_sample(window));
    assert_eq!(state, WedgeState::Wedged);
}

#[test]
fn wedged_state_persists_while_condition_keeps_holding() {
    let mut detector = WedgeDetector::new(cfg());
    let window = Duration::from_secs(10);
    for _ in 0..3 {
        detector.observe(hot_quiet_sample(window));
    }
    assert_eq!(detector.state(), WedgeState::Wedged);
    // A fourth qualifying window stays Wedged (streak keeps growing).
    assert_eq!(
        detector.observe(hot_quiet_sample(window)),
        WedgeState::Wedged
    );
}

// ── spin + output -> Healthy ─────────────────────────────────────────

#[test]
fn spin_with_console_output_never_wedges() {
    let mut detector = WedgeDetector::new(cfg());
    let window = Duration::from_secs(10);
    // 100% single-thread CPU, but IO write bytes comfortably above the
    // epsilon every window: the "quiet" half of the signature never
    // holds, so the streak can never build.
    for _ in 0..20 {
        let sample = Sample {
            hottest_thread_user_delta: window,
            wall_delta: window,
            io_write_delta: 50_000,
        };
        assert_eq!(detector.observe(sample), WedgeState::Healthy);
    }
}

// ── multi-thread spread load -> Healthy ─────────────────────────────

#[test]
fn multi_thread_spread_load_never_wedges() {
    let mut detector = WedgeDetector::new(cfg());
    let window = Duration::from_secs(10);
    // Total subtree CPU is high, but spread across threads so no single
    // thread exceeds the 90% threshold (here: ~25% each).
    for _ in 0..20 {
        let sample = Sample {
            hottest_thread_user_delta: window / 4,
            wall_delta: window,
            io_write_delta: 0,
        };
        assert_eq!(detector.observe(sample), WedgeState::Healthy);
    }
}

// ── brief spike then idle -> streak resets ──────────────────────────

#[test]
fn brief_spike_then_idle_resets_streak() {
    let mut detector = WedgeDetector::new(cfg());
    let window = Duration::from_secs(10);

    // Two qualifying windows (Suspect), then one healthy window.
    detector.observe(hot_quiet_sample(window));
    assert_eq!(
        detector.observe(hot_quiet_sample(window)),
        WedgeState::Suspect { streak: 2 }
    );
    let idle = Sample {
        hottest_thread_user_delta: Duration::ZERO,
        wall_delta: window,
        io_write_delta: 0,
    };
    assert_eq!(detector.observe(idle), WedgeState::Healthy);

    // Post-dip: needs the full required_streak again, not a partial
    // credit from before the dip.
    assert_eq!(
        detector.observe(hot_quiet_sample(window)),
        WedgeState::Suspect { streak: 1 }
    );
    assert_eq!(
        detector.observe(hot_quiet_sample(window)),
        WedgeState::Suspect { streak: 2 }
    );
    assert_eq!(
        detector.observe(hot_quiet_sample(window)),
        WedgeState::Wedged
    );
}

// ── recovery after Wedged clears the flag ───────────────────────────

#[test]
fn recovery_after_wedged_returns_to_healthy() {
    let mut detector = WedgeDetector::new(cfg());
    let window = Duration::from_secs(10);
    for _ in 0..3 {
        detector.observe(hot_quiet_sample(window));
    }
    assert_eq!(detector.state(), WedgeState::Wedged);

    // Output resumes: one window with IO above epsilon clears it.
    let recovered = Sample {
        hottest_thread_user_delta: window,
        wall_delta: window,
        io_write_delta: 10_000,
    };
    assert_eq!(detector.observe(recovered), WedgeState::Healthy);
    assert_eq!(detector.streak_wall(), Duration::ZERO);
}

// ── boundary conditions ──────────────────────────────────────────────

#[test]
fn exactly_at_user_pct_threshold_counts_as_hot() {
    let mut detector = WedgeDetector::new(cfg());
    let window = Duration::from_secs(10);
    // Exactly 90% of the window.
    let sample = Sample {
        hottest_thread_user_delta: Duration::from_millis(9_000),
        wall_delta: window,
        io_write_delta: 0,
    };
    assert_eq!(detector.observe(sample), WedgeState::Suspect { streak: 1 });
}

#[test]
fn just_below_user_pct_threshold_is_healthy() {
    let mut detector = WedgeDetector::new(cfg());
    let window = Duration::from_secs(10);
    let sample = Sample {
        hottest_thread_user_delta: Duration::from_millis(8_999),
        wall_delta: window,
        io_write_delta: 0,
    };
    assert_eq!(detector.observe(sample), WedgeState::Healthy);
}

#[test]
fn io_write_delta_exactly_at_epsilon_counts_as_quiet() {
    let mut detector = WedgeDetector::new(cfg());
    let window = Duration::from_secs(10);
    let sample = Sample {
        hottest_thread_user_delta: window,
        wall_delta: window,
        io_write_delta: 4096, // == epsilon
    };
    assert_eq!(detector.observe(sample), WedgeState::Suspect { streak: 1 });
}

#[test]
fn io_write_delta_one_byte_over_epsilon_is_healthy() {
    let mut detector = WedgeDetector::new(cfg());
    let window = Duration::from_secs(10);
    let sample = Sample {
        hottest_thread_user_delta: window,
        wall_delta: window,
        io_write_delta: 4097,
    };
    assert_eq!(detector.observe(sample), WedgeState::Healthy);
}

#[test]
fn zero_wall_delta_is_never_hot() {
    let mut detector = WedgeDetector::new(cfg());
    // Defensive: a degenerate zero-duration window must not divide by
    // zero or panic, and must not count as "hot".
    let sample = Sample {
        hottest_thread_user_delta: Duration::from_secs(1),
        wall_delta: Duration::ZERO,
        io_write_delta: 0,
    };
    assert_eq!(detector.observe(sample), WedgeState::Healthy);
}

// ── streak_wall accumulation (drives the "no output for Xs" message) ─

#[test]
fn streak_wall_accumulates_across_qualifying_windows() {
    let mut detector = WedgeDetector::new(cfg());
    let window = Duration::from_secs(10);
    for _ in 0..3 {
        detector.observe(hot_quiet_sample(window));
    }
    assert_eq!(detector.state(), WedgeState::Wedged);
    assert_eq!(detector.streak_wall(), Duration::from_secs(30));
}

// ── default config sanity ────────────────────────────────────────────

#[test]
fn default_cfg_matches_documented_constants() {
    let cfg = WedgeDetectorCfg::default();
    assert_eq!(cfg.user_pct_threshold, DEFAULT_USER_PCT_THRESHOLD);
    assert_eq!(cfg.io_epsilon_bytes, DEFAULT_IO_EPSILON_BYTES);
    assert_eq!(cfg.required_streak, DEFAULT_REQUIRED_STREAK);
}

#[test]
fn default_window_times_streak_is_within_acceptance_band() {
    let total = DEFAULT_TICK * DEFAULT_REQUIRED_STREAK;
    assert!(total >= Duration::from_secs(60), "total={total:?}");
    assert!(total <= Duration::from_secs(120), "total={total:?}");
}

// ── watchdog cfg / lifecycle smoke tests ─────────────────────────────

#[test]
fn watchdog_cfg_new_is_enabled_with_defaults() {
    let cfg = WedgeWatchdogCfg::new(1234, "codex");
    assert!(cfg.enabled);
    assert_eq!(cfg.pid, 1234);
    assert_eq!(cfg.backend_label, "codex");
    assert_eq!(cfg.tick, DEFAULT_TICK);
    assert_eq!(cfg.required_streak, DEFAULT_REQUIRED_STREAK);
}

#[test]
fn watchdog_cfg_disabled_is_disabled() {
    let cfg = WedgeWatchdogCfg::disabled();
    assert!(!cfg.enabled);
}

#[test]
fn disabled_watchdog_spawns_no_thread_and_stops_cleanly() {
    let mut w = WedgeWatchdog::spawn(WedgeWatchdogCfg::disabled());
    w.stop();
    // Drop must also be a clean no-op.
}

#[test]
fn spawn_for_pid_none_is_inert() {
    let mut w = WedgeWatchdog::spawn_for_pid(None, "claude");
    w.stop();
}
