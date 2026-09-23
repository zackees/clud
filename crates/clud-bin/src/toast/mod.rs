//! In-terminal toast notifications (#1189).
//!
//! A toast is a short, keyed message ("cpu 287 % · …") that clud surfaces
//! while a harness TUI owns the screen. Before #1189 these were `eprintln!`s
//! written straight onto the terminal the child was drawing on, which
//! interleaved with the child's redraws and left garbage behind.
//!
//! This module separates *what* is shown from *how*:
//!
//! - [`ToastBoard`] / [`ToastHub`]: the model. One toast per topic key, with
//!   an optional expiry. Producers publish [`ToastEvent`]s through a
//!   [`ToastSink`]; nothing here touches a terminal.
//! - [`compositor::Compositor`]: runs inside the PTY pump's writer thread and
//!   renders the visible toast through the best tier the terminal supports
//!   ([`tier::ToastTier`]): kitty graphics, text cells on the alternate
//!   screen, or a fallback surface (Claude's status line, the terminal title).
//! - [`statusline`]: the fallback for subprocess mode, where clud never sees
//!   the child's bytes — a state file read by `clud statusline`, which Claude
//!   Code runs as its `statusLine` command.
//!
//! See `docs/architecture/toasts.md` for the full design and DD-071 for why
//! toasts are composited inside the terminal stream rather than drawn in an
//! external window.

pub mod compositor;
pub mod kitty;
pub mod launch;
pub mod mouse;
pub mod raster;
pub mod statusline;
pub mod text_tier;
pub mod tier;
pub mod tracker;
mod usage_ledger;

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};

/// How loudly a toast should render. Ordered: a higher severity wins when two
/// topics are visible at once.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Severity {
    Info,
    Warn,
    Alert,
}

/// One keyed message. Publishing a toast with a key that is already visible
/// replaces it in place, which is how a live reading updates without stacking.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Toast {
    pub key: String,
    pub text: String,
    pub severity: Severity,
    pub shown_at: Instant,
    /// `None` means "until closed": the producer owns the lifetime (a CPU
    /// episode that is still running). `Some` expires on its own.
    pub expires_at: Option<Instant>,
}

impl Toast {
    pub fn new(
        key: impl Into<String>,
        text: impl Into<String>,
        severity: Severity,
        now: Instant,
    ) -> Self {
        Self {
            key: key.into(),
            text: text.into(),
            severity,
            shown_at: now,
            expires_at: None,
        }
    }

    #[must_use]
    pub fn expiring_after(mut self, ttl: Duration) -> Self {
        self.expires_at = Some(self.shown_at + ttl);
        self
    }

    pub fn is_expired(&self, now: Instant) -> bool {
        self.expires_at.is_some_and(|at| now >= at)
    }

    /// Whether two toasts render identically. Timestamps are excluded so a
    /// republished-but-unchanged reading does not force a redraw.
    pub fn same_content(&self, other: &Toast) -> bool {
        self.key == other.key && self.text == other.text && self.severity == other.severity
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ToastEvent {
    Show(Toast),
    Close { key: String },
}

/// Topic-keyed toast set with expiry. Pure: callers pass `now`.
#[derive(Debug, Default, Clone)]
pub struct ToastBoard {
    toasts: BTreeMap<String, Toast>,
}

impl ToastBoard {
    /// Apply one event. Returns whether anything visible could have changed.
    pub fn apply(&mut self, event: ToastEvent) -> bool {
        match event {
            ToastEvent::Show(toast) => {
                let unchanged = self.toasts.get(&toast.key).is_some_and(|existing| {
                    existing.same_content(&toast) && existing.expires_at == toast.expires_at
                });
                // Keep the original `shown_at` for an unchanged republish so a
                // live reading does not keep jumping to the top of the order.
                if unchanged {
                    return false;
                }
                self.toasts.insert(toast.key.clone(), toast);
                true
            }
            ToastEvent::Close { key } => self.toasts.remove(&key).is_some(),
        }
    }

    /// Drop expired toasts. Returns whether any were removed.
    pub fn expire(&mut self, now: Instant) -> bool {
        let before = self.toasts.len();
        self.toasts.retain(|_, toast| !toast.is_expired(now));
        before != self.toasts.len()
    }

    /// The toast to render: highest severity, then most recently shown.
    pub fn visible(&self, now: Instant) -> Option<&Toast> {
        self.toasts
            .values()
            .filter(|toast| !toast.is_expired(now))
            .max_by_key(|toast| (toast.severity, toast.shown_at))
    }

    pub fn is_empty(&self) -> bool {
        self.toasts.is_empty()
    }

    pub fn next_deadline(&self) -> Option<Instant> {
        self.toasts.values().filter_map(|t| t.expires_at).min()
    }
}

/// Thread-safe board shared between producers (the CPU banner thread), the
/// compositor (PTY writer thread), and the stdin path (click-to-dismiss).
///
/// A monotonically increasing `version` lets the compositor detect changes
/// with one lock and no channel wiring, which matters because the compositor
/// is rebuilt for every PTY iteration while producers live for the session.
#[derive(Debug, Default)]
pub struct ToastHub {
    state: Mutex<HubState>,
}

#[derive(Debug, Default)]
struct HubState {
    board: ToastBoard,
    version: u64,
}

/// What the compositor needs from the hub at one instant.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HubSnapshot {
    pub version: u64,
    pub visible: Option<Toast>,
    pub is_empty: bool,
}

impl ToastHub {
    pub fn new() -> Arc<Self> {
        Arc::new(Self::default())
    }

