//! Per-route health: what an upstream failure means for the *route*, as
//! distinct from what it means for the retry loop (#968).
//!
//! [`crate::codex_upstream`] already answers "can this attempt succeed if I
//! try again right now?" and then discards the answer. Failover needs a second,
//! longer-lived question: "can this *route* serve anything at all, and if not,
//! until when?" A drained account and a malformed request are both
//! [`FailureClass::Permanent`] to the retry loop and could not be more
//! different to a router — the first must move traffic elsewhere, the second
//! must never move it, because the next route would reject the same bytes.
//!
//! Time is always passed in rather than read, so every rule here is testable
//! without sleeping. See
//! [`docs/architecture/provider-failover.md`](../../../docs/architecture/provider-failover.md).

use std::collections::HashMap;
use std::time::{Duration, Instant};

use crate::codex_history::ConversationRoute;
use crate::codex_upstream::{FailureClass, UpstreamFailure};

/// Cooldown for an exhausted route whose provider named no reset time.
///
/// Deliberately short relative to a real daily reset: a route that is still
/// exhausted answers with another exhaustion and re-arms the clock, whereas an
/// over-long guess would strand a recovered route on an expensive fallback.
pub const DEFAULT_EXHAUSTED_COOLDOWN: Duration = Duration::from_secs(15 * 60);

/// Cooldown for an ordinary throttle that carried no `Retry-After`.
pub const DEFAULT_THROTTLE_COOLDOWN: Duration = Duration::from_secs(20);

/// Ceiling on any provider-stated cooldown. A provider reporting a multi-day
/// reset is believed about *being* exhausted, not about how long clud should
/// stop asking.
pub const MAX_COOLDOWN: Duration = Duration::from_secs(60 * 60);

/// Consecutive throttles that promote a route from "slow down" to "spent".
pub const THROTTLE_ESCALATION_THRESHOLD: u32 = 3;

/// What one upstream failure says about the route that produced it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RouteVerdict {
    /// Nothing is wrong with the route; the attempt failed on its own terms.
    Healthy,
    /// Rate limited right now. The same route can serve again after `cooldown`.
    Throttled { cooldown: Duration },
    /// A billing period's allowance is spent. Fail over until `cooldown`.
    Exhausted { cooldown: Duration },
    /// The balance is spent. No clock: only a credential or config change
    /// clears it, so no amount of waiting helps.
    Drained,
    /// The credential was rejected.
    Unauthenticated,
    /// The request itself will never be accepted. **Never** fails over —
    /// replaying it only spends a second account to reproduce the same error.
    RequestFatal,
}

impl RouteVerdict {
    /// Whether this verdict should move traffic to another route.
    pub fn fails_over(self) -> bool {
        matches!(
            self,
            Self::Exhausted { .. } | Self::Drained | Self::Unauthenticated
        )
    }

    /// Stable, non-sensitive word for notices and `clud route status`.
    pub fn reason(self) -> &'static str {
        match self {
            Self::Healthy => "healthy",
            Self::Throttled { .. } => "throttled",
            Self::Exhausted { .. } => "exhausted",
            Self::Drained => "drained",
            Self::Unauthenticated => "unauthenticated",
            Self::RequestFatal => "request-fatal",
        }
    }

    /// Read a failure as a statement about its route.
    ///
    /// Status is consulted before class because the three statuses that name a
    /// *route* condition — `401`/`403` rejected credential, `402` spent
    /// balance — are otherwise indistinguishable from an ordinary permanent
    /// rejection.
    pub fn from_failure(failure: &UpstreamFailure) -> Self {
        match failure.status() {
            401 | 403 => return Self::Unauthenticated,
            402 => return Self::Drained,
            _ => {}
        }
        if failure.class() == FailureClass::Exhausted {
            return Self::Exhausted {
                cooldown: clamp_cooldown(
                    failure
                        .resets_in()
                        .or_else(|| failure.retry_after())
                        .unwrap_or(DEFAULT_EXHAUSTED_COOLDOWN),
                ),
            };
        }
        if failure.status() == 429 {
            return Self::Throttled {
                cooldown: clamp_cooldown(
                    failure
                        .retry_after()
                        .or_else(|| failure.resets_in())
                        .unwrap_or(DEFAULT_THROTTLE_COOLDOWN),
                ),
            };
        }
        match failure.class() {
            FailureClass::Transient | FailureClass::Unknown => Self::Healthy,
            FailureClass::Permanent => Self::RequestFatal,
            // Handled above; a route cannot be exhausted and not exhausted.
            FailureClass::Exhausted => Self::Exhausted {
                cooldown: DEFAULT_EXHAUSTED_COOLDOWN,
            },
        }
    }
}

