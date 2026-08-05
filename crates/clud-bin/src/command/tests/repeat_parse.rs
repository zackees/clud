use super::*;
#[test]
fn test_parse_repeat_interval_accepted_forms() {
    // The four forms the issue explicitly calls out.
    assert_eq!(parse_repeat_interval("30s").unwrap(), 30);
    assert_eq!(parse_repeat_interval("5m").unwrap(), 5 * 60);
    assert_eq!(parse_repeat_interval("1h").unwrap(), 60 * 60);
    assert_eq!(parse_repeat_interval("24h").unwrap(), 24 * 60 * 60);
}

#[test]
fn test_parse_repeat_interval_accepts_uppercase_unit() {
    // Case-insensitivity is a small kindness; matches argparse-style ergonomics
    // and the spec doesn't forbid it.
    assert_eq!(parse_repeat_interval("30S").unwrap(), 30);
    assert_eq!(parse_repeat_interval("2H").unwrap(), 7200);
}

#[test]
fn test_parse_repeat_interval_trims_surrounding_whitespace() {
    assert_eq!(parse_repeat_interval("  1h  ").unwrap(), 3600);
}

#[test]
fn test_parse_repeat_interval_rejects_empty() {
    let err = parse_repeat_interval("").unwrap_err();
    assert!(err.contains("empty"), "expected empty-error, got: {err}");
    let err = parse_repeat_interval("   ").unwrap_err();
    assert!(err.contains("empty"), "expected empty-error, got: {err}");
}

#[test]
fn test_parse_repeat_interval_rejects_zero() {
    // A zero interval would busy-loop the scheduler.
    let err = parse_repeat_interval("0s").unwrap_err();
    assert!(
        err.contains("greater than zero"),
        "expected zero-error, got: {err}"
    );
    let err = parse_repeat_interval("0h").unwrap_err();
    assert!(err.contains("greater than zero"), "got: {err}");
}

#[test]
fn test_parse_repeat_interval_rejects_missing_unit() {
    let err = parse_repeat_interval("30").unwrap_err();
    assert!(
        err.to_lowercase().contains("unit"),
        "expected unit-error, got: {err}"
    );
}

#[test]
fn test_parse_repeat_interval_rejects_missing_value() {
    // "s" alone — split point at index 0 → leading-non-digit error.
    let err = parse_repeat_interval("s").unwrap_err();
    assert!(
        err.contains("positive integer"),
        "expected leading-digit error, got: {err}"
    );
}

#[test]
fn test_parse_repeat_interval_rejects_negative() {
    // The leading `-` is non-digit; trips the "must start with positive integer"
    // branch. We don't accept negatives at all.
    let err = parse_repeat_interval("-1h").unwrap_err();
    assert!(
        err.contains("positive integer"),
        "expected leading-digit error, got: {err}"
    );
}

#[test]
fn test_parse_repeat_interval_rejects_fractional() {
    // "1.5h" — the `.` is non-digit, so we split into "1" + ".5h" and the
    // unit ".5h" doesn't match s/m/h.
    let err = parse_repeat_interval("1.5h").unwrap_err();
    assert!(
        err.to_lowercase().contains("unit"),
        "expected unit-error for fractional input, got: {err}"
    );
}

#[test]
fn test_parse_repeat_interval_rejects_unknown_unit() {
    for bad in &["30d", "1y", "1w", "30sec", "1hr", "10ms"] {
        let err = parse_repeat_interval(bad).unwrap_err();
        assert!(
            err.to_lowercase().contains("unit") || err.to_lowercase().contains("unsupported"),
            "expected unit-error for {bad:?}, got: {err}"
        );
    }
}

#[test]
fn test_parse_repeat_interval_rejects_overflow() {
    // u64::MAX seconds * 3600 obviously overflows; we should bubble that up
    // as a clean "too large" error, not panic.
    let err = parse_repeat_interval("18446744073709551615h").unwrap_err();
    assert!(
        err.contains("too large") || err.contains("invalid"),
        "expected overflow-error, got: {err}"
    );
}

#[test]
fn test_parse_repeat_interval_rejects_garbage() {
    // Note: the parser intentionally trims the unit part, so `"1 h"` and
    // similar inner-whitespace inputs are accepted (they normalize to `1h`).
    // We only assert genuinely malformed inputs here.
    for bad in &["abc", "1h2m", "h1", "1x", "-1h", "1.5h", " ", "0"] {
        assert!(
            parse_repeat_interval(bad).is_err(),
            "expected error for {bad:?}"
        );
    }
}

// ---- Flag-precedence: repeat_implies_no_done_warning ----
