//! Windows helpers for `crate::runner`'s tests.

/// No-op: Windows has no executable permission bit.
#[cfg(test)]
pub(crate) fn mark_executable_for_test(_path: &std::path::Path) {}
