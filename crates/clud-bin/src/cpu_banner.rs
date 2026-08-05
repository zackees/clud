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

/// Issue #709: ceiling the rebuild interval backs off to while the subtree
/// stays quiet.
///
/// The full-system walk is the banner's dominant cost — #553 measured it at
/// 225 ms on a loaded box and 2.09 s saturated — and on an idle session it
/// rediscovers the same pids every 30 s forever. Four times fewer walks on a
/// quiet session, with no loss of responsiveness: see
/// [`RebuildCadence::record_walk`] for why activity snaps straight back.
const TREE_REBUILD_IDLE_INTERVAL: Duration = Duration::from_secs(120);

/// Summed subtree CPU (percent of one core) at or above which a session
/// counts as *active* for rebuild-backoff purposes.
///
/// Deliberately far below [`RELATIVE_HOST_FRACTION`]'s banner threshold: this
/// is "is anything happening at all", not "is this worth warning about".
const REBUILD_QUIET_PCT: f32 = 5.0;

/// "Quiet enough to back off" must never be confused with "quiet enough not to
/// warn". Enforced at compile time rather than by a test: it is a relationship
/// between two constants, so a build failure is the honest signal.
const _: () = assert!(REBUILD_QUIET_PCT < RELATIVE_HOST_FRACTION * 100.0);

/// Consecutive quiet rebuilds before backing off one step.
const REBUILD_QUIET_WALKS_BEFORE_BACKOFF: u32 = 2;

/// Backoff state for the subtree rebuild (issue #709).
///
/// Same shape as `console_title::KeeperCadence`, and split out as a pure state
/// machine for the same reason: the policy is testable without a process tree,
/// a thread, or two minutes of waiting.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RebuildCadence {
    interval: Duration,
    quiet_walks: u32,
}

impl RebuildCadence {
    pub fn new() -> Self {
        Self {
            interval: TREE_REBUILD_INTERVAL,
            quiet_walks: 0,
        }
    }

    pub fn interval(self) -> Duration {
        self.interval
    }

    /// Record one rebuild and return the cadence for the next one.
    ///
    /// `quiet` means no tick since the previous rebuild saw the subtree above
    /// [`REBUILD_QUIET_PCT`].
    ///
    /// Activity resets to the base interval in one step rather than stepping
    /// down. The backoff exists to make *doing nothing* cheap; it must never
    /// be the reason a newly-spawned descendant goes undiscovered. The caller
    /// additionally forces an immediate rebuild on the transition — backing
    /// off is only ever paid for by an idle session.
    pub fn record_walk(self, quiet: bool) -> Self {
        if !quiet {
            return Self::new();
        }
        let quiet_walks = self.quiet_walks.saturating_add(1);
        if quiet_walks < REBUILD_QUIET_WALKS_BEFORE_BACKOFF {
            return Self {
                interval: self.interval,
                quiet_walks,
            };
        }
        Self {
            interval: self
                .interval
                .saturating_mul(2)
                .min(TREE_REBUILD_IDLE_INTERVAL),
            quiet_walks: 0,
        }
    }
}

impl Default for RebuildCadence {
    fn default() -> Self {
        Self::new()
    }
}

/// Pure decision for whether [`Sampler::tick`] must pay for a full-system
/// walk this tick (vs. reusing the cached subtree pid list for a targeted
/// refresh). Rebuilds when the cache is empty (first tick), when there is
/// no record of a prior walk, or when the prior walk is at least `interval`
/// old — where `interval` comes from [`RebuildCadence`] (#709), not a
/// constant.
fn needs_tree_rebuild(
    cache_empty: bool,
    last_walk: Option<Instant>,
    now: Instant,
    interval: Duration,
) -> bool {
    if cache_empty {
        return true;
    }
    match last_walk {
        None => true,
        Some(walked_at) => now.duration_since(walked_at) >= interval,
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
    /// When `cached_pids` was last (re)built. `None` before the first tick,
    /// and reset to `None` to force an immediate rebuild when a backed-off
    /// session comes back to life (#709).
    last_tree_walk: Option<Instant>,
    /// How long to wait between full-system walks. Backs off while the
    /// subtree stays quiet (#709).
    rebuild_cadence: RebuildCadence,
    /// Whether any tick since the last walk saw the subtree above
    /// [`REBUILD_QUIET_PCT`].
    busy_since_last_walk: bool,
}

impl Sampler {
    pub fn new() -> Self {
        Self {
            system: System::new(),
            started_at: Instant::now(),
            cached_pids: Vec::new(),
            last_tree_walk: None,
            rebuild_cadence: RebuildCadence::new(),
            busy_since_last_walk: false,
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

        if needs_tree_rebuild(
            self.cached_pids.is_empty(),
            self.last_tree_walk,
            now,
            self.rebuild_cadence.interval(),
        ) {
            // Full-system refresh: the only way to discover new/dead
            // descendants and rebuild the parent-PID graph.
            self.system
                .refresh_processes_specifics(ProcessesToUpdate::All, true, refresh_kind);
            self.cached_pids = collect_subtree(&self.system, root);
            self.last_tree_walk = Some(now);
            // #709: a walk that found a quiet session earns a longer gap
            // before the next one.
            self.rebuild_cadence = self.rebuild_cadence.record_walk(!self.busy_since_last_walk);
            self.busy_since_last_walk = false;
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
        // #709: any real activity ends the quiet run. If we had already backed
        // off, rediscover the subtree on the *next* tick rather than waiting
        // out the remaining gap — a busy session must never be sampled against
        // a stale pid list, which is the whole accuracy risk of backing off.
        if subtree_cpu_pct >= REBUILD_QUIET_PCT {
            if self.rebuild_cadence.interval() > TREE_REBUILD_INTERVAL {
                self.last_tree_walk = None;
            }
            self.rebuild_cadence = RebuildCadence::new();
            self.busy_since_last_walk = true;
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
#[path = "cpu_banner_tests.rs"]
mod tests;
