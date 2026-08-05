//! Cross-path Ctrl+C exit timing.
//!
//! Records the moment the process most recently observes Ctrl+C (whether
//! under the direct runner, an attached daemon session, or the centralized
//! launch path) and, just before the process exits, writes a JSON event
//! under `<state_dir>/ctrl_c_events/<unix-ms>-<pid>.json` capturing the
//! elapsed wall-clock time from "Ctrl+C seen" to "about to exit". The
//! daemon dashboard reads these files and surfaces them on `clud ui` so
//! the recurring "Ctrl+C takes forever to drop me back at the shell"
//! problem has hard numbers attached.
//!
//! Every Ctrl+C re-stamps the observation point (issue #285 rec 1): the
//! prior `OnceLock` design only stamped the very first Ctrl+C of the
//! process's lifetime, so a user who pressed Ctrl+C once to clear a
//! backend prompt, kept working, then later pressed Ctrl+C to exit, would
//! see the entire intervening session attributed to a single "slow"
//! event. The latest observation always wins.
//!
//! In addition, the teardown sites record the daemon-handoff outcome
//! (issue #285 rec 2) so the dashboard can distinguish "daemon adopted
//! the kill in <100ms" from "fell back to synchronous kill_tree" at a
//! glance.
//!
//! The on-disk format is intentionally tiny and forwards-compatible:
//! unknown fields are ignored on read, and the directory is capped so a
//! long-running daemon never accumulates more than [`MAX_RETAINED_EVENTS`]
//! files. Existing per-session [`crate::daemon::types::CtrlCProfile`]
//! handoff/kill telemetry is unchanged.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};
use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

pub const EVENTS_DIRNAME: &str = "ctrl_c_events";

/// Hard cap on retained events. The dashboard only needs the recent tail,
/// and we don't want a debugging dir to balloon over a long-lived daemon.
pub const MAX_RETAINED_EVENTS: usize = 50;

/// Cap returned by [`read_recent_events`] so `/state.json` payloads stay
/// small even right after a burst of interrupts.
pub const DASHBOARD_EVENT_LIMIT: usize = 20;

/// Default rapid-succession window (in milliseconds) used by the Windows
/// double-Ctrl+C guard. Sits inside the 1–3s range called out in issue
/// #377's "Proposed direction": short enough that a deliberate exit feels
/// snappy, long enough that a held-down or repeated keystroke doesn't
/// tear down clud accidentally. Overridable via `CLUD_CTRL_C_WINDOW_MS`.
pub const DOUBLE_TAP_WINDOW_MS_DEFAULT: u64 = 1500;

/// Env var that overrides [`DOUBLE_TAP_WINDOW_MS_DEFAULT`]. Values
/// outside `[50, 10_000]` are ignored — we want operators to tune the
/// window, not disable it by smuggling a `0` through the same knob.
pub const ENV_DOUBLE_TAP_WINDOW_MS: &str = "CLUD_CTRL_C_WINDOW_MS";

/// Env var that turns the Windows double-tap guard off entirely
/// (`CLUD_NO_DOUBLE_CTRL_C=1`). Provided so a user who prefers the old
/// single-press teardown can fall back without a code change.
pub const ENV_DISABLE_DOUBLE_TAP: &str = "CLUD_NO_DOUBLE_CTRL_C";

/// Origin of the interrupt — the dashboard groups events by this so the
/// "is it the daemon attach path that's slow or the direct runner?"
/// question has a one-glance answer.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum InvocationKind {
    Direct,
    Attach,
    Centralized,
}

impl InvocationKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Direct => "direct",
            Self::Attach => "attach",
            Self::Centralized => "centralized",
        }
    }
}