fn clamp_cooldown(value: Duration) -> Duration {
    value.clamp(Duration::from_secs(1), MAX_COOLDOWN)
}

/// A route's availability at one instant.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RouteState {
    /// Usable now.
    Available,
    /// Unusable until the clock runs out.
    Cooling {
        remaining: Duration,
        reason: &'static str,
    },
    /// Unusable with no clock; needs operator action.
    Down { reason: &'static str },
}

impl RouteState {
    pub fn is_available(self) -> bool {
        matches!(self, Self::Available)
    }

    /// Word for `clud route status` and notices.
    pub fn reason(self) -> &'static str {
        match self {
            Self::Available => "available",
            Self::Cooling { reason, .. } | Self::Down { reason } => reason,
        }
    }
}

#[derive(Debug, Clone, Copy)]
struct Entry {
    /// Set while the route is cooling. `None` once it has been served again.
    until: Option<Instant>,
    /// Set for clock-less failures; cleared only by [`RouteLedger::clear`].
    down: bool,
    reason: &'static str,
    consecutive_throttles: u32,
}

impl Default for Entry {
    fn default() -> Self {
        Self {
            until: None,
            down: false,
            reason: "available",
            consecutive_throttles: 0,
        }
    }
}

/// Which routes can serve right now, and until when.
///
/// Deliberately not a global: one ledger is owned by one launch-scoped
/// gateway, so a wedged account in one session never suppresses a route in
/// another.
#[derive(Debug, Default)]
pub struct RouteLedger {
    entries: HashMap<ConversationRoute, Entry>,
}

impl RouteLedger {
    pub fn new() -> Self {
        Self::default()
    }

    /// Fold one verdict into the route's health.
    ///
    /// Returns the state the route is in afterwards, so a caller can emit its
    /// single notice without a second lookup.
    pub fn record(
        &mut self,
        route: ConversationRoute,
        verdict: RouteVerdict,
        now: Instant,
    ) -> RouteState {
        let entry = self.entries.entry(route).or_default();
        match verdict {
            // A request-fatal failure says nothing about the route, so it must
            // not clear an existing cooldown either.
            RouteVerdict::RequestFatal => {}
            RouteVerdict::Healthy => *entry = Entry::default(),
            RouteVerdict::Throttled { cooldown } => {
                entry.consecutive_throttles = entry.consecutive_throttles.saturating_add(1);
                if entry.consecutive_throttles >= THROTTLE_ESCALATION_THRESHOLD {
                    // Repeated throttling with no progress is indistinguishable
                    // from a spent allowance from the caller's side, and
                    // treating it as one stops a hot loop against a wall.
                    entry.until = Some(now + DEFAULT_EXHAUSTED_COOLDOWN);
                    entry.reason = "exhausted";
                } else {
                    entry.until = Some(now + cooldown);
                    entry.reason = "throttled";
                }
            }
            RouteVerdict::Exhausted { cooldown } => {
                entry.consecutive_throttles = 0;
                entry.until = Some(now + cooldown);
                entry.reason = "exhausted";
            }
            RouteVerdict::Drained | RouteVerdict::Unauthenticated => {
                entry.consecutive_throttles = 0;
                entry.down = true;
                entry.reason = verdict.reason();
            }
        }
        self.state(route, now)
    }

    /// Record that the route served a request. Clears a cooldown that has
    /// been outlived and resets throttle escalation.
    pub fn record_success(&mut self, route: ConversationRoute) {
        self.entries.insert(route, Entry::default());
    }

    pub fn state(&self, route: ConversationRoute, now: Instant) -> RouteState {
        let Some(entry) = self.entries.get(&route) else {
            return RouteState::Available;
        };
        if entry.down {
            return RouteState::Down {
                reason: entry.reason,
            };
        }
        match entry.until {
            Some(until) if until > now => RouteState::Cooling {
                remaining: until.saturating_duration_since(now),
                reason: entry.reason,
            },
            _ => RouteState::Available,
        }
    }

    pub fn is_available(&self, route: ConversationRoute, now: Instant) -> bool {
        self.state(route, now).is_available()
    }

    /// Forget a route's health. Used when its credential or configuration
    /// changes, which is the only thing that can clear a clock-less failure.
    pub fn clear(&mut self, route: ConversationRoute) {
        self.entries.remove(&route);
    }

