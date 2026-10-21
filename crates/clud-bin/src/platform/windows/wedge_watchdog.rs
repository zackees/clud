//! Windows sampler for `crate::wedge_watchdog`: Toolhelp subtree walk,
//! hottest-thread user time, and owning-process IO write bytes.

use crate::wedge_watchdog::{Sample, GATE_USER_PCT_THRESHOLD};
use std::collections::{HashMap, HashSet};
use std::time::Duration;
use std::time::Instant;

use windows::Win32::Foundation::{CloseHandle, FILETIME};
use windows::Win32::System::Diagnostics::ToolHelp::{
    CreateToolhelp32Snapshot, Process32FirstW, Process32NextW, Thread32First, Thread32Next,
    PROCESSENTRY32W, TH32CS_SNAPPROCESS, TH32CS_SNAPTHREAD, THREADENTRY32,
};
use windows::Win32::System::Threading::{
    GetProcessIoCounters, GetProcessTimes, GetThreadTimes, OpenProcess, OpenThread, IO_COUNTERS,
    PROCESS_QUERY_LIMITED_INFORMATION, THREAD_QUERY_LIMITED_INFORMATION,
};

/// One tick's worth of raw measurement, carrying enough identity (which
/// pid/tid was hottest) for the warning + log line in the outer module.
pub struct SampledTick {
    pub sample: Sample,
    pub hot_pid: u32,
    pub hot_tid: u32,
}

/// Stateful raw sampler. Remembers the previous tick's per-thread
/// user-mode time and per-process IO write bytes so it can report
/// deltas. Opens no OS handles between ticks — every
/// `OpenThread`/`OpenProcess` handle is opened, queried, and closed
/// within a single `tick()` call.
pub struct ThreadIoSampler {
    prev_thread_user_100ns: HashMap<(u32, u32), u64>,
    prev_io_write_bytes: HashMap<u32, u64>,
    /// Per-process user time, for the #709 gate. Cheap to maintain — one
    /// `GetProcessTimes` per subtree pid — and it is what lets a healthy
    /// tick skip the host-wide thread enumeration entirely.
    prev_process_user_100ns: HashMap<u32, u64>,
    prev_at: Option<Instant>,
}

impl ThreadIoSampler {
    pub(crate) fn new() -> Self {
        Self {
            prev_thread_user_100ns: HashMap::new(),
            prev_io_write_bytes: HashMap::new(),
            prev_process_user_100ns: HashMap::new(),
            prev_at: None,
        }
    }

