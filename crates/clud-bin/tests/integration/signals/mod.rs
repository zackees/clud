//! Signal-handling integration tests: the Unix signal-kind matrix and the
//! Windows console control events. Test IDs are
//! `signals::platform::<host>::<module>::<test_name>`.
//!
//! The two modules are mutually exclusive by platform, so both live in the
//! category's host boundary: `platform/unix/ctrlc_signal_kinds.rs` and
//! `platform/windows/ctrlc_windows_events.rs` (#1953).

mod platform;