/// Specific console-control event / signal that fired clud's interrupt
/// handler.
///
/// On Windows, `ctrlc::set_handler` folds five distinct console events
/// (`CTRL_C_EVENT`, `CTRL_BREAK_EVENT`, `CTRL_CLOSE_EVENT`,
/// `CTRL_LOGOFF_EVENT`, `CTRL_SHUTDOWN_EVENT`) into one callback, so by
/// default we can't tell a real keyboard Ctrl+C from a
/// `GenerateConsoleCtrlEvent` call somewhere in the descendant tree.
/// The Windows probe in [`crate::startup`] inspects `dwCtrlType` before
/// the ctrlc handler runs and stores the result here so the dashboard
/// can show *which* event actually fired.
///
/// On Unix, `ctrlc` (without the `termination` feature, which clud does
/// not enable) only ever installs a `SIGINT` handler — so `CtrlC` is
/// stamped directly by [`crate::startup::run_ctrl_c_handler`]. clud
/// separately installs its own handler (issue #517) for `SIGTERM` /
/// `SIGHUP` / `SIGQUIT` — signals `ctrlc` never touches — so those get
/// their own variants below and flip the same interrupted flag as
/// Ctrl+C.
///
/// `None` in [`CtrlCEvent::ctrl_event_kind`] means no probe/handler
/// stamped a kind for this process's observed interrupt (pre-upgrade
/// event files, or a code path that predates this field).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CtrlEventKind {
    /// `CTRL_C_EVENT` on Windows, `SIGINT` on Unix. The classic
    /// keyboard Ctrl+C — or a `GenerateConsoleCtrlEvent` broadcast
    /// from a sibling/descendant.
    CtrlC,
    /// `CTRL_BREAK_EVENT` on Windows, `SIGBREAK` on Unix. Almost
    /// never a keyboard press in modern terminals; usually a
    /// `GenerateConsoleCtrlEvent` from a process trying to terminate
    /// a console group.
    CtrlBreak,
    /// `CTRL_CLOSE_EVENT`. The console window's close button was
    /// clicked, the host window is being destroyed, or `EndTask` was
    /// invoked. The OS gives the handler ~5 seconds before killing
    /// the process.
    CtrlClose,
    /// `CTRL_LOGOFF_EVENT`. Only delivered to service processes —
    /// extremely unlikely in a foreground CLI but recorded for
    /// completeness.
    CtrlLogoff,
    /// `CTRL_SHUTDOWN_EVENT`. System shutdown. Same service-process
    /// caveat as `CtrlLogoff`.
    CtrlShutdown,
    /// `SIGTERM` on Unix. Typically `kill`, `docker stop`, or a
    /// process supervisor (systemd, launchd) asking clud to exit
    /// gracefully. Windows has no direct equivalent; this variant is
    /// only ever stamped on Unix builds.
    Term,
    /// `SIGHUP` on Unix. The controlling terminal (or its session
    /// leader) went away — the closest Unix analogue of Windows'
    /// `CTRL_CLOSE_EVENT`. Without clud's own handler this signal
    /// kills the process under the OS default disposition before any
    /// clud code runs; issue #517 makes it a normal, observed
    /// interrupt instead.
    Hup,
    /// `SIGQUIT` on Unix (Ctrl+\\ at the terminal). The direct Unix
    /// analogue of a terminal-forced quit. Without clud's own handler
    /// this triggers the OS default disposition (core dump +
    /// terminate) and bypasses clud's interrupt path entirely.
    Quit,
    /// The probe saw a `dwCtrlType` value the Win32 docs don't define,
    /// or (in principle) an unmapped signal number. Stored so a future
    /// OS revision that adds a new control event doesn't get silently
    /// dropped on the floor.
    Unknown,
}

/// Classifies a Ctrl+C observation as either the first press in a
/// rapid-succession window (treated as a soft interrupt — clud stays
/// running so the child can handle it cooperatively) or the second
/// press that actually triggers clud teardown.
///
/// Recorded in the forensic event so the dashboard can distinguish
/// "user pressed Ctrl+C once and it got swallowed correctly" from
/// "user pressed Ctrl+C twice in rapid succession and we exited".
/// Optional in [`CtrlCEvent`] so legacy event files (and event files
/// written by non-Windows paths that never engage the double-tap guard)
/// stay parseable.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CtrlPressKind {
    /// First press observed in a fresh rapid-succession window. clud
    /// suppressed the teardown and printed a hint; the child still got
    /// its own copy of the console-control event from the OS.
    FirstSoft,
    /// Second press observed within the rapid-succession window — or
    /// any press on a path where the double-tap guard is disabled.
    /// This is the press that flipped clud's interrupted flag.
    SecondExit,
}

impl CtrlPressKind {
    /// Numeric encoding for the [`AtomicU32`] storage. Must round-trip
    /// through [`Self::from_raw`].
    pub const fn to_raw(self) -> u32 {
        match self {
            CtrlPressKind::FirstSoft => 0,
            CtrlPressKind::SecondExit => 1,
        }
    }

