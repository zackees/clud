//! The `cli` category's host boundary (#1953): the only place in
//! `tests/integration/cli/` that selects a host. Host-only test modules
//! live in the concrete trees below.

#[cfg(windows)]
mod windows;
