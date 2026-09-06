use super::*;

fn cfg_with(num_cpus: usize) -> CpuBannerCfg {
    CpuBannerCfg {
        enabled: true,
        originator_pid: 1,
        num_cpus,
        heartbeat_secs: 30,
        tick: DEFAULT_TICK,
        sustained_ticks: DEFAULT_SUSTAINED_TICKS,
    }
}

fn sample(at: Instant, cpu: f32, rss: u64, count: usize) -> Sample {
    Sample {
        at,
        subtree_cpu_pct: cpu,
        subtree_rss_bytes: rss,
        proc_count: count,
        oldest_age: Duration::from_secs(60),
    }
}

#[test]
fn trigger_floor_at_50pct_for_one_cpu() {
    assert_eq!(cfg_with(1).trigger_pct(), 50.0);
}

#[test]
fn trigger_relative_kicks_in_at_4_cpus() {
    // 4 × 100 × 0.20 = 80 > 50 floor.
    assert_eq!(cfg_with(4).trigger_pct(), 80.0);
}

#[test]
fn trigger_at_12_cpus_matches_issue_example() {
    // 12 × 100 × 0.20 = 240 (within f32 rounding); user explicitly
    // mentioned 300 % on a 12-CPU system as a value that should fire.
    assert!((cfg_with(12).trigger_pct() - 240.0).abs() < 0.01);
    // 300 % comfortably exceeds 240 % trigger.
    assert!(300.0 >= cfg_with(12).trigger_pct());
}

#[test]
fn trigger_at_32_cpus_uses_relative() {
    assert_eq!(cfg_with(32).trigger_pct(), 640.0);
}

#[test]
fn disabled_cfg_never_emits() {
    let mut state = CpuBannerState::default();
    let cfg = CpuBannerCfg::disabled();
    let now = Instant::now();
    for i in 0..10 {
        // 100,000% would otherwise blow past every threshold.
        let s = sample(now + Duration::from_secs(i), 100_000.0, u64::MAX, 999);
        assert!(state.poll(s, &cfg).is_none(), "tick {i} should not emit");
    }
}

#[test]
fn below_trigger_never_emits() {
    let mut state = CpuBannerState::default();
    let cfg = cfg_with(12); // trigger=240
    let now = Instant::now();
    for i in 0..10 {
        let s = sample(now + Duration::from_secs(i * 2), 100.0, 1 << 30, 5);
        assert!(state.poll(s, &cfg).is_none());
    }
}

#[test]
fn crossover_requires_three_sustained_ticks() {
    let mut state = CpuBannerState::default();
    let cfg = cfg_with(1); // trigger=50
    let now = Instant::now();

    // Two ticks above: no banner yet (sustained_ticks=3).
    for i in 0..2 {
        let s = sample(now + Duration::from_secs(i * 2), 80.0, 0, 1);
        assert!(state.poll(s, &cfg).is_none(), "tick {i}");
    }
    // Third tick fires the crossover.
    let s = sample(now + Duration::from_secs(4), 80.0, 0, 1);
    let line = state.poll(s, &cfg).expect("crossover should fire");
    assert_eq!(line.kind, BannerKind::Crossover);
    assert!((line.cpu_pct - 80.0).abs() < 0.01);
}

#[test]
fn single_dip_resets_sustained_counter() {
    let mut state = CpuBannerState::default();
    let cfg = cfg_with(1);
    let now = Instant::now();

    // Two above, one below, two above → no crossover (counter reset).
    for i in 0..2 {
        assert!(state
            .poll(sample(now + Duration::from_secs(i * 2), 80.0, 0, 1), &cfg)
            .is_none());
    }
    assert!(state
        .poll(sample(now + Duration::from_secs(4), 10.0, 0, 1), &cfg)
        .is_none());
    // Need 3 more above-ticks now.
    for i in 0..2 {
        assert!(
            state
                .poll(
                    sample(now + Duration::from_secs(6 + i * 2), 80.0, 0, 1),
                    &cfg
                )
                .is_none(),
            "post-dip tick {i} should not fire yet"
        );
    }
    let line = state
        .poll(sample(now + Duration::from_secs(10), 80.0, 0, 1), &cfg)
        .expect("third post-dip tick fires");
    assert_eq!(line.kind, BannerKind::Crossover);
}

