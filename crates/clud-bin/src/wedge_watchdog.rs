//! Issue #541: wedge watchdog for a monitored backend child process.
//!
//! ## The incident
//!
//! A live `clud --codex` session (2026-07-22, Windows 10, WT 1.24) showed
//! `codex.exe`'s TUI frozen while the agent kept working underneath. Live
//! diagnosis found: 37 threads total, one thread permanently `Running` and
//! consuming 46.5 of the process's 48.6 CPU-minutes (97% user-mode — a pure
//! busy-loop, not a stuck syscall), while process IO read/write bytes sat at
//! 0 B/s for the whole ~48 minute episode. Root cause is upstream
//! (openai/codex#33755, a `syntect` highlighting livelock; fixed on codex's
//! main), but `clud` owns the UX, launched the process, and gave the user no
//! signal that tokens were burning against a dead UI.
//!
//! ## Detection signature
//!
//! Over [`DEFAULT_REQUIRED_STREAK`] consecutive [`DEFAULT_TICK`]-spaced
//! observation windows (9 × 10 s ≈ 90 s, inside the 60-120 s acceptance
//! band), the monitored child's process subtree must show **both**:
//!
//! - (a) a single thread consuming ≥ [`DEFAULT_USER_PCT_THRESHOLD`] (90 %)
//!   of one core in user-mode time, and
//! - (b) subtree IO write-bytes delta ≤ [`DEFAULT_IO_EPSILON_BYTES`] (a
//!   small epsilon, not a strict zero, so incidental heartbeat writes don't
//!   mask a real wedge).
//!
//! Either condition failing resets the streak — this rules out both
//! "busy but healthy" processes (high CPU *with* output) and spread
//! multi-thread compute (no single thread dominates).
//!
//! ## Architecture
//!
//! - [`WedgeDetector`] is the pure, platform-free decision core: feed it
//!   [`Sample`] values (wall-clock deltas the caller already measured) and
//!   read back a [`WedgeState`]. No I/O, no OS calls — exhaustively unit
//!   tested below without needing a real spinning process.
//! - The `win` submodule (Windows-only) is the platform sampler: it walks
//!   the process subtree rooted at the monitored pid via
//!   `CreateToolhelp32Snapshot`, finds the single hottest thread via
//!   `OpenThread` + `GetThreadTimes`, and reads that thread's owning
//!   process's IO write bytes via `OpenProcess` + `GetProcessIoCounters`.
//! - [`WedgeWatchdog`] is the background-thread wiring (same shape as
//!   [`crate::cpu_banner::BannerWatcher`]): spawns a sampler + detector loop,
//!   prints one rate-limited warning per wedge episode, and logs the
//!   measured signature via [`crate::verbose_log`].
//! - Non-Windows builds compile [`WedgeWatchdog::spawn`] to a no-op (no
//!   thread spawned) — detection is unavailable there today, so the CI
//!   matrix still passes cleanly.

use std::sync::mpsc;
#[cfg(windows)]
use std::thread;
use std::thread::JoinHandle;
use std::time::Duration;

/// Tick cadence for the background sampler. 9 ticks × 10 s ≈ 90 s total,
/// inside the issue's 60-120 s acceptance band for detection latency.
pub const DEFAULT_TICK: Duration = Duration::from_secs(10);

/// Consecutive qualifying windows required before [`WedgeDetector`] reports
/// [`WedgeState::Wedged`]. `DEFAULT_TICK * DEFAULT_REQUIRED_STREAK` = 90 s.
pub const DEFAULT_REQUIRED_STREAK: u32 = 9;

/// Fraction of one core's user-mode time a single thread must sustain to
/// count as "hot". The live incident measured 97% user-mode on the pinned
/// thread; 90% leaves headroom for sampling jitter while still requiring a
/// genuine single-thread pin (a well-behaved TUI's render thread wakes,
/// draws, and sleeps — it doesn't sit at 90%+ of one core).
pub const DEFAULT_USER_PCT_THRESHOLD: f64 = 0.90;

/// Small epsilon for "no console output", not a strict zero, so incidental
/// heartbeat/log writes from an otherwise-dead TUI don't mask a real wedge.
/// The live incident measured exactly 0 B/s; 4 KiB over a 10 s window is
/// generous slack in the other direction.
pub const DEFAULT_IO_EPSILON_BYTES: u64 = 4096;

/// Process-level user-time fraction that arms the expensive per-thread walk
/// (#709).
///
/// Deliberately *below* [`DEFAULT_USER_PCT_THRESHOLD`]: a thread's user time
/// can never exceed its process's, so any value at or above the detector's own
/// threshold would already be sound — the slack exists so the gate can never be
/// the component that decides a borderline case. It only ever answers "is the
/// expensive measurement worth taking".
///
/// Gated like [`crate::console_title::KeeperCadence`]: consumed by the
/// Windows-only sampler, and by the Tier-1 tests on every platform.
#[cfg(any(windows, test))]
pub(crate) const GATE_USER_PCT_THRESHOLD: f64 = 0.80;

/// The gate must arm *below* the detector's threshold, or it could skip the
/// measurement for a subtree that is about to qualify as wedged. Enforced at
/// compile time rather than by a test: it is a relationship between two
/// constants, so a build failure is the honest signal.
#[cfg(any(windows, test))]
const _: () = assert!(GATE_USER_PCT_THRESHOLD < DEFAULT_USER_PCT_THRESHOLD);

