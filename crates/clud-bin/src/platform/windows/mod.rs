//! Windows implementations of the `crate::platform` areas.

pub mod codex_auth;
#[cfg(test)]
pub mod gc_test_fs;
pub mod host;
pub mod login_env;
pub mod provider_auth;
pub mod repo_worktree_reclaim_exec;
pub mod session_registry;
pub mod session_tmp_continuation;
pub mod subprocess;
pub mod worker_shared;
