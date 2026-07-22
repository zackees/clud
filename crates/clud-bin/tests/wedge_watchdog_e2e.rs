//! Ignored end-to-end probe for #541 (wedge watchdog).
//!
//! The pure decision core (`WedgeDetector`) is exhaustively unit tested in
//! `crates/clud-bin/src/wedge_watchdog.rs` without touching the OS. These
//! tests instead drive the *real* Windows sampler (`Toolhelp32`,
//! `GetThreadTimes`, `GetProcessIoCounters`) through the public
//! `WedgeWatchdog` API against real spinning threads in this test process
//! itself — no separate testbin needed, since the sampler walks the process
//! subtree rooted at whatever pid it's given, and this test's own pid
//! qualifies.
//!
//! Ignored (like `win32_hooking_probe.rs`) because each test deliberately
//! pins a CPU core for real wall-clock seconds, which is unsuitable for the
//! normal CI matrix (`bash test` / `bash test --integration` never pass
//! `--include-ignored`). Run manually:
//!
//! ```bash
//! soldr cargo test -p clud --test wedge_watchdog_e2e -- --ignored --nocapture --test-threads=1
//! ```
//!
//! `--test-threads=1` matters: all three tests share one process, and the
//! sampler's "hottest thread" measurement would get noisy if two of these
//! tests' spinner threads were alive at once.

#[cfg(not(target_os = "windows"))]
#[test]
#[ignore = "Windows-only #541 wedge-watchdog E2E probe"]
fn wedge_watchdog_e2e_windows_only() {}

#[cfg(target_os = "windows")]
mod win {
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::mpsc;
    use std::sync::Arc;
    use std::thread::{self, JoinHandle};
    use std::time::{Duration, Instant};

    use clud::wedge_watchdog::{WedgeState, WedgeWatchdog, WedgeWatchdogCfg};

    /// Fast-cadence detector config so the E2E tests finish in a couple of
    /// seconds instead of the production 90s window. `required_streak: 4`
    /// with a 120ms tick needs ~5 qualifying ticks (1 baseline + 4 counted)
    /// ≈ 600ms of sustained signal before `Wedged` fires.
    fn fast_cfg(state_tx: mpsc::Sender<WedgeState>) -> WedgeWatchdogCfg {
        let mut cfg = WedgeWatchdogCfg::new(std::process::id(), "test");
        cfg.tick = Duration::from_millis(120);
        cfg.required_streak = 4;
        // Slightly below the 90% production default: this test process
        // shares the core with the OS scheduler and (occasionally) the
        // `cargo test` harness's own bookkeeping, so the spin thread won't
        // always hit a clean 100%.
        cfg.user_pct_threshold = 0.80;
        cfg.state_tx = Some(state_tx);
        cfg
    }

    /// Busy-spins one thread at ~100% of one core, no I/O, until `stop` is
    /// set. Checking an `AtomicBool` is not I/O — it doesn't touch the
    /// io-write-bytes signature the detector watches.
    fn spawn_quiet_spinner(stop: Arc<AtomicBool>) -> JoinHandle<()> {
        thread::Builder::new()
            .name("e2e-quiet-spinner".into())
            .spawn(move || {
                let mut acc: u64 = 0;
                while !stop.load(Ordering::Relaxed) {
                    for i in 0..200_000u64 {
                        acc = acc.wrapping_add(std::hint::black_box(i));
                    }
                }
                std::hint::black_box(acc);
            })
            .expect("spawn quiet spinner")
    }

    /// A second thread that periodically writes real bytes to a file (a
    /// stand-in for "TUI still emitting console output") while the spinner
    /// above keeps one thread pinned. Exercises the detector's "hot but not
    /// quiet" -> Healthy path against genuine `GetProcessIoCounters` deltas.
    fn spawn_writer(stop: Arc<AtomicBool>, path: std::path::PathBuf) -> JoinHandle<()> {
        thread::Builder::new()
            .name("e2e-writer".into())
            .spawn(move || {
                use std::io::Write;
                let mut file = std::fs::File::create(&path).expect("create writer sink");
                let chunk = vec![0xABu8; 8192];
                while !stop.load(Ordering::Relaxed) {
                    let _ = file.write_all(&chunk);
                    let _ = file.flush();
                    thread::sleep(Duration::from_millis(40));
                }
            })
            .expect("spawn writer thread")
    }