// ─── Pure cores for the two #709 changes ───────────────────────────────
//
// Both live here rather than in the Windows sampler so they are asserted on
// every platform CI builds for, the same discipline `WedgeDetector` follows.

/// Could any single thread in the subtree be at the wedge threshold?
///
/// Compares each process's *user-mode* delta against `gate_pct * wall`. A
/// thread's user time can never exceed its process's, so a `false` here proves
/// no thread qualifies and the caller may skip the host-wide thread walk.
///
/// **Fails open.** Returns `true` whenever the question cannot be answered —
/// no wall delta, no baseline, an empty reading, or a pid with no previous
/// sample. This gate exists to save work on healthy sessions, never to decide
/// that a session is healthy.
///
/// Only the Windows sampler calls this in production, but the decision is
/// platform-free so the tests exercise it everywhere.
#[cfg(any(windows, test))]
pub(crate) fn subtree_could_hide_a_hot_thread(
    prev_process_user_100ns: &std::collections::HashMap<u32, u64>,
    cur_process_user_100ns: &std::collections::HashMap<u32, u64>,
    wall_delta: Option<Duration>,
    gate_pct: f64,
) -> bool {
    let Some(wall) = wall_delta else {
        return true;
    };
    if prev_process_user_100ns.is_empty() || cur_process_user_100ns.is_empty() {
        return true;
    }
    let wall_100ns = (wall.as_nanos() / 100) as u64;
    if wall_100ns == 0 {
        return true;
    }
    let need = ((wall_100ns as f64) * gate_pct) as u64;
    cur_process_user_100ns.iter().any(|(pid, user)| {
        prev_process_user_100ns
            .get(pid)
            // A pid with no baseline is new this tick; we cannot rule it out.
            .is_none_or(|prev| user.saturating_sub(*prev) >= need)
    })
}

/// Every descendant of `root` in a parent→children adjacency map, including
/// `root` itself.
///
/// **The visited set is load-bearing, not tidiness.** Windows recycles PIDs, so
/// a Toolhelp parent-pid graph can contain a cycle — a recycled number becoming
/// its own ancestor. Without it this walk never terminates and the output grows
/// without bound (#709).
///
/// Only the Windows sampler calls this in production; the walk itself is
/// platform-free so the cycle cases are asserted everywhere.
#[cfg(any(windows, test))]
pub(crate) fn descendants_of(
    children: &std::collections::HashMap<u32, Vec<u32>>,
    root: u32,
) -> Vec<u32> {
    let mut seen = std::collections::HashSet::from([root]);
    let mut stack = vec![root];
    let mut out = vec![root];
    while let Some(cur) = stack.pop() {
        let Some(kids) = children.get(&cur) else {
            continue;
        };
        for &kid in kids {
            if seen.insert(kid) {
                out.push(kid);
                stack.push(kid);
            }
        }
    }
    out
}

// ─── Pure decision core ────────────────────────────────────────────────

/// One observation window's measurement, fed into [`WedgeDetector::observe`].
/// Platform-free: the caller (production sampler or a test) has already done
/// all the OS-specific work and reduced it to three deltas.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Sample {
    /// User-mode CPU time consumed by the single hottest thread in the
    /// monitored subtree during this window.
    pub hottest_thread_user_delta: Duration,
    /// Wall-clock time elapsed since the previous sample.
    pub wall_delta: Duration,
    /// IO write-bytes delta (`IO_COUNTERS::WriteTransferCount` on Windows)
    /// for the process that owns the hottest thread, during this window.
    pub io_write_delta: u64,
}

/// Detector state. `Suspect` carries the current streak so callers/tests can
/// observe progress toward `Wedged` without waiting for the full window.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WedgeState {
    Healthy,
    Suspect { streak: u32 },
    Wedged,
}

/// Tunable thresholds for [`WedgeDetector`]. See the `DEFAULT_*` constants
/// for rationale; callers (tests, or a future settings.json knob) can
/// override any of them.
#[derive(Debug, Clone, Copy)]
pub struct WedgeDetectorCfg {
    pub user_pct_threshold: f64,
    pub io_epsilon_bytes: u64,
    pub required_streak: u32,
}

impl Default for WedgeDetectorCfg {
    fn default() -> Self {
        Self {
            user_pct_threshold: DEFAULT_USER_PCT_THRESHOLD,
            io_epsilon_bytes: DEFAULT_IO_EPSILON_BYTES,
            required_streak: DEFAULT_REQUIRED_STREAK,
        }
    }
}

/// Pure state machine. No I/O, no OS calls — drive it from any sampler, or
/// directly from a test.
#[derive(Debug)]
pub struct WedgeDetector {
    cfg: WedgeDetectorCfg,
    streak: u32,
    streak_wall: Duration,
    state: WedgeState,
}

impl WedgeDetector {
    pub fn new(cfg: WedgeDetectorCfg) -> Self {
        Self {
            cfg,
            streak: 0,
            streak_wall: Duration::ZERO,
            state: WedgeState::Healthy,
        }
    }

