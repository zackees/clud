//! Diagnostics and observability integration tests (#1056).
//!
//! Crash reporting, symbol resolution, the telemetry endpoint, the Win32
//! hooking probe, tier refresh, and the Windows runtime cache hop. Each
//! former top-level `tests/*.rs` file is a module here, so the category
//! links one test executable instead of six. Test IDs are
//! `diagnostics::<module>::<test_name>`.

#[path = "../common/exe.rs"]
mod exe;

mod crash_report;
mod runtime_cache_hop_windows;
mod symbols;
mod telemetry_endpoint;
mod tier_refresh_probe;
mod win32_hooking_probe;
