mod attach;
mod client;
mod commands;
mod conhost_reaper;
mod daemon_events;
mod entry;
mod gc_service;
mod http;
mod io_helpers;
mod keys;
mod paths;
mod proc_sampler;
mod process_utils;
mod rp_broker;
mod server;
mod session_tmp_sweep;
mod sessions;
mod target_sweep;
mod top;
mod types;
pub mod uv_cache_sweep;
mod wire_prost;
mod worker;
mod worker_shared;
mod watch_service;

pub use client::{
    daemon_client_metrics, ensure_daemon, gc_client_insert, gc_client_list,
    gc_client_list_repo_visits, gc_client_purge, gc_client_reconcile, gc_client_record_repo_visit,
    try_handoff_kill_to_daemon, try_request_orphan_reap, GcPurgeOutcome,
};
pub use client::try_register_gc_watch;
pub use entry::{experimental_enabled, handle_special_command, run_centralized_session};
pub use http::{
    dashboard_url_from_info, fetch_state_json, read_dashboard_info, read_dashboard_port,
    DashboardInfo,
};
// Issue #469: re-exports for the telemetry integration test under
// `tests/telemetry_endpoint.rs` which spawns the dashboard server
// directly and asserts the full HTTP round-trip.
pub use http::{
    spawn_dashboard_telemetry_only, DashboardState, TelemetryEntry, TelemetryIngest,
    TelemetryPidDetail, TelemetryPidSummary, TelemetryStore,
};
pub use paths::{default_state_dir, default_trash_dir};
pub use types::{ListRow, RepoVisit, ENV_NO_DAEMON};
pub use types::GcWatchRoot;