    /// Feed one window's measurement. Both the single-thread user% and the
    /// io-quiet conditions must hold for the window to count toward the
    /// streak; either failing resets it immediately back to `Healthy`
    /// (covers both "brief spike then idle" and "recovery after Wedged").
    pub fn observe(&mut self, sample: Sample) -> WedgeState {
        let hot = user_pct(&sample) >= self.cfg.user_pct_threshold;
        let quiet = sample.io_write_delta <= self.cfg.io_epsilon_bytes;

        if hot && quiet {
            self.streak = self.streak.saturating_add(1);
            self.streak_wall += sample.wall_delta;
            self.state = if self.streak >= self.cfg.required_streak {
                WedgeState::Wedged
            } else {
                WedgeState::Suspect {
                    streak: self.streak,
                }
            };
        } else {
            self.streak = 0;
            self.streak_wall = Duration::ZERO;
            self.state = WedgeState::Healthy;
        }
        self.state
    }

    pub fn state(&self) -> WedgeState {
        self.state
    }

    /// Total wall-clock span of the current qualifying streak. Used to
    /// render "no console output for Xs" in the warning line.
    pub fn streak_wall(&self) -> Duration {
        self.streak_wall
    }
}

fn user_pct(sample: &Sample) -> f64 {
    if sample.wall_delta.is_zero() {
        return 0.0;
    }
    sample.hottest_thread_user_delta.as_secs_f64() / sample.wall_delta.as_secs_f64()
}

// ─── Watchdog wiring ────────────────────────────────────────────────────

/// Caller-built configuration for [`WedgeWatchdog::spawn`]. Mirrors the
/// shape of [`crate::cpu_banner::CpuBannerCfg`].
#[derive(Debug, Clone)]
pub struct WedgeWatchdogCfg {
    pub enabled: bool,
    /// PID of the monitored child (the direct child clud spawned — the
    /// sampler walks its full descendant subtree, since the actual TUI
    /// binary is often several process hops below, e.g. cmd -> node ->
    /// node -> codex.exe in the live incident).
    pub pid: u32,
    /// Backend executable name, used in the warning line
    /// (`"codex"` / `"claude"`).
    pub backend_label: String,
    pub tick: Duration,
    pub required_streak: u32,
    pub user_pct_threshold: f64,
    pub io_epsilon_bytes: u64,
    /// Test-only hook: when set, every observed [`WedgeState`] transition
    /// is also sent here so tests can assert on the real background loop
    /// without scraping stderr. `None` in production.
    pub state_tx: Option<mpsc::Sender<WedgeState>>,
}

impl WedgeWatchdogCfg {
    pub fn new(pid: u32, backend_label: impl Into<String>) -> Self {
        Self {
            enabled: true,
            pid,
            backend_label: backend_label.into(),
            tick: DEFAULT_TICK,
            required_streak: DEFAULT_REQUIRED_STREAK,
            user_pct_threshold: DEFAULT_USER_PCT_THRESHOLD,
            io_epsilon_bytes: DEFAULT_IO_EPSILON_BYTES,
            state_tx: None,
        }
    }

    /// Disabled variant — [`WedgeWatchdog::spawn`] is a no-op (no thread).
    pub fn disabled() -> Self {
        Self {
            enabled: false,
            pid: 0,
            backend_label: String::new(),
            tick: DEFAULT_TICK,
            required_streak: DEFAULT_REQUIRED_STREAK,
            user_pct_threshold: DEFAULT_USER_PCT_THRESHOLD,
            io_epsilon_bytes: DEFAULT_IO_EPSILON_BYTES,
            state_tx: None,
        }
    }

    /// Only called from the Windows watchdog loop; other platforms have no
    /// sampler, so gate it to keep `-D dead_code` green on the CI matrix.
    #[cfg(windows)]
    fn detector_cfg(&self) -> WedgeDetectorCfg {
        WedgeDetectorCfg {
            user_pct_threshold: self.user_pct_threshold,
            io_epsilon_bytes: self.io_epsilon_bytes,
            required_streak: self.required_streak,
        }
    }
}

/// Background watcher thread. Joins on `Drop`, same contract as
/// [`crate::cpu_banner::BannerWatcher`] — construct it right after starting
/// the monitored child and let it fall out of scope (or call `stop()`
/// explicitly) when that child's iteration ends, so each process gets a
/// fresh baseline.
pub struct WedgeWatchdog {
    stop_tx: Option<mpsc::Sender<()>>,
    handle: Option<JoinHandle<()>>,
}

impl WedgeWatchdog {
    /// Spawn the watcher. Inert (no thread) when `cfg.enabled` is false or
    /// on a platform without a sampler implementation.
    pub fn spawn(cfg: WedgeWatchdogCfg) -> Self {
        if !cfg.enabled || !platform_supported() {
            return Self {
                stop_tx: None,
                handle: None,
            };
        }
        #[cfg(windows)]
        {
            let (tx, rx) = mpsc::channel();
            let handle = thread::Builder::new()
                .name("clud-wedge-watchdog".into())
                .spawn(move || run_watchdog_loop(cfg, rx))
                .ok();
            Self {
                stop_tx: Some(tx),
                handle,
            }
        }
        #[cfg(not(windows))]
        {
            let _ = cfg;
            Self {
                stop_tx: None,
                handle: None,
            }
        }
    }

