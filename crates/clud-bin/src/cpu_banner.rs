//! Issue #466 (slice of #463): CPU-burn banner in the foreground clud
//! terminal.
//!
//! When the foreground `clud` session's subtree (self + descendants) burns
//! meaningful CPU, this module periodically prints a one-line status banner
//! to stderr so the user notices before they hear the fan:
//!
//! ```text
//! [clud] cpu 287 % · 2.9 / 12 cores · rss 1.42 GiB · 24 procs · 7 m
//! ```
//!
//! Three pieces:
//!
//! - [`CpuBannerState`] — the pure state machine (crossover / sustained
//!   heartbeat / hysteretic drop-out / suppression window). Tested
//!   without `sysinfo`; downstream consumers can drive it from any
//!   sampler.
//! - [`Sampler`] — keeps one persistent `sysinfo::System` and per tick
//!   sums `cpu_usage()` + `memory()` across the subtree rooted at
//!   `originator_pid`. Uses the parent-PID graph (cheap), not the
//!   env-tag scan (expensive); breakaway descendants escape this view
//!   and are #340 territory. Issue #540: most ticks do a *targeted*
//!   sysinfo refresh of just the cached subtree pids instead of a
//!   full-system refresh, and the tick cadence itself backs off
//!   ([`sample_interval`]) as the subtree grows, so a large fan-out
//!   (rustc/node swarms, several concurrent clud sessions) can't turn
//!   the banner meant to report CPU burn into a measurable contributor.
//! - [`BannerWatcher`] — background thread that joins the two on a
//!   `tick` cadence and writes banners to stderr. Drop joins the
//!   thread.
//!
//! Suppression: caller (in `main.rs`) constructs `CpuBannerCfg` with
//! `enabled = false` for `--no-cpu-banner`, `--dry-run`, `--detach`,
//! `--detachable`, `--repeat`, or when the settings.json toggle is off.

use std::collections::HashMap;
use std::sync::mpsc;
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use sysinfo::{Pid, ProcessRefreshKind, ProcessesToUpdate, System};

/// Default tick cadence. 2 s matches the parent #463 default. Crossover
/// fires after `DEFAULT_SUSTAINED_TICKS * DEFAULT_TICK` (= 6 s), which is
/// inside the acceptance criterion's 6 s bound.
pub const DEFAULT_TICK: Duration = Duration::from_secs(2);

/// Default heartbeat between banner re-prints while sustained.
pub const DEFAULT_HEARTBEAT_SECS: u64 = 30;

/// Default sustained-tick count before first banner. Filters compile
/// spikes / GC pauses.
pub const DEFAULT_SUSTAINED_TICKS: u32 = 3;

/// Hysteretic drop-out multiplier: subtree must fall below
/// `DROP_OUT_FACTOR × trigger_pct()` before the clear-banner arms. Same
/// anti-flap rationale as the parent #463 sampler tier demotion.
pub const DROP_OUT_FACTOR: f32 = 0.7;

/// Minimum episode length before a clear-banner is printed. Episodes
/// shorter than this were transient spikes; clearing would be noise.
pub const MIN_EPISODE_FOR_CLEAR_SECS: u64 = 60;

/// After a clear-banner, hold the next crossover for at least this long
/// to prevent rapid-cycle flapping during oscillating loads.
pub const SUPPRESSION_AFTER_CLEAR_SECS: u64 = 60;

/// Absolute floor for the trigger: half a core is notable on any host.
const ABSOLUTE_FLOOR_PCT: f32 = 50.0;

/// Relative fraction of host capacity that triggers the banner.
const RELATIVE_HOST_FRACTION: f32 = 0.20;

/// Issue #540: how long a [`Sampler`]'s cached subtree pid list is reused
/// before the next tick pays for a full-system walk to rediscover
/// new/dead descendants. Deliberately slower than every `sample_interval`
/// tier (2 s/5 s/10 s) — the expensive part of a tick is the full-system
/// `refresh_processes_specifics(ProcessesToUpdate::All, ..)` enumeration,
/// not the cheap DFS over the resulting parent-PID map, so backing off
/// *that* is what bounds the cost. Between rebuilds, ticks use a targeted
/// `ProcessesToUpdate::Some(&cached_pids)` refresh instead.
const TREE_REBUILD_INTERVAL: Duration = Duration::from_secs(30);

