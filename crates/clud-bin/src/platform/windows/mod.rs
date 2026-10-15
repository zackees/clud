//! Windows implementations of the `crate::platform` areas.

pub mod ctrl_c_track;
pub mod host;
pub mod job_orphan_reaper;
pub mod process_identity;
pub mod process_tree;
pub mod reaper_facts;
pub mod runner;
pub mod runner_execution;
pub mod subprocess;
pub mod trampoline;
pub mod wedge_watchdog;
#[cfg(test)]
mod win_creation_flags_tests;
