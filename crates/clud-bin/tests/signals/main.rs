//! Signal-handling integration tests (#1056).
//!
//! The Unix signal-kind matrix and the Windows console control events.
//! Each former top-level `tests/*.rs` file is a module here, so the
//! category links one test executable instead of two. Test IDs are
//! `signals::<module>::<test_name>`.
//!
//! The two modules are mutually exclusive by platform; each keeps its own
//! inner `#![cfg(unix)]` / `#![cfg(windows)]` attribute.

#[path = "../common/exe.rs"]
mod exe;

mod ctrlc_signal_kinds;
mod ctrlc_windows_events;
