//! Signal-handling integration tests: the Unix signal-kind matrix and the
//! Windows console control events. Test IDs are
//! `signals::<module>::<test_name>`.
//!
//! The two modules are mutually exclusive by platform; each keeps its own
//! inner `#![cfg(unix)]` / `#![cfg(windows)]` attribute.

mod ctrlc_signal_kinds;
mod ctrlc_windows_events;
