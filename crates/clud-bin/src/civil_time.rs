//! One civil-date algorithm for the whole crate.
//!
//! `days -> (year, month, day)` is Howard Hinnant's public-domain
//! `civil_from_days` (<http://howardhinnant.github.io/date_algorithms.html>).
//! It used to be copied four times — `loop_artifacts::unix_to_ymd_hms`,
//! `command::loop_task::unix_to_ymd_hms`, `trash::civil_from_days` and
//! `tool_query::days_to_string` — and the copies had drifted: two did the
//! arithmetic in `u64` with `as` casts, two in `i64` with `div_euclid`.
//! Issue #1206 collapsed them here.
//!
//! Scope is the calendar arithmetic *only*. Every caller keeps its own
//! `format!` string, because those strings are already baked into data on
//! disk: `trash` names quarantine directories `YYYYMMDDTHHMMSSZ`, and
//! `loop_artifacts` writes ISO-8601 into persisted `info.json` / `log.txt`.
//! Do not "unify" formatting into this module.
//!
//! No `chrono` / `time` dependency: the algorithm is twenty lines and a
//! crate is not worth the build cost (#1206, Decisions).

/// The UTC civil fields `civil_from_unix_secs` returns, in order:
/// `(year, month, day, hour, minute, second)`.
pub type CivilFields = (i32, u32, u32, u32, u32, u32);

#[cfg(test)]
mod tests {
    use super::*;

    /// Known-date table. Each expectation was computed by hand from day
    /// counts, not from this implementation, so it is an independent check.
    #[test]
    fn known_dates_split_into_expected_civil_fields() {
        let cases: &[(i64, CivilFields)] = &[
            // Epoch, and the day after it (the two values trash asserts).
            (0, (1970, 1, 1, 0, 0, 0)),
            (86_400, (1970, 1, 2, 0, 0, 0)),
            // Pre-epoch: euclidean, not truncating-toward-zero.
            (-1, (1969, 12, 31, 23, 59, 59)),
            // 2000 is a leap year (divisible by 400), so Feb has a 29th.
            (951_782_400, (2000, 2, 29, 0, 0, 0)),
            (951_868_800, (2000, 3, 1, 0, 0, 0)),
            // 2100 is NOT a leap year (divisible by 100, not 400).
            (4_102_444_800, (2100, 1, 1, 0, 0, 0)),
            // Dec 31 / Jan 1 boundary across a leap year's 366th day.
            (1_230_767_999, (2008, 12, 31, 23, 59, 59)),
            (1_230_768_000, (2009, 1, 1, 0, 0, 0)),
        ];
        for (unix_secs, expected) in cases {
            assert_eq!(
                civil_from_unix_secs(*unix_secs),
                *expected,
                "unix_secs={unix_secs}"
            );
        }
    }

    #[test]
    fn seconds_of_day_walk_the_clock() {
        assert_eq!(civil_from_unix_secs(59), (1970, 1, 1, 0, 0, 59));
        assert_eq!(civil_from_unix_secs(60), (1970, 1, 1, 0, 1, 0));
        assert_eq!(civil_from_unix_secs(3_599), (1970, 1, 1, 0, 59, 59));
        assert_eq!(civil_from_unix_secs(3_600), (1970, 1, 1, 1, 0, 0));
        assert_eq!(civil_from_unix_secs(86_399), (1970, 1, 1, 23, 59, 59));
    }
}