/// Issue #540: subtree-size tiers below which a full-system rebuild+refresh
/// every `DEFAULT_TICK` is cheap enough not to matter. Above them, the
/// sampler backs off `sample_interval()` so a large fan-out (rustc/node
/// swarms, several concurrent clud sessions) doesn't turn the banner
/// itself into a measurable CPU cost.
const SMALL_SUBTREE_MAX: usize = 25;
const MEDIUM_SUBTREE_MAX: usize = 50;

/// Pure function: adaptive tick cadence for [`BannerWatcher`]'s loop,
/// keyed off the subtree size observed on the *previous* tick. `<= 25`
/// procs uses the normal [`DEFAULT_TICK`] (2 s); `26..=50` backs off to
/// 5 s; `> 50` backs off to 10 s. Banner accuracy/latency may lag by up
/// to this interval while sustained/heartbeat state is unaffected — see
/// [`CpuBannerState::poll`], which counts ticks, not wall-clock time.
pub fn sample_interval(subtree_size: usize) -> Duration {
    if subtree_size <= SMALL_SUBTREE_MAX {
        DEFAULT_TICK
    } else if subtree_size <= MEDIUM_SUBTREE_MAX {
        Duration::from_secs(5)
    } else {
        Duration::from_secs(10)
    }
}

/// Pure decision for whether [`Sampler::tick`] must pay for a full-system
/// walk this tick (vs. reusing the cached subtree pid list for a targeted
/// refresh). Rebuilds when the cache is empty (first tick), when there is
/// no record of a prior walk, or when the prior walk is at least
/// [`TREE_REBUILD_INTERVAL`] old.
fn needs_tree_rebuild(cache_empty: bool, last_walk: Option<Instant>, now: Instant) -> bool {
    if cache_empty {
        return true;
    }
    match last_walk {
        None => true,
        Some(walked_at) => now.duration_since(walked_at) >= TREE_REBUILD_INTERVAL,
    }
}

/// Caller-built configuration. `enabled = false` makes [`BannerWatcher::spawn`]
/// a no-op and [`CpuBannerState::poll`] always return `None`.
#[derive(Debug, Clone)]
pub struct CpuBannerCfg {
    pub enabled: bool,
    pub originator_pid: u32,
    pub num_cpus: usize,
    pub heartbeat_secs: u64,
    pub tick: Duration,
    pub sustained_ticks: u32,
}

impl CpuBannerCfg {
    pub fn new(originator_pid: u32, num_cpus: usize) -> Self {
        Self {
            enabled: true,
            originator_pid,
            num_cpus,
            heartbeat_secs: DEFAULT_HEARTBEAT_SECS,
            tick: DEFAULT_TICK,
            sustained_ticks: DEFAULT_SUSTAINED_TICKS,
        }
    }

    /// Disabled variant — caller uses this for `--no-cpu-banner`, settings
    /// override, and the non-interactive modes that always suppress.
    pub fn disabled() -> Self {
        Self {
            enabled: false,
            originator_pid: 0,
            num_cpus: 1,
            heartbeat_secs: DEFAULT_HEARTBEAT_SECS,
            tick: DEFAULT_TICK,
            sustained_ticks: DEFAULT_SUSTAINED_TICKS,
        }
    }

    /// `max(50 %, 0.20 × num_cpus × 100 %)` — absolute floor (half a core
    /// is notable on any box) combined with a relative cap (20 % of host
    /// capacity, so we don't whine on fat boxes while clud is nibbling).
    pub fn trigger_pct(&self) -> f32 {
        let relative = RELATIVE_HOST_FRACTION * (self.num_cpus as f32) * 100.0;
        ABSOLUTE_FLOOR_PCT.max(relative)
    }
}