    /// One duty-cycled thread: busy for `active`, asleep for `idle`, on
    /// repeat. Used in groups so total subtree CPU is high but no single
    /// thread's per-window user% crosses the hot threshold.
    fn spawn_duty_thread(
        stop: Arc<AtomicBool>,
        active: Duration,
        idle: Duration,
    ) -> JoinHandle<()> {
        thread::Builder::new()
            .name("e2e-duty-spinner".into())
            .spawn(move || {
                let mut acc: u64 = 0;
                while !stop.load(Ordering::Relaxed) {
                    let until = Instant::now() + active;
                    while Instant::now() < until {
                        for i in 0..50_000u64 {
                            acc = acc.wrapping_add(std::hint::black_box(i));
                        }
                    }
                    thread::sleep(idle);
                }
                std::hint::black_box(acc);
            })
            .expect("spawn duty-cycle thread")
    }

    fn join_all(stop: &Arc<AtomicBool>, handles: Vec<JoinHandle<()>>) {
        stop.store(true, Ordering::Relaxed);
        for h in handles {
            let _ = h.join();
        }
    }

    /// AC1: "A synthetic child that spins one thread at 100% user-mode with
    /// zero console writes is flagged by the watchdog within the detection
    /// window." Here the "child" is this test process's own spinner thread,
    /// sampled by the real Windows subtree walker.
    #[test]
    #[ignore = "pins a CPU core for ~1s; run manually (see module docs)"]
    fn e2e_spin_with_no_output_reaches_wedged() {
        let stop = Arc::new(AtomicBool::new(false));
        let spinner = spawn_quiet_spinner(Arc::clone(&stop));

        let (tx, rx) = mpsc::channel();
        let _watchdog = WedgeWatchdog::spawn(fast_cfg(tx));

        let deadline = Instant::now() + Duration::from_secs(5);
        let mut reached_wedged = false;
        while Instant::now() < deadline {
            if let Ok(state) = rx.recv_timeout(Duration::from_millis(250)) {
                if state == WedgeState::Wedged {
                    reached_wedged = true;
                    break;
                }
            }
        }

        join_all(&stop, vec![spinner]);
        assert!(
            reached_wedged,
            "expected WedgeState::Wedged within 5s of a real quiet CPU spin"
        );
    }

    /// AC2 (part 1): "A busy-but-healthy child (high CPU **with** console
    /// output) ... NOT flagged." Same CPU-pinned thread as above, plus a
    /// second thread doing real periodic file writes standing in for TUI
    /// output.
    #[test]
    #[ignore = "pins a CPU core for ~1.5s; run manually (see module docs)"]
    fn e2e_spin_with_periodic_output_stays_healthy() {
        let dir = std::env::temp_dir();
        let sink = dir.join(format!("clud_wedge_e2e_writer_{}.bin", std::process::id()));

        let stop = Arc::new(AtomicBool::new(false));
        let spinner = spawn_quiet_spinner(Arc::clone(&stop));
        let writer = spawn_writer(Arc::clone(&stop), sink.clone());

        let (tx, rx) = mpsc::channel();
        let _watchdog = WedgeWatchdog::spawn(fast_cfg(tx));

        let deadline = Instant::now() + Duration::from_millis(1500);
        let mut saw_wedged = false;
        while Instant::now() < deadline {
            if let Ok(state) = rx.recv_timeout(Duration::from_millis(100)) {
                if state == WedgeState::Wedged {
                    saw_wedged = true;
                }
            }
        }

        join_all(&stop, vec![spinner, writer]);
        let _ = std::fs::remove_file(&sink);
        assert!(
            !saw_wedged,
            "a CPU-pinned thread with real concurrent IO writes must never reach Wedged"
        );
    }

    /// AC2 (part 2): "... or multi-thread compute) [is] NOT flagged." Spread
    /// load across several duty-cycled threads so no single thread holds
    /// the hot threshold.
    #[test]
    #[ignore = "spins several threads for ~1.5s; run manually (see module docs)"]
    fn e2e_multi_thread_spread_load_stays_healthy() {
        let stop = Arc::new(AtomicBool::new(false));
        let handles: Vec<JoinHandle<()>> = (0..4)
            .map(|_| {
                spawn_duty_thread(
                    Arc::clone(&stop),
                    Duration::from_millis(15),
                    Duration::from_millis(85),
                )
            })
            .collect();

        let (tx, rx) = mpsc::channel();
        let _watchdog = WedgeWatchdog::spawn(fast_cfg(tx));

        let deadline = Instant::now() + Duration::from_millis(1500);
        let mut saw_wedged = false;
        while Instant::now() < deadline {
            if let Ok(state) = rx.recv_timeout(Duration::from_millis(100)) {
                if state == WedgeState::Wedged {
                    saw_wedged = true;
                }
            }
        }

        join_all(&stop, handles);
        assert!(
            !saw_wedged,
            "spread multi-thread load (no single dominant thread) must never reach Wedged"
        );
    }
}