    /// Decode a value previously written by [`Self::to_raw`]. `None`
    /// for the [`PRESS_KIND_UNRECORDED`] sentinel; any other unexpected
    /// value collapses to [`Self::SecondExit`] so a future encoding bug
    /// can't silently downgrade the press to "first" (which would be
    /// the dangerous direction — we'd skip teardown when we shouldn't).
    pub const fn from_raw(raw: u32) -> Option<Self> {
        match raw {
            v if v == PRESS_KIND_UNRECORDED => None,
            0 => Some(CtrlPressKind::FirstSoft),
            _ => Some(CtrlPressKind::SecondExit),
        }
    }
}

impl CtrlEventKind {
    /// Numeric encoding used by the atomic storage. Must round-trip
    /// through [`Self::from_raw`].
    pub const fn to_raw(self) -> u32 {
        match self {
            CtrlEventKind::CtrlC => 0,
            CtrlEventKind::CtrlBreak => 1,
            CtrlEventKind::CtrlClose => 2,
            CtrlEventKind::CtrlLogoff => 5,
            CtrlEventKind::CtrlShutdown => 6,
            // 3, 4, and 7-99 are left open in case a future Windows
            // revision defines a new dwCtrlType in that range — Unix-only
            // variants live at 100+ so the two spaces never collide.
            CtrlEventKind::Term => 100,
            CtrlEventKind::Hup => 101,
            CtrlEventKind::Quit => 102,
            CtrlEventKind::Unknown => u32::MAX - 1,
        }
    }