    /// Convenience for callers holding `Option<u32>` (e.g.
    /// `NativeProcess::pid()` / `NativePtyProcess::pid()`, which can be
    /// `None` in edge cases). `None` spawns an inert watchdog.
    pub fn spawn_for_pid(pid: Option<u32>, backend_label: impl Into<String>) -> Self {
        match pid {
            Some(pid) => Self::spawn(WedgeWatchdogCfg::new(pid, backend_label)),
            None => Self::spawn(WedgeWatchdogCfg::disabled()),
        }
    }

    /// Explicit shutdown. Idempotent; safe to call before `Drop`. Returns
    /// promptly even mid-tick — the loop blocks on `recv_timeout(tick)`,
    /// which wakes immediately once the stop signal arrives.
    pub fn stop(&mut self) {
        if let Some(tx) = self.stop_tx.take() {
            let _ = tx.send(());
        }
        if let Some(h) = self.handle.take() {
            let _ = h.join();
        }
    }
}

impl Drop for WedgeWatchdog {
    fn drop(&mut self) {
        self.stop();
    }
}

fn platform_supported() -> bool {
    cfg!(windows)
}

#[cfg(windows)]
fn run_watchdog_loop(cfg: WedgeWatchdogCfg, stop_rx: mpsc::Receiver<()>) {
    let mut sampler = win::ThreadIoSampler::new();
    let mut detector = WedgeDetector::new(cfg.detector_cfg());
    let mut warned_this_episode = false;

    loop {
        if stop_rx.recv_timeout(cfg.tick).is_ok() {
            return;
        }
        let Some(tick) = sampler.tick(cfg.pid) else {
            continue;
        };
        let state = detector.observe(tick.sample);
        if let Some(tx) = &cfg.state_tx {
            let _ = tx.send(state);
        }
        match state {
            WedgeState::Wedged => {
                if !warned_this_episode {
                    warned_this_episode = true;
                    emit_wedge_warning(&cfg, &tick, detector.streak_wall());
                }
            }
            WedgeState::Healthy => {
                // Rate-limit: only the next Wedged transition after a
                // recovery warns again (one warning per wedge episode).
                warned_this_episode = false;
            }
            WedgeState::Suspect { .. } => {}
        }
    }
}

/// Print the user-visible warning and log the measured signature. Split out
/// from the loop so the message format is easy to eyeball/test in isolation.
#[cfg(windows)]
fn emit_wedge_warning(cfg: &WedgeWatchdogCfg, tick: &win::SampledTick, streak_wall: Duration) {
    let pct = user_pct(&tick.sample) * 100.0;
    let secs = streak_wall.as_secs();
    let backend = &cfg.backend_label;
    eprintln!(
        "clud: WARNING — {backend} TUI appears wedged (1 thread at {pct:.0}% CPU, no console \
         output for {secs}s). The session may still be working underneath. Consider restarting \
         the TUI: Ctrl+C, then `clud -c` (or `clud --resume`) to reconnect the {backend} session."
    );
    crate::verbose_log::log(format_args!(
        "[clud] wedge-watchdog: WEDGED root_pid={root_pid} hot_pid={hot_pid} hot_tid={hot_tid} \
         user_pct={pct:.1} io_write_delta_bytes={io} streak_secs={secs} tick={tick_ms}ms",
        root_pid = cfg.pid,
        hot_pid = tick.hot_pid,
        hot_tid = tick.hot_tid,
        io = tick.sample.io_write_delta,
        tick_ms = cfg.tick.as_millis(),
    ));
}

// ─── Windows-only platform sampler ─────────────────────────────────────

#[cfg(windows)]
mod win {
    use super::{Duration, Sample, GATE_USER_PCT_THRESHOLD};
    use std::collections::{HashMap, HashSet};
    use std::time::Instant;

    use windows::Win32::Foundation::{CloseHandle, FILETIME};
    use windows::Win32::System::Diagnostics::ToolHelp::{
        CreateToolhelp32Snapshot, Process32FirstW, Process32NextW, Thread32First, Thread32Next,
        PROCESSENTRY32W, TH32CS_SNAPPROCESS, TH32CS_SNAPTHREAD, THREADENTRY32,
    };
    use windows::Win32::System::Threading::{
        GetProcessIoCounters, GetProcessTimes, GetThreadTimes, OpenProcess, OpenThread,
        IO_COUNTERS, PROCESS_QUERY_LIMITED_INFORMATION, THREAD_QUERY_LIMITED_INFORMATION,
    };

    /// One tick's worth of raw measurement, carrying enough identity (which
    /// pid/tid was hottest) for the warning + log line in the outer module.
    pub(super) struct SampledTick {
        pub sample: Sample,
        pub hot_pid: u32,
        pub hot_tid: u32,
    }

    /// Stateful raw sampler. Remembers the previous tick's per-thread
    /// user-mode time and per-process IO write bytes so it can report
    /// deltas. Opens no OS handles between ticks — every
    /// `OpenThread`/`OpenProcess` handle is opened, queried, and closed
    /// within a single `tick()` call.
    pub(super) struct ThreadIoSampler {
        prev_thread_user_100ns: HashMap<(u32, u32), u64>,
        prev_io_write_bytes: HashMap<u32, u64>,
        /// Per-process user time, for the #709 gate. Cheap to maintain — one
        /// `GetProcessTimes` per subtree pid — and it is what lets a healthy
        /// tick skip the host-wide thread enumeration entirely.
        prev_process_user_100ns: HashMap<u32, u64>,
        prev_at: Option<Instant>,
    }

