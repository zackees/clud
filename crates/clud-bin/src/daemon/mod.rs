mod attach;
mod client;
mod commands;
mod entry;
mod gc_service;
mod http;
mod io_helpers;
mod keys;
mod paths;
mod process_utils;
mod server;
mod sessions;
mod types;
mod wire_prost;
mod worker;
mod worker_shared;

pub use client::{
    ensure_daemon, gc_client_insert, gc_client_list, gc_client_list_repo_visits, gc_client_purge,
    gc_client_reconcile, gc_client_record_repo_visit, try_handoff_kill_to_daemon, GcPurgeOutcome,
};
pub use entry::{experimental_enabled, handle_special_command, run_centralized_session};
pub use http::{
    dashboard_url_from_info, fetch_state_json, read_dashboard_info, read_dashboard_port,
    DashboardInfo,
};
pub use paths::{default_state_dir, default_trash_dir};
pub use types::{ListRow, RepoVisit, ENV_NO_DAEMON};
