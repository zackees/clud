//! Reaper and process-lifetime integration tests: orphan sweeping, batch
//! drain, daemon survival, subprocess capture lifetime, and the wedge
//! watchdog. Test IDs are `reaper::<module>::<test_name>`.
//!
//! Platform gates stay as the inner `#![cfg(windows)]` attribute at the
//! top of each Windows-only file, so the harness compiles the same set of
//! tests on every lane.

mod daemon_spawn_hygiene;
mod fixture_ids;
mod orphan_reap;
mod reaper_batch_drain_windows;
mod reaper_daemon_survival_windows;
mod reaper_orphan_sweep_survival;
mod subprocess_capture_lifecycle_windows;
mod tool_shell_lifecycle_windows;
mod wedge_watchdog_e2e;

/// These tests run host-wide sweeps that reap any CLUD-tagged process with a
/// dead originator. libtest runs them on parallel threads, and one test's
/// sweep kills another test's just-spawned child — the un-wait()ed child then
/// becomes a zombie, invisible to every environ scan and permanently missing
/// from the candidate set (the #994 reaper flake, "candidates=[]" with
/// `/proc/<pid>/environ` → EACCES). Serialize every test that performs a
/// host-wide sweep.
pub(crate) static REAPER_TEST_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