#[test]
fn sustained_heartbeat_after_30s() {
    let mut state = CpuBannerState::default();
    let cfg = cfg_with(1);
    let now = Instant::now();

    // Drive through crossover at t=4s.
    for i in 0..3 {
        state.poll(sample(now + Duration::from_secs(i * 2), 80.0, 0, 1), &cfg);
    }
    assert!(state.in_episode);

    // 28s later: no heartbeat yet.
    assert!(state
        .poll(sample(now + Duration::from_secs(32), 80.0, 0, 1), &cfg)
        .is_none());
    // 34s later (30s after last print at t=4s): heartbeat fires.
    let line = state
        .poll(sample(now + Duration::from_secs(34), 80.0, 0, 1), &cfg)
        .expect("heartbeat should fire");
    assert_eq!(line.kind, BannerKind::Sustained);
}

#[test]
fn hysteretic_dropout_only_below_07_factor() {
    let mut state = CpuBannerState::default();
    let cfg = cfg_with(1); // trigger=50, clear=35
    let now = Instant::now();
    for i in 0..3 {
        state.poll(sample(now + Duration::from_secs(i * 2), 80.0, 0, 1), &cfg);
    }
    assert!(state.in_episode);

    // 40% is below trigger (50) but above clear (35) → no banner,
    // still in episode.
    assert!(state
        .poll(sample(now + Duration::from_secs(6), 40.0, 0, 1), &cfg)
        .is_none());
    assert!(
        state.in_episode,
        "between trigger and clear, stay in episode"
    );
}

#[test]
fn clear_banner_fires_only_for_long_episodes() {
    let mut state = CpuBannerState::default();
    let cfg = cfg_with(1);
    let now = Instant::now();
    // Crossover at t=4s. Drop at t=10s → episode age = 6s, below
    // MIN_EPISODE_FOR_CLEAR_SECS (60) → no clear banner.
    for i in 0..3 {
        state.poll(sample(now + Duration::from_secs(i * 2), 80.0, 0, 1), &cfg);
    }
    assert!(state
        .poll(sample(now + Duration::from_secs(10), 0.0, 0, 1), &cfg)
        .is_none());
    assert!(!state.in_episode);
}

#[test]
fn clear_banner_fires_after_long_episode() {
    let mut state = CpuBannerState::default();
    let cfg = cfg_with(1);
    let now = Instant::now();
    // Crossover at t=4s. Drop at t=70s → episode age = 66s ≥ 60s.
    for i in 0..3 {
        state.poll(sample(now + Duration::from_secs(i * 2), 80.0, 0, 1), &cfg);
    }
    let line = state
        .poll(sample(now + Duration::from_secs(70), 0.0, 0, 1), &cfg)
        .expect("clear banner should fire");
    assert_eq!(line.kind, BannerKind::Clear);
}

#[test]
fn suppression_holds_next_crossover_after_clear() {
    let mut state = CpuBannerState::default();
    let cfg = cfg_with(1);
    let now = Instant::now();
    // Long episode → clear → suppression armed for 60s.
    for i in 0..3 {
        state.poll(sample(now + Duration::from_secs(i * 2), 80.0, 0, 1), &cfg);
    }
    assert_eq!(
        state
            .poll(sample(now + Duration::from_secs(70), 0.0, 0, 1), &cfg)
            .unwrap()
            .kind,
        BannerKind::Clear
    );

    // Within 60s of clear: even sustained high CPU is suppressed.
    for i in 0..3 {
        let s = sample(now + Duration::from_secs(72 + i * 2), 200.0, 0, 1);
        assert!(state.poll(s, &cfg).is_none(), "tick {i} suppressed");
    }
    // After suppression window (clear at 70 + 60 = 130s), crossover
    // can fire again after 3 sustained ticks.
    for i in 0..2 {
        let s = sample(now + Duration::from_secs(132 + i * 2), 200.0, 0, 1);
        state.poll(s, &cfg);
    }
    let line = state
        .poll(sample(now + Duration::from_secs(136), 200.0, 0, 1), &cfg)
        .expect("crossover after suppression");
    assert_eq!(line.kind, BannerKind::Crossover);
}

