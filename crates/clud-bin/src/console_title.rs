//! Set the console window title to `clud <cwd-name>` on launch and keep
//! it pinned for the lifetime of the process.
//!
//! On Windows, when `clud` runs in cmd.exe / Windows Terminal, the title
//! bar otherwise shows the host shell's title — usually a generic
//! `Command Prompt` or the path to cmd.exe. Stamping `clud <cwd-name>`
//! makes it obvious at a glance which window is the active session and
//! which directory it's working in.
//!
//! Just stamping once isn't enough: clud spawns a TUI backend
//! (claude.exe / codex.exe) that — along with any tool subprocess it
//! invokes (git, npm, build runners) — emits OSC 0/2 title-set escape
//! sequences continuously. Two complementary defenses keep our title
//! visible:
//!
//! 1. [`keep_setting_in_background`] starts a low-frequency poller that
//!    re-applies our title whenever the live console title drifts. This
//!    is the only option in subprocess mode (the default Claude path on
//!    Windows) because the child inherits clud's stdio handles directly
//!    — we can't intercept its OSC bytes.
//!
//! 2. [`OscTitleStripper`] is a stream-resumable byte filter used by the
//!    PTY pump (`session.rs`) to drop OSC 0/2 sequences from the child's
//!    output before they reach our terminal. PTY mode is opt-in
//!    (`--pty`) and used by `clud loop` on POSIX. With the stripper in
//!    place the title doesn't flicker — the keeper rarely fires.
//!
//! POSIX terminals are out of scope for the title-setting half; the
//! cross-platform stubs here are no-ops so the call sites in `main.rs`
//! don't need a `cfg`. The OSC stripper is platform-agnostic because
//! the PTY pump runs on every platform.

use std::sync::{Arc, Mutex, OnceLock};
#[cfg(any(windows, test))]
use std::time::Duration;
use std::time::Instant;

#[cfg(any(windows, test))]
const CPU_FLASH_THRESHOLD_PCT: f32 = 70.0;
/// Fallback RPC cadence, used only when the daemon publishes no snapshot file
/// (an older daemon, or one that has not sampled yet).
///
/// #547 dropped this from 2 s to 15 s: it is now a compatibility path, not the
/// steady state, and 2 s of polling per open terminal is the cost the issue
/// exists to remove. The snapshot path costs one `stat` per keeper pass.
#[cfg(windows)]
const CPU_METRICS_FALLBACK_POLL_INTERVAL: Duration = Duration::from_secs(15);
#[cfg(any(windows, test))]
const CPU_ALERT_TTL: Duration = Duration::from_millis(2500);
#[cfg(any(windows, test))]
const CPU_FLASH_INTERVAL: Duration = Duration::from_millis(500);

/// Keeper cadence while anything is changing — unchanged from before
/// (issue #547): drift is corrected within a noticeable beat, and the flash
/// animation needs this resolution to look like a flash.
#[cfg(any(windows, test))]
const KEEPER_FAST_INTERVAL: Duration = Duration::from_millis(750);

/// Ceiling the keeper backs off to once nothing has changed for a while.
///
/// Chosen against the drift it must correct: a title clobbered by a child
/// process is a cosmetic fault, and correcting it within 3 s of a quiet
/// terminal is unnoticeable, while 4× fewer wakeups per open terminal is not.
#[cfg(any(windows, test))]
const KEEPER_IDLE_INTERVAL: Duration = Duration::from_millis(3_000);

/// Consecutive unchanged passes before backing off one step.
///
/// Four passes ≈ 3 s of genuine quiet, so a terminal being actively typed in
/// never reaches the slow cadence.
#[cfg(any(windows, test))]
const KEEPER_STABLE_PASSES_BEFORE_BACKOFF: u32 = 4;

/// Backoff state for the title keeper (issue #547).
///
/// Split out as a pure state machine — like `update_cpu_alert_locked` — so the
/// cadence policy is testable without a console, a thread, or 3 s of waiting.
#[cfg(any(windows, test))]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct KeeperCadence {
    interval: Duration,
    stable_passes: u32,
}

#[cfg(any(windows, test))]
impl KeeperCadence {
    pub fn new() -> Self {
        Self {
            interval: KEEPER_FAST_INTERVAL,
            stable_passes: 0,
        }
    }

    pub fn interval(self) -> Duration {
        self.interval
    }