    /// Sample the subtree rooted at `root_pid`. Returns `None` on the
    /// first call (no baseline for deltas yet) or when no thread in the
    /// subtree has a computable delta this tick (e.g. the subtree
    /// vanished, or every thread present is new since the last tick).
    ///
    /// Since #709 a tick whose subtree is provably cool at the *process*
    /// level short-circuits before the host-wide thread enumeration and
    /// reports an explicitly healthy sample. It reports rather than
    /// returns `None` because the caller treats `None` as "no
    /// observation", which would leave a partial wedge streak standing
    /// across an idle stretch.
    pub fn tick(&mut self, root_pid: u32) -> Option<SampledTick> {
        let now = Instant::now();
        let subtree = subtree_pids(root_pid);

        // #709 gate: the detection signature is "one *thread* at >=90% of a
        // core". A thread's user time can never exceed its process's, so if
        // no process in the subtree burned that much user time this window,
        // no thread in it did either — and the expensive per-thread walk
        // cannot change the answer.
        //
        // That walk is the costliest thing the watchdog does: a
        // `TH32CS_SNAPTHREAD` snapshot enumerates **every thread on the
        // host** before being filtered down to the subtree, then opens each
        // surviving thread. Skipping it on a healthy session drops the
        // steady-state cost to a handful of `GetProcessTimes` calls.
        //
        // Safe by construction: the gate can only skip work when the
        // process-level total already rules out a wedge, so it cannot
        // produce a false negative.
        let wall_delta = self.prev_at.map(|prev| now.saturating_duration_since(prev));
        let cur_process_user = process_user_times(&subtree);
        let hot_enough = crate::wedge_watchdog::subtree_could_hide_a_hot_thread(
            &self.prev_process_user_100ns,
            &cur_process_user,
            wall_delta,
            GATE_USER_PCT_THRESHOLD,
        );

        let thread_ids = if hot_enough {
            threads_for_pids(&subtree)
        } else {
            Vec::new()
        };

        let mut cur_thread_user: HashMap<(u32, u32), u64> = HashMap::new();
        // (pid, tid, delta_100ns) of the single hottest thread this tick.
        let mut hottest: Option<(u32, u32, u64)> = None;

        for (pid, tid) in thread_ids {
            let Some(user_100ns) = thread_user_time_100ns(tid) else {
                continue;
            };
            if let Some(prev) = self.prev_thread_user_100ns.get(&(pid, tid)) {
                let delta = user_100ns.saturating_sub(*prev);
                let is_hotter = hottest.map(|(_, _, d)| delta > d).unwrap_or(true);
                if is_hotter {
                    hottest = Some((pid, tid, delta));
                }
            }
            cur_thread_user.insert((pid, tid), user_100ns);
        }

        let mut cur_io_write: HashMap<u32, u64> = HashMap::new();
        for pid in &subtree {
            if let Some(bytes) = process_io_write_bytes(*pid) {
                cur_io_write.insert(*pid, bytes);
            }
        }

        // A gated-out tick must still be *reported* as healthy, not
        // swallowed. `run_watchdog_loop` does `let Some(tick) = … else
        // { continue }`, so returning `None` here would leave the
        // detector's streak untouched — eight hot windows followed by an
        // idle stretch and one more hot window would reach `Wedged`
        // without nine consecutive qualifying windows. The gate proved
        // no thread is hot, so say exactly that.
        if !hot_enough {
            // `None` on the very first tick, as before — there is no
            // baseline to measure a window against.
            let result = self.prev_at.map(|prev_at| SampledTick {
                sample: Sample {
                    hottest_thread_user_delta: Duration::ZERO,
                    wall_delta: now.saturating_duration_since(prev_at),
                    io_write_delta: 0,
                },
                hot_pid: root_pid,
                hot_tid: 0,
            });
            // The per-thread map goes stale while gated, so drop it. The
            // first armed tick after a quiet stretch re-establishes a
            // baseline and reports nothing, costing one extra window of
            // detection latency (90 s -> 100 s, still inside the issue's
            // 60-120 s acceptance band).
            self.prev_thread_user_100ns.clear();
            self.prev_io_write_bytes = cur_io_write;
            self.prev_process_user_100ns = cur_process_user;
            self.prev_at = Some(now);
            return result;
        }

        let result = match (self.prev_at, hottest) {
            (Some(prev_at), Some((hot_pid, hot_tid, hot_delta_100ns))) => {
                let io_write_delta = match (
                    cur_io_write.get(&hot_pid),
                    self.prev_io_write_bytes.get(&hot_pid),
                ) {
                    (Some(cur), Some(prev)) => cur.saturating_sub(*prev),
                    // No IO baseline for the hot process yet (it's new
                    // to the subtree this tick) — treat as quiet rather
                    // than stalling detection; the next tick will have
                    // a real baseline.
                    _ => 0,
                };
                Some(SampledTick {
                    sample: Sample {
                        hottest_thread_user_delta: Duration::from_nanos(
                            hot_delta_100ns.saturating_mul(100),
                        ),
                        wall_delta: now.saturating_duration_since(prev_at),
                        io_write_delta,
                    },
                    hot_pid,
                    hot_tid,
                })
            }
            _ => None,
        };

        self.prev_thread_user_100ns = cur_thread_user;
        self.prev_io_write_bytes = cur_io_write;
        self.prev_process_user_100ns = cur_process_user;
        self.prev_at = Some(now);
        result
    }
}