    impl ThreadIoSampler {
        pub(super) fn new() -> Self {
            Self {
                prev_thread_user_100ns: HashMap::new(),
                prev_io_write_bytes: HashMap::new(),
                prev_process_user_100ns: HashMap::new(),
                prev_at: None,
            }
        }

        /// Sample the subtree rooted at `root_pid`. Returns `None` on the
        /// first call (no baseline for deltas yet) or when no thread in the
        /// subtree has a computable delta this tick (e.g. the subtree
        /// vanished, or every thread present is new since the last tick).
        ///
        /// Since #709 a tick whose subtree is provably cool at the *process*
        /// level short-circuits before the host-wide thread enumeration and
        /// reports an explicitly healthy sample. It reports rather than
        /// returns `None` because the caller treats `None` as "no
        /// observation", which would leave a partial wedge streak standing
        /// across an idle stretch.
        pub(super) fn tick(&mut self, root_pid: u32) -> Option<SampledTick> {
            let now = Instant::now();
            let subtree = subtree_pids(root_pid);

            // #709 gate: the detection signature is "one *thread* at >=90% of a
            // core". A thread's user time can never exceed its process's, so if
            // no process in the subtree burned that much user time this window,
            // no thread in it did either — and the expensive per-thread walk
            // cannot change the answer.
            //
            // That walk is the costliest thing the watchdog does: a
            // `TH32CS_SNAPTHREAD` snapshot enumerates **every thread on the
            // host** before being filtered down to the subtree, then opens each
            // surviving thread. Skipping it on a healthy session drops the
            // steady-state cost to a handful of `GetProcessTimes` calls.
            //
            // Safe by construction: the gate can only skip work when the
            // process-level total already rules out a wedge, so it cannot
            // produce a false negative.
            let wall_delta = self.prev_at.map(|prev| now.saturating_duration_since(prev));
            let cur_process_user = process_user_times(&subtree);
            let hot_enough = super::subtree_could_hide_a_hot_thread(
                &self.prev_process_user_100ns,
                &cur_process_user,
                wall_delta,
                GATE_USER_PCT_THRESHOLD,
            );

            let thread_ids = if hot_enough {
                threads_for_pids(&subtree)
            } else {
                Vec::new()
            };

            let mut cur_thread_user: HashMap<(u32, u32), u64> = HashMap::new();
            // (pid, tid, delta_100ns) of the single hottest thread this tick.
            let mut hottest: Option<(u32, u32, u64)> = None;

            for (pid, tid) in thread_ids {
                let Some(user_100ns) = thread_user_time_100ns(tid) else {
                    continue;
                };
                if let Some(prev) = self.prev_thread_user_100ns.get(&(pid, tid)) {
                    let delta = user_100ns.saturating_sub(*prev);
                    let is_hotter = hottest.map(|(_, _, d)| delta > d).unwrap_or(true);
                    if is_hotter {
                        hottest = Some((pid, tid, delta));
                    }
                }
                cur_thread_user.insert((pid, tid), user_100ns);
            }

            let mut cur_io_write: HashMap<u32, u64> = HashMap::new();
            for pid in &subtree {
                if let Some(bytes) = process_io_write_bytes(*pid) {
                    cur_io_write.insert(*pid, bytes);
                }
            }

            // A gated-out tick must still be *reported* as healthy, not
            // swallowed. `run_watchdog_loop` does `let Some(tick) = … else
            // { continue }`, so returning `None` here would leave the
            // detector's streak untouched — eight hot windows followed by an
            // idle stretch and one more hot window would reach `Wedged`
            // without nine consecutive qualifying windows. The gate proved
            // no thread is hot, so say exactly that.
            if !hot_enough {
                // `None` on the very first tick, as before — there is no
                // baseline to measure a window against.
                let result = self.prev_at.map(|prev_at| SampledTick {
                    sample: Sample {
                        hottest_thread_user_delta: Duration::ZERO,
                        wall_delta: now.saturating_duration_since(prev_at),
                        io_write_delta: 0,
                    },
                    hot_pid: root_pid,
                    hot_tid: 0,
                });
                // The per-thread map goes stale while gated, so drop it. The
                // first armed tick after a quiet stretch re-establishes a
                // baseline and reports nothing, costing one extra window of
                // detection latency (90 s -> 100 s, still inside the issue's
                // 60-120 s acceptance band).
                self.prev_thread_user_100ns.clear();
                self.prev_io_write_bytes = cur_io_write;
                self.prev_process_user_100ns = cur_process_user;
                self.prev_at = Some(now);
                return result;
            }

            let result = match (self.prev_at, hottest) {
                (Some(prev_at), Some((hot_pid, hot_tid, hot_delta_100ns))) => {
                    let io_write_delta = match (
                        cur_io_write.get(&hot_pid),
                        self.prev_io_write_bytes.get(&hot_pid),
                    ) {
                        (Some(cur), Some(prev)) => cur.saturating_sub(*prev),
                        // No IO baseline for the hot process yet (it's new
                        // to the subtree this tick) — treat as quiet rather
                        // than stalling detection; the next tick will have
                        // a real baseline.
                        _ => 0,
                    };
                    Some(SampledTick {
                        sample: Sample {
                            hottest_thread_user_delta: Duration::from_nanos(
                                hot_delta_100ns.saturating_mul(100),
                            ),
                            wall_delta: now.saturating_duration_since(prev_at),
                            io_write_delta,
                        },
                        hot_pid,
                        hot_tid,
                    })
                }
                _ => None,
            };

            self.prev_thread_user_100ns = cur_thread_user;
            self.prev_io_write_bytes = cur_io_write;
            self.prev_process_user_100ns = cur_process_user;
            self.prev_at = Some(now);
            result
        }
    }