#[test]
fn render_plain_matches_acceptance_format() {
    let line = BannerLine {
        kind: BannerKind::Crossover,
        cpu_pct: 287.0,
        rss_bytes: (1.42_f64 * 1024.0 * 1024.0 * 1024.0) as u64,
        proc_count: 24,
        age: Duration::from_secs(7 * 60),
        num_cpus: 12,
        trigger_pct: 240.0,
    };
    let s = line.render_plain();
    // 287 / 100 = 2.87 → formats as "2.9" with `{:.1}`.
    assert!(
        s.starts_with("[clud] cpu 287 % · 2.9 / 12 cores · rss 1.42 GiB"),
        "{s}"
    );
    assert!(s.contains("24 procs"), "{s}");
    assert!(s.contains("7 m"), "{s}");
}

#[test]
fn render_clear_format_has_no_cpu_number() {
    let line = BannerLine {
        kind: BannerKind::Clear,
        cpu_pct: 10.0,
        rss_bytes: 100 * 1024 * 1024,
        proc_count: 2,
        age: Duration::from_secs(2 * 60),
        num_cpus: 4,
        trigger_pct: 80.0,
    };
    let s = line.render_plain();
    assert!(s.starts_with("[clud] cpu back to normal"), "{s}");
    assert!(!s.contains('%'), "clear banner shouldn't show a pct: {s}");
}

#[test]
fn render_styles_scale_with_severity() {
    let line = |ratio: f32| BannerLine {
        kind: BannerKind::Crossover,
        cpu_pct: 100.0 * ratio,
        rss_bytes: 0,
        proc_count: 1,
        age: Duration::from_secs(0),
        num_cpus: 1,
        trigger_pct: 100.0,
    };
    // 1.5× → dim
    assert!(line(1.5).render().contains("\x1b[2m"));
    // 2.5× → yellow
    assert!(line(2.5).render().contains("\x1b[33m"));
    // 4.5× → red
    assert!(line(4.5).render().contains("\x1b[31"));
    // Clear → no style
    let clear = BannerLine {
        kind: BannerKind::Clear,
        cpu_pct: 0.0,
        rss_bytes: 0,
        proc_count: 1,
        age: Duration::from_secs(120),
        num_cpus: 1,
        trigger_pct: 100.0,
    };
    assert!(!clear.render().contains("\x1b["), "{}", clear.render());
}

#[test]
fn format_rss_picks_gib_at_threshold() {
    assert_eq!(format_rss(1024 * 1024 * 1024), "1.00 GiB");
    assert_eq!(format_rss(512 * 1024 * 1024), "512 MiB");
    assert_eq!(format_rss(0), "0 MiB");
}

#[test]
fn format_age_buckets_into_human_units() {
    assert_eq!(format_age(Duration::from_secs(45)), "45 s");
    assert_eq!(format_age(Duration::from_secs(120)), "2 m");
    assert_eq!(format_age(Duration::from_secs(7200)), "2 h");
}

/// Sysinfo sampler smoke test: against the test process itself.
/// We can't predict the exact CPU% but we can assert the call works
/// and returns sensible values (proc_count >= 1, RSS > 0).
#[test]
fn sampler_returns_at_least_self() {
    let mut sampler = Sampler::new();
    let self_pid = std::process::id();
    // Two ticks separated by enough time for sysinfo to compute cpu%.
    let _ = sampler.tick(self_pid);
    std::thread::sleep(Duration::from_millis(250));
    let s = sampler.tick(self_pid);
    assert!(
        s.proc_count >= 1,
        "expected at least self in subtree, got {}",
        s.proc_count
    );
    assert!(s.subtree_rss_bytes > 0, "self RSS should be non-zero");
    assert!(
        s.subtree_cpu_pct >= 0.0,
        "cpu_pct should be non-negative, got {}",
        s.subtree_cpu_pct
    );
}