    /// Record one keeper pass and return the cadence for the next sleep.
    ///
    /// `changed` means anything the keeper exists to react to moved: the title
    /// drifted and was re-stamped, or the CPU alert changed state.
    ///
    /// A change snaps straight back to the fast cadence rather than stepping
    /// down gradually. Responsiveness is the feature here; the backoff only
    /// exists to make *doing nothing* cheap, so it must never be the reason a
    /// visible change is slow to appear.
    pub fn record_pass(self, changed: bool) -> Self {
        if changed {
            return Self::new();
        }
        let stable_passes = self.stable_passes.saturating_add(1);
        if stable_passes < KEEPER_STABLE_PASSES_BEFORE_BACKOFF {
            return Self {
                interval: self.interval,
                stable_passes,
            };
        }
        // Doubling rather than jumping to the ceiling: a terminal that goes
        // quiet for a few seconds and then changes again pays only a small
        // latency penalty, while one quiet for minutes still reaches 3 s.
        let doubled = self.interval.saturating_mul(2).min(KEEPER_IDLE_INTERVAL);
        Self {
            interval: doubled,
            stable_passes: 0,
        }
    }
}

#[cfg(any(windows, test))]
impl Default for KeeperCadence {
    fn default() -> Self {
        Self::new()
    }
}

#[derive(Debug)]
#[cfg_attr(not(any(windows, test)), allow(dead_code))]
struct TitleState {
    base_title: String,
    cpu_alert: Option<CpuAlert>,
}

#[derive(Debug)]
#[cfg_attr(not(any(windows, test)), allow(dead_code))]
struct CpuAlert {
    title: String,
    observed_at: Instant,
    expires_at: Instant,
}

/// The title we want the console to display, shared between the
/// foreground thread and the keeper thread. Empty string means
/// `set_for_current_cwd` was never called and the keeper should idle.
fn title_state_cell() -> &'static Arc<Mutex<TitleState>> {
    static CELL: OnceLock<Arc<Mutex<TitleState>>> = OnceLock::new();
    CELL.get_or_init(|| {
        Arc::new(Mutex::new(TitleState {
            base_title: String::new(),
            cpu_alert: None,
        }))
    })
}

/// Set the console title to `clud <cwd-basename>` for the current
/// working directory and record it so `keep_setting_in_background` can
/// re-apply it after drift. Best-effort — failures are silent.
///
/// Called once near the top of `main`.
pub fn set_for_current_cwd() {
    let basename = current_cwd_basename().unwrap_or_else(|| "?".to_string());
    let title = title_for_cwd_name(&basename);
    let mut state = title_state_cell()
        .lock()
        .expect("title state mutex poisoned");
    state.base_title = title.clone();
    state.cpu_alert = None;
    drop(state);
    set_title(&title);
}

/// Spawn a Windows-only daemon thread that re-applies the desired title
/// every ~750 ms whenever the live console title has drifted away. No-op
/// if `set_for_current_cwd` was never called (desired title is empty).
///
/// Idempotent: the `OnceLock` guarantees at most one keeper thread per
/// process, even if this is called more than once.
///
/// The thread is a daemon — it has no join handle and runs until
/// process exit. The 750 ms cadence is a tradeoff: short enough that
/// drift is corrected within a noticeable beat, long enough that
/// re-stamping doesn't visibly compete with a child that legitimately
/// changes the title (e.g. the OSC stripper covers PTY mode, the keeper
/// is the safety net for subprocess mode).
pub fn keep_setting_in_background() {
    static STARTED: OnceLock<()> = OnceLock::new();
    STARTED.get_or_init(spawn_keeper_thread);
}

#[cfg(windows)]
fn spawn_keeper_thread() {
    let _ = std::thread::Builder::new()
        .name("clud-title-keeper".into())
        .spawn(|| {
            let mut last_metrics_poll = None::<Instant>;
            let mut last_snapshot_mtime = None::<std::time::SystemTime>;
            let mut cadence = KeeperCadence::new();
            let mut last_title = None::<String>;
            loop {
                let now = Instant::now();
                // #547: one `stat` per pass. When the daemon publishes, this is
                // the whole cost of the alert -- no connection, no daemon work,
                // and no read at all unless the mtime moved.
                if !refresh_cpu_alert_from_snapshot(now, &mut last_snapshot_mtime)
                    && last_metrics_poll
                        .map(|last| last.elapsed() >= CPU_METRICS_FALLBACK_POLL_INTERVAL)
                        .unwrap_or(true)
                {
                    // Compatibility path only: an older daemon that publishes
                    // nothing still gets its alert, just at 15 s instead of 2 s.
                    last_metrics_poll = Some(now);
                    refresh_cpu_alert_from_daemon(now);
                }
                let want = current_desired_title(Instant::now());
                let mut changed = false;
                if !want.is_empty() {
                    let current = read_console_title();
                    if current.as_deref() != Some(want.as_str()) {
                        set_title(&want);
                        changed = true;
                    }
                }
                // The desired title moving is itself a change even when the
                // console already agreed — that is how the flash animation
                // advances, and backing off through it would make the alert
                // stutter.
                if last_title.as_deref() != Some(want.as_str()) {
                    changed = true;
                    last_title = Some(want);
                }
                cadence = cadence.record_pass(changed);
                std::thread::sleep(cadence.interval());
            }
        });
}