/// DFS over the Toolhelp32 process snapshot's parent-pid graph, rooted
/// at `root_pid`. Falls back to `[root_pid]` alone if the snapshot
/// fails or the root isn't visible (e.g. it just exited) so callers
/// degrade gracefully instead of panicking.
fn subtree_pids(root_pid: u32) -> Vec<u32> {
    let mut children: HashMap<u32, Vec<u32>> = HashMap::new();
    let mut seen_root = false;

    // SAFETY: `CreateToolhelp32Snapshot` with `TH32CS_SNAPPROCESS` takes
    // a system-wide snapshot; `0` for the pid parameter is required and
    // ignored for this flag per the Win32 contract. The returned handle
    // is closed below before returning.
    let Ok(snapshot) = (unsafe { CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0) }) else {
        return vec![root_pid];
    };
    let mut entry = PROCESSENTRY32W {
        dwSize: std::mem::size_of::<PROCESSENTRY32W>() as u32,
        ..Default::default()
    };
    // SAFETY: `entry` is stack-allocated with `dwSize` set per the
    // Win32 contract for `Process32FirstW`/`Process32NextW`.
    if unsafe { Process32FirstW(snapshot, &mut entry) }.is_ok() {
        loop {
            if entry.th32ProcessID == root_pid {
                seen_root = true;
            }
            children
                .entry(entry.th32ParentProcessID)
                .or_default()
                .push(entry.th32ProcessID);
            if unsafe { Process32NextW(snapshot, &mut entry) }.is_err() {
                break;
            }
        }
    }
    let _ = unsafe { CloseHandle(snapshot) };

    if !seen_root {
        return vec![root_pid];
    }

    crate::wedge_watchdog::descendants_of(&children, root_pid)
}

/// Every `(pid, tid)` in `pids` via a single system-wide
/// `TH32CS_SNAPTHREAD` snapshot, filtered down to the wanted pid set.
fn threads_for_pids(pids: &[u32]) -> Vec<(u32, u32)> {
    let mut out = Vec::new();
    let wanted: HashSet<u32> = pids.iter().copied().collect();
    if wanted.is_empty() {
        return out;
    }

    // SAFETY: see `subtree_pids` — same snapshot contract, different
    // flag/struct.
    let Ok(snapshot) = (unsafe { CreateToolhelp32Snapshot(TH32CS_SNAPTHREAD, 0) }) else {
        return out;
    };
    let mut entry = THREADENTRY32 {
        dwSize: std::mem::size_of::<THREADENTRY32>() as u32,
        ..Default::default()
    };
    if unsafe { Thread32First(snapshot, &mut entry) }.is_ok() {
        loop {
            if wanted.contains(&entry.th32OwnerProcessID) {
                out.push((entry.th32OwnerProcessID, entry.th32ThreadID));
            }
            if unsafe { Thread32Next(snapshot, &mut entry) }.is_err() {
                break;
            }
        }
    }
    let _ = unsafe { CloseHandle(snapshot) };
    out
}

/// User-mode time for one thread, in 100 ns ticks (raw `FILETIME`
/// units) — `None` if the thread has already exited or we lack access.
fn thread_user_time_100ns(tid: u32) -> Option<u64> {
    // SAFETY: `OpenThread` with a valid access mask and thread id;
    // failure returns `Err` (mapped to `None` below) rather than an
    // invalid handle.
    let handle = unsafe { OpenThread(THREAD_QUERY_LIMITED_INFORMATION, false, tid) }.ok()?;
    let mut creation = FILETIME::default();
    let mut exit = FILETIME::default();
    let mut kernel = FILETIME::default();
    let mut user = FILETIME::default();
    // SAFETY: all four out-pointers are stack-allocated `FILETIME`
    // values valid for the duration of the call, per the
    // `GetThreadTimes` contract.
    let result =
        unsafe { GetThreadTimes(handle, &mut creation, &mut exit, &mut kernel, &mut user) };
    let _ = unsafe { CloseHandle(handle) };
    result.ok()?;
    Some(filetime_to_u64(user))
}

