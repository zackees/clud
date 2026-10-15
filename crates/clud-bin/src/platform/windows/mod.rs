//! Windows implementations of the `crate::platform` areas.

pub mod alias_link;
pub mod codex_auth;
pub mod dnd;
#[cfg(test)]
pub mod gc_test_fs;
pub mod host;
pub mod login_env;
pub mod provider_auth;
pub mod repo_worktree_reclaim_exec;
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