/// `BannerWatcher::spawn` with `enabled = false` returns an inert
/// handle that no-ops on `stop()` and `Drop`.
#[test]
fn disabled_watcher_is_inert() {
    let mut w = BannerWatcher::spawn(CpuBannerCfg::disabled());
    w.stop();
    // Drop is fine — should not panic / hang.
}

// -- Issue #540: adaptive sample interval + targeted-refresh pid list --

#[test]
fn sample_interval_small_subtree_uses_default_tick() {
    assert_eq!(sample_interval(0), DEFAULT_TICK);
    assert_eq!(sample_interval(1), DEFAULT_TICK);
    assert_eq!(sample_interval(25), DEFAULT_TICK, "25 is the <=25 boundary");
}

#[test]
fn sample_interval_medium_subtree_backs_off_to_5s() {
    assert_eq!(
        sample_interval(26),
        Duration::from_secs(5),
        "26 crosses into 26-50"
    );
    assert_eq!(
        sample_interval(50),
        Duration::from_secs(5),
        "50 is the 26-50 boundary"
    );
}

#[test]
fn sample_interval_large_subtree_backs_off_to_10s() {
    assert_eq!(
        sample_interval(51),
        Duration::from_secs(10),
        "51 crosses into >50"
    );
    assert_eq!(sample_interval(500), Duration::from_secs(10));
}

/// Pid-list building exercised against a hand-built (mocked) process
/// tree — no `sysinfo::System` involved. Verifies the DFS walk
/// includes root + all descendants and excludes unrelated subtrees.
#[test]
fn collect_subtree_from_children_walks_mocked_tree() {
    let root = Pid::from_u32(1);
    let mut children: HashMap<Pid, Vec<Pid>> = HashMap::new();
    // root(1) -> 2, 3 ; 2 -> 4 ; 4 -> 5 (deep chain)
    children.insert(root, vec![Pid::from_u32(2), Pid::from_u32(3)]);
    children.insert(Pid::from_u32(2), vec![Pid::from_u32(4)]);
    children.insert(Pid::from_u32(4), vec![Pid::from_u32(5)]);
    // Unrelated subtree rooted elsewhere must not leak in.
    children.insert(Pid::from_u32(99), vec![Pid::from_u32(100)]);

    let mut pids: Vec<u32> = collect_subtree_from_children(&children, root)
        .into_iter()
        .map(Pid::as_u32)
        .collect();
    pids.sort_unstable();
    assert_eq!(pids, vec![1, 2, 3, 4, 5]);
}

#[test]
fn collect_subtree_from_children_root_with_no_children() {
    let root = Pid::from_u32(42);
    let children: HashMap<Pid, Vec<Pid>> = HashMap::new();
    let pids = collect_subtree_from_children(&children, root);
    assert_eq!(pids, vec![root]);
}

/// Pure decision logic for the tree-rebuild cadence: empty cache or a
/// walk older than `TREE_REBUILD_INTERVAL` forces a rebuild; a recent
/// walk does not. Exercised without any real timing/sleep by doing
/// `Instant` arithmetic instead of `Instant::now()` deltas.
#[test]
fn needs_tree_rebuild_pure_decision() {
    let t0 = Instant::now();
    let base = TREE_REBUILD_INTERVAL;
    assert!(
        needs_tree_rebuild(true, Some(t0), t0, base),
        "empty cache always rebuilds"
    );
    assert!(
        needs_tree_rebuild(false, None, t0, base),
        "no prior walk always rebuilds"
    );
    let just_under = t0 + base - Duration::from_millis(1);
    assert!(
        !needs_tree_rebuild(false, Some(t0), just_under, base),
        "under the interval should reuse the cached list"
    );
    let at_or_over = t0 + base;
    assert!(
        needs_tree_rebuild(false, Some(t0), at_or_over, base),
        "at/over the interval should rebuild"
    );
}

