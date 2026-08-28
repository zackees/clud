//! Reaper and process-lifetime integration tests (#1056).
//!
//! Orphan sweeping, batch drain, daemon survival, subprocess capture
//! lifetime, and the wedge watchdog. Each former top-level `tests/*.rs`
//! file is a module here, so the category links one test executable
//! instead of seven. Test IDs are `reaper::<module>::<test_name>`.
//!
//! Platform gates stay as the inner `#![cfg(windows)]` attribute at the
//! top of each Windows-only file, so this target still compiles to the
//! same set of tests on every lane.

#[path = "../common/mod.rs"]
mod common;

#[path = "../common/exe.rs"]
mod exe;

mod orphan_reap;
mod reaper_batch_drain_windows;
mod reaper_daemon_survival_windows;
mod reaper_orphan_sweep_survival;
mod subprocess_capture_lifecycle_windows;
mod tool_shell_lifecycle_windows;
mod wedge_watchdog_e2e;
