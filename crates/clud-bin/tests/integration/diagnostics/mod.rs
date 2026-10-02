//! Diagnostics and observability integration tests: crash reporting, symbol
//! resolution, the telemetry endpoint, the Win32 hooking probe, tier refresh,
//! and the Windows runtime cache hop. Test IDs are
//! `diagnostics::<module>::<test_name>`.

mod crash_report;
mod runtime_cache_hop_windows;
mod symbols;
mod telemetry_endpoint;
mod tier_refresh_probe;
mod win32_hooking_probe;
