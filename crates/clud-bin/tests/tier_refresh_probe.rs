//! #687 premise probe: does a *targeted* sysinfo refresh avoid the host walk?
//!
//! The tier model in #687 assigns several migration targets to
//! "T1 / `Pids(set)`" — read a handful of PIDs instead of the host. That is
//! only a saving if `ProcessesToUpdate::Some` is actually cheaper than `All`.
//!
//! **On Windows it is not.** Measured here: `Some([self])` costs the same as
//! `All` (ratio ~1.0 across 484- and 530-process hosts) — sysinfo enumerates
//! the host and then filters, so the resulting table holds one process but the
//! bill is unchanged. clud's own `process_identity::start_time_of`, which
//! calls `OpenProcess` + `GetProcessTimes` directly, is ~4 orders of magnitude
//! cheaper.
//!
//! The conclusion for #687 is therefore *not* "use a sysinfo tier for
//! per-PID": it is to split the migration targets by what they actually need —
//! **identity** (`pid` + `start_time`, answerable by a direct per-PID OS call
//! at ~zero cost) versus **topology** (`parent_pid`, which has no cheap
//! per-PID Win32 answer and is what genuinely needs the daemon-owned service).
//!
//! Kept rather than deleted, alongside `win32_hooking_probe.rs`, because the
//! result is **platform-specific**: sysinfo's Linux backend reads `/proc/<pid>`
//! and may well make `Some` genuinely targeted there. Anyone extending the
//! tier model to another platform should re-run this before trusting the
//! Windows conclusion.
//!
//! ```text
//! soldr cargo test -p clud --test tier_refresh_probe -- --ignored --nocapture
//! ```
//!
//! `#[ignore]`d — a timing probe, not a gate.

use std::time::Instant;

use sysinfo::{Pid, ProcessRefreshKind, ProcessesToUpdate, System};

fn median(mut v: Vec<f64>) -> f64 {
    v.sort_by(|a, b| a.partial_cmp(b).unwrap());
    v[v.len() / 2]
}

#[test]
#[ignore = "timing probe for #687; run manually"]
fn targeted_refresh_vs_host_walk() {
    let self_pid = Pid::from_u32(std::process::id());
    let kind = ProcessRefreshKind::nothing();

    // Warm: first refresh on a fresh System pays one-time setup.
    let mut system = System::new();
    system.refresh_processes_specifics(ProcessesToUpdate::All, true, kind);
    let host_count = system.processes().len();

    let mut all = Vec::new();
    for _ in 0..7 {
        let t = Instant::now();
        system.refresh_processes_specifics(ProcessesToUpdate::All, true, kind);
        all.push(t.elapsed().as_secs_f64() * 1000.0);
    }

    let mut some = Vec::new();
    for _ in 0..7 {
        let t = Instant::now();
        system.refresh_processes_specifics(ProcessesToUpdate::Some(&[self_pid]), true, kind);
        some.push(t.elapsed().as_secs_f64() * 1000.0);
    }

    // A fresh System each time is the shape a one-shot caller actually has
    // (`process_identity::start_time_of`, the reaper's per-PID lookups).
    let mut cold_some = Vec::new();
    for _ in 0..7 {
        let mut s = System::new();
        let t = Instant::now();
        s.refresh_processes_specifics(ProcessesToUpdate::Some(&[self_pid]), true, kind);
        cold_some.push(t.elapsed().as_secs_f64() * 1000.0);
    }

    let m_all = median(all);
    let m_some = median(some);
    let m_cold = median(cold_some);

    eprintln!("#687 tier probe: host_processes={host_count}");
    eprintln!("  All (warm System):          {m_all:.2} ms");
    eprintln!("  Some([self]) (warm System): {m_some:.2} ms");
    eprintln!("  Some([self]) (cold System): {m_cold:.2} ms");
    eprintln!(
        "  ratio warm Some/All = {:.3}   cold Some/All = {:.3}",
        m_some / m_all,
        m_cold / m_all
    );

    // Also: does a targeted refresh on a warm System give us the four T1
    // fields for a pid, without having walked the host?
    let mut s = System::new();
    s.refresh_processes_specifics(ProcessesToUpdate::Some(&[self_pid]), true, kind);
    let visible = s.processes().len();
    let p = s.process(self_pid);
    eprintln!(
        "  cold Some([self]) -> {visible} process(es) in the table; \
         self present={} parent={:?} start_time={:?}",
        p.is_some(),
        p.and_then(|p| p.parent()),
        p.map(|p| p.start_time()),
    );

    // The alternative: clud's own hand-rolled per-PID Win32 query, which does
    // not go through sysinfo at all.
    let mut direct = Vec::new();
    for _ in 0..7 {
        let t = Instant::now();
        let _ = clud::process_identity::start_time_of(std::process::id());
        direct.push(t.elapsed().as_secs_f64() * 1000.0);
    }
    let m_direct = median(direct);
    eprintln!("  process_identity::start_time_of (direct Win32): {m_direct:.4} ms");
    eprintln!(
        "  direct is {:.0}x cheaper than a sysinfo targeted refresh",
        m_some / m_direct.max(f64::MIN_POSITIVE)
    );
}