    /// Decode a value previously written by [`Self::to_raw`]. Returns
    /// [`CtrlEventKind::Unknown`] for any unexpected input so callers
    /// never have to handle "impossible" cases.
    pub const fn from_raw(raw: u32) -> Self {
        match raw {
            0 => CtrlEventKind::CtrlC,
            1 => CtrlEventKind::CtrlBreak,
            2 => CtrlEventKind::CtrlClose,
            5 => CtrlEventKind::CtrlLogoff,
            6 => CtrlEventKind::CtrlShutdown,
            100 => CtrlEventKind::Term,
            101 => CtrlEventKind::Hup,
            102 => CtrlEventKind::Quit,
            _ => CtrlEventKind::Unknown,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CtrlCEvent {
    pub pid: u32,
    pub observed_at_ms: u64,
    pub exit_at_ms: u64,
    pub elapsed_ms: u64,
    pub kind: InvocationKind,
    pub exit_code: i32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cwd: Option<String>,
    /// Whether the daemon adopted the kill on the fast path. `None`
    /// means the teardown site never recorded an outcome (older event
    /// files, or `clud --no-daemon` paths that don't run the teardown
    /// helper). The dashboard surfaces this so "daemon adopted" vs
    /// "synchronous fallback" is one-glance.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub handed_off: Option<bool>,
    /// Free-form tag explaining the handoff outcome
    /// (e.g. `"ctrl_c_subprocess"` on success or
    /// `"daemon_unreachable"` / `"no_state_dir"` on failure). Optional
    /// so old event files stay parseable.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub handoff_reason: Option<String>,
    /// Specific Windows console-control event that fired the handler.
    /// `None` on Unix and on pre-upgrade event files. Critical for
    /// telling "user pressed Ctrl+C" from "some descendant called
    /// `GenerateConsoleCtrlEvent`" — they exit identically through
    /// the same handler but mean very different things.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ctrl_event_kind: Option<CtrlEventKind>,
    /// Best-effort forensic context captured after clud observed a
    /// Windows console-control event. Win32 does not expose the sender
    /// of `CTRL_C_EVENT`; this snapshot records the console/process
    /// context that existed when clud began interrupt teardown.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub forensics: Option<CtrlCForensics>,
    /// Press classification recorded by the double-Ctrl+C guard (issue
    /// #377). `Some(FirstSoft)` means clud suppressed teardown and a
    /// follow-up press was needed; `Some(SecondExit)` means this is the
    /// press that flipped the interrupted flag. `None` on legacy event
    /// files and on platforms where the guard is intentionally not
    /// engaged (currently: non-Windows).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub press_kind: Option<CtrlPressKind>,
    /// Time between this Ctrl+C and the previous one, in milliseconds.
    /// `None` if this is the first Ctrl+C of the process's lifetime or
    /// the event was written by a pre-#377 build. The double-tap window
    /// (`CLUD_CTRL_C_WINDOW_MS`, default
    /// [`DOUBLE_TAP_WINDOW_MS_DEFAULT`]) is compared against this value.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub elapsed_since_prior_ms: Option<u64>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct CtrlCForensics {
    pub captured_at_ms: u64,
    pub current_pid: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub current_parent_pid: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub child_root_pid: Option<u32>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub child_tree_pids: Vec<u32>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub ancestor_pids: Vec<u32>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub console_process_pids: Vec<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub foreground_window_pid: Option<u32>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub processes: Vec<CtrlCProcessSnapshot>,
    pub source_limit: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct CtrlCProcessSnapshot {
    pub pid: u32,
    pub parent_pid: u32,
    pub exe: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub roles: Vec<String>,
}

// Process-wide observation point. Re-stamped on every Ctrl+C the signal
// handler sees (issue #285 rec 1), so `build_event` always measures from
// the most recent press — not the very first one. `AtomicU64::store` is
// signal-safe on every platform clud targets, so this works equally well
// from POSIX signal handlers and from the Windows console-handler thread.
//
// A zero value means "never observed" — the unix epoch is the natural
// sentinel and saves us a separate boolean flag.
static OBSERVED_UNIX_MS: AtomicU64 = AtomicU64::new(0);

/// Process-wide last-observed control event kind, populated by the
/// Windows probe installed in [`crate::startup::install_ctrl_c_flag`].
/// `u32::MAX` is the "never observed" sentinel; real values come from
/// [`CtrlEventKind::to_raw`].
///
/// Lives in its own atomic (separate from `OBSERVED_UNIX_MS`) so the
/// timestamp updates from the existing `ctrlc` handler don't have to
/// race with the kind-recording probe — the two writers touch
/// independent locations.
const KIND_UNRECORDED: u32 = u32::MAX;
static OBSERVED_EVENT_KIND: AtomicU32 = AtomicU32::new(KIND_UNRECORDED);

/// Process-wide most-recent press classification recorded by the
/// double-Ctrl+C guard. `PRESS_KIND_UNRECORDED` means the guard never
/// fired in this process (non-Windows, opt-out via `CLUD_NO_DOUBLE_CTRL_C`,
/// or pre-handler exit). Decoded via [`CtrlPressKind::from_raw`].
pub const PRESS_KIND_UNRECORDED: u32 = u32::MAX;
static OBSERVED_PRESS_KIND: AtomicU32 = AtomicU32::new(PRESS_KIND_UNRECORDED);

/// Stores the gap between the most recent press and the one before it,
/// in milliseconds. `0` is the sentinel for "no prior press" — a real
/// gap of zero is indistinguishable from a coarse-timer clock that hasn't
/// advanced yet, and the forensic field is optional, so we don't lose
/// anything by collapsing both cases into "None".
static OBSERVED_ELAPSED_SINCE_PRIOR_MS: AtomicU64 = AtomicU64::new(0);

/// Process-wide handoff outcome. Recorded by the teardown sites
/// (`runner::teardown_interrupted_child`, `session::interrupt_pty_process`)
/// after they decide whether the daemon adopted the kill or the legacy
/// fallback ran. Lives in a `Mutex` (not an atomic) because the reason
/// string would otherwise need bespoke encoding; teardown sites run on
/// the main thread, never inside a signal handler, so lock acquisition
/// is safe here.
#[derive(Debug, Clone)]
pub struct HandoffOutcome {
    pub handed_off: bool,
    pub reason: Option<String>,
}

static HANDOFF_OUTCOME: Mutex<Option<HandoffOutcome>> = Mutex::new(None);
static FORENSICS: Mutex<Option<CtrlCForensics>> = Mutex::new(None);

/// Mark the process as having observed Ctrl+C. Safe to call from a signal
/// handler — no allocations, no locks, just an atomic store.
///
/// Unlike the prior `OnceLock`-based design, every call overwrites the
/// previous timestamp (issue #285 rec 1). This is intentional: we want
/// `elapsed_ms` to measure "the Ctrl+C that exited clud → shell return",
/// not "the very first Ctrl+C ever seen → exit", which conflated multiple
/// presses across a long session into a single bogus 5-minute event.
///
/// Thin wrapper around [`record_observed_returning_prior`] for sites
/// that don't need the previous timestamp.
pub fn record_observed() {
    let _ = record_observed_returning_prior();
}

/// Re-stamp the process-wide observation point and return whatever was
/// previously stored. The Windows double-Ctrl+C handler (issue #377)
/// uses the returned prior to decide whether this press lands inside a
/// rapid-succession window.
///
/// Signal-safe: a single `AtomicU64::swap` with `SeqCst` ordering. No
/// allocations, no locks. A return value of `0` means "no prior press"
/// (the unix-epoch sentinel that [`build_event`] also keys off of), so
/// callers don't need a separate boolean flag.
pub fn record_observed_returning_prior() -> u64 {
    let now = unix_millis_now();
    OBSERVED_UNIX_MS.swap(now, Ordering::SeqCst)
}

pub fn was_observed() -> bool {
    OBSERVED_UNIX_MS.load(Ordering::SeqCst) != 0
}

/// Record which specific console-control event the Windows probe saw.
/// Called from the `SetConsoleCtrlHandler` callback installed by
/// [`crate::startup::install_ctrl_c_flag`] before the `ctrlc` handler
/// fires. Signal-safe: a single atomic store, no allocation, no lock.
///
/// The last write wins, matching the [`record_observed`] semantics
/// above: a burst of events maps to "the kind of the most recent one".
pub fn record_event_kind(kind: CtrlEventKind) {
    OBSERVED_EVENT_KIND.store(kind.to_raw(), Ordering::SeqCst);
}

/// Read the kind recorded by [`record_event_kind`]. Returns `None`
/// when the probe never fired — Unix builds, or pre-probe code paths
/// where Ctrl+C was observed but no kind was attributed.
pub fn observed_event_kind() -> Option<CtrlEventKind> {
    let raw = OBSERVED_EVENT_KIND.load(Ordering::SeqCst);
    if raw == KIND_UNRECORDED {
        None
    } else {
        Some(CtrlEventKind::from_raw(raw))
    }
}

/// Record the press classification decided by the double-Ctrl+C guard
/// (issue #377). Signal-safe: single atomic store. Last writer wins,
/// matching the timestamp + event-kind semantics above.
pub fn record_press_kind(kind: CtrlPressKind) {
    OBSERVED_PRESS_KIND.store(kind.to_raw(), Ordering::SeqCst);
}

/// Read the most recent press classification. `None` when no press has
/// been classified yet (non-Windows paths, opt-out, or pre-handler).
pub fn observed_press_kind() -> Option<CtrlPressKind> {
    CtrlPressKind::from_raw(OBSERVED_PRESS_KIND.load(Ordering::SeqCst))
}

/// Stamp the millisecond gap between the most recent press and the one
/// before it. Signal-safe. `0` is reserved as the "no prior press"
/// sentinel — callers must not stamp a literal zero gap or they'll lose
/// the field on the forensic event.
pub fn record_elapsed_since_prior_ms(elapsed_ms: u64) {
    OBSERVED_ELAPSED_SINCE_PRIOR_MS.store(elapsed_ms, Ordering::SeqCst);
}

/// Read the recorded gap, or `None` if no prior press was recorded.
pub fn observed_elapsed_since_prior_ms() -> Option<u64> {
    let v = OBSERVED_ELAPSED_SINCE_PRIOR_MS.load(Ordering::SeqCst);
    if v == 0 {
        None
    } else {
        Some(v)
    }
}

/// Effective rapid-succession window in milliseconds. Reads
/// `CLUD_CTRL_C_WINDOW_MS` and falls back to
/// [`DOUBLE_TAP_WINDOW_MS_DEFAULT`]. Values outside `[50, 10_000]` are
/// rejected — the env var is a tuning knob, not a back door to disable
/// the guard (use [`ENV_DISABLE_DOUBLE_TAP`] for that).
pub fn double_tap_window_ms() -> u64 {
    let Ok(raw) = std::env::var(ENV_DOUBLE_TAP_WINDOW_MS) else {
        return DOUBLE_TAP_WINDOW_MS_DEFAULT;
    };
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return DOUBLE_TAP_WINDOW_MS_DEFAULT;
    }
    match trimmed.parse::<u64>() {
        Ok(v) if (50..=10_000).contains(&v) => v,
        _ => DOUBLE_TAP_WINDOW_MS_DEFAULT,
    }
}

/// Whether the Windows double-Ctrl+C guard is engaged for this process.
/// Returns `false` on non-Windows (the guard is Windows-only by design)
/// or when `CLUD_NO_DOUBLE_CTRL_C` is set to a truthy value
/// (`1`/`true`/`yes`/`on`, case-insensitive).
pub fn double_tap_enabled() -> bool {
    if !cfg!(windows) {
        return false;
    }
    match std::env::var(ENV_DISABLE_DOUBLE_TAP) {
        Ok(raw) => !env_var_is_truthy(&raw),
        Err(_) => true,
    }
}

fn env_var_is_truthy(raw: &str) -> bool {
    matches!(
        raw.trim().to_ascii_lowercase().as_str(),
        "1" | "true" | "yes" | "on"
    )
}

/// Record the daemon-handoff outcome (issue #285 rec 2). Called from
/// `runner::teardown_interrupted_child` / `session::interrupt_pty_process`
/// right after they consult `try_handoff_kill_to_daemon`. The last
/// outcome before exit wins, matching the observation-point semantics
/// above. Best-effort: a poisoned mutex is silently ignored so this
/// helper can never block exit.
pub fn record_handoff(handed_off: bool, reason: Option<&str>) {
    if let Ok(mut guard) = HANDOFF_OUTCOME.lock() {
        *guard = Some(HandoffOutcome {
            handed_off,
            reason: reason.map(|s| s.to_string()),
        });
    }
}

/// Capture best-effort context for a Ctrl+C event. This is intentionally
/// called from teardown code, not from the signal/control handler itself:
/// Win32 does not report the sender PID for `CTRL_C_EVENT`, and anything
/// richer than atomics would be the wrong work to do inside the handler.
pub fn record_forensics(child_root_pid: Option<u32>) {
    let Some(snapshot) = platform_forensics(child_root_pid) else {
        return;
    };
    if let Ok(mut guard) = FORENSICS.lock() {
        *guard = Some(snapshot);
    }
}

/// If Ctrl+C was observed during this process's lifetime, write an event
/// file under `<state_dir>/ctrl_c_events/`. Best-effort: every error path
/// is silent. This must never block exit.
pub fn flush_on_exit(state_dir: &Path, kind: InvocationKind, exit_code: i32) {
    let Some(event) = build_event(kind, exit_code) else {
        return;
    };
    let _ = write_event(state_dir, &event);
}

fn build_event(kind: InvocationKind, exit_code: i32) -> Option<CtrlCEvent> {
    let observed_at_ms = OBSERVED_UNIX_MS.load(Ordering::SeqCst);
    if observed_at_ms == 0 {
        return None;
    }
    let exit_at_ms = unix_millis_now();
    let elapsed_ms = exit_at_ms.saturating_sub(observed_at_ms);
    let cwd = std::env::current_dir()
        .ok()
        .map(|p| p.to_string_lossy().into_owned());
    let (handed_off, handoff_reason) = HANDOFF_OUTCOME
        .lock()
        .ok()
        .and_then(|g| g.clone())
        .map(|o| (Some(o.handed_off), o.reason))
        .unwrap_or((None, None));
    let ctrl_event_kind = observed_event_kind();
    let forensics = FORENSICS.lock().ok().and_then(|g| g.clone());
    let press_kind = observed_press_kind();
    let elapsed_since_prior_ms = observed_elapsed_since_prior_ms();
    Some(CtrlCEvent {
        pid: std::process::id(),
        observed_at_ms,
        exit_at_ms,
        elapsed_ms,
        kind,
        exit_code,
        cwd,
        handed_off,
        handoff_reason,
        ctrl_event_kind,
        forensics,
        press_kind,
        elapsed_since_prior_ms,
    })
}

fn write_event(state_dir: &Path, event: &CtrlCEvent) -> io::Result<()> {
    let dir = events_dir(state_dir);
    fs::create_dir_all(&dir)?;
    let filename = format!("{:013}-{}.json", event.exit_at_ms, event.pid);
    let path = dir.join(filename);
    let bytes = serde_json::to_vec(event)
        .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e.to_string()))?;
    fs::write(&path, bytes)?;
    prune_old_events(&dir, MAX_RETAINED_EVENTS);
    Ok(())
}

pub fn events_dir(state_dir: &Path) -> PathBuf {
    state_dir.join(EVENTS_DIRNAME)
}

/// Read newest-first up to `limit` events from `<state_dir>/ctrl_c_events/`.
/// Used by the dashboard. Missing dir → empty Vec.
pub fn read_recent_events(state_dir: &Path, limit: usize) -> Vec<CtrlCEvent> {
    let dir = events_dir(state_dir);
    let entries = match fs::read_dir(&dir) {
        Ok(it) => it,
        Err(_) => return Vec::new(),
    };
    let mut events: Vec<CtrlCEvent> = entries
        .flatten()
        .filter(|e| e.path().extension().and_then(|x| x.to_str()) == Some("json"))
        .filter_map(|e| {
            let bytes = fs::read(e.path()).ok()?;
            serde_json::from_slice::<CtrlCEvent>(&bytes).ok()
        })
        .collect();
    events.sort_by(|a, b| b.exit_at_ms.cmp(&a.exit_at_ms));
    events.truncate(limit);
    events
}

fn prune_old_events(dir: &Path, keep: usize) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    let mut files: Vec<(u64, PathBuf)> = entries
        .flatten()
        .filter_map(|e| {
            let path = e.path();
            if path.extension().and_then(|x| x.to_str()) != Some("json") {
                return None;
            }
            let mtime = e
                .metadata()
                .ok()
                .and_then(|m| m.modified().ok())
                .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
                .map(|d| d.as_secs())
                .unwrap_or(0);
            Some((mtime, path))
        })
        .collect();
    if files.len() <= keep {
        return;
    }
    // Newest first; keep the head, delete the rest.
    files.sort_by(|a, b| b.0.cmp(&a.0));
    for (_, path) in files.into_iter().skip(keep) {
        let _ = fs::remove_file(path);
    }
}