    /// DFS over the Toolhelp32 process snapshot's parent-pid graph, rooted
    /// at `root_pid`. Falls back to `[root_pid]` alone if the snapshot
    /// fails or the root isn't visible (e.g. it just exited) so callers
    /// degrade gracefully instead of panicking.
    fn subtree_pids(root_pid: u32) -> Vec<u32> {
        let mut children: HashMap<u32, Vec<u32>> = HashMap::new();
        let mut seen_root = false;

        // SAFETY: `CreateToolhelp32Snapshot` with `TH32CS_SNAPPROCESS` takes
        // a system-wide snapshot; `0` for the pid parameter is required and
        // ignored for this flag per the Win32 contract. The returned handle
        // is closed below before returning.
        let Ok(snapshot) = (unsafe { CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0) }) else {
            return vec![root_pid];
        };
        let mut entry = PROCESSENTRY32W {
            dwSize: std::mem::size_of::<PROCESSENTRY32W>() as u32,
            ..Default::default()
        };
        // SAFETY: `entry` is stack-allocated with `dwSize` set per the
        // Win32 contract for `Process32FirstW`/`Process32NextW`.
        if unsafe { Process32FirstW(snapshot, &mut entry) }.is_ok() {
            loop {
                if entry.th32ProcessID == root_pid {
                    seen_root = true;
                }
                children
                    .entry(entry.th32ParentProcessID)
                    .or_default()
                    .push(entry.th32ProcessID);
                if unsafe { Process32NextW(snapshot, &mut entry) }.is_err() {
                    break;
                }
            }
        }
        let _ = unsafe { CloseHandle(snapshot) };

        if !seen_root {
            return vec![root_pid];
        }

