//! The rate-limit floor (#1743, phase 3).
//!
//! Every upstream response carries `X-RateLimit-Limit`, `-Remaining` and
//! `-Reset` for the identity that made it. The broker keeps the newest
//! values per identity. Once `remaining` falls below the reserve (by default
//! 10% of `limit`), refreshes that nobody is waiting on at a terminal
//! (subscriptions, watchers, scripts) are deferred until the window resets,
//! so interactive reads keep the budget that is left. A deferred read is
//! served from the cache and marked stale, or falls back to the real `gh`;
//! it is never answered with anything GitHub did not send.

use std::collections::HashMap;
use std::sync::Mutex;

use super::upstream::Response;

/// Percent of the limit kept for interactive reads when no setting names
/// one (`git.gh_read_broker_reserve_pct`, `CLUD_GH_BROKER_RESERVE_PCT`).
pub const DEFAULT_RESERVE_PCT: u64 = 10;

/// One identity's newest rate-limit headers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Window {
    pub limit: u64,
    pub remaining: u64,
    /// Unix seconds at which the window resets.
    pub reset_s: u64,
}

impl Window {
    /// The `core` window a response reports, if it reports one. Other
    /// resources (`search`, `graphql`, ...) have their own budgets.
    pub fn of(response: &Response) -> Option<Self> {
        let resource = response.header("x-ratelimit-resource").unwrap_or("core");
        if !resource.eq_ignore_ascii_case("core") {
            return None;
        }
        let num = |name: &str| response.header(name)?.trim().parse::<u64>().ok();
        Some(Self {
            limit: num("x-ratelimit-limit")?,
            remaining: num("x-ratelimit-remaining")?,
            reset_s: num("x-ratelimit-reset")?,
        })
    }

    /// Whether `remaining` is under `reserve_pct` of `limit` and the window
    /// has not reset yet at `now_ms`.
    pub fn below_floor(&self, reserve_pct: u64, now_ms: u64) -> bool {
        let reset_ms = self.reset_s.saturating_mul(1000);
        now_ms < reset_ms
            && self.limit > 0
            && u128::from(self.remaining) * 100 < u128::from(self.limit) * u128::from(reserve_pct)
    }
}

/// Newest window per identity (the hash of the caller's `gh` and forwarded
/// env, so two tokens never share a floor).
#[derive(Default)]
pub struct Budget {
    windows: Mutex<HashMap<String, Window>>,
}

impl Budget {
    /// Record the window `response` reports for `identity`. A response from
    /// an older window never replaces a newer one, and within one window
    /// the lower `remaining` wins: concurrent responses arrive out of order.
    pub fn observe(&self, identity: &str, response: &Response) {
        let Some(window) = Window::of(response) else {
            return;
        };
        let mut windows = self.windows.lock().unwrap_or_else(|p| p.into_inner());
        let entry = windows.entry(identity.to_string()).or_insert(window);
        if window.reset_s > entry.reset_s
            || (window.reset_s == entry.reset_s && window.remaining < entry.remaining)
        {
            *entry = window;
        }
    }

    /// The window `identity` is deferred in, if it is below the floor.
    pub fn deferred(&self, identity: &str, reserve_pct: u64, now_ms: u64) -> Option<Window> {
        let windows = self.windows.lock().unwrap_or_else(|p| p.into_inner());
        windows
            .get(identity)
            .copied()
            .filter(|window| window.below_floor(reserve_pct, now_ms))
    }
}

/// The reserve in percent: `CLUD_GH_BROKER_RESERVE_PCT`, else the setting
/// `git.gh_read_broker_reserve_pct`, else [`DEFAULT_RESERVE_PCT`]. Values
/// above 100 are clamped; 0 turns the floor off.
pub fn reserve_pct_from(env: Option<&str>, setting: Option<u64>) -> u64 {
    env.and_then(|value| value.trim().parse::<u64>().ok())
        .or(setting)
        .unwrap_or(DEFAULT_RESERVE_PCT)
        .min(100)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    fn response(remaining: u64, reset: u64, resource: Option<&str>) -> Response {
        let mut headers = vec![
            ("X-RateLimit-Limit".to_string(), "5000".to_string()),
            ("X-RateLimit-Remaining".to_string(), remaining.to_string()),
            ("X-RateLimit-Reset".to_string(), reset.to_string()),
        ];
        if let Some(resource) = resource {
            headers.push(("X-RateLimit-Resource".to_string(), resource.to_string()));
        }
        Response {
            status: 200,
            headers,
            body: Arc::new(Vec::new()),
        }
    }

    #[test]
    fn the_floor_applies_below_the_reserve_until_the_window_resets() {
        let budget = Budget::default();
        budget.observe("me", &response(499, 2_000, Some("core")));
        assert!(budget.deferred("me", 10, 1_999_000).is_some());
        assert!(budget.deferred("me", 10, 2_000_000).is_none(), "reset");
        assert!(budget.deferred("me", 0, 1_000_000).is_none(), "floor off");
        assert!(budget.deferred("someone-else", 10, 1_000_000).is_none());
        budget.observe("you", &response(500, 2_000, None));
        assert!(
            budget.deferred("you", 10, 1_000_000).is_none(),
            "at the floor"
        );
    }

    #[test]
    fn out_of_order_and_other_resources_never_raise_the_count() {
        let budget = Budget::default();
        budget.observe("me", &response(400, 2_000, Some("core")));
        budget.observe("me", &response(4_000, 2_000, Some("core")));
        budget.observe("me", &response(4_900, 2_000, Some("graphql")));
        assert_eq!(budget.deferred("me", 10, 0).map(|w| w.remaining), Some(400));
        // A new window replaces the old one.
        budget.observe("me", &response(4_999, 5_600, Some("core")));
        assert!(budget.deferred("me", 10, 0).is_none());
    }

    #[test]
    fn the_reserve_comes_from_env_then_setting_then_default() {
        assert_eq!(reserve_pct_from(None, None), DEFAULT_RESERVE_PCT);
        assert_eq!(reserve_pct_from(None, Some(25)), 25);
        assert_eq!(reserve_pct_from(Some("5"), Some(25)), 5);
        assert_eq!(reserve_pct_from(Some("junk"), Some(25)), 25);
        assert_eq!(reserve_pct_from(Some("250"), None), 100);
    }
}