/// Which banner the state machine just emitted.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BannerKind {
    /// First banner after subtree CPU stayed above trigger for the
    /// configured sustained-tick count.
    Crossover,
    /// Heartbeat re-print while still above trigger.
    Sustained,
    /// Episode-ended notice, fires only if the episode lasted at least
    /// `MIN_EPISODE_FOR_CLEAR_SECS`.
    Clear,
}

/// One banner ready for rendering. Pure data — render via [`BannerLine::render`]
/// (ANSI-styled) or [`BannerLine::render_plain`] (no escapes; what tests
/// inspect).
#[derive(Debug, Clone, PartialEq)]
pub struct BannerLine {
    pub kind: BannerKind,
    pub cpu_pct: f32,
    pub rss_bytes: u64,
    pub proc_count: usize,
    pub age: Duration,
    pub num_cpus: usize,
    pub trigger_pct: f32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Style {
    None,
    Dim,
    Yellow,
    Red,
}

impl BannerLine {
    /// ANSI-styled rendering. Dim while 1×–2× over trigger, yellow
    /// 2×–4×, red ≥ 4×; clear-banner has no styling.
    pub fn render(&self) -> String {
        let plain = self.render_plain();
        match self.style() {
            Style::None => plain,
            Style::Dim => format!("\x1b[2m{plain}\x1b[0m"),
            Style::Yellow => format!("\x1b[33m{plain}\x1b[0m"),
            Style::Red => format!("\x1b[31;1m{plain}\x1b[0m"),
        }
    }

    /// Unstyled rendering. Used by tests and any non-TTY caller.
    pub fn render_plain(&self) -> String {
        match self.kind {
            BannerKind::Clear => format!(
                "[clud] cpu back to normal · {} · {} procs · {}",
                format_rss(self.rss_bytes),
                self.proc_count,
                format_age(self.age),
            ),
            BannerKind::Crossover | BannerKind::Sustained => format!(
                "[clud] cpu {:.0} % · {:.1} / {} cores · rss {} · {} procs · {}",
                self.cpu_pct,
                self.cpu_pct / 100.0,
                self.num_cpus,
                format_rss(self.rss_bytes),
                self.proc_count,
                format_age(self.age),
            ),
        }
    }