    pub fn publish(&self, event: ToastEvent) {
        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        if state.board.apply(event) {
            state.version = state.version.wrapping_add(1);
        }
    }

    /// Close whatever is visible now — the click-to-dismiss path.
    pub fn dismiss_visible(&self, now: Instant) {
        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        let Some(key) = state.board.visible(now).map(|toast| toast.key.clone()) else {
            return;
        };
        if state.board.apply(ToastEvent::Close { key }) {
            state.version = state.version.wrapping_add(1);
        }
    }

    /// Expire due toasts and report the visible one.
    pub fn snapshot(&self, now: Instant) -> HubSnapshot {
        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        if state.board.expire(now) {
            state.version = state.version.wrapping_add(1);
        }
        HubSnapshot {
            version: state.version,
            visible: state.board.visible(now).cloned(),
            is_empty: state.board.is_empty(),
        }
    }
}

/// Where a producer's toasts go. Cheap to clone; producers never know which
/// surface ends up rendering.
#[derive(Clone, Default)]
pub enum ToastSink {
    /// No surface for this launch (toasts disabled, or no harness surface).
    #[default]
    Discard,
    /// PTY mode: the compositor renders from this hub.
    Hub(Arc<ToastHub>),
    /// Subprocess mode: `clud statusline` renders from this state file.
    StatusFile(Arc<statusline::StatusStateWriter>),
    /// Tests: record every event in order.
    Recorder(Arc<Mutex<Vec<ToastEvent>>>),
}

impl ToastSink {
    pub fn publish(&self, event: ToastEvent) {
        match self {
            Self::Discard => {}
            Self::Hub(hub) => hub.publish(event),
            Self::StatusFile(writer) => writer.publish(event),
            Self::Recorder(events) => events.lock().unwrap_or_else(|e| e.into_inner()).push(event),
        }
    }

    pub fn is_discard(&self) -> bool {
        matches!(self, Self::Discard)
    }
}

impl std::fmt::Debug for ToastSink {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let name = match self {
            Self::Discard => "Discard",
            Self::Hub(_) => "Hub",
            Self::StatusFile(_) => "StatusFile",
            Self::Recorder(_) => "Recorder",
        };
        f.write_str(name)
    }
}

/// Per-launch toast configuration, resolved once in `main.rs`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ToastLaunchCfg {
    /// Any toast producer is active and `[foreground.toasts] enabled` is on.
    pub enabled: bool,
    /// Inject a chained `statusLine` into Claude launches.
    pub claude_statusline: bool,
}

impl ToastLaunchCfg {
    pub fn disabled() -> Self {
        Self {
            enabled: false,
            claude_statusline: false,
        }
    }
}

#[cfg(test)]
#[path = "mod_tests.rs"]
mod tests;
