mod activity;
mod api_session_http;
pub mod api_session_lifecycle;
pub mod api_sessions;
pub mod api_turn_controller;
mod attach;
mod client;
mod client_compat;
mod client_leases;
mod commands;
mod cpu_alert_publish;
mod daemon_events;
mod entry;
mod gc_service;
mod handover_registry;
mod headless_adapter;
mod http;
mod io_helpers;
mod keys;
mod paths;
mod proc_sampler;
mod process_utils;
mod rp_broker;
mod runtime_config;
mod server;
mod session_tmp_sweep;
mod sessions;
mod target_sweep;
mod top;
mod types;
pub mod uv_cache_sweep;
mod watch_service;
mod wire_prost;
mod worker;
mod worker_shared;

pub use client::try_register_gc_watch;
pub use client::{
    acquire_foreground_client_lease, daemon_client_metrics, ensure_daemon, gc_client_insert,
    gc_client_list, gc_client_list_repo_visits, gc_client_purge, gc_client_reconcile,
    gc_client_record_repo_visit, is_incompatible_daemon_error, print_incompatible_daemon_error,
    try_handoff_kill_to_daemon, try_request_orphan_reap, ForegroundClientLease, GcPurgeOutcome,
};
pub use entry::{experimental_enabled, handle_special_command, run_centralized_session};
pub use http::{
    dashboard_url_from_info, fetch_state_json, read_api_info, read_dashboard_info,
    read_dashboard_port, DashboardInfo,
};
// Issue #469: re-exports for the telemetry integration test under
// `tests/diagnostics/telemetry_endpoint.rs` which spawns the dashboard server
// directly and asserts the full HTTP round-trip.
#[cfg(test)]
pub use cpu_alert_publish::SAMPLE_INTERVAL as CPU_SAMPLE_INTERVAL_FOR_TEST;
pub use cpu_alert_publish::{metrics_snapshot_path, MetricsSnapshot};
#[cfg(windows)]
pub(crate) use daemon_events::log_event as log_structured_event;
pub use http::{
    spawn_dashboard_telemetry_only, DashboardState, TelemetryEntry, TelemetryIngest,
    TelemetryPidDetail, TelemetryPidSummary, TelemetryStore,
};
pub use paths::{default_state_dir, default_trash_dir};
pub use types::GcWatchRoot;
pub use types::{ListRow, RepoVisit, ENV_ALLOW_DAEMON_SPAWN};