#[cfg(not(windows))]
fn spawn_keeper_thread() {
    // Title management is out of scope on POSIX (matches the per-call
    // `set_title` no-op). Don't spawn a thread that does nothing.
}

#[cfg(windows)]
fn read_console_title() -> Option<String> {
    extern "system" {
        fn GetConsoleTitleW(buf: *mut u16, size: u32) -> u32;
    }
    let mut buf: Vec<u16> = vec![0; 1024];
    // SAFETY: `buf` is a valid, mutable, aligned u16 buffer of length
    // 1024. GetConsoleTitleW writes at most `size` u16s to `buf`.
    let n = unsafe { GetConsoleTitleW(buf.as_mut_ptr(), buf.len() as u32) };
    if n == 0 {
        // 0 = empty title or error (e.g. no console attached). Treat as
        // unknown so the keeper doesn't try to re-stamp.
        return None;
    }
    Some(String::from_utf16_lossy(&buf[..n as usize]))
}

/// Format `<cwd-name>` into the canonical title string. Pure helper —
/// unit-tested on every platform.
pub fn title_for_cwd_name(cwd_name: &str) -> String {
    format!("clud {}", cwd_name)
}

#[cfg(any(windows, test))]
fn title_for_cpu_alert(base_title: &str, cpu_pct: f32) -> String {
    format!("{base_title} CPU {:.0}%", cpu_pct)
}

#[cfg(windows)]
fn current_desired_title(now: Instant) -> String {
    let mut state = title_state_cell()
        .lock()
        .expect("title state mutex poisoned");
    current_desired_title_locked(&mut state, now)
}

#[cfg(any(windows, test))]
fn current_desired_title_locked(state: &mut TitleState, now: Instant) -> String {
    let Some(alert) = &state.cpu_alert else {
        return state.base_title.clone();
    };
    if now >= alert.expires_at {
        state.cpu_alert = None;
        return state.base_title.clone();
    }
    let flash_tick = now.saturating_duration_since(alert.observed_at).as_millis()
        / CPU_FLASH_INTERVAL.as_millis();
    if flash_tick % 2 == 0 {
        alert.title.clone()
    } else {
        state.base_title.clone()
    }
}

/// What the keeper learned from one snapshot check (#547).
#[cfg(any(windows, test))]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SnapshotCheck {
    /// The file is absent — this daemon does not publish. The caller falls back
    /// to the RPC at [`CPU_METRICS_FALLBACK_POLL_INTERVAL`].
    Absent,
    /// Present and unchanged since the last look. Nothing to do, and nothing
    /// was read: this is the idle steady state, and it costs one `stat`.
    Unchanged,
    /// Present and newer than the last look — worth reading and parsing.
    Changed,
}

/// Decide whether the snapshot is worth reading, from its mtime alone.
///
/// Pure so the "idle costs one stat, zero reads" property is asserted directly
/// rather than inferred from a benchmark. `last_seen` is the mtime observed on
/// the previous pass.
#[cfg(any(windows, test))]
pub(crate) fn classify_snapshot(
    mtime: Option<std::time::SystemTime>,
    last_seen: &mut Option<std::time::SystemTime>,
) -> SnapshotCheck {
    let Some(mtime) = mtime else {
        // Deliberately does not clear `last_seen`: a daemon restart rewrites
        // the file with a fresh mtime, which reads as Changed either way.
        return SnapshotCheck::Absent;
    };
    if *last_seen == Some(mtime) {
        return SnapshotCheck::Unchanged;
    }
    *last_seen = Some(mtime);
    SnapshotCheck::Changed
}

/// Refresh the CPU alert, preferring the published snapshot over an RPC (#547).
///
/// Returns `true` when the snapshot path answered, so the caller knows not to
/// run the fallback timer.
#[cfg(windows)]
fn refresh_cpu_alert_from_snapshot(
    now: Instant,
    last_seen: &mut Option<std::time::SystemTime>,
) -> bool {
    let Ok(state_dir) = crate::daemon::default_state_dir() else {
        clear_cpu_alert();
        return false;
    };
    let path = crate::daemon::metrics_snapshot_path(&state_dir);
    let mtime = std::fs::metadata(&path)
        .and_then(|meta| meta.modified())
        .ok();
    match classify_snapshot(mtime, last_seen) {
        SnapshotCheck::Absent => false,
        SnapshotCheck::Unchanged => true,
        SnapshotCheck::Changed => {
            match std::fs::read_to_string(&path)
                .ok()
                .and_then(|text| serde_json::from_str::<crate::daemon::MetricsSnapshot>(&text).ok())
            {
                Some(snapshot) => update_cpu_alert(snapshot.cpu_pct, now),
                // Torn or malformed read: treat as no alert, same as the RPC
                // error path always has.
                None => clear_cpu_alert(),
            }
            true
        }
    }
}

