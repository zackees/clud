//! Reaper and process-lifetime integration tests: orphan sweeping, batch
//! drain, daemon survival, subprocess capture lifetime, and the wedge
//! watchdog. Test IDs are `reaper::<module>::<test_name>`.
//!
//! Host-only modules live in the category's host boundary, `platform/`
//! (#1953): Windows-only files under `platform/windows/` and Unix-only code
//! under `platform/unix/`. Their test IDs gain a `platform::<host>::` segment.

mod daemon_spawn_hygiene;
mod fixture_ids;
mod orphan_reap;
mod platform;
mod reaper_orphan_sweep_survival;

/// These tests run host-wide sweeps that reap any CLUD-tagged process with a
/// dead originator. libtest runs them on parallel threads, and one test's
/// sweep kills another test's just-spawned child — the un-wait()ed child then
/// becomes a zombie, invisible to every environ scan and permanently missing
/// from the candidate set (the #994 reaper flake, "candidates=[]" with
/// `/proc/<pid>/environ` → EACCES). Serialize every test that performs a
/// host-wide sweep.
pub(crate) static REAPER_TEST_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