/// User-mode CPU time for each pid, in 100 ns ticks (#709).
///
/// One `OpenProcess` + `GetProcessTimes` per pid — microseconds each, and
/// bounded by the subtree rather than by the host. This is the cheap
/// measurement that lets a healthy tick skip `threads_for_pids`, whose
/// `TH32CS_SNAPTHREAD` snapshot enumerates every thread on the machine.
/// Pids that cannot be opened are simply absent; the caller fails open.
fn process_user_times(pids: &[u32]) -> HashMap<u32, u64> {
    let mut out = HashMap::with_capacity(pids.len());
    for &pid in pids {
        // SAFETY: `OpenProcess` with a valid access mask and pid; failure
        // returns `Err` and the pid is skipped.
        let Ok(handle) = (unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid) })
        else {
            continue;
        };
        let mut creation = FILETIME::default();
        let mut exit = FILETIME::default();
        let mut kernel = FILETIME::default();
        let mut user = FILETIME::default();
        // SAFETY: all four out-pointers are stack-allocated `FILETIME`
        // values valid for the duration of the call.
        let result =
            unsafe { GetProcessTimes(handle, &mut creation, &mut exit, &mut kernel, &mut user) };
        let _ = unsafe { CloseHandle(handle) };
        if result.is_ok() {
            out.insert(pid, filetime_to_u64(user));
        }
    }
    out
}

/// IO write-transfer byte count for one process — `None` if it has
/// already exited or we lack access.
fn process_io_write_bytes(pid: u32) -> Option<u64> {
    // SAFETY: `OpenProcess` with a valid access mask and pid; failure
    // returns `Err` (mapped to `None` below).
    let handle = unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid) }.ok()?;
    let mut counters = IO_COUNTERS::default();
    // SAFETY: `counters` is a stack-allocated out-pointer valid for the
    // duration of the call, per the `GetProcessIoCounters` contract.
    let result = unsafe { GetProcessIoCounters(handle, &mut counters) };
    let _ = unsafe { CloseHandle(handle) };
    result.ok()?;
    Some(counters.WriteTransferCount)
}

fn filetime_to_u64(ft: FILETIME) -> u64 {
    ((ft.dwHighDateTime as u64) << 32) | ft.dwLowDateTime as u64
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn filetime_to_u64_combines_high_and_low_words() {
        let ft = FILETIME {
            dwLowDateTime: 0x1234_5678,
            dwHighDateTime: 0x0000_0001,
        };
        assert_eq!(filetime_to_u64(ft), 0x0000_0001_1234_5678);
    }

    #[test]
    fn filetime_to_u64_zero_is_zero() {
        assert_eq!(filetime_to_u64(FILETIME::default()), 0);
    }

    /// Smoke test against the real OS: two ticks on our own pid should
    /// not panic, and by the second tick we have a baseline (this
    /// process always has at least one thread with a computable user
    /// time delta). Mirrors `cpu_banner`'s `sampler_returns_at_least_self`.
    #[test]
    fn sampler_ticks_against_real_process_without_panicking() {
        let mut sampler = ThreadIoSampler::new();
        let self_pid = std::process::id();
        assert!(
            sampler.tick(self_pid).is_none(),
            "first tick has no baseline"
        );
        // Burn a little real CPU so there's a non-zero delta to find.
        let mut acc: u64 = 0;
        for i in 0..5_000_000u64 {
            acc = acc.wrapping_add(std::hint::black_box(i));
        }
        std::hint::black_box(acc);
        let second = sampler.tick(self_pid);
        assert!(second.is_some(), "second tick should find a baseline");
    }
}