#[cfg(windows)]
fn refresh_cpu_alert_from_daemon(now: Instant) {
    let Ok(state_dir) = crate::daemon::default_state_dir() else {
        clear_cpu_alert();
        return;
    };
    match crate::daemon::daemon_client_metrics(&state_dir) {
        Ok((_pid, cpu_pct)) => update_cpu_alert(cpu_pct, now),
        Err(_) => clear_cpu_alert(),
    }
}

#[cfg(windows)]
fn update_cpu_alert(cpu_pct: f32, now: Instant) {
    let mut state = title_state_cell()
        .lock()
        .expect("title state mutex poisoned");
    update_cpu_alert_locked(&mut state, cpu_pct, now);
}

#[cfg(any(windows, test))]
fn update_cpu_alert_locked(state: &mut TitleState, cpu_pct: f32, now: Instant) {
    if cpu_pct > CPU_FLASH_THRESHOLD_PCT && !state.base_title.is_empty() {
        state.cpu_alert = Some(CpuAlert {
            title: title_for_cpu_alert(&state.base_title, cpu_pct),
            observed_at: now,
            expires_at: now + CPU_ALERT_TTL,
        });
    } else {
        state.cpu_alert = None;
    }
}

#[cfg(windows)]
fn clear_cpu_alert() {
    title_state_cell()
        .lock()
        .expect("title state mutex poisoned")
        .cpu_alert = None;
}

/// Best-effort lookup of the current working directory's leaf name.
/// Returns `None` if `current_dir()` fails or the path has no final
/// component (e.g. the filesystem root on Windows like `C:\`).
fn current_cwd_basename() -> Option<String> {
    let cwd = std::env::current_dir().ok()?;
    cwd.file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .or_else(|| {
            // On `C:\` (drive root) there's no `file_name`; fall back to
            // the drive letter so the title is still informative.
            cwd.to_string_lossy()
                .split(':')
                .next()
                .map(|s| s.to_string())
        })
}

#[cfg(windows)]
fn set_title(title: &str) {
    // SetConsoleTitleW writes to the console window owning this
    // process — which is the cmd.exe / Windows Terminal we want. Wide
    // (UTF-16) form so non-ASCII cwd names render correctly.
    extern "system" {
        fn SetConsoleTitleW(lp_console_title: *const u16) -> i32;
    }
    let mut wide: Vec<u16> = title.encode_utf16().collect();
    wide.push(0);
    // SAFETY: `wide` is a properly null-terminated UTF-16 buffer with
    // a stable address until the unsafe block ends.
    unsafe {
        let _ = SetConsoleTitleW(wide.as_ptr());
    }
}

#[cfg(not(windows))]
fn set_title(_title: &str) {
    // Out of scope per the issue; intentional no-op.
}

// ─── OSC 0/2 stream filter ──────────────────────────────────────────────

/// Stream-resumable filter that drops OSC 0 and OSC 2 (window-title)
/// escape sequences from a byte stream and passes everything else
/// through verbatim.
///
/// OSC syntax: `ESC ] Ps ; Pt ST` where `ST` is `BEL` (0x07) or
/// `ESC \\` (0x1B 0x5C). OSC 0 sets icon name + window title; OSC 2
/// sets only the window title. Other OSC numbers (8 hyperlinks, 10/11
/// colors, 52 clipboard, 133 prompt marks, …) pass through.
///
/// The filter survives across `process()` calls: an OSC sequence split
/// across reads is handled correctly, including ST split between two
/// chunks.
pub struct OscTitleStripper {
    state: OscState,
    /// Buffered digits between `ESC ]` and `;`. Used to decide swallow
    /// vs passthrough once the `;` arrives.
    digits: Vec<u8>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum OscState {
    Normal,
    AfterEsc,
    InOscNumber,
    SwallowOscBody,
    SwallowAfterEsc,
    PassthroughOscBody,
    PassthroughAfterEsc,
}

impl OscTitleStripper {
    pub fn new() -> Self {
        Self {
            state: OscState::Normal,
            digits: Vec::new(),
        }
    }

    /// Process a chunk and return the bytes that should be forwarded
    /// downstream (terminal stdout, in production).
    pub fn process(&mut self, chunk: &[u8]) -> Vec<u8> {
        let mut out = Vec::with_capacity(chunk.len());
        for &b in chunk {
            self.process_byte(b, &mut out);
        }
        out
    }