    /// Every known route with its state, in a stable order for display.
    pub fn snapshot(&self, now: Instant) -> Vec<(ConversationRoute, RouteState)> {
        let mut rows: Vec<_> = self
            .entries
            .keys()
            .map(|route| (*route, self.state(*route, now)))
            .collect();
        rows.sort_by_key(|(route, _)| route.as_str());
        rows
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn failure(status: u16, body: &str) -> UpstreamFailure {
        UpstreamFailure::from_parts(status, |_| None, body, Duration::from_secs(60))
    }

    fn failure_with_retry_after(status: u16, body: &str, seconds: &str) -> UpstreamFailure {
        UpstreamFailure::from_parts(
            status,
            |name| (name == "retry-after").then(|| seconds.to_string()),
            body,
            MAX_COOLDOWN,
        )
    }

    /// The taxonomy in one table. Each row is a distinct routing decision, and
    /// the point of the test is that they stay distinct: collapsing any two of
    /// them either strands a session on a dead route or replays a malformed
    /// request onto a second paid account.
    #[test]
    fn every_taxonomy_row_maps_to_its_own_verdict() {
        let cases: &[(u16, &str, RouteVerdict)] = &[
            (
                429,
                r#"{"error":{"message":"Rate limit exceeded: free-models-per-day-stealth"}}"#,
                RouteVerdict::Exhausted {
                    cooldown: DEFAULT_EXHAUSTED_COOLDOWN,
                },
            ),
            (
                429,
                r#"{"error":{"code":"rate_limit_exceeded"}}"#,
                RouteVerdict::Throttled {
                    cooldown: DEFAULT_THROTTLE_COOLDOWN,
                },
            ),
            (
                402,
                r#"{"error":{"message":"This request requires more credits"}}"#,
                RouteVerdict::Drained,
            ),
            (
                401,
                r#"{"error":{"message":"invalid key"}}"#,
                RouteVerdict::Unauthenticated,
            ),
            (
                403,
                r#"{"error":{"message":"forbidden"}}"#,
                RouteVerdict::Unauthenticated,
            ),
            (
                400,
                r#"{"error":{"message":"bad shape"}}"#,
                RouteVerdict::RequestFatal,
            ),
            (
                422,
                r#"{"error":{"message":"unprocessable"}}"#,
                RouteVerdict::RequestFatal,
            ),
            (
                503,
                r#"{"error":{"message":"service unavailable"}}"#,
                RouteVerdict::Healthy,
            ),
        ];
        for (status, body, expected) in cases {
            assert_eq!(
                RouteVerdict::from_failure(&failure(*status, body)),
                *expected,
                "status {status} body {body}"
            );
        }
    }

    /// A malformed request must never descend the ladder, and must not clear a
    /// cooldown the route is already serving.
    #[test]
    fn request_fatal_never_fails_over_and_never_heals_a_route() {
        let fatal = RouteVerdict::from_failure(&failure(400, "{}"));
        assert!(!fatal.fails_over());

        let now = Instant::now();
        let mut ledger = RouteLedger::new();
        ledger.record(
            ConversationRoute::Claude,
            RouteVerdict::Exhausted {
                cooldown: Duration::from_secs(300),
            },
            now,
        );
        let after = ledger.record(ConversationRoute::Claude, fatal, now);
        assert!(
            !after.is_available(),
            "a 400 must not resurrect an exhausted route: {after:?}"
        );
    }

    #[test]
    fn only_route_terminal_verdicts_fail_over() {
        assert!(RouteVerdict::Drained.fails_over());
        assert!(RouteVerdict::Unauthenticated.fails_over());
        assert!(RouteVerdict::Exhausted {
            cooldown: Duration::from_secs(1)
        }
        .fails_over());
        assert!(!RouteVerdict::Healthy.fails_over());
        assert!(!RouteVerdict::RequestFatal.fails_over());
        assert!(
            !RouteVerdict::Throttled {
                cooldown: Duration::from_secs(1)
            }
            .fails_over(),
            "a throttle is served in place, not failed over"
        );
    }

    /// A provider-stated reset is honoured, and a provider claiming a six-day
    /// reset does not park the route for six days.
    #[test]
    fn stated_resets_are_honoured_and_clamped() {
        let stated = failure(
            429,
            r#"{"error":{"code":"usage_limit_reached","resets_in_seconds":300}}"#,
        );
        assert_eq!(
            RouteVerdict::from_failure(&stated),
            RouteVerdict::Exhausted {
                cooldown: Duration::from_secs(300)
            }
        );

        let absurd = failure(
            429,
            r#"{"error":{"code":"usage_limit_reached","resets_in_seconds":529498}}"#,
        );
        assert_eq!(
            RouteVerdict::from_failure(&absurd),
            RouteVerdict::Exhausted {
                cooldown: MAX_COOLDOWN
            }
        );

        let throttle = failure_with_retry_after(429, r#"{"error":{"code":"slow_down"}}"#, "45");
        assert_eq!(
            RouteVerdict::from_failure(&throttle),
            RouteVerdict::Throttled {
                cooldown: Duration::from_secs(45)
            }
        );
    }

    #[test]
    fn a_cooling_route_becomes_available_again_when_its_clock_runs_out() {
        let now = Instant::now();
        let mut ledger = RouteLedger::new();
        let state = ledger.record(
            ConversationRoute::DeepSeek,
            RouteVerdict::Exhausted {
                cooldown: Duration::from_secs(600),
            },
            now,
        );
        assert!(matches!(state, RouteState::Cooling { .. }));
        assert_eq!(state.reason(), "exhausted");
        assert!(!ledger.is_available(ConversationRoute::DeepSeek, now));
        assert!(!ledger.is_available(ConversationRoute::DeepSeek, now + Duration::from_secs(599)));
        assert!(
            ledger.is_available(ConversationRoute::DeepSeek, now + Duration::from_secs(601)),
            "the rung must rejoin the ladder once its reset passes"
        );
    }

    /// A spent balance has no clock. Waiting cannot fix it, so the ledger must
    /// not pretend otherwise; only an explicit clear restores it.
    #[test]
    fn a_drained_route_has_no_clock_and_only_an_explicit_clear_restores_it() {
        let now = Instant::now();
        let mut ledger = RouteLedger::new();
        let state = ledger.record(ConversationRoute::Claude, RouteVerdict::Drained, now);
        assert_eq!(state, RouteState::Down { reason: "drained" });
        assert!(!ledger.is_available(ConversationRoute::Claude, now + Duration::from_secs(86_400)));
        ledger.clear(ConversationRoute::Claude);
        assert!(ledger.is_available(ConversationRoute::Claude, now));
    }

    /// Repeated throttling with no progress is a wall, not a hiccup.
    #[test]
    fn repeated_throttles_escalate_to_exhaustion() {
        let now = Instant::now();
        let mut ledger = RouteLedger::new();
        let throttle = RouteVerdict::Throttled {
            cooldown: Duration::from_secs(5),
        };
        for _ in 1..THROTTLE_ESCALATION_THRESHOLD {
            let state = ledger.record(ConversationRoute::Codex, throttle, now);
            assert_eq!(state.reason(), "throttled");
        }
        let escalated = ledger.record(ConversationRoute::Codex, throttle, now);
        assert_eq!(escalated.reason(), "exhausted");
        assert!(
            !ledger.is_available(ConversationRoute::Codex, now + Duration::from_secs(30)),
            "escalation must outlast the throttle's own short cooldown"
        );
    }

    #[test]
    fn a_served_request_clears_the_routes_history() {
        let now = Instant::now();
        let mut ledger = RouteLedger::new();
        ledger.record(
            ConversationRoute::Codex,
            RouteVerdict::Throttled {
                cooldown: Duration::from_secs(30),
            },
            now,
        );
        ledger.record_success(ConversationRoute::Codex);
        assert!(ledger.is_available(ConversationRoute::Codex, now));

        // ...and the escalation counter went with it.
        for _ in 1..THROTTLE_ESCALATION_THRESHOLD {
            ledger.record(
                ConversationRoute::Codex,
                RouteVerdict::Throttled {
                    cooldown: Duration::from_secs(5),
                },
                now,
            );
        }
        assert_eq!(
            ledger.state(ConversationRoute::Codex, now).reason(),
            "throttled"
        );
    }

    #[test]
    fn a_snapshot_lists_every_known_route_in_a_stable_order() {
        let now = Instant::now();
        let mut ledger = RouteLedger::new();
        ledger.record(ConversationRoute::DeepSeek, RouteVerdict::Drained, now);
        ledger.record(
            ConversationRoute::Claude,
            RouteVerdict::Exhausted {
                cooldown: Duration::from_secs(60),
            },
            now,
        );
        let rows = ledger.snapshot(now);
        assert_eq!(
            rows.iter().map(|(route, _)| *route).collect::<Vec<_>>(),
            vec![ConversationRoute::Claude, ConversationRoute::DeepSeek]
        );
        assert_eq!(rows[0].1.reason(), "exhausted");
        assert_eq!(rows[1].1, RouteState::Down { reason: "drained" });
    }
}