fn unix_millis_now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

/// **Test-only.** Shared mutex so tests in multiple modules can
/// serialize against the process-global statics
/// (`OBSERVED_*` + `HANDOFF_OUTCOME` + `FORENSICS` + the env vars
/// the guard reads). Cargo's parallel test runner would otherwise
/// interleave writes and produce flaky assertions.
#[cfg(test)]
pub(crate) fn test_state_lock() -> &'static std::sync::Mutex<()> {
    static LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
    &LOCK
}

/// **Test-only.** Reset the observation + handoff state so tests that
/// exercise `build_event` / `flush_on_exit` can simulate a fresh
/// process. Real processes only ever transition once per run.
#[cfg(test)]
pub(crate) fn reset_for_test() {
    OBSERVED_UNIX_MS.store(0, Ordering::SeqCst);
    OBSERVED_EVENT_KIND.store(KIND_UNRECORDED, Ordering::SeqCst);
    OBSERVED_PRESS_KIND.store(PRESS_KIND_UNRECORDED, Ordering::SeqCst);
    OBSERVED_ELAPSED_SINCE_PRIOR_MS.store(0, Ordering::SeqCst);
    if let Ok(mut g) = HANDOFF_OUTCOME.lock() {
        *g = None;
    }
    if let Ok(mut g) = FORENSICS.lock() {
        *g = None;
    }
}