/// #709: the interval is a parameter now, so a backed-off sampler really
/// does skip walks it used to take.
#[test]
fn a_backed_off_interval_suppresses_a_walk_the_base_interval_would_take() {
    let t0 = Instant::now();
    let at_base = t0 + TREE_REBUILD_INTERVAL;
    assert!(
        needs_tree_rebuild(false, Some(t0), at_base, TREE_REBUILD_INTERVAL),
        "sanity: the base interval rebuilds here"
    );
    assert!(
        !needs_tree_rebuild(false, Some(t0), at_base, TREE_REBUILD_IDLE_INTERVAL),
        "the backed-off interval must not rebuild yet"
    );
    assert!(needs_tree_rebuild(
        false,
        Some(t0),
        t0 + TREE_REBUILD_IDLE_INTERVAL,
        TREE_REBUILD_IDLE_INTERVAL
    ));
}

#[test]
fn rebuild_cadence_starts_at_the_base_interval() {
    assert_eq!(RebuildCadence::new().interval(), TREE_REBUILD_INTERVAL);
}

#[test]
fn rebuild_cadence_holds_until_enough_quiet_walks() {
    let mut cadence = RebuildCadence::new();
    for _ in 0..(REBUILD_QUIET_WALKS_BEFORE_BACKOFF - 1) {
        cadence = cadence.record_walk(true);
        assert_eq!(cadence.interval(), TREE_REBUILD_INTERVAL);
    }
    cadence = cadence.record_walk(true);
    assert_eq!(cadence.interval(), TREE_REBUILD_INTERVAL * 2);
}

#[test]
fn sustained_quiet_reaches_the_ceiling_and_stops() {
    let mut cadence = RebuildCadence::new();
    for _ in 0..200 {
        cadence = cadence.record_walk(true);
    }
    assert_eq!(
        cadence.interval(),
        TREE_REBUILD_IDLE_INTERVAL,
        "backoff must clamp, not grow without bound"
    );
}

/// Responsiveness is the feature; the backoff only makes idling cheap.
/// A busy walk must return to the base interval in one step, never a
/// gradual ramp — a session that just started building needs its new
/// descendants discovered now.
#[test]
fn any_activity_snaps_the_rebuild_cadence_straight_back() {
    let mut cadence = RebuildCadence::new();
    for _ in 0..200 {
        cadence = cadence.record_walk(true);
    }
    assert_eq!(cadence.interval(), TREE_REBUILD_IDLE_INTERVAL);
    assert_eq!(
        cadence.record_walk(false).interval(),
        TREE_REBUILD_INTERVAL,
        "a busy walk must reset in one step"
    );
}

