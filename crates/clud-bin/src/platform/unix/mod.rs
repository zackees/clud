//! Unix (Linux and macOS) implementations of the `crate::platform` areas.

pub mod alias_link;
pub mod backend_bootstrap;
pub mod block_bad_cmd_io;
pub mod block_bad_cmd_repeat;
#[cfg(test)]
mod build_id;
pub mod codex_auth;
pub mod config;
pub mod dnd;
#[cfg(test)]
pub mod gc_test_fs;
#[cfg(test)]
mod gh_broker;
pub mod grind_scripts;
pub mod host;
#[cfg(test)]
pub mod large_file_guard;
pub mod log_event;
pub mod login_env;
#[cfg(test)]
mod nounset;
pub mod optimize;
pub mod provider_auth;
pub mod repo_worktree_reclaim_exec;
pub mod runtime_cache;
pub mod self_install;
pub mod self_install_activation;
pub mod session_registry;
pub mod session_tmp_continuation;
pub mod shim_fs;
pub mod shim_main;
pub mod shim_telemetry;
pub mod subprocess;
pub mod usage_ledger;
pub mod video;
pub mod voice;
pub mod worker_shared;