    fn style(&self) -> Style {
        if matches!(self.kind, BannerKind::Clear) {
            return Style::None;
        }
        if self.trigger_pct <= 0.0 {
            return Style::Dim;
        }
        let ratio = self.cpu_pct / self.trigger_pct;
        if ratio >= 4.0 {
            Style::Red
        } else if ratio >= 2.0 {
            Style::Yellow
        } else {
            Style::Dim
        }
    }
}

/// Per-tick observation. Constructed by [`Sampler::tick`] in production,
/// or directly in unit tests.
#[derive(Debug, Clone, Copy)]
pub struct Sample {
    pub at: Instant,
    pub subtree_cpu_pct: f32,
    pub subtree_rss_bytes: u64,
    pub proc_count: usize,
    /// Wall-clock age of the foreground session (sampler creation time
    /// → now). Used as the fallback `age` for the Clear banner whose
    /// episode-start has already been cleared.
    pub oldest_age: Duration,
}

/// State machine. Pure: no I/O, no `sysinfo`. Drive it from any sampler.
#[derive(Debug, Default)]
pub struct CpuBannerState {
    sustained_count: u32,
    in_episode: bool,
    episode_started_at: Option<Instant>,
    last_print_at: Option<Instant>,
    suppressed_until: Option<Instant>,
}

impl CpuBannerState {
    /// Feed one tick. Returns `Some(BannerLine)` when the state machine
    /// has crossed a threshold and the caller should print; otherwise
    /// `None`.
    pub fn poll(&mut self, sample: Sample, cfg: &CpuBannerCfg) -> Option<BannerLine> {
        if !cfg.enabled {
            return None;
        }
        let trigger = cfg.trigger_pct();
        let above = sample.subtree_cpu_pct >= trigger;
        let clear_threshold = DROP_OUT_FACTOR * trigger;

        if above {
            // Suppression check first: during a suppression window after
            // a recent Clear banner, swallow above-ticks silently AND keep
            // the sustained counter at zero, so the user gets the full
            // sustained-ticks grace period once suppression lifts (anti-
            // flap for oscillating loads). Suppression only applies while
            // we are NOT in an episode — once in episode, heartbeats win.
            if !self.in_episode {
                if let Some(until) = self.suppressed_until {
                    if sample.at < until {
                        self.sustained_count = 0;
                        return None;
                    }
                }
            }
            self.sustained_count = self.sustained_count.saturating_add(1);
            if !self.in_episode {
                if self.sustained_count >= cfg.sustained_ticks {
                    self.in_episode = true;
                    self.episode_started_at = Some(sample.at);
                    self.last_print_at = Some(sample.at);
                    self.suppressed_until = None;
                    return Some(self.make_line(BannerKind::Crossover, sample, cfg));
                }
                return None;
            }
            // Sustained: heartbeat re-print if due.
            let heartbeat = Duration::from_secs(cfg.heartbeat_secs);
            if let Some(last) = self.last_print_at {
                if sample.at.duration_since(last) >= heartbeat {
                    self.last_print_at = Some(sample.at);
                    return Some(self.make_line(BannerKind::Sustained, sample, cfg));
                }
            }
            None
        } else {
            self.sustained_count = 0;
            if !self.in_episode {
                return None;
            }
            // Between 0.7× and 1.0× — stay in episode, no banner.
            if sample.subtree_cpu_pct >= clear_threshold {
                return None;
            }
            // Below clear threshold — episode ends.
            let episode_age = self
                .episode_started_at
                .map(|started| sample.at.duration_since(started))
                .unwrap_or_default();
            self.in_episode = false;
            self.episode_started_at = None;
            self.last_print_at = None;
            if episode_age >= Duration::from_secs(MIN_EPISODE_FOR_CLEAR_SECS) {
                self.suppressed_until =
                    Some(sample.at + Duration::from_secs(SUPPRESSION_AFTER_CLEAR_SECS));
                return Some(self.make_line(BannerKind::Clear, sample, cfg));
            }
            None
        }
    }

    fn make_line(&self, kind: BannerKind, sample: Sample, cfg: &CpuBannerCfg) -> BannerLine {
        let age = match self.episode_started_at {
            Some(started) => sample.at.duration_since(started),
            None => sample.oldest_age,
        };
        BannerLine {
            kind,
            cpu_pct: sample.subtree_cpu_pct,
            rss_bytes: sample.subtree_rss_bytes,
            proc_count: sample.proc_count,
            age,
            num_cpus: cfg.num_cpus,
            trigger_pct: cfg.trigger_pct(),
        }
    }
}

/// Sysinfo-backed sampler. Owns one persistent `System`. Issue #540: most
/// ticks now do a *targeted* `ProcessesToUpdate::Some(&cached_pids)`
/// refresh of just the tracked subtree instead of a full-system refresh;
/// the subtree pid list itself (which requires a full-system walk to
/// discover new/dead descendants) is only rebuilt every
/// [`TREE_REBUILD_INTERVAL`]. Subtree is the parent-PID-graph walk from
/// `originator_pid` — well-behaved descendants, not breakaway children.
///
/// Staleness trade-offs (accepted per #540): descendants spawned after
/// the last walk are invisible until the next rebuild, dead pids merely
/// drop out of the sums, and a recycled pid could briefly count a foreign
/// process — all bounded by `TREE_REBUILD_INTERVAL` and irrelevant to the
/// banner's coarse thresholds.
pub struct Sampler {
    system: System,
    started_at: Instant,
    /// Subtree pid list from the last full-system walk. Reused for
    /// targeted refreshes until [`needs_tree_rebuild`] says otherwise.
    cached_pids: Vec<Pid>,
    /// When `cached_pids` was last (re)built. `None` before the first tick.
    last_tree_walk: Option<Instant>,
}

impl Sampler {
    pub fn new() -> Self {
        Self {
            system: System::new(),
            started_at: Instant::now(),
            cached_pids: Vec::new(),
            last_tree_walk: None,
        }
    }