    fn process_byte(&mut self, b: u8, out: &mut Vec<u8>) {
        match self.state {
            OscState::Normal => {
                if b == 0x1b {
                    self.state = OscState::AfterEsc;
                } else {
                    out.push(b);
                }
            }
            OscState::AfterEsc => match b {
                b']' => {
                    self.state = OscState::InOscNumber;
                    self.digits.clear();
                }
                0x1b => {
                    // ESC ESC: emit the first ESC, stay waiting on the second.
                    out.push(0x1b);
                }
                _ => {
                    out.push(0x1b);
                    out.push(b);
                    self.state = OscState::Normal;
                }
            },
            OscState::InOscNumber => {
                if b.is_ascii_digit() {
                    self.digits.push(b);
                } else if b == b';' {
                    if self.digits == b"0" || self.digits == b"2" {
                        self.state = OscState::SwallowOscBody;
                    } else {
                        out.push(0x1b);
                        out.push(b']');
                        out.extend_from_slice(&self.digits);
                        out.push(b';');
                        self.state = OscState::PassthroughOscBody;
                    }
                    self.digits.clear();
                } else if b == 0x07 {
                    // BEL with empty/numeric body and no `;` — terminator
                    // for a malformed OSC. Drop quietly; nothing visible
                    // was set.
                    self.digits.clear();
                    self.state = OscState::Normal;
                } else if b == 0x1b {
                    // ESC inside the number — could be the start of an
                    // ST (`ESC \\`). Emit prefix as passthrough so we
                    // don't lose the sequence on a real terminal.
                    out.push(0x1b);
                    out.push(b']');
                    out.extend_from_slice(&self.digits);
                    self.digits.clear();
                    self.state = OscState::PassthroughAfterEsc;
                } else {
                    // Non-digit, non-`;` byte. Bogus OSC — flush prefix
                    // and that byte, switch to passthrough until ST.
                    out.push(0x1b);
                    out.push(b']');
                    out.extend_from_slice(&self.digits);
                    out.push(b);
                    self.digits.clear();
                    self.state = OscState::PassthroughOscBody;
                }
            }
            OscState::SwallowOscBody => match b {
                0x07 => self.state = OscState::Normal,
                0x1b => self.state = OscState::SwallowAfterEsc,
                _ => {}
            },
            OscState::SwallowAfterEsc => match b {
                b'\\' | 0x07 => self.state = OscState::Normal,
                0x1b => {} // stay
                _ => self.state = OscState::SwallowOscBody,
            },
            OscState::PassthroughOscBody => {
                out.push(b);
                match b {
                    0x07 => self.state = OscState::Normal,
                    0x1b => self.state = OscState::PassthroughAfterEsc,
                    _ => {}
                }
            }
            OscState::PassthroughAfterEsc => {
                out.push(b);
                if b == b'\\' || b == 0x07 {
                    self.state = OscState::Normal;
                } else if b != 0x1b {
                    self.state = OscState::PassthroughOscBody;
                }
            }
        }
    }
}

impl Default for OscTitleStripper {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // Issue #547: the keeper backoff state machine. Pure, so the whole policy
    // is exercised without a console, a thread, or seconds of real waiting.

    #[test]
    fn a_fresh_keeper_starts_at_the_fast_cadence() {
        assert_eq!(KeeperCadence::new().interval(), KEEPER_FAST_INTERVAL);
    }

    #[test]
    fn the_cadence_holds_until_enough_quiet_passes() {
        // Backing off on the first quiet pass would slow a terminal that is
        // merely between keystrokes.
        let mut cadence = KeeperCadence::new();
        for _ in 0..(KEEPER_STABLE_PASSES_BEFORE_BACKOFF - 1) {
            cadence = cadence.record_pass(false);
            assert_eq!(cadence.interval(), KEEPER_FAST_INTERVAL);
        }
        cadence = cadence.record_pass(false);
        assert_eq!(cadence.interval(), KEEPER_FAST_INTERVAL * 2);
    }

    #[test]
    fn sustained_quiet_reaches_the_ceiling_and_stops() {
        let mut cadence = KeeperCadence::new();
        for _ in 0..200 {
            cadence = cadence.record_pass(false);
        }
        assert_eq!(
            cadence.interval(),
            KEEPER_IDLE_INTERVAL,
            "backoff must clamp, not grow without bound"
        );
    }

    #[test]
    fn any_change_snaps_straight_back_to_fast() {
        // The acceptance criterion: an externally-changed title is re-stamped
        // within one fast beat of the next check, not after a gradual ramp.
        // Responsiveness is the feature; the backoff only makes idling cheap.
        let mut cadence = KeeperCadence::new();
        for _ in 0..200 {
            cadence = cadence.record_pass(false);
        }
        assert_eq!(cadence.interval(), KEEPER_IDLE_INTERVAL);

        let after_change = cadence.record_pass(true);
        assert_eq!(
            after_change.interval(),
            KEEPER_FAST_INTERVAL,
            "a change must reset to fast in one step"
        );
    }