#[cfg(windows)]
fn platform_forensics(child_root_pid: Option<u32>) -> Option<CtrlCForensics> {
    use std::collections::{HashMap, HashSet};

    use windows::Win32::Foundation::CloseHandle;
    use windows::Win32::System::Console::GetConsoleProcessList;
    use windows::Win32::System::Diagnostics::ToolHelp::{
        CreateToolhelp32Snapshot, Process32FirstW, Process32NextW, PROCESSENTRY32W,
        TH32CS_SNAPPROCESS,
    };
    use windows::Win32::System::Threading::GetCurrentProcessId;
    use windows::Win32::UI::WindowsAndMessaging::{GetForegroundWindow, GetWindowThreadProcessId};

    #[derive(Clone)]
    struct Entry {
        pid: u32,
        parent_pid: u32,
        exe: String,
    }

    fn process_entries() -> Vec<Entry> {
        let Ok(snapshot) = (unsafe { CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0) }) else {
            return Vec::new();
        };
        let mut entries = Vec::new();
        let mut entry = PROCESSENTRY32W {
            dwSize: std::mem::size_of::<PROCESSENTRY32W>() as u32,
            ..Default::default()
        };
        if unsafe { Process32FirstW(snapshot, &mut entry) }.is_ok() {
            loop {
                entries.push(Entry {
                    pid: entry.th32ProcessID,
                    parent_pid: entry.th32ParentProcessID,
                    exe: nul_terminated_wide_to_string(&entry.szExeFile),
                });
                if unsafe { Process32NextW(snapshot, &mut entry) }.is_err() {
                    break;
                }
            }
        }
        let _ = unsafe { CloseHandle(snapshot) };
        entries
    }

    fn nul_terminated_wide_to_string(buf: &[u16]) -> String {
        let len = buf.iter().position(|&unit| unit == 0).unwrap_or(buf.len());
        String::from_utf16_lossy(&buf[..len])
    }

    fn console_process_pids() -> Vec<u32> {
        let mut buf = vec![0u32; 128];
        let count = unsafe { GetConsoleProcessList(&mut buf) };
        if count == 0 {
            return Vec::new();
        }
        if count as usize > buf.len() {
            buf.resize(count as usize, 0);
            let count = unsafe { GetConsoleProcessList(&mut buf) };
            buf.truncate(count as usize);
        } else {
            buf.truncate(count as usize);
        }
        buf.sort_unstable();
        buf.dedup();
        buf
    }

    fn foreground_window_pid() -> Option<u32> {
        let hwnd = unsafe { GetForegroundWindow() };
        if hwnd.is_invalid() {
            return None;
        }
        let mut pid = 0u32;
        unsafe {
            GetWindowThreadProcessId(hwnd, Some(&mut pid));
        }
        (pid != 0).then_some(pid)
    }

    fn descendant_pids(entries: &[Entry], root: u32) -> Vec<u32> {
        let mut children: HashMap<u32, Vec<u32>> = HashMap::new();
        for entry in entries {
            children
                .entry(entry.parent_pid)
                .or_default()
                .push(entry.pid);
        }
        let mut stack = vec![root];
        let mut out = Vec::new();
        while let Some(pid) = stack.pop() {
            if let Some(next) = children.get(&pid) {
                for child in next {
                    out.push(*child);
                    stack.push(*child);
                }
            }
        }
        out
    }

    fn ancestor_pids(entries_by_pid: &HashMap<u32, Entry>, current_pid: u32) -> Vec<u32> {
        let mut out = Vec::new();
        let mut seen = HashSet::new();
        let mut pid = current_pid;
        for _ in 0..64 {
            let Some(entry) = entries_by_pid.get(&pid) else {
                break;
            };
            let parent = entry.parent_pid;
            if parent == 0 || parent == pid || !seen.insert(parent) {
                break;
            }
            out.push(parent);
            pid = parent;
        }
        out
    }

    let entries = process_entries();
    let by_pid: HashMap<u32, Entry> = entries.iter().map(|e| (e.pid, e.clone())).collect();
    let current_pid = unsafe { GetCurrentProcessId() };
    let current_parent_pid = by_pid.get(&current_pid).map(|e| e.parent_pid);
    let child_tree_pids = child_root_pid
        .map(|pid| descendant_pids(&entries, pid))
        .unwrap_or_default();
    let ancestor_pids = ancestor_pids(&by_pid, current_pid);
    let console_process_pids = console_process_pids();
    let foreground_window_pid = foreground_window_pid();

    let mut wanted = HashSet::new();
    wanted.insert(current_pid);
    if let Some(pid) = current_parent_pid {
        wanted.insert(pid);
    }
    if let Some(pid) = child_root_pid {
        wanted.insert(pid);
    }
    for pid in &child_tree_pids {
        wanted.insert(*pid);
    }
    for pid in &ancestor_pids {
        wanted.insert(*pid);
    }
    for pid in &console_process_pids {
        wanted.insert(*pid);
    }
    if let Some(pid) = foreground_window_pid {
        wanted.insert(pid);
    }

    let mut processes: Vec<CtrlCProcessSnapshot> = wanted
        .into_iter()
        .filter_map(|pid| {
            let entry = by_pid.get(&pid)?;
            let mut roles = Vec::new();
            if pid == current_pid {
                roles.push("clud".to_string());
            }
            if Some(pid) == current_parent_pid {
                roles.push("clud_parent".to_string());
            }
            if Some(pid) == child_root_pid {
                roles.push("child_root".to_string());
            }
            if child_tree_pids.contains(&pid) {
                roles.push("child_descendant".to_string());
            }
            if ancestor_pids.contains(&pid) {
                roles.push("clud_ancestor".to_string());
            }
            if console_process_pids.contains(&pid) {
                roles.push("same_console".to_string());
            }
            if Some(pid) == foreground_window_pid {
                roles.push("foreground_window_owner".to_string());
            }
            Some(CtrlCProcessSnapshot {
                pid,
                parent_pid: entry.parent_pid,
                exe: entry.exe.clone(),
                roles,
            })
        })
        .collect();
    processes.sort_by_key(|p| p.pid);

    Some(CtrlCForensics {
        captured_at_ms: unix_millis_now(),
        current_pid,
        current_parent_pid,
        child_root_pid,
        child_tree_pids,
        ancestor_pids,
        console_process_pids,
        foreground_window_pid,
        processes,
        source_limit: "win32_console_control_events_do_not_expose_sender_pid".to_string(),
    })
}

#[cfg(not(windows))]
fn platform_forensics(_child_root_pid: Option<u32>) -> Option<CtrlCForensics> {
    None
}

#[cfg(test)]
#[path = "ctrl_c_track_tests.rs"]
mod tests;
