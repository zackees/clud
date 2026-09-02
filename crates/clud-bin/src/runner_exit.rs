//! Exit-status normalization for runner backends.

pub(super) fn normalize_exit_code(code: i32) -> i32 {
    match code {
        -2 => 130,
        -9 => 137,
        -15 => 143,
        _ => code,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::launch_log::SIGNAL_EXIT_CODES;

    /// Issue #1020: `silent_bridge_reason` must not label a signal death
    /// "failed before reaching the gateway", and it recognizes those deaths by
    /// the codes this function produces. The two lists are a pair — if a new
    /// signal mapping lands here without being excluded there, a Ctrl+C-shaped
    /// exit silently becomes a failure diagnosis again.
    #[test]
    fn normalize_exit_code_signal_outputs_are_all_excluded_from_silent_bridge() {
        for raw in [-2, -9, -15] {
            let normalized = normalize_exit_code(raw);
            assert!(
                SIGNAL_EXIT_CODES.contains(&normalized),
                "normalize_exit_code({raw}) = {normalized}, which launch_log::SIGNAL_EXIT_CODES \
                 does not cover — a signal death would be recorded as a gateway failure"
            );
        }
    }

    #[test]
    fn non_signal_codes_pass_through_unchanged() {
        for code in [0, 1, 2, 3, 127, 130, 137, 143] {
            assert_eq!(normalize_exit_code(code), code);
        }
    }
}