        super::descendants_of(&children, root_pid)
    }

    /// Every `(pid, tid)` in `pids` via a single system-wide
    /// `TH32CS_SNAPTHREAD` snapshot, filtered down to the wanted pid set.
    fn threads_for_pids(pids: &[u32]) -> Vec<(u32, u32)> {
        let mut out = Vec::new();
        let wanted: HashSet<u32> = pids.iter().copied().collect();
        if wanted.is_empty() {
            return out;
        }

        // SAFETY: see `subtree_pids` — same snapshot contract, different
        // flag/struct.
        let Ok(snapshot) = (unsafe { CreateToolhelp32Snapshot(TH32CS_SNAPTHREAD, 0) }) else {
            return out;
        };
        let mut entry = THREADENTRY32 {
            dwSize: std::mem::size_of::<THREADENTRY32>() as u32,
            ..Default::default()
        };
        if unsafe { Thread32First(snapshot, &mut entry) }.is_ok() {
            loop {
                if wanted.contains(&entry.th32OwnerProcessID) {
                    out.push((entry.th32OwnerProcessID, entry.th32ThreadID));
                }
                if unsafe { Thread32Next(snapshot, &mut entry) }.is_err() {
                    break;
                }
            }
        }
        let _ = unsafe { CloseHandle(snapshot) };
        out
    }

    /// User-mode time for one thread, in 100 ns ticks (raw `FILETIME`
    /// units) — `None` if the thread has already exited or we lack access.
    fn thread_user_time_100ns(tid: u32) -> Option<u64> {
        // SAFETY: `OpenThread` with a valid access mask and thread id;
        // failure returns `Err` (mapped to `None` below) rather than an
        // invalid handle.
        let handle = unsafe { OpenThread(THREAD_QUERY_LIMITED_INFORMATION, false, tid) }.ok()?;
        let mut creation = FILETIME::default();
        let mut exit = FILETIME::default();
        let mut kernel = FILETIME::default();
        let mut user = FILETIME::default();
        // SAFETY: all four out-pointers are stack-allocated `FILETIME`
        // values valid for the duration of the call, per the
        // `GetThreadTimes` contract.
        let result =
            unsafe { GetThreadTimes(handle, &mut creation, &mut exit, &mut kernel, &mut user) };
        let _ = unsafe { CloseHandle(handle) };
        result.ok()?;
        Some(filetime_to_u64(user))
    }

    /// User-mode CPU time for each pid, in 100 ns ticks (#709).
    ///
    /// One `OpenProcess` + `GetProcessTimes` per pid — microseconds each, and
    /// bounded by the subtree rather than by the host. This is the cheap
    /// measurement that lets a healthy tick skip `threads_for_pids`, whose
    /// `TH32CS_SNAPTHREAD` snapshot enumerates every thread on the machine.
    /// Pids that cannot be opened are simply absent; the caller fails open.
    fn process_user_times(pids: &[u32]) -> HashMap<u32, u64> {
        let mut out = HashMap::with_capacity(pids.len());
        for &pid in pids {
            // SAFETY: `OpenProcess` with a valid access mask and pid; failure
            // returns `Err` and the pid is skipped.
            let Ok(handle) =
                (unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid) })
            else {
                continue;
            };
            let mut creation = FILETIME::default();
            let mut exit = FILETIME::default();
            let mut kernel = FILETIME::default();
            let mut user = FILETIME::default();
            // SAFETY: all four out-pointers are stack-allocated `FILETIME`
            // values valid for the duration of the call.
            let result = unsafe {
                GetProcessTimes(handle, &mut creation, &mut exit, &mut kernel, &mut user)
            };
            let _ = unsafe { CloseHandle(handle) };
            if result.is_ok() {
                out.insert(pid, filetime_to_u64(user));
            }
        }
        out
    }

    /// IO write-transfer byte count for one process — `None` if it has
    /// already exited or we lack access.
    fn process_io_write_bytes(pid: u32) -> Option<u64> {
        // SAFETY: `OpenProcess` with a valid access mask and pid; failure
        // returns `Err` (mapped to `None` below).
        let handle = unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid) }.ok()?;
        let mut counters = IO_COUNTERS::default();
        // SAFETY: `counters` is a stack-allocated out-pointer valid for the
        // duration of the call, per the `GetProcessIoCounters` contract.
        let result = unsafe { GetProcessIoCounters(handle, &mut counters) };
        let _ = unsafe { CloseHandle(handle) };
        result.ok()?;
        Some(counters.WriteTransferCount)
    }

    fn filetime_to_u64(ft: FILETIME) -> u64 {
        ((ft.dwHighDateTime as u64) << 32) | ft.dwLowDateTime as u64
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn filetime_to_u64_combines_high_and_low_words() {
            let ft = FILETIME {
                dwLowDateTime: 0x1234_5678,
                dwHighDateTime: 0x0000_0001,
            };
            assert_eq!(filetime_to_u64(ft), 0x0000_0001_1234_5678);
        }

        #[test]
        fn filetime_to_u64_zero_is_zero() {
            assert_eq!(filetime_to_u64(FILETIME::default()), 0);
        }

        /// Smoke test against the real OS: two ticks on our own pid should
        /// not panic, and by the second tick we have a baseline (this
        /// process always has at least one thread with a computable user
        /// time delta). Mirrors `cpu_banner`'s `sampler_returns_at_least_self`.
        #[test]
        fn sampler_ticks_against_real_process_without_panicking() {
            let mut sampler = ThreadIoSampler::new();
            let self_pid = std::process::id();
            assert!(
                sampler.tick(self_pid).is_none(),
                "first tick has no baseline"
            );
            // Burn a little real CPU so there's a non-zero delta to find.
            let mut acc: u64 = 0;
            for i in 0..5_000_000u64 {
                acc = acc.wrapping_add(std::hint::black_box(i));
            }
            std::hint::black_box(acc);
            let second = sampler.tick(self_pid);
            assert!(second.is_some(), "second tick should find a baseline");
        }
    }
}

/// #709 — the two pure cores: the cheap gate, and the cycle-safe walk.
#[cfg(test)]
mod gate_tests {
    use super::*;
    use std::collections::HashMap;

    const WINDOW: Duration = Duration::from_secs(10);

    /// 10 s window in 100 ns ticks — one full core for the whole window.
    const ONE_CORE: u64 = 100_000_000;

    fn map(pairs: &[(u32, u64)]) -> HashMap<u32, u64> {
        pairs.iter().copied().collect()
    }

    #[test]
    fn an_idle_subtree_does_not_arm_the_thread_walk() {
        // Every process used ~1% of a core. No thread inside them can be at
        // 90%, so the host-wide enumeration is pure waste.
        let prev = map(&[(10, 0), (11, 0), (12, 0)]);
        let cur = map(&[
            (10, ONE_CORE / 100),
            (11, ONE_CORE / 100),
            (12, ONE_CORE / 100),
        ]);
        assert!(!subtree_could_hide_a_hot_thread(
            &prev,
            &cur,
            Some(WINDOW),
            GATE_USER_PCT_THRESHOLD
        ));
    }

    #[test]
    fn a_pinned_process_arms_the_thread_walk() {
        let prev = map(&[(10, 0), (11, 0)]);
        let cur = map(&[(10, ONE_CORE / 100), (11, ONE_CORE)]);
        assert!(subtree_could_hide_a_hot_thread(
            &prev,
            &cur,
            Some(WINDOW),
            GATE_USER_PCT_THRESHOLD
        ));
    }

    /// The gate must never be the reason a real wedge goes unseen. The
    /// detector's own threshold is 90% of a core; the gate arms below that, so
    /// a process sitting exactly at the detection threshold always gets
    /// measured.
    #[test]
    fn the_gate_arms_below_the_detectors_own_threshold() {
        let prev = map(&[(10, 0)]);
        let at_detector_threshold =
            map(&[(10, (ONE_CORE as f64 * DEFAULT_USER_PCT_THRESHOLD) as u64)]);
        assert!(subtree_could_hide_a_hot_thread(
            &prev,
            &at_detector_threshold,
            Some(WINDOW),
            GATE_USER_PCT_THRESHOLD
        ));
    }