    #[test]
    fn the_stable_counter_resets_on_change_too() {
        // Otherwise a terminal alternating change/quiet/change/quiet would
        // accumulate quiet passes across the changes and eventually back off
        // while still active.
        let mut cadence = KeeperCadence::new();
        for _ in 0..(KEEPER_STABLE_PASSES_BEFORE_BACKOFF - 1) {
            cadence = cadence.record_pass(false);
        }
        cadence = cadence.record_pass(true);
        for _ in 0..(KEEPER_STABLE_PASSES_BEFORE_BACKOFF - 1) {
            cadence = cadence.record_pass(false);
            assert_eq!(
                cadence.interval(),
                KEEPER_FAST_INTERVAL,
                "quiet passes must not carry over a change"
            );
        }
    }

    #[test]
    fn worst_case_drift_latency_stays_within_the_stated_budget() {
        // #547 budgets an above-threshold transition reaching the title within
        // 5 s. The keeper's contribution is bounded by the ceiling.
        assert!(
            KEEPER_IDLE_INTERVAL <= Duration::from_secs(5),
            "ceiling must fit the stated alert-latency budget"
        );
    }

    #[test]
    fn title_uses_clud_prefix_with_cwd_name() {
        assert_eq!(title_for_cwd_name("clud"), "clud clud");
        assert_eq!(title_for_cwd_name("my-app"), "clud my-app");
    }

    #[test]
    fn title_handles_non_ascii_cwd_name() {
        // Cyrillic + emoji to verify the formatter doesn't choke on
        // non-ASCII paths (which then flow through SetConsoleTitleW's
        // wide-string conversion on Windows).
        assert_eq!(title_for_cwd_name("проект"), "clud проект");
        assert_eq!(title_for_cwd_name("🚀"), "clud 🚀");
    }

    #[test]
    fn title_passes_through_empty_name() {
        // Defensive: even an empty cwd name produces a well-formed
        // title rather than panicking.
        assert_eq!(title_for_cwd_name(""), "clud ");
    }

    #[test]
    fn title_does_not_trim_or_normalize_input() {
        // We're explicit about what we format — no surprise trims, no
        // case changes. The cwd basename is shown verbatim.
        assert_eq!(title_for_cwd_name(" spaced "), "clud  spaced ");
        assert_eq!(title_for_cwd_name("MIXED-Case"), "clud MIXED-Case");
    }

    #[test]
    fn current_cwd_basename_returns_some_in_test_env() {
        // Cargo runs tests with the manifest dir as cwd, which always
        // has a leaf component (`clud-bin`). Smoke test the helper
        // without asserting the specific value.
        let got = current_cwd_basename();
        assert!(got.is_some(), "expected Some(_) cwd basename in test env");
        assert!(!got.unwrap().is_empty(), "basename should not be empty");
    }

    #[test]
    fn set_for_current_cwd_does_not_panic() {
        // Smoke test on every platform — the POSIX stub is a no-op,
        // and on Windows SetConsoleTitleW returns silently when there
        // is no console (e.g. inside `cargo test` under a CI runner).
        set_for_current_cwd();
    }

    #[test]
    fn set_for_current_cwd_records_desired_title() {
        // The keeper thread reads from this cell to decide whether to
        // re-stamp; if the cell stays empty after `set_for_current_cwd`,
        // the keeper would idle forever. Verify the value is captured.
        set_for_current_cwd();
        let stored = title_state_cell()
            .lock()
            .expect("title state mutex")
            .base_title
            .clone();
        assert!(
            stored.starts_with("clud "),
            "desired title should be the formatted form, got {stored:?}"
        );
        assert!(
            !stored.trim_end().eq("clud"),
            "desired title should include a basename component, got {stored:?}"
        );
    }

    #[test]
    fn cpu_alert_title_formats_percentage() {
        assert_eq!(title_for_cpu_alert("clud repo", 72.4), "clud repo CPU 72%");
        assert_eq!(title_for_cpu_alert("clud repo", 72.5), "clud repo CPU 72%");
        assert_eq!(title_for_cpu_alert("clud repo", 72.6), "clud repo CPU 73%");
    }

    #[test]
    fn cpu_alert_flashes_between_alert_and_base_title() {
        let now = Instant::now();
        let mut state = TitleState {
            base_title: "clud repo".to_string(),
            cpu_alert: None,
        };
        update_cpu_alert_locked(&mut state, 71.0, now);

        assert_eq!(
            current_desired_title_locked(&mut state, now),
            "clud repo CPU 71%"
        );
        assert_eq!(
            current_desired_title_locked(&mut state, now + CPU_FLASH_INTERVAL),
            "clud repo"
        );
        assert_eq!(
            current_desired_title_locked(&mut state, now + CPU_FLASH_INTERVAL * 2),
            "clud repo CPU 71%"
        );
    }

