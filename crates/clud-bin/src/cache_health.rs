//! Bounded, content-free cache-credit health tracking for bridge traffic.
//!
//! This deliberately owns aggregate counts only. It never receives prompts,
//! response text, credentials, or a raw harness session identifier.

use std::collections::{HashMap, VecDeque};

/// Provider-reported token counts for one completed request.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TokenUsage {
    /// Total provider input, including cache reads when the provider reports
    /// that convention.
    pub input_tokens: u64,
    pub cached_input_tokens: u64,
    pub output_tokens: u64,
}

/// Cumulative, provider-reported terminal usage for one bounded conversation
/// entry. The tracker owns these values only; callers never provide prompt
/// text, cache keys, credentials, or raw harness session identifiers.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct UsageTotals {
    pub request_count: u64,
    pub input_tokens: u64,
    pub cached_input_tokens: u64,
    pub output_tokens: u64,
}

impl TokenUsage {
    pub fn is_valid(self) -> bool {
        self.cached_input_tokens <= self.input_tokens
    }

    pub fn uncached_input_tokens(self) -> u64 {
        self.input_tokens.saturating_sub(self.cached_input_tokens)
    }
}

/// Public, non-sensitive cache condition for one logical conversation.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CacheHealth {
    Cold,
    Healthy,
    Degraded,
    FuseTripped,
}

const MAX_CONVERSATIONS: usize = 256;
const LARGE_INPUT_TOKENS: u64 = 50_000;
const MAX_LOW_CACHE_PERCENT: u64 = 10;
const CONSECUTIVE_LOW_CACHE_TURNS: u8 = 3;

#[derive(Clone, Copy)]
struct Window {
    cold_turn_pending: bool,
    low_cache_turns: u8,
    health: CacheHealth,
    totals: UsageTotals,
}

impl Default for Window {
    fn default() -> Self {
        Self {
            cold_turn_pending: true,
            low_cache_turns: 0,
            health: CacheHealth::Cold,
            totals: UsageTotals::default(),
        }
    }
}

/// A small LRU-like registry. Keys are already hashed `ConversationKey`s; they
/// are kept solely to associate aggregate counters with a live bridge session.
#[derive(Default)]
pub struct CacheHealthTracker {
    windows: HashMap<String, Window>,
    recency: VecDeque<String>,
}

impl CacheHealthTracker {
    /// Return the cached condition before a request is sent.
    pub fn health(&self, conversation: &str) -> CacheHealth {
        self.windows
            .get(conversation)
            .map_or(CacheHealth::Cold, |window| window.health)
    }

    /// Return cumulative valid terminal usage for this active conversation.
    /// A compact/provider boundary starts a fresh aggregate window; clear
    /// removes it altogether.
    pub fn totals(&self, conversation: &str) -> UsageTotals {
        self.windows
            .get(conversation)
            .map_or_else(UsageTotals::default, |window| window.totals)
    }

    /// Record a terminal provider usage report. Missing or impossible data is
    /// ignored so malformed telemetry cannot trip, reset, or poison a later
    /// valid health window.
    pub fn record(&mut self, conversation: &str, usage: TokenUsage) -> CacheHealth {
        if !usage.is_valid() {
            return self.health(conversation);
        }
        self.touch(conversation);
        let window = self.windows.entry(conversation.to_string()).or_default();
        window.totals.request_count = window.totals.request_count.saturating_add(1);
        window.totals.input_tokens = window
            .totals
            .input_tokens
            .saturating_add(usage.input_tokens);
        window.totals.cached_input_tokens = window
            .totals
            .cached_input_tokens
            .saturating_add(usage.cached_input_tokens);
        window.totals.output_tokens = window
            .totals
            .output_tokens
            .saturating_add(usage.output_tokens);
        if usage.input_tokens < LARGE_INPUT_TOKENS {
            return window.health;
        }
        if window.cold_turn_pending {
            window.cold_turn_pending = false;
            window.low_cache_turns = 0;
            window.health = CacheHealth::Cold;
            return window.health;
        }
        let low_cache = usage.cached_input_tokens.saturating_mul(100)
            < usage.input_tokens.saturating_mul(MAX_LOW_CACHE_PERCENT);
        if low_cache {
            window.low_cache_turns = window.low_cache_turns.saturating_add(1);
            window.health = if window.low_cache_turns >= CONSECUTIVE_LOW_CACHE_TURNS {
                CacheHealth::FuseTripped
            } else {
                CacheHealth::Degraded
            };
        } else {
            window.low_cache_turns = 0;
            window.health = CacheHealth::Healthy;
        }
        window.health
    }