    /// Test-only hook: force the *next* `tick` to rebuild the subtree pid
    /// list via a full-system refresh, regardless of
    /// [`TREE_REBUILD_INTERVAL`]. Used by the #540 cost benchmark to
    /// reproduce the pre-fix "full refresh every tick" baseline so it can
    /// be measured against the new targeted-refresh behavior.
    #[cfg(test)]
    fn force_rebuild_next_tick(&mut self) {
        self.last_tree_walk = None;
    }

    pub fn tick(&mut self, originator_pid: u32) -> Sample {
        let root = Pid::from_u32(originator_pid);
        let now = Instant::now();
        let refresh_kind = ProcessRefreshKind::nothing().with_cpu().with_memory();

        if needs_tree_rebuild(self.cached_pids.is_empty(), self.last_tree_walk, now) {
            // Full-system refresh: the only way to discover new/dead
            // descendants and rebuild the parent-PID graph.
            self.system
                .refresh_processes_specifics(ProcessesToUpdate::All, true, refresh_kind);
            self.cached_pids = collect_subtree(&self.system, root);
            self.last_tree_walk = Some(now);
        } else {
            // Targeted refresh: only the cached subtree pids (#540) — the
            // cost win over the old every-tick full refresh.
            self.system.refresh_processes_specifics(
                ProcessesToUpdate::Some(&self.cached_pids),
                true,
                refresh_kind,
            );
        }

        let mut subtree_cpu_pct = 0.0_f32;
        let mut subtree_rss = 0_u64;
        let mut count = 0_usize;
        for pid in &self.cached_pids {
            if let Some(proc) = self.system.process(*pid) {
                subtree_cpu_pct += proc.cpu_usage();
                subtree_rss += proc.memory();
                count += 1;
            }
        }
        Sample {
            at: Instant::now(),
            subtree_cpu_pct,
            subtree_rss_bytes: subtree_rss,
            proc_count: count,
            oldest_age: self.started_at.elapsed(),
        }
    }
}

impl Default for Sampler {
    fn default() -> Self {
        Self::new()
    }
}

/// DFS over the parent-PID graph starting at `root`. Includes `root`.
/// Cheap (microseconds even at N=5000); the cost is dominated by the
/// preceding `refresh_processes_specifics`.
fn collect_subtree(system: &System, root: Pid) -> Vec<Pid> {
    let mut children: HashMap<Pid, Vec<Pid>> = HashMap::new();
    for (pid, proc) in system.processes() {
        if let Some(parent) = proc.parent() {
            children.entry(parent).or_default().push(*pid);
        }
    }
    collect_subtree_from_children(&children, root)
}

/// DFS over a pre-built parent→children pid map starting at `root`.
/// Includes `root` even if it has no entry in `children`. Split out from
/// [`collect_subtree`] so the walk itself is unit-testable against a
/// hand-built map, without a real `sysinfo::System` (#540).
fn collect_subtree_from_children(children: &HashMap<Pid, Vec<Pid>>, root: Pid) -> Vec<Pid> {
    let mut stack = vec![root];
    let mut out = vec![root];
    while let Some(cur) = stack.pop() {
        if let Some(kids) = children.get(&cur) {
            for k in kids {
                out.push(*k);
                stack.push(*k);
            }
        }
    }
    out
}

/// Background watcher. Joins on `Drop`; call [`BannerWatcher::stop`] for
/// explicit shutdown if you want to bound the join.
pub struct BannerWatcher {
    stop_tx: Option<mpsc::Sender<()>>,
    handle: Option<JoinHandle<()>>,
}

impl BannerWatcher {
    /// Spawn the watcher. `enabled = false` returns an inert handle —
    /// no thread, no banners.
    pub fn spawn(cfg: CpuBannerCfg) -> Self {
        if !cfg.enabled {
            return Self {
                stop_tx: None,
                handle: None,
            };
        }
        let (tx, rx) = mpsc::channel();
        let handle = thread::Builder::new()
            .name("clud-cpu-banner".into())
            .spawn(move || run_watcher_loop(cfg, rx))
            .ok();
        Self {
            stop_tx: Some(tx),
            handle,
        }
    }