    #[test]
    fn the_gate_fails_open_when_it_cannot_answer() {
        let populated = map(&[(10, ONE_CORE)]);
        // No wall delta yet (first tick).
        assert!(subtree_could_hide_a_hot_thread(
            &populated,
            &populated,
            None,
            GATE_USER_PCT_THRESHOLD
        ));
        // No baseline.
        assert!(subtree_could_hide_a_hot_thread(
            &HashMap::new(),
            &populated,
            Some(WINDOW),
            GATE_USER_PCT_THRESHOLD
        ));
        // Nothing readable this tick.
        assert!(subtree_could_hide_a_hot_thread(
            &populated,
            &HashMap::new(),
            Some(WINDOW),
            GATE_USER_PCT_THRESHOLD
        ));
        // Zero-width window — a division that would otherwise be meaningless.
        assert!(subtree_could_hide_a_hot_thread(
            &populated,
            &populated,
            Some(Duration::ZERO),
            GATE_USER_PCT_THRESHOLD
        ));
    }

    /// A pid seen for the first time this tick has no baseline, so its delta
    /// is unknowable — it must arm the walk rather than be treated as quiet.
    #[test]
    fn a_process_new_this_tick_arms_the_walk() {
        let prev = map(&[(10, 0)]);
        let cur = map(&[(10, 0), (99, 0)]);
        assert!(subtree_could_hide_a_hot_thread(
            &prev,
            &cur,
            Some(WINDOW),
            GATE_USER_PCT_THRESHOLD
        ));
    }

    /// A gated-out window must reach the detector as an explicit *healthy*
    /// observation, not as "no observation".
    ///
    /// This pins the regression the gate nearly introduced. The loop does
    /// `let Some(tick) = sampler.tick(..) else { continue }`, so if a cool
    /// window returned `None` the streak would simply not advance — and would
    /// not reset either. Eight hot windows, an idle stretch of any length,
    /// then one more hot window would reach `Wedged` on nine *non-consecutive*
    /// windows, which is exactly what the streak exists to prevent.
    #[test]
    fn an_idle_stretch_between_hot_windows_must_break_the_streak() {
        let window = Duration::from_secs(10);
        let hot = Sample {
            hottest_thread_user_delta: Duration::from_millis(9_500),
            wall_delta: window,
            io_write_delta: 0,
        };
        // What the gated path now emits.
        let gated_cool = Sample {
            hottest_thread_user_delta: Duration::ZERO,
            wall_delta: window,
            io_write_delta: 0,
        };

        let mut detector = WedgeDetector::new(WedgeDetectorCfg::default());
        for _ in 0..(DEFAULT_REQUIRED_STREAK - 1) {
            detector.observe(hot);
        }
        assert!(matches!(detector.observe(gated_cool), WedgeState::Healthy));
        assert!(
            matches!(detector.observe(hot), WedgeState::Suspect { streak: 1 }),
            "a cool window must restart the streak from scratch"
        );
    }

    // ─── descendants_of ────────────────────────────────────────────────

    fn tree(pairs: &[(u32, &[u32])]) -> HashMap<u32, Vec<u32>> {
        pairs.iter().map(|(p, k)| (*p, k.to_vec())).collect()
    }

    #[test]
    fn a_plain_tree_yields_every_descendant_once() {
        let children = tree(&[(1, &[2, 3]), (2, &[4]), (3, &[5])]);
        let mut got = descendants_of(&children, 1);
        got.sort_unstable();
        assert_eq!(got, vec![1, 2, 3, 4, 5]);
    }

    #[test]
    fn a_root_with_no_children_is_just_itself() {
        assert_eq!(descendants_of(&HashMap::new(), 42), vec![42]);
    }

    /// The regression this walk's visited set exists for. Windows recycles
    /// PIDs, so a Toolhelp parent-pid graph can name a descendant as its own
    /// ancestor. Before #709 this looped forever, growing the output vector
    /// until the process died — a hang, not a wrong answer.
    #[test]
    fn a_pid_reuse_cycle_terminates_instead_of_hanging() {
        let children = tree(&[(1, &[2]), (2, &[3]), (3, &[1])]);
        let mut got = descendants_of(&children, 1);
        got.sort_unstable();
        assert_eq!(got, vec![1, 2, 3], "each pid must appear exactly once");
    }

    #[test]
    fn a_self_parented_pid_terminates() {
        let children = tree(&[(7, &[7])]);
        assert_eq!(descendants_of(&children, 7), vec![7]);
    }

    /// A diamond cannot occur in a real parent-pid graph (each pid has one
    /// parent), but the walk must not double-count if the snapshot is torn.
    #[test]
    fn a_diamond_does_not_double_count() {
        let children = tree(&[(1, &[2, 3]), (2, &[4]), (3, &[4])]);
        let mut got = descendants_of(&children, 1);
        got.sort_unstable();
        assert_eq!(got, vec![1, 2, 3, 4]);
    }
}

#[cfg(test)]
mod tests {
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
}
