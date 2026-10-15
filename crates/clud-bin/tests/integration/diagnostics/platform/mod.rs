//! The `diagnostics` category's host boundary (#1953): the only place in
//! `tests/integration/diagnostics/` that selects a host. Host-only test modules
//! live in the concrete trees below.

#[cfg(unix)]
mod unix;

#[cfg(windows)]
mod windows;