    /// Explicit shutdown. Idempotent; safe to call before `Drop`.
    pub fn stop(&mut self) {
        if let Some(tx) = self.stop_tx.take() {
            let _ = tx.send(());
        }
        if let Some(h) = self.handle.take() {
            let _ = h.join();
        }
    }
}

impl Drop for BannerWatcher {
    fn drop(&mut self) {
        self.stop();
    }
}

fn run_watcher_loop(cfg: CpuBannerCfg, stop_rx: mpsc::Receiver<()>) {
    let mut sampler = Sampler::new();
    let mut state = CpuBannerState::default();
    // Prime: sysinfo needs two refreshes for non-zero cpu_usage. Do one
    // up-front so the first real tick has meaningful data.
    let _ = sampler.tick(cfg.originator_pid);
    // Issue #540: adaptive cadence from here on — a large subtree backs
    // off the tick interval so the sampler's own refresh cost stays
    // bounded. `cfg.tick` (== DEFAULT_TICK) seeds the first real wait,
    // which matches what `sample_interval` would return for a small
    // subtree anyway.
    let mut interval = cfg.tick;
    loop {
        if stop_rx.recv_timeout(interval).is_ok() {
            return;
        }
        let sample = sampler.tick(cfg.originator_pid);
        interval = sample_interval(sample.proc_count);
        if let Some(line) = state.poll(sample, &cfg) {
            eprintln!("{}", line.render());
        }
    }
}

fn format_rss(bytes: u64) -> String {
    let mib = bytes as f64 / (1024.0 * 1024.0);
    let gib = mib / 1024.0;
    if gib >= 1.0 {
        format!("{gib:.2} GiB")
    } else {
        format!("{mib:.0} MiB")
    }
}

fn format_age(d: Duration) -> String {
    let secs = d.as_secs();
    if secs >= 3600 {
        format!("{} h", secs / 3600)
    } else if secs >= 60 {
        format!("{} m", secs / 60)
    } else {
        format!("{secs} s")
    }
}

#[cfg(test)]
mod tests {
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
        assert!(
            needs_tree_rebuild(true, Some(t0), t0),
            "empty cache always rebuilds"
        );
        assert!(
            needs_tree_rebuild(false, None, t0),
            "no prior walk always rebuilds"
        );
        let just_under = t0 + TREE_REBUILD_INTERVAL - Duration::from_millis(1);
        assert!(
            !needs_tree_rebuild(false, Some(t0), just_under),
            "under the interval should reuse the cached list"
        );
        let at_or_over = t0 + TREE_REBUILD_INTERVAL;
        assert!(
            needs_tree_rebuild(false, Some(t0), at_or_over),
            "at/over the interval should rebuild"
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
        use std::process::{Child, Command, Stdio};

        const SPAWN_COUNT: usize = 55;
        let mut children: Vec<Child> = Vec::new();
        for _ in 0..SPAWN_COUNT {
            // Windows: `ping -n 31 127.0.0.1` ≈ a 30 s sleep. Deliberately
            // NOT `cmd /C timeout /T 30` — with Git-for-Windows on PATH,
            // `timeout` can resolve to GNU coreutils' timeout, which
            // rejects `/T` and exits instantly, collapsing the subtree
            // this bench is supposed to measure.
            let spawned = if cfg!(windows) {
                Command::new("ping")
                    .args(["-n", "31", "127.0.0.1"])
                    .stdout(Stdio::null())
                    .stderr(Stdio::null())
                    .spawn()
            } else {
                Command::new("sleep")
                    .arg("30")
                    .stdout(Stdio::null())
                    .stderr(Stdio::null())
                    .spawn()
            };
            if let Ok(c) = spawned {
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

        for c in &mut children {
            let _ = c.kill();
            let _ = c.wait();
        }
    }
}