    /// An explicit compaction, clear, or provider boundary starts a new cold
    /// window. It does not inherit a prior miss streak into new history.
    pub fn mark_boundary(&mut self, conversation: &str) {
        self.touch(conversation);
        self.windows
            .insert(conversation.to_string(), Window::default());
    }

    /// A clear applies to every main/agent conversation below its session
    /// prefix. The prefix is a digest-derived value, never the raw session id.
    pub fn clear_session(&mut self, session_prefix: &str) {
        self.windows
            .retain(|conversation, _| !conversation.starts_with(session_prefix));
        self.recency
            .retain(|conversation| !conversation.starts_with(session_prefix));
    }

    fn touch(&mut self, conversation: &str) {
        self.recency.retain(|existing| existing != conversation);
        self.recency.push_back(conversation.to_string());
        while self.recency.len() > MAX_CONVERSATIONS {
            if let Some(expired) = self.recency.pop_front() {
                self.windows.remove(&expired);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const LARGE: TokenUsage = TokenUsage {
        input_tokens: 100_000,
        cached_input_tokens: 0,
        output_tokens: 1,
    };
    const HEALTHY: TokenUsage = TokenUsage {
        input_tokens: 100_000,
        cached_input_tokens: 95_000,
        output_tokens: 1,
    };

    #[test]
    fn incident_trace_trips_before_a_fourth_large_uncached_replay() {
        let mut tracker = CacheHealthTracker::default();
        assert_eq!(
            tracker.record("session-safe-main", LARGE),
            CacheHealth::Cold
        );
        assert_eq!(
            tracker.record("session-safe-main", LARGE),
            CacheHealth::Degraded
        );
        assert_eq!(
            tracker.record("session-safe-main", LARGE),
            CacheHealth::Degraded
        );
        assert_eq!(
            tracker.record("session-safe-main", LARGE),
            CacheHealth::FuseTripped
        );
    }

    #[test]
    fn healthy_and_boundary_cold_turns_do_not_trip() {
        let mut tracker = CacheHealthTracker::default();
        assert_eq!(
            tracker.record("session-safe-main", LARGE),
            CacheHealth::Cold
        );
        assert_eq!(
            tracker.record("session-safe-main", HEALTHY),
            CacheHealth::Healthy
        );
        tracker.mark_boundary("session-safe-main");
        assert_eq!(
            tracker.record("session-safe-main", LARGE),
            CacheHealth::Cold
        );
        assert_eq!(
            tracker.record("session-safe-main", HEALTHY),
            CacheHealth::Healthy
        );
    }

    #[test]
    fn a_boundary_clears_an_armed_fuse_before_the_next_cold_turn() {
        let mut tracker = CacheHealthTracker::default();
        for _ in 0..4 {
            tracker.record("session-safe-main", LARGE);
        }
        assert_eq!(
            tracker.health("session-safe-main"),
            CacheHealth::FuseTripped
        );
        tracker.mark_boundary("session-safe-main");
        assert_eq!(tracker.health("session-safe-main"), CacheHealth::Cold);
        assert_eq!(
            tracker.record("session-safe-main", LARGE),
            CacheHealth::Cold
        );
    }

    #[test]
    fn agents_keep_independent_windows_and_bad_usage_is_inert() {
        let mut tracker = CacheHealthTracker::default();
        for _ in 0..4 {
            tracker.record("session-safe-agent-a", LARGE);
        }
        assert_eq!(
            tracker.health("session-safe-agent-a"),
            CacheHealth::FuseTripped
        );
        assert_eq!(tracker.health("session-safe-agent-b"), CacheHealth::Cold);
        assert_eq!(
            tracker.record(
                "session-safe-agent-b",
                TokenUsage {
                    input_tokens: 1,
                    cached_input_tokens: 2,
                    output_tokens: 0,
                },
            ),
            CacheHealth::Cold
        );
        assert_eq!(
            tracker.record("session-safe-agent-b", HEALTHY),
            CacheHealth::Cold
        );
    }

    #[test]
    fn totals_include_each_valid_terminal_report_but_not_malformed_data() {
        let mut tracker = CacheHealthTracker::default();
        tracker.record("session-safe-main", LARGE);
        tracker.record(
            "session-safe-main",
            TokenUsage {
                input_tokens: 20,
                cached_input_tokens: 10,
                output_tokens: 3,
            },
        );
        tracker.record(
            "session-safe-main",
            TokenUsage {
                input_tokens: 1,
                cached_input_tokens: 2,
                output_tokens: 99,
            },
        );
        assert_eq!(
            tracker.totals("session-safe-main"),
            UsageTotals {
                request_count: 2,
                input_tokens: 100_020,
                cached_input_tokens: 10,
                output_tokens: 4,
            }
        );
        tracker.mark_boundary("session-safe-main");
        assert_eq!(tracker.totals("session-safe-main"), UsageTotals::default());
    }
}