/// Issue #540 acceptance criterion: measured sampler cost for a 50+
/// process subtree. `#[ignore]`d — spawns real child processes and
/// takes >1 s; run manually via:
/// `soldr cargo test -p clud-bin --lib cpu_banner::tests::bench_sampler_cost_50_procs -- --ignored --nocapture`
///
/// Compares the old-behavior full-refresh-every-tick cost against the
/// new targeted-refresh cost for the same subtree, so the delta (not
/// just the absolute number, which is host-dependent) documents the
/// fix. See the PR body for a captured run's numbers.
#[test]
#[ignore]
fn bench_sampler_cost_50_procs() {
    use running_process::{CommandSpec, NativeProcess, ProcessConfig, StderrMode, StdinMode};

    const SPAWN_COUNT: usize = 55;
    let mut children: Vec<NativeProcess> = Vec::new();
    for _ in 0..SPAWN_COUNT {
        // Windows: `ping -n 31 127.0.0.1` ≈ a 30 s sleep. Deliberately
        // NOT `cmd /C timeout /T 30` — with Git-for-Windows on PATH,
        // `timeout` can resolve to GNU coreutils' timeout, which
        // rejects `/T` and exits instantly, collapsing the subtree
        // this bench is supposed to measure.
        let command = if cfg!(windows) {
            CommandSpec::Argv(vec![
                "ping".to_string(),
                "-n".to_string(),
                "31".to_string(),
                "127.0.0.1".to_string(),
            ])
        } else {
            CommandSpec::Argv(vec!["sleep".to_string(), "30".to_string()])
        };
        let c = NativeProcess::new(ProcessConfig {
            command,
            cwd: None,
            env: None,
            capture: false,
            stderr_mode: StderrMode::Stdout,
            creationflags: None,
            create_process_group: false,
            stdin_mode: StdinMode::Null,
            nice: None,
        });
        if c.start().is_ok() {
            children.push(c);
        }
    }
    // Let the OS register the new process-table entries.
    std::thread::sleep(Duration::from_millis(500));

    let self_pid = std::process::id();
    let mut sampler = Sampler::new();
    let primed = sampler.tick(self_pid);
    println!("subtree size after spawn: {}", primed.proc_count);

    const ITERS: u32 = 20;

    // Old-behavior baseline: force a full rebuild every tick.
    let full_start = Instant::now();
    for _ in 0..ITERS {
        sampler.force_rebuild_next_tick();
        sampler.tick(self_pid);
    }
    let full_elapsed = full_start.elapsed();

    // New behavior: within the rebuild window, ticks are targeted.
    let targeted_start = Instant::now();
    for _ in 0..ITERS {
        sampler.tick(self_pid);
    }
    let targeted_elapsed = targeted_start.elapsed();

    println!(
        "full-refresh: {ITERS} ticks in {full_elapsed:?} ({:?}/tick)",
        full_elapsed / ITERS
    );
    println!(
        "targeted-refresh: {ITERS} ticks in {targeted_elapsed:?} ({:?}/tick)",
        targeted_elapsed / ITERS
    );

    for c in &children {
        let _ = c.kill();
        let _ = c.wait(Some(Duration::from_secs(2)));
    }
}

// -- #1172: the stop is bounded ------------------------------------------

#[test]
fn a_disabled_watcher_stops_inert() {
    let mut watcher = BannerWatcher::spawn(CpuBannerCfg::disabled());
    assert_eq!(watcher.stop(), StopOutcome::Inert);
    assert_eq!(watcher.stop(), StopOutcome::Inert, "idempotent");
}

#[test]
fn a_responsive_thread_is_joined() {
    let mut watcher = BannerWatcher::spawn_body(|stop_rx| {
        let _ = stop_rx.recv();
    });
    let started = Instant::now();
    assert_eq!(watcher.stop(), StopOutcome::Joined);
    assert!(started.elapsed() < STOP_JOIN_BUDGET, "{:?}", started.elapsed());
    assert_eq!(watcher.stop(), StopOutcome::Inert, "idempotent");
}

/// The #1172 failure shape: the thread is inside a refresh and cannot see
/// the stop signal until it returns. The old unbounded join waited for the
/// whole refresh; a `clud -p` exit must not.
#[test]
fn a_thread_that_misses_the_deadline_is_detached_not_waited_for() {
    let (release_tx, release_rx) = std::sync::mpsc::channel::<()>();
    let mut watcher = BannerWatcher::spawn_body(move |_stop_rx| {
        // Stand-in for a sysinfo refresh that outlives the budget.
        let _ = release_rx.recv();
    });
    let budget = Duration::from_millis(50);
    let started = Instant::now();
    assert_eq!(watcher.stop_within(budget), StopOutcome::Detached);
    let waited = started.elapsed();
    assert!(waited >= budget, "{waited:?}");
    assert!(
        waited < budget * 10,
        "detach must not wait on the thread: {waited:?}"
    );
    // Let the detached thread finish so the test process exits cleanly.
    let _ = release_tx.send(());
    assert_eq!(watcher.stop(), StopOutcome::Inert, "nothing left to stop");
}

#[test]
fn a_body_that_panics_still_counts_as_finished() {
    let mut watcher = BannerWatcher::spawn_body(|_stop_rx| {
        panic!("sampler exploded");
    });
    // The `finished` channel closes on unwind, so this is a join, not a
    // budget-long wait followed by a detach.
    assert_eq!(watcher.stop(), StopOutcome::Joined);
}