    #[test]
    fn cpu_alert_clears_below_threshold_and_after_ttl() {
        let now = Instant::now();
        let mut state = TitleState {
            base_title: "clud repo".to_string(),
            cpu_alert: None,
        };

        update_cpu_alert_locked(&mut state, 70.0, now);
        assert!(state.cpu_alert.is_none(), "70% is not above threshold");

        update_cpu_alert_locked(&mut state, 70.1, now);
        assert!(state.cpu_alert.is_some());

        update_cpu_alert_locked(&mut state, 10.0, now + Duration::from_secs(1));
        assert!(state.cpu_alert.is_none());

        update_cpu_alert_locked(&mut state, 80.0, now);
        assert_eq!(
            current_desired_title_locked(&mut state, now + CPU_ALERT_TTL),
            "clud repo"
        );
        assert!(state.cpu_alert.is_none());
    }

    #[test]
    fn keep_setting_in_background_is_idempotent_and_does_not_panic() {
        // Calling more than once must not spawn duplicate keeper
        // threads (OnceLock guard) and must not panic on POSIX where
        // the spawn helper is a no-op.
        keep_setting_in_background();
        keep_setting_in_background();
    }

    // ─── OscTitleStripper ──────────────────────────────────────────────

    #[test]
    fn osc_stripper_passthrough_for_plain_bytes() {
        let mut s = OscTitleStripper::new();
        assert_eq!(s.process(b"hello world\n"), b"hello world\n");
    }

    #[test]
    fn osc_stripper_drops_osc_0_with_bel_terminator() {
        let mut s = OscTitleStripper::new();
        let chunk = b"before\x1b]0;child-title\x07after";
        assert_eq!(s.process(chunk), b"beforeafter");
    }

    #[test]
    fn osc_stripper_drops_osc_2_with_st_terminator() {
        let mut s = OscTitleStripper::new();
        // ST = ESC \ (0x1B 0x5C)
        let chunk = b"x\x1b]2;another-title\x1b\\y";
        assert_eq!(s.process(chunk), b"xy");
    }

    #[test]
    fn osc_stripper_passes_through_osc_10_color_query() {
        // OSC 10 is the foreground-color query; vt100-class TUIs send
        // it to discover the terminal palette. Stripping it would hang
        // the child waiting for a reply that never comes.
        let mut s = OscTitleStripper::new();
        let chunk = b"\x1b]10;?\x07";
        assert_eq!(s.process(chunk), b"\x1b]10;?\x07");
    }

    #[test]
    fn osc_stripper_passes_through_osc_8_hyperlink() {
        let mut s = OscTitleStripper::new();
        // OSC 8 ; ; <url> ST <text> OSC 8 ; ; ST
        let chunk = b"\x1b]8;;https://example.com\x1b\\link\x1b]8;;\x1b\\";
        assert_eq!(s.process(chunk), chunk);
    }

    #[test]
    fn osc_stripper_passes_through_osc_133_prompt_marks() {
        let mut s = OscTitleStripper::new();
        let chunk = b"\x1b]133;A\x07$ ls\n\x1b]133;B\x07";
        assert_eq!(s.process(chunk), chunk);
    }

    #[test]
    fn osc_stripper_handles_split_across_chunks() {
        // Worst-case: OSC sequence split into many tiny chunks. The
        // filter must reassemble the prefix decision and the ST
        // detection across calls.
        let mut s = OscTitleStripper::new();
        let mut got = Vec::new();
        for piece in [
            &b"abc\x1b"[..],
            b"]",
            b"0",
            b";",
            b"split-title",
            b"\x07",
            b"xyz",
        ] {
            got.extend(s.process(piece));
        }
        assert_eq!(got, b"abcxyz");
    }

    #[test]
    fn osc_stripper_handles_st_split_across_chunks() {
        let mut s = OscTitleStripper::new();
        let mut got = Vec::new();
        got.extend(s.process(b"\x1b]0;t\x1b"));
        got.extend(s.process(b"\\tail"));
        assert_eq!(got, b"tail");
    }

    #[test]
    fn osc_stripper_handles_back_to_back_title_oscs() {
        let mut s = OscTitleStripper::new();
        let chunk = b"a\x1b]0;t1\x07b\x1b]2;t2\x1b\\c";
        assert_eq!(s.process(chunk), b"abc");
    }

    #[test]
    fn osc_stripper_lone_esc_is_buffered_until_resolved() {
        // A bare ESC by itself isn't emitted until we know whether it's
        // starting an OSC — but if the next byte isn't `]`, both bytes
        // must surface. This protects CSI sequences (ESC [ …) from
        // being eaten.
        let mut s = OscTitleStripper::new();
        assert_eq!(s.process(b"\x1b[31mred\x1b[0m"), b"\x1b[31mred\x1b[0m");
    }

    #[test]
    fn osc_stripper_double_esc_does_not_eat_either() {
        // ESC ESC ] 0 ; … BEL: the first ESC isn't OSC-related, the
        // second one starts an OSC. We must emit the first ESC.
        let mut s = OscTitleStripper::new();
        assert_eq!(s.process(b"\x1b\x1b]0;x\x07"), b"\x1b");
    }

