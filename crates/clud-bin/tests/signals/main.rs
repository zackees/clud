//! Signal-handling integration tests (#1056).
//!
//! The Unix signal-kind matrix, the Windows console control events, and the
//! force-kill terminal restore (#1705).
//! Each former top-level `tests/*.rs` file is a module here, so the
//! category links one test executable instead of two. Test IDs are
//! `signals::<module>::<test_name>`.
//!
//! The two ctrlc modules are mutually exclusive by platform; each keeps its
//! own inner `#![cfg(unix)]` / `#![cfg(windows)]` attribute.
//! `term_guard_restore` has a half for each.

#[path = "../common/exe.rs"]
mod exe;

mod ctrlc_signal_kinds;
mod ctrlc_windows_events;
mod term_guard_restore;
