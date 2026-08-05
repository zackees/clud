use super::{DEFAULT_GC_LOCKED_HARD_AGE_DAYS, ENV_GC_LOCKED_HARD_AGE_DAYS, SECS_PER_DAY};
use std::time::Duration;

pub(super) fn locked_hard_age_from_env() -> Duration {
    let raw = std::env::var(ENV_GC_LOCKED_HARD_AGE_DAYS).ok();
    locked_hard_age_from_raw(raw.as_deref())
}

pub(super) fn locked_hard_age_from_raw(raw: Option<&str>) -> Duration {
    let days = raw
        .and_then(|value| value.trim().parse::<u64>().ok())
        .unwrap_or(DEFAULT_GC_LOCKED_HARD_AGE_DAYS);
    Duration::from_secs(days.saturating_mul(SECS_PER_DAY))
}