    #[test]
    fn osc_stripper_malformed_osc_with_letter_passes_through() {
        // OSC `]X;` is bogus — but conservatively pass it through
        // rather than swallowing arbitrary bytes that might be
        // user-visible. Real terminals would also pass it through.
        let mut s = OscTitleStripper::new();
        let chunk = b"\x1b]X;weird\x07";
        assert_eq!(s.process(chunk), chunk);
    }

    #[test]
    fn osc_stripper_multidigit_non_title_passthrough() {
        // OSC 52 (clipboard) starts with `5` — must not be confused
        // with `2`. The digit accumulator handles multi-digit numbers.
        let mut s = OscTitleStripper::new();
        let chunk = b"\x1b]52;c;SGVsbG8=\x07";
        assert_eq!(s.process(chunk), chunk);
    }

    // ---- #547: publish/stat replaces the per-client 2 s poll ----

    use std::time::{Duration as StdDuration, SystemTime};

    /// The acceptance criterion, stated as a count: once the daemon publishes,
    /// an idle client performs **zero** reads (and therefore zero daemon
    /// connections) while nothing changes. Before #547 this path was one
    /// `Metrics` round-trip every 2 s, forever.
    #[test]
    fn an_idle_client_reads_the_snapshot_zero_times() {
        let mtime = SystemTime::UNIX_EPOCH + StdDuration::from_secs(1_000);
        let mut last_seen = None;

        // First look: the file is new to us, so it is read once.
        assert_eq!(
            classify_snapshot(Some(mtime), &mut last_seen),
            SnapshotCheck::Changed
        );

        // Every subsequent pass over an unchanged file costs one `stat` and
        // nothing else. 200 passes ~ 10 minutes of idle at the fast cadence.
        let reads = (0..200)
            .filter(|_| classify_snapshot(Some(mtime), &mut last_seen) == SnapshotCheck::Changed)
            .count();
        assert_eq!(reads, 0, "an idle client must not read the snapshot at all");
    }

    /// The falling edge has to reach the client too, or an alert sticks. A
    /// rewritten file has a new mtime, so it is read again.
    #[test]
    fn a_republished_snapshot_is_read_again() {
        let mut last_seen = None;
        let first = SystemTime::UNIX_EPOCH + StdDuration::from_secs(1_000);
        let second = first + StdDuration::from_secs(2);

        assert_eq!(
            classify_snapshot(Some(first), &mut last_seen),
            SnapshotCheck::Changed
        );
        assert_eq!(
            classify_snapshot(Some(first), &mut last_seen),
            SnapshotCheck::Unchanged
        );
        assert_eq!(
            classify_snapshot(Some(second), &mut last_seen),
            SnapshotCheck::Changed
        );
    }

    /// An older daemon publishes nothing. The client must notice and fall back
    /// to the RPC rather than silently losing the alert -- graceful degradation
    /// is what makes the file the *preferred* path rather than the only one.
    #[test]
    fn an_absent_snapshot_reports_absent_so_the_caller_can_fall_back() {
        let mut last_seen = None;
        assert_eq!(
            classify_snapshot(None, &mut last_seen),
            SnapshotCheck::Absent
        );
        assert!(
            last_seen.is_none(),
            "absence must not be recorded as a seen mtime"
        );
    }

    /// A daemon restart republishes with a fresh mtime, and the client must
    /// pick that up even though it had already seen an earlier file.
    #[test]
    fn a_daemon_restart_is_observed_through_the_absent_gap() {
        let mut last_seen = None;
        let before = SystemTime::UNIX_EPOCH + StdDuration::from_secs(1_000);
        let after = before + StdDuration::from_secs(30);

        assert_eq!(
            classify_snapshot(Some(before), &mut last_seen),
            SnapshotCheck::Changed
        );
        // Daemon down: file removed.
        assert_eq!(
            classify_snapshot(None, &mut last_seen),
            SnapshotCheck::Absent
        );
        // Daemon back, having republished.
        assert_eq!(
            classify_snapshot(Some(after), &mut last_seen),
            SnapshotCheck::Changed
        );
    }

    /// Alert latency budget from #547: an above-threshold transition must reach
    /// the title within 5 s. The daemon samples every 2 s and the client checks
    /// every keeper pass, whose *slowest* cadence is the idle ceiling.
    #[test]
    fn the_alert_latency_budget_is_met_by_construction() {
        let worst_case = crate::daemon::CPU_SAMPLE_INTERVAL_FOR_TEST + KEEPER_IDLE_INTERVAL;
        assert!(
            worst_case <= Duration::from_secs(5),
            "worst-case alert latency {worst_case:?} exceeds the 5s budget"
        );
    }
}
