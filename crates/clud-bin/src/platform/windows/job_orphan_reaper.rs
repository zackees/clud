//! Windows foreground Job completion-port listener for
//! `crate::job_orphan_reaper` (#569, #616). Every decision it acts on comes
//! from the platform-neutral `crate::job_orphan_reaper::model`.

use std::collections::{HashMap, HashSet};
use std::ffi::c_void;
use std::mem::{size_of, zeroed};
use std::ptr::null_mut;
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc, Mutex,
};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use windows::core::PCWSTR;
use windows::Win32::Foundation::{
    CloseHandle, GetLastError, HANDLE, INVALID_HANDLE_VALUE, WAIT_TIMEOUT,
};
use windows::Win32::System::Diagnostics::ToolHelp::{
    CreateToolhelp32Snapshot, Process32FirstW, Process32NextW, PROCESSENTRY32W, TH32CS_SNAPPROCESS,
};
use windows::Win32::System::JobObjects::{
    AssignProcessToJobObject, CreateJobObjectW, JobObjectAssociateCompletionPortInformation,
    JobObjectBasicProcessIdList, QueryInformationJobObject, SetInformationJobObject,
    JOBOBJECT_ASSOCIATE_COMPLETION_PORT, JOBOBJECT_BASIC_PROCESS_ID_LIST,
};
use windows::Win32::System::Threading::{
    GetCurrentProcess, OpenProcess, TerminateProcess, PROCESS_TERMINATE,
};
use windows::Win32::System::IO::{
    CreateIoCompletionPort, GetQueuedCompletionStatus, PostQueuedCompletionStatus,
};

use crate::job_orphan_reaper::{
    DecisionAction, FactsSnapshot, ProcessMeta, ReapDecision, RegisteredBackend, Signal,
};

use crate::job_orphan_reaper::{
    batch_needs_reconcile, ACTIVE_PROCESS_ZERO, EXIT_PROCESS, NEW_PROCESS,
};

/// Upper bound on completion-port messages folded into one batch (#706).
///
/// The drain is non-blocking, so this is not a latency knob — it only
/// bounds the work done between two `stop` checks, so a session that is
/// spawning processes faster than the reaper can classify them still
/// shuts down promptly. Anything left in the queue is picked up by the
/// next iteration.
const MAX_DRAIN_BATCH: usize = 256;

/// Minimum pause between full-host Toolhelp snapshots for one foreground
/// session. A deferred scan waits for the next slot; it never authorizes a
/// destructive action without metadata.
const SCAN_BACKOFF_BASE: Duration = Duration::from_millis(50);
const SCAN_BACKOFF_MAX_MULTIPLIER: u32 = 10;

/// Minimum interval between retry-resolution scans for unresolved new
/// PIDs.  Without this, every 200 ms quiet-period tick that finds *any*
/// unresolved PID re-enumerates every process on the host — a cost that
/// grows with cumulative processes-ever-seen.  A 2-second floor cuts scan
/// frequency by 10× while keeping the per-PID retirement latency bounded
/// (at most `MAX_METADATA_RETRIES × 2 s ≈ 20 s` before a PID is retired).
const RETRY_SCAN_INTERVAL: Duration = Duration::from_secs(2);

use crate::job_orphan_reaper::TrackerProcesses;

/// Milliseconds since the tracker's own start.
///
/// A monotonic tracker-local clock, never a wall clock: every TTL here is
/// a duration, and a wall clock that steps backwards would freeze the
/// eviction sweep for the size of the step.
fn now_ms() -> u64 {
    use std::sync::OnceLock;
    use std::time::Instant;
    static ORIGIN: OnceLock<Instant> = OnceLock::new();
    ORIGIN.get_or_init(Instant::now).elapsed().as_millis() as u64
}

use crate::reap_log::{ReapAction, ReapCounters, ReapEvent, ReapLog, ReapPhase};

/// Session accounting and its log (#673 Phases 0 and 5).
///
/// `epoch` is folded into `session` at `ACTIVE_PROCESS_ZERO`, which clears
/// the tracked maps mid-session: per-epoch counts are accumulated rather
/// than lost.
#[derive(Default)]
struct Telemetry {
    session: ReapCounters,
    epoch: ReapCounters,
    log: Option<ReapLog>,
    flight_recorder: Option<crate::reap_log::ReapFlightRecorder>,
}

impl Telemetry {
    fn totals(&self) -> ReapCounters {
        let mut totals = self.session.clone();
        totals.absorb(&self.epoch);
        totals
    }

    fn roll_epoch(&mut self) {
        let epoch = std::mem::take(&mut self.epoch);
        self.session.absorb(&epoch);
    }

    fn record(&mut self, event: ReapEvent) {
        if let Some(log) = self.log.as_mut() {
            log.record(&event);
        }
    }

    fn checkpoint(&mut self) {
        let totals = self.totals();
        if let Some(recorder) = self.flight_recorder.as_mut() {
            recorder.checkpoint(&totals);
        }
    }
}

fn with_telemetry(telemetry: &Mutex<Telemetry>, f: impl FnOnce(&mut Telemetry)) {
    if let Ok(mut telemetry) = telemetry.lock() {
        f(&mut telemetry);
    }
}

/// State the facts collector carries across passes.
///
/// The daemon-marker cache is the reason this is not rebuilt each tick: it
/// reads a process's environment once per identity, ever, instead of once
/// per exit for every process on the host.
#[derive(Default)]
struct FactsCollector {
    daemon_marker: crate::process_scan::DaemonMarkerCache,
    spare_images: Vec<String>,
}

/// Adaptive, fail-closed host-snapshot gate. Large completion batches
/// extend the cooldown. The first scan is immediate so a short-lived
/// client cannot exit before its identity is recorded; the cooldown then
/// prevents simultaneous foreground agents from scanning on every burst.
///
/// `last_retry_scan` gates `retry_unresolved_new_processes`: the retry
/// path re-reads the host table, and without a floor it fires on every
/// 200 ms quiet-period tick while any PID is unresolved.
struct ScanBackoff {
    next_allowed: Instant,
    last_retry_scan: Instant,
}

impl ScanBackoff {
    fn new() -> Self {
        let now = Instant::now();
        Self {
            next_allowed: now,
            // Seed far enough in the past that the first
            // `claim_retry_scan` succeeds immediately — a cold
            // session must resolve its initial process state
            // without waiting for the interval.
            last_retry_scan: now - RETRY_SCAN_INTERVAL,
        }
    }

    /// Waits for the next permitted scan slot, returning whether the
    /// caller was deferred.  Deferral is deliberately not a metadata
    /// miss: a live process must be observed once its bounded cooldown
    /// expires, rather than being permanently disconnected from the
    /// reaping graph.
    fn acquire(&mut self, work_items: usize) -> bool {
        let now = Instant::now();
        let deferred = now < self.next_allowed;
        if deferred {
            std::thread::sleep(self.next_allowed - now);
        }
        let now = Instant::now();
        let multiplier = (1 + (work_items as u32 / 8)).min(SCAN_BACKOFF_MAX_MULTIPLIER);
        self.next_allowed = now + SCAN_BACKOFF_BASE * multiplier;
        deferred
    }

    /// Whether enough wall time has passed since the last retry scan to
    /// justify another full host enumeration.  Returns `true` and records
    /// the scan time when the interval has elapsed.
    fn claim_retry_scan(&mut self) -> bool {
        let now = Instant::now();
        if now - self.last_retry_scan >= RETRY_SCAN_INTERVAL {
            self.last_retry_scan = now;
            true
        } else {
            false
        }
    }
}

/// The two immutable kernel handles consumed by the listener thread. They
/// travel together so adding diagnostics does not keep growing the
/// listener's argument list past the project Clippy limit.
struct ListenerHandles {
    port: HANDLE,
    job_value: usize,
    wait: ListenerWait,
}

enum ListenerWait {
    Standard,
    #[cfg(test)]
    Instrumented {
        timeout_ms: u32,
        before_wait: Option<std::sync::mpsc::SyncSender<()>>,
    },
}

impl ListenerWait {
    fn timeout_ms(&self) -> u32 {
        match self {
            Self::Standard => 200,
            #[cfg(test)]
            Self::Instrumented { timeout_ms, .. } => *timeout_ms,
        }
    }

    fn notify_before_wait(&mut self) {
        #[cfg(test)]
        if let Self::Instrumented { before_wait, .. } = self {
            if let Some(before_wait) = before_wait.take() {
                let _ = before_wait.send(());
            }
        }
    }
}

pub struct ForegroundJobTracker {
    job: HANDLE,
    port: HANDLE,
    stop: Arc<AtomicBool>,
    backends: Arc<Mutex<Vec<RegisteredBackend>>>,
    processes: Arc<Mutex<TrackerProcesses>>,
    collector: Arc<Mutex<FactsCollector>>,
    telemetry: Arc<Mutex<Telemetry>>,
    listener: Option<JoinHandle<()>>,
}

impl ForegroundJobTracker {
    /// Installs after daemon startup. Failure is intentionally non-fatal:
    /// existing originator-tag exit cleanup remains the fallback.
    pub fn install() -> Option<Self> {
        Self::install_with_options(ListenerWait::Standard, true)
    }

    fn install_with_options(wait: ListenerWait, assign_current_process: bool) -> Option<Self> {
        unsafe {
            let job = CreateJobObjectW(None, PCWSTR::null()).ok()?;
            let port = CreateIoCompletionPort(INVALID_HANDLE_VALUE, None, 0, 1).ok()?;
            let assoc = JOBOBJECT_ASSOCIATE_COMPLETION_PORT {
                CompletionKey: job.0,
                CompletionPort: port,
            };
            if SetInformationJobObject(
                job,
                JobObjectAssociateCompletionPortInformation,
                &assoc as *const _ as *const c_void,
                size_of::<JOBOBJECT_ASSOCIATE_COMPLETION_PORT>() as u32,
            )
            .is_err()
                || (assign_current_process
                    && AssignProcessToJobObject(job, GetCurrentProcess()).is_err())
            {
                let _ = CloseHandle(port);
                let _ = CloseHandle(job);
                return None;
            }
            let stop = Arc::new(AtomicBool::new(false));
            let backends = Arc::new(Mutex::new(Vec::new()));
            let processes = Arc::new(Mutex::new(TrackerProcesses::default()));
            let collector = Arc::new(Mutex::new(FactsCollector {
                daemon_marker: crate::process_scan::DaemonMarkerCache::new(),
                spare_images: crate::job_orphan_reaper::configured_spare_images(
                    std::env::var("CLUD_REAPER_SPARE_IMAGES").ok().as_deref(),
                ),
            }));
            let telemetry = Arc::new(Mutex::new(Telemetry {
                log: session_reap_log(),
                flight_recorder: session_reap_flight_recorder(),
                ..Telemetry::default()
            }));
            let scan_backoff = Arc::new(Mutex::new(ScanBackoff::new()));
            // windows::Win32::Foundation::HANDLE intentionally does not
            // implement Send because it wraps a raw pointer. The kernel
            // handle value itself is process-wide and remains owned by
            // ForegroundJobTracker, so pass only its integer value across
            // the thread boundary and reconstruct the typed wrapper there.
            let port_value = port.0 as usize;
            let job_value = job.0 as usize;
            // Named so live diagnosis can attribute its CPU without a
            // debugger: #706 was diagnosed off a native stack precisely
            // because this thread showed up unnamed in per-thread
            // sampling, next to named peers like `clud-cpu-banner`.
            let listener = thread::Builder::new()
                .name("clud-reap-listener".into())
                .spawn({
                    let stop = Arc::clone(&stop);
                    let backends = Arc::clone(&backends);
                    let processes = Arc::clone(&processes);
                    let collector = Arc::clone(&collector);
                    let telemetry = Arc::clone(&telemetry);
                    let scan_backoff = Arc::clone(&scan_backoff);
                    move || {
                        listen(
                            ListenerHandles {
                                port: HANDLE(port_value as *mut c_void),
                                job_value,
                                wait,
                            },
                            stop,
                            backends,
                            processes,
                            collector,
                            telemetry,
                            scan_backoff,
                        )
                    }
                })
                .expect("spawn clud-reap-listener");
            Some(Self {
                job,
                port,
                stop,
                backends,
                processes,
                collector,
                telemetry,
                listener: Some(listener),
            })
        }
    }

    /// Register the exact backend process launched by the runner.
    ///
    /// The Job Object observes every descendant, but this explicit PID is
    /// the authority boundary that prevents arbitrary nested shells from
    /// being promoted to tool roots.
    pub fn register_backend(&self, pid: u32, executable_name: &str) {
        let start_time = crate::process_identity::start_time_of(pid);
        if let Ok(mut backends) = self.backends.lock() {
            backends.retain(|backend| backend.pid != pid);
            backends.push(RegisteredBackend::new(pid, executable_name, start_time));
        }

        // Registration happens immediately after spawn, but a very short
        // lived backend can still post exit notifications first. Seed the
        // root synchronously and replay every unprocessed exit so that
        // listener ordering cannot make cleanup disappear.
        if let Some(mut process) = snapshot_pid(&self.telemetry, pid) {
            process.start_time = start_time;
            if let Ok(mut processes) = self.processes.lock() {
                processes.known.entry(pid).or_insert(process);
                if start_time != crate::process_identity::UNKNOWN_START_TIME {
                    processes.unresolved_new_pids.remove(&pid);
                }
            }
        }
        reconcile_pending(
            &self.processes,
            &self.backends,
            &self.collector,
            &self.telemetry,
            self.job.0 as usize,
            false,
        );
    }

    /// Re-plan every exit the tracker gave up on, against a fresh process
    /// table, and execute whatever the plan now produces (#673 Phase 2d).
    ///
    /// Runtime abandonment is bounded by a TTL, so an exit that never
    /// resolved stops costing reconcile time — but the leak it was
    /// supposed to clean up would otherwise survive for the rest of the
    /// session, and **nothing else catches it**: the daemon's periodic
    /// sweep filters `!parent_alive`, so it only sees orphans whose
    /// originating clud is dead, while this reaper exists precisely for
    /// leaks of a *live* clud. The two sets are disjoint by construction.
    ///
    /// This is additive over `orphan_reaper::scan_and_report`, which finds
    /// descendants by originator tag: a descendant whose environment was
    /// rebuilt somewhere in the spawn chain carries no tag and is invisible
    /// to that scan, but is still linked into this graph by parent PID.
    ///
    /// Returns the number of triggers re-planned.
    pub fn sweep_abandoned_at_exit(&self) -> usize {
        let abandoned = match self.processes.lock() {
            Ok(mut processes) => processes.take_abandoned(),
            Err(_) => return 0,
        };
        if abandoned.is_empty() {
            return 0;
        }

        let registered = self
            .backends
            .lock()
            .map(|backends| backends.clone())
            .unwrap_or_default();
        let live = snapshot(&self.telemetry);

        let Ok(mut tracker) = self.processes.lock() else {
            return 0;
        };
        // Refresh liveness from the fresh table without disturbing the
        // recorded identities: a PID absent from it has exited, and a PID
        // present under a *different* identity is a recycled number that
        // must not inherit the old process's place in the tree.
        let observed_at_ms = now_ms();
        for process in tracker.known.values_mut() {
            if !live.contains_key(&process.pid) {
                // #892: stamp the eviction clock, not just the flag. Without
                // the stamp `purge` can never evict this identity, and every
                // exit noticed by a scan rather than a notification stayed in
                // `known` for the life of the session.
                crate::job_orphan_reaper::mark_exited(process, observed_at_ms);
            } else if process.alive {
                let recycled =
                    crate::process_identity::start_time_of(process.pid) != process.start_time;
                if recycled {
                    // The number is live but belongs to someone else, so the
                    // recorded identity is gone and needs the same stamp.
                    crate::job_orphan_reaper::mark_exited(process, observed_at_ms);
                }
            }
        }

        let (known, _ledger) = tracker.split();
        let graph = crate::job_orphan_reaper::ProcessGraph::build(known.values(), &registered);
        let facts = {
            let Ok(mut collector) = self.collector.lock() else {
                return 0;
            };
            collect_facts(self.job.0 as usize, &graph, &mut collector)
        };
        let spares = crate::job_orphan_reaper::build_spare_list(&facts, graph.spare_candidates());

        let mut swept = 0usize;
        let mut claimed = Vec::new();
        for (pid, start_time) in abandoned {
            let Some(trigger) = graph.meta(pid) else {
                continue;
            };
            if trigger.start_time != start_time {
                continue;
            }
            swept += 1;
            let decisions = crate::job_orphan_reaper::plan_shell_exit(&graph, &spares, pid);
            if !decisions.is_empty() {
                claimed.push(decisions);
            }
        }
        drop(graph);
        drop(tracker);

        for decisions in claimed {
            execute_decisions(decisions, &spares, &self.telemetry, ReapPhase::Exit);
        }
        swept
    }

    /// Flush the reap log and return the session's exit summary.
    ///
    /// Empty when nothing was tracked — an idle session must not print a
    /// row of zeroes. `measure` adds the Phase 0 series, which is what
    /// `--verbose` asks for.
    pub fn finish_and_report(&self, measure: bool) -> Vec<String> {
        let Ok(mut telemetry) = self.telemetry.lock() else {
            return Vec::new();
        };
        telemetry.roll_epoch();
        if let Some(log) = telemetry.log.as_mut() {
            log.flush();
        }
        let totals = telemetry.totals();
        let path = telemetry.log.as_ref().map(|log| log.path().to_path_buf());
        let mut lines = totals.summary_lines(path.as_deref()).unwrap_or_default();
        if measure && !lines.is_empty() {
            lines.push(totals.measurement_line());
        }
        lines
    }
}

/// Enumerate every PID still in the Job Object, apply the spare list,
/// explicitly terminate non-spared members, and log each decision to
/// reap.jsonl.  Only then is the job handle safe to close — `KILL_ON_JOB_CLOSE`
/// becomes a crash-only safety net instead of the primary kill mechanism.
///
/// A PID whose image name cannot be resolved is left for the kernel
/// safety net rather than risked as a wrong kill.
#[expect(
    clippy::too_many_lines,
    reason = "complexity ratchet baseline (zackees/ci.yml#229); split this function"
)]
fn explicitly_reap_job_members(
    job: HANDLE,
    processes: &Mutex<TrackerProcesses>,
    collector: &Mutex<FactsCollector>,
    telemetry: &Mutex<Telemetry>,
) {
    // ---- enumerate job member PIDs ----
    let members: Vec<u32> = unsafe {
        let mut buf: Vec<u8> = vec![0u8; 4096];
        loop {
            let info = buf.as_mut_ptr() as *mut JOBOBJECT_BASIC_PROCESS_ID_LIST;
            let len = buf.len() as u32;
            match QueryInformationJobObject(
                Some(job),
                JobObjectBasicProcessIdList,
                info as *mut _,
                len,
                None,
            ) {
                Ok(()) => {
                    let list = &*info;
                    let count = list.NumberOfProcessIdsInList as usize;
                    // ProcessIdList is `[usize; 1]` — the declared anchor of
                    // a variable-length array.  Use pointer arithmetic to
                    // reach the actual elements.
                    let ids: Vec<u32> =
                        std::slice::from_raw_parts(list.ProcessIdList.as_ptr(), count)
                            .iter()
                            .map(|pid| *pid as u32)
                            .collect();
                    break ids;
                }
                Err(e) if e.code() == windows_core::HRESULT::from_win32(234) => {
                    // ERROR_MORE_DATA — double the buffer and retry.
                    if buf.len() > 1_048_576 {
                        // 1 MiB safety ceiling: a job with >32K members
                        // is not plausible; stop rather than OOM-loop.
                        return;
                    }
                    buf.resize(buf.len() * 2, 0);
                }
                Err(_) => return,
            }
        }
    };

    if members.is_empty() {
        return;
    }

    // Never reap ourselves — we are the process that owns the job.
    let our_pid = std::process::id();
    let members: Vec<u32> = members.into_iter().filter(|&pid| pid != our_pid).collect();
    if members.is_empty() {
        return;
    }

    // ---- resolve image names: known map first, targeted read as fallback ----
    let mut pids_to_resolve: Vec<u32> = Vec::new();
    let mut image_names: HashMap<u32, String> = HashMap::new();
    if let Ok(guard) = processes.lock() {
        for &pid in &members {
            if let Some(meta) = guard.known.get(&pid) {
                image_names.insert(pid, meta.image_name.clone());
            } else {
                pids_to_resolve.push(pid);
            }
        }
    } else {
        return;
    }
    // Targeted read for any PID not in `known`.
    for pid in pids_to_resolve {
        if let Some(meta) = snapshot_pid(telemetry, pid) {
            image_names.insert(pid, meta.image_name);
        }
        // If we still cannot resolve the image name, the PID is
        // left in the job for the kernel safety net rather than
        // risked as a wrong kill.
    }

    // ---- classify: cheap image-based checks first ----
    // Absence from this map means "reap"; presence means "spare for this reason."
    let mut spared: HashMap<u32, &'static str> = HashMap::new();
    let spare_images = crate::reaper_facts::configured_spare_images_from_env();

    for (&pid, name) in &image_names {
        if crate::reaper_facts::is_docker_desktop_family_root(name) {
            spared.insert(pid, "docker_desktop_process_family");
        } else if spare_images.iter().any(|cfg| {
            crate::reaper_facts::normalized_image(cfg)
                == crate::reaper_facts::normalized_image(name)
        }) {
            spared.insert(pid, "configured_spare_list");
        }
    }

    // ---- collect host facts for remaining candidates ----
    let remaining: Vec<u32> = members
        .iter()
        .copied()
        .filter(|pid| image_names.contains_key(pid) && !spared.contains_key(pid))
        .collect();
    if !remaining.is_empty() {
        // A one-time cost at exit: collect OS facts for the unresolvable
        // candidates, then run the full spare-signal pipeline.
        // #2007: breakaway consoles can remain in this outer tracking job.
        // Preserve the same declared-daemon facts used by the live reaper.
        let identities: Vec<(u32, u64)> = remaining
            .iter()
            .map(|&pid| (pid, crate::process_identity::start_time_of(pid)))
            .collect();
        let declared_daemons = collector
            .lock()
            .map(|mut collector| collector.daemon_marker.declared_daemons_among(&identities))
            .unwrap_or_default();
        let facts =
            crate::reaper_facts::collect_host_facts(&remaining, &declared_daemons, spare_images);
        for &pid in &remaining {
            let name = image_names.get(&pid).map(String::as_str).unwrap_or("");
            if let Some(reason) = crate::reaper_facts::spare_signal(&facts, pid, name) {
                spared.insert(pid, reason.as_str());
            }
        }
    }

    // ---- act: reap the non-spared, log everything ----
    let now_ms = now_ms();
    for &pid in &members {
        let (action, reason) = match spared.get(&pid) {
            Some(reason) => (ReapAction::Spared, *reason),
            None => {
                // Explicit reap via TerminateProcess.
                unsafe {
                    if let Ok(handle) = OpenProcess(PROCESS_TERMINATE, false, pid) {
                        let _ = TerminateProcess(handle, 1);
                        let _ = CloseHandle(handle);
                    }
                }
                (ReapAction::Reaped, "job_member")
            }
        };

        let event = ReapEvent {
            ts_ms: now_ms,
            pid: Some(pid),
            parent_pid: None,
            start_time: None,
            image_name: image_names.get(&pid).cloned(),
            action,
            reason,
            phase: ReapPhase::Exit,
        };
        with_telemetry(telemetry, |t| {
            t.record(event);
            match action {
                ReapAction::Reaped => t.epoch.reaped_at_exit += 1,
                ReapAction::Spared => t.epoch.spared += 1,
                _ => {}
            }
            t.epoch.decisions_emitted += 1;
        });
    }
}

impl Drop for ForegroundJobTracker {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        // Wake the listener instead of waiting for its 200 ms poll. More
        // importantly, the matching stop check after the wait prevents a
        // shutdown-time timeout from entering one final host scan and
        // reconciliation pass. Under Windows CI process churn that pass
        // can outlive the caller's entire command timeout (#594).
        unsafe {
            let _ = PostQueuedCompletionStatus(self.port, 0, 0, None);
        }
        if let Some(listener) = self.listener.take() {
            let _ = listener.join();
        }
        // #894: enumerate, spare, and reap job members before closing the
        // tracking job. Persistent members must keep their spare facts.
        explicitly_reap_job_members(self.job, &self.processes, &self.collector, &self.telemetry);
        unsafe {
            let _ = CloseHandle(self.port);
            let _ = CloseHandle(self.job);
        }
    }
}

fn listen(
    mut handles: ListenerHandles,
    stop: Arc<AtomicBool>,
    backends: Arc<Mutex<Vec<RegisteredBackend>>>,
    processes: Arc<Mutex<TrackerProcesses>>,
    collector: Arc<Mutex<FactsCollector>>,
    telemetry: Arc<Mutex<Telemetry>>,
    scan_backoff: Arc<Mutex<ScanBackoff>>,
) {
    while !stop.load(Ordering::Acquire) {
        with_telemetry(&telemetry, |t| {
            t.epoch.ticks += 1;
            t.checkpoint();
        });
        handles.wait.notify_before_wait();
        let (mut message, mut key, mut payload) = (0u32, 0usize, null_mut());
        let wait_result = unsafe {
            GetQueuedCompletionStatus(
                handles.port,
                &mut message,
                &mut key,
                &mut payload,
                handles.wait.timeout_ms(),
            )
        };
        // `Drop` posts a wake packet after setting `stop`. Check it before
        // interpreting either that packet or a simultaneous timeout/job
        // notification, so shutdown never starts another expensive pass.
        if stop.load(Ordering::Acquire) {
            break;
        }
        if wait_result.is_err() {
            if unsafe { GetLastError().0 } == WAIT_TIMEOUT.0 {
                let metadata_complete =
                    retry_unresolved_new_processes(&processes, &telemetry, &scan_backoff);
                // The completion-port timeout *is* the quiet-period
                // detector: it fires only when no job notification arrived,
                // which is exactly the condition provisional-empty
                // finalization requires. Moving this onto an independent
                // timer would let it fire while notifications stream.
                reconcile_pending(
                    &processes,
                    &backends,
                    &collector,
                    &telemetry,
                    handles.job_value,
                    metadata_complete,
                );
                continue;
            }
            break;
        }
        let _ = key;

        // #706: fold every message already sitting in the queue into this
        // pass. The follow-up drain uses a **zero** timeout, so it only
        // collects what is already queued and adds no latency — but it
        // lets the whole batch share one host process-table read and one
        // reconcile pass.
        //
        // Before this, both were per *message*. At a measured ~178
        // process spawns/second (`bash.exe` churn from agent tool calls)
        // and ~20 ms per full `CreateToolhelp32Snapshot` enumeration, the
        // per-message scan alone was ~3.6 cores of kernel time per
        // session — and it multiplies by concurrent sessions, since every
        // scan is `O(all processes on the host)` and each session's own
        // processes inflate that count for everyone else.
        let mut batch = Vec::with_capacity(16);
        batch.push((message, payload as usize as u32));
        while batch.len() < MAX_DRAIN_BATCH {
            let (mut m, mut k, mut p) = (0u32, 0usize, null_mut());
            if unsafe { GetQueuedCompletionStatus(handles.port, &mut m, &mut k, &mut p, 0) }
                .is_err()
            {
                // Either the queue is empty (WAIT_TIMEOUT) or the port is
                // closing. Both mean "stop draining"; a closing port is
                // caught by the blocking wait at the top of the loop.
                break;
            }
            let _ = k;
            batch.push((m, p as usize as u32));
        }
        let batch_len = batch.len() as u64;
        with_telemetry(&telemetry, |t| {
            t.epoch.peak_batch = t.epoch.peak_batch.max(batch_len);
        });

        apply_batch(
            &batch,
            &processes,
            &backends,
            &collector,
            &telemetry,
            &scan_backoff,
            handles.job_value,
        );
    }
}

#[cfg(test)]
mod shutdown_tests {
    use super::*;

    #[test]
    fn dropping_idle_tracker_wakes_listener_without_poll_delay() {
        let (before_wait, entered_wait) = std::sync::mpsc::sync_channel(0);
        let tracker = ForegroundJobTracker::install_with_options(
            ListenerWait::Instrumented {
                timeout_ms: 10_000,
                before_wait: Some(before_wait),
            },
            false,
        )
        .expect("install empty foreground Job tracker");

        // This empty test Job cannot produce a notification. The
        // rendezvous occurs immediately before a deliberately long wait,
        // so a passive shutdown must pay the full timeout while an active
        // completion-port wake returns promptly.
        entered_wait
            .recv_timeout(Duration::from_secs(5))
            .expect("listener never reached its completion-port wait");

        let started = Instant::now();
        drop(tracker);
        let elapsed = started.elapsed();
        assert!(
            elapsed < Duration::from_secs(5),
            "tracker shutdown waited for the injected completion-port timeout: {elapsed:?}"
        );
    }
}

/// Apply one drained batch of completion-port messages.
///
/// Two costs that used to be paid per message are paid per batch here:
/// the host process-table enumeration (taken lazily, at most once, by
/// [`BatchTable`]) and the trailing [`reconcile_pending`] pass. Messages
/// are still applied strictly in arrival order, so an
/// `ACTIVE_PROCESS_ZERO` in the middle of a batch still resets the tracker
/// exactly where it fell.
fn apply_batch(
    batch: &[(u32, u32)],
    processes: &Arc<Mutex<TrackerProcesses>>,
    backends: &Arc<Mutex<Vec<RegisteredBackend>>>,
    collector: &Arc<Mutex<FactsCollector>>,
    telemetry: &Arc<Mutex<Telemetry>>,
    scan_backoff: &Arc<Mutex<ScanBackoff>>,
    job_value: usize,
) {
    let mut table = BatchTable::new(telemetry, scan_backoff, batch.len());

    for &(message, pid) in batch {
        match message {
            NEW_PROCESS => {
                let observed = table.observe(pid);
                if let Ok(mut processes) = processes.lock() {
                    let TrackerProcesses {
                        known,
                        unresolved_new_pids,
                        ..
                    } = &mut *processes;
                    crate::job_orphan_reaper::record_new_process_observation(
                        known,
                        unresolved_new_pids,
                        pid,
                        observed,
                    );
                }
                with_telemetry(telemetry, |t| t.epoch.tracked += 1);
            }
            EXIT_PROCESS => {
                let needs_final_observation = processes
                    .lock()
                    .map(|processes| processes.unresolved_new_pids.contains(&pid))
                    .unwrap_or(false);
                let final_observation = needs_final_observation
                    .then(|| table.observe(pid))
                    .flatten();
                let metadata_failed = if let Ok(mut processes) = processes.lock() {
                    let TrackerProcesses {
                        known,
                        unresolved_new_pids,
                        ..
                    } = &mut *processes;
                    crate::job_orphan_reaper::record_process_exit(
                        known,
                        unresolved_new_pids,
                        pid,
                        final_observation,
                        now_ms(),
                    )
                } else {
                    false
                };
                if metadata_failed {
                    record_metadata_miss(telemetry, pid);
                }
                // Only shell images become reap triggers, so only they
                // enter the `shell_exits_observed` denominator.
                let entered_backlog = processes
                    .lock()
                    .map(|processes| {
                        crate::job_orphan_reaper::pending_exit_pids(
                            &processes.known,
                            &processes.processed_exits,
                        )
                        .contains(&pid)
                    })
                    .unwrap_or(false);
                if entered_backlog {
                    with_telemetry(telemetry, |t| t.epoch.shell_exits_observed += 1);
                }
            }
            ACTIVE_PROCESS_ZERO => {
                // The job emptied: nothing tracked can still be relevant.
                // This is the *only* clear point, and it never fires while
                // the backend lives — which is why the incremental purge
                // sweep exists (#673 Phase 2a).
                if let Ok(mut processes) = processes.lock() {
                    *processes = TrackerProcesses::default();
                }
                // Accounting is per-epoch; fold it into the session totals
                // before the reset so the exit summary still adds up.
                with_telemetry(telemetry, |t| t.roll_epoch());
            }
            _ => {}
        }
    }

    // Folding rule lives in `model` so it is asserted on every platform.
    let messages = batch
        .iter()
        .map(|&(message, _)| message)
        .collect::<Vec<_>>();
    if batch_needs_reconcile(&messages) {
        // #673 Phase 1b: this path used to run an unguarded full-host
        // environment scan — 442 `ReadProcessMemory` PEB reads — on
        // *every* descendant exit, including the `git.exe`/`rg.exe`/
        // `conhost.exe` churn that can never become a reap trigger. Route
        // through the same backlog guard as every other caller: with
        // nothing pending, the pass returns before evaluating a single
        // signal.
        reconcile_pending(processes, backends, collector, telemetry, job_value, false);
    }
}

/// The host process table for one batch, read at most once and only if
/// some message in the batch actually needs it.
///
/// A batch made entirely of exits for already-resolved PIDs never touches
/// Toolhelp at all.
struct BatchTable<'a> {
    table: Option<HashMap<u32, ProcessMeta>>,
    telemetry: &'a Arc<Mutex<Telemetry>>,
    scan_backoff: &'a Arc<Mutex<ScanBackoff>>,
    work_items: usize,
}

impl<'a> BatchTable<'a> {
    fn new(
        telemetry: &'a Arc<Mutex<Telemetry>>,
        scan_backoff: &'a Arc<Mutex<ScanBackoff>>,
        work_items: usize,
    ) -> Self {
        Self {
            table: None,
            telemetry,
            scan_backoff,
            work_items,
        }
    }

    /// Observe `pid` against this batch's table, stamping the authoritative
    /// creation time from the process handle.
    ///
    /// `start_time_of` is a per-PID `OpenProcess` + `GetProcessTimes` —
    /// microseconds, and unlike the table it cannot be stale — so it stays
    /// per-call rather than being folded into the shared read.
    fn observe(&mut self, pid: u32) -> Option<ProcessMeta> {
        if self.table.is_none() {
            let deferred = self
                .scan_backoff
                .lock()
                .map(|mut gate| gate.acquire(self.work_items))
                .unwrap_or(true);
            if deferred {
                with_telemetry(self.telemetry, |t| t.epoch.host_scans_deferred += 1);
            }
        }
        let telemetry = self.telemetry;
        let table = self.table.get_or_insert_with(|| snapshot(telemetry));
        let mut process = table.get(&pid).cloned()?;
        process.start_time = crate::process_identity::start_time_of(pid);
        Some(process)
    }
}

fn retry_unresolved_new_processes(
    processes: &Mutex<TrackerProcesses>,
    telemetry: &Mutex<Telemetry>,
    scan_backoff: &Mutex<ScanBackoff>,
) -> bool {
    let unresolved_empty = processes
        .lock()
        .map(|p| p.unresolved_new_pids.is_empty())
        .unwrap_or(true);
    if unresolved_empty {
        return true;
    }

    // Rate-limit retry scans to RETRY_SCAN_INTERVAL.  Without this floor,
    // every 200 ms quiet-period tick that finds *any* unresolved PID
    // re-enumerates every process on the host — a cost that grows with
    // cumulative processes-ever-seen.  Returning the current (stale)
    // `metadata_complete` is safe: a `false` only delays provisional-empty
    // finalization; it never authorizes a kill.
    let claim = scan_backoff
        .lock()
        .map(|mut gate| gate.claim_retry_scan())
        .unwrap_or(true);
    if !claim {
        return processes
            .lock()
            .map(|p| p.metadata_complete())
            .unwrap_or(true);
    }

    let unresolved = processes
        .lock()
        .map(|processes| {
            processes
                .unresolved_new_pids
                .iter()
                .copied()
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    // Double-check: the set may have drained between the is_empty check
    // and now.
    if unresolved.is_empty() {
        return true;
    }

    let deferred = scan_backoff
        .lock()
        .map(|mut gate| gate.acquire(unresolved.len()))
        .unwrap_or(true);
    if deferred {
        with_telemetry(telemetry, |t| t.epoch.host_scans_deferred += 1);
    }

    let mut current = snapshot(telemetry);
    let observations = unresolved
        .into_iter()
        .map(|pid| {
            let mut observed = current.remove(&pid);
            if let Some(process) = observed.as_mut() {
                process.start_time = crate::process_identity::start_time_of(pid);
            }
            (pid, observed)
        })
        .collect::<Vec<_>>();

    let Ok(mut processes) = processes.lock() else {
        return false;
    };
    for (pid, observed) in observations {
        let TrackerProcesses {
            known,
            unresolved_new_pids,
            ..
        } = &mut *processes;
        crate::job_orphan_reaper::record_new_process_observation(
            known,
            unresolved_new_pids,
            pid,
            observed,
        );
    }
    // #673 Phase 2b: the finalization gate is global, so a PID whose
    // metadata never resolves used to block every unrelated pending exit
    // for the session's life. Charge this quiet period against the
    // stragglers and retire the ones that have run out of retries.
    let retired = processes.bump_unresolved_retries(now_ms());
    let complete = processes.metadata_complete();
    // Release the tracker before touching telemetry: the two locks are
    // never held together anywhere else, and taking them in one order here
    // and the other order on the completion-port path is how a deadlock
    // gets introduced.
    drop(processes);
    for pid in retired {
        record_metadata_miss(telemetry, pid);
    }
    complete
}

/// One reconcile pass over the whole pending backlog.
///
/// Everything expensive happens **once per pass**, behind the empty-backlog
/// guard, and over the reaper's own job membership rather than the host
/// (#673 Phases 1–3):
///
/// - the process graph and its role classification are built once and
///   shared across every pending PID, instead of being rebuilt from a full
///   deep clone of the process map per pending PID;
/// - the spare-list is evaluated once, over the tracked candidate set, with
///   the cheap OS gates short-circuiting before any environment read.
fn reconcile_pending(
    processes: &Mutex<TrackerProcesses>,
    backends: &Mutex<Vec<RegisteredBackend>>,
    collector: &Mutex<FactsCollector>,
    telemetry: &Mutex<Telemetry>,
    job_value: usize,
    finalize_provisional_empty: bool,
) {
    let registered = backends
        .lock()
        .map(|backends| backends.clone())
        .unwrap_or_default();

    let Ok(mut tracker) = processes.lock() else {
        return;
    };
    let now = now_ms();
    // The one purge sweep. It runs before the guard below, because a pass
    // with an empty backlog is exactly when the tracked keyspace has the
    // most to give back.
    tracker.purge(now);

    // Only *live* unresolved PIDs gate finalization: a retry can still
    // resolve them into a real nested-shell / conhost / daemon boundary.
    // PIDs that exited unresolved are gone from `known` and can never
    // reappear, so they no longer participate in the reap graph (see
    // `record_process_exit`).
    let control = crate::job_orphan_reaper::ReplayControl {
        finalize_provisional_empty,
        metadata_complete: tracker.metadata_complete(),
        now_ms: now,
    };

    let (known, mut ledger) = tracker.split();
    let pending = crate::job_orphan_reaper::pending_exit_pids(known, ledger.processed_exits);
    let known_len = known.len();
    if pending.is_empty() {
        // Never log a no-op pass. The size series is still worth having,
        // because a *shrinking* `known` on an idle session is the whole
        // point of Phase 2.
        with_telemetry(telemetry, |t| t.epoch.observe_sizes(known_len, 0));
        return;
    }
    with_telemetry(telemetry, |t| {
        t.epoch.reconcile_passes += 1;
        t.epoch.observe_sizes(known_len, pending.len());
    });

    let graph = crate::job_orphan_reaper::ProcessGraph::build(known.values(), &registered);
    let facts = {
        let Ok(mut collector) = collector.lock() else {
            return;
        };
        let facts = collect_facts(job_value, &graph, &mut collector);
        let env_reads = collector.daemon_marker.env_reads();
        with_telemetry(telemetry, |t| {
            t.epoch.env_reads = t.epoch.env_reads.max(env_reads);
        });
        facts
    };
    let spares = crate::job_orphan_reaper::build_spare_list(&facts, graph.spare_candidates());

    let mut claimed = Vec::new();
    let mut abandoned = Vec::new();
    for pid in pending {
        match crate::job_orphan_reaper::claim_exit_replay(
            &graph,
            &spares,
            &mut ledger,
            pid,
            control,
        ) {
            crate::job_orphan_reaper::ExitClaim::Execute(decisions) => claimed.push(decisions),
            crate::job_orphan_reaper::ExitClaim::Abandoned => abandoned.push((
                pid,
                graph.meta(pid).map(|meta| meta.start_time),
                graph
                    .meta(pid)
                    .map(|meta| meta.image_name.clone())
                    .unwrap_or_default(),
            )),
            crate::job_orphan_reaper::ExitClaim::Deferred => {}
        }
    }
    drop(graph);
    drop(tracker);

    with_telemetry(telemetry, |t| {
        t.epoch.finalized += claimed.len() as u64;
        t.epoch.abandoned += abandoned.len() as u64;
        for (pid, start_time, image_name) in &abandoned {
            t.record(ReapEvent {
                ts_ms: now,
                pid: Some(*pid),
                parent_pid: None,
                start_time: *start_time,
                image_name: Some(image_name.clone()),
                action: ReapAction::Abandoned,
                reason: "deferral_ttl_expired",
                phase: ReapPhase::Runtime,
            });
        }
    });

    for decisions in claimed {
        execute_decisions(decisions, &spares, telemetry, ReapPhase::Runtime);
    }
}

/// Fill a [`FactsSnapshot`] for the tracked candidate set.
///
/// Ordered cheapest-first so the expensive queries run over the smallest
/// possible set: job membership and session id are one syscall each and no
/// memory read, and every PID they rule out is a PID whose environment is
/// never touched.
fn collect_facts(
    job_value: usize,
    graph: &crate::job_orphan_reaper::ProcessGraph<'_>,
    collector: &mut FactsCollector,
) -> FactsSnapshot {
    let mut snapshot = FactsSnapshot {
        spare_images: collector.spare_images.clone(),
        // Windows has no POSIX session leader; say so rather than
        // answering "no".
        unavailable: HashSet::from([Signal::SessionLeader]),
        ..FactsSnapshot::default()
    };

    let candidates: Vec<(u32, u64)> = graph
        .spare_candidates()
        .map(|(pid, _)| pid)
        .filter_map(|pid| graph.meta(pid).map(|meta| (pid, meta.start_time)))
        .collect();

    let job = HANDLE(job_value as *mut c_void);
    for &(pid, _) in &candidates {
        match win32::process_in_job(job, pid) {
            Some(false) => {
                snapshot.outside_job.insert(pid);
                continue;
            }
            None => {
                // The handle could not be opened at all, which on Windows
                // is overwhelmingly an access-denied on a higher-privilege
                // process. A kill would fail anyway.
                snapshot.foreign_owner.insert(pid);
                continue;
            }
            Some(true) => {}
        }
        if win32::session_id(pid) == Some(0) {
            snapshot.service_session.insert(pid);
        }
    }

    // Only PIDs that survived the free gates are worth an environment read
    // or a table lookup.
    let survivors: Vec<(u32, u64)> = candidates
        .iter()
        .copied()
        .filter(|(pid, _)| {
            !snapshot.outside_job.contains(pid)
                && !snapshot.foreign_owner.contains(pid)
                && !snapshot.service_session.contains(pid)
        })
        .collect();
    if survivors.is_empty() {
        return snapshot;
    }

    snapshot.declared_daemons = collector.daemon_marker.declared_daemons_among(&survivors);

    let listening = win32::listening_pids();
    snapshot.listening = survivors
        .iter()
        .map(|(pid, _)| *pid)
        .filter(|pid| listening.contains(pid))
        .collect();
    snapshot.active_desktop_shells = win32::active_desktop_shell_pid()
        .filter(|pid| survivors.iter().any(|(candidate, _)| candidate == pid))
        .into_iter()
        .collect();

    snapshot
}

/// The Win32 half of the [`crate::job_orphan_reaper::ProcessFacts`] signals.
///
/// Deliberately the *only* place these syscalls appear: every reap decision
/// consumes the resulting [`FactsSnapshot`] as data, which is what makes
/// the decision table unit-testable on every platform (#674).
mod win32 {
    use std::collections::HashSet;

    use windows::Win32::Foundation::{CloseHandle, HANDLE};
    use windows::Win32::NetworkManagement::IpHelper::{
        GetExtendedTcpTable, MIB_TCPTABLE_OWNER_PID, TCP_TABLE_OWNER_PID_LISTENER,
    };
    use windows::Win32::Networking::WinSock::AF_INET;
    use windows::Win32::System::JobObjects::IsProcessInJob;
    use windows::Win32::System::RemoteDesktop::ProcessIdToSessionId;
    use windows::Win32::System::Threading::{
        OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION, PROCESS_TERMINATE,
    };
    use windows::Win32::UI::WindowsAndMessaging::{GetShellWindow, GetWindowThreadProcessId};

    /// `Some(false)` = broke away from our job. `None` = the process could
    /// not be opened for the query at all.
    pub(super) fn process_in_job(job: HANDLE, pid: u32) -> Option<bool> {
        unsafe {
            let handle = OpenProcess(
                PROCESS_QUERY_LIMITED_INFORMATION | PROCESS_TERMINATE,
                false,
                pid,
            )
            .ok()?;
            let mut in_job = windows_core::BOOL(0);
            let queried = IsProcessInJob(handle, Some(job), &mut in_job).is_ok();
            let _ = CloseHandle(handle);
            queried.then(|| in_job.as_bool())
        }
    }

    /// Windows terminal-services session id. `0` is the services session.
    pub(super) fn session_id(pid: u32) -> Option<u32> {
        unsafe {
            let mut session = 0u32;
            ProcessIdToSessionId(pid, &mut session)
                .is_ok()
                .then_some(session)
        }
    }

    /// PID owning the active Windows shell window, if Explorer has created
    /// one. This is an OS-authoritative shell identity, not a GUI heuristic.
    pub(super) fn active_desktop_shell_pid() -> Option<u32> {
        unsafe {
            let window = GetShellWindow();
            if window.0.is_null() {
                return None;
            }
            let mut pid = 0;
            GetWindowThreadProcessId(window, Some(&mut pid));
            (pid != 0).then_some(pid)
        }
    }

    /// PIDs owning an IPv4 TCP listener.
    ///
    /// One syscall for the whole table, not one per PID — this is the
    /// signal that catches a build-cache or language server that neither
    /// requested job breakaway nor set the cooperative daemon marker.
    pub(super) fn listening_pids() -> HashSet<u32> {
        unsafe {
            let mut size = 0u32;
            // First call sizes the buffer; ERROR_INSUFFICIENT_BUFFER is the
            // documented success path here.
            let _ = GetExtendedTcpTable(
                None,
                &mut size,
                false,
                AF_INET.0 as u32,
                TCP_TABLE_OWNER_PID_LISTENER,
                0,
            );
            if size == 0 {
                return HashSet::new();
            }
            let mut buffer = vec![0u8; size as usize];
            if GetExtendedTcpTable(
                Some(buffer.as_mut_ptr().cast()),
                &mut size,
                false,
                AF_INET.0 as u32,
                TCP_TABLE_OWNER_PID_LISTENER,
                0,
            ) != 0
            {
                return HashSet::new();
            }
            let table = &*(buffer.as_ptr() as *const MIB_TCPTABLE_OWNER_PID);
            let rows =
                std::slice::from_raw_parts(table.table.as_ptr(), table.dwNumEntries as usize);
            rows.iter().map(|row| row.dwOwningPid).collect()
        }
    }
}

fn execute_decisions(
    decisions: Vec<ReapDecision>,
    spares: &crate::job_orphan_reaper::SpareList,
    telemetry: &Mutex<Telemetry>,
    phase: ReapPhase,
) {
    if decisions.is_empty() {
        return;
    }

    // The tree walk prunes at every spared PID, so both the plan's own
    // spare decisions and the whole OS-derived spare-list must be visible
    // to it — sparing a daemon while killing its children would leave it
    // wedged mid-work.
    let spared: HashSet<u32> = decisions
        .iter()
        .filter(|decision| decision.action == DecisionAction::Spare)
        .filter_map(|decision| decision.candidate_pid)
        .chain(spares.keys().copied())
        .collect();

    let ts_ms = now_ms();
    for mut decision in decisions {
        if decision.action == DecisionAction::Reap
            && !crate::job_orphan_reaper::candidate_identity_is_live(&decision)
        {
            // A downgrade is still a decision, and it counts as a spare in
            // the `decisions_emitted == reaped + spared` identity.
            decision.action = DecisionAction::Spare;
            decision.reason =
                crate::job_orphan_reaper::ReapDecisionReason::CandidateIdentityChanged;
            log_decision(&decision);
            census(telemetry, &decision, phase, ts_ms);
            continue;
        }

        log_decision(&decision);
        census(telemetry, &decision, phase, ts_ms);
        if decision.action != DecisionAction::Reap {
            continue;
        }
        let identity = crate::job_orphan_reaper::candidate_identity(&decision)
            .expect("validated reap decision identity");
        crate::process_tree::kill_tree_filtered_automatic(identity, &|pid| !spared.contains(&pid));
    }
}

/// Count one decision and write its line.
///
/// `Handoff` is neither a kill nor a protection, so it is logged but does
/// not enter the `decisions_emitted == reaped + spared` identity — the
/// inner shell is still running and will produce its own decision when it
/// exits.
fn census(telemetry: &Mutex<Telemetry>, decision: &ReapDecision, phase: ReapPhase, ts_ms: u64) {
    let action = match decision.action {
        DecisionAction::Reap => ReapAction::Reaped,
        DecisionAction::Spare => ReapAction::Spared,
        DecisionAction::Handoff => return,
    };
    with_telemetry(telemetry, |t| {
        t.epoch.decisions_emitted += 1;
        match (action, phase) {
            (ReapAction::Reaped, ReapPhase::Runtime) => t.epoch.reaped_runtime += 1,
            (ReapAction::Reaped, ReapPhase::Exit) => t.epoch.reaped_at_exit += 1,
            _ => t.epoch.spared += 1,
        }
        t.record(ReapEvent {
            ts_ms,
            pid: decision.candidate_pid,
            parent_pid: decision.candidate_parent_pid,
            start_time: decision.candidate_start_time,
            image_name: decision.candidate_image.clone(),
            action,
            reason: decision.reason_str(),
            phase,
        });
    });
}

/// This session's reap log, or `None` when there is no state dir to write
/// into (CI / minimal containers). Reaping still happens; it is just quiet.
fn session_reap_log() -> Option<ReapLog> {
    let state_dir = crate::daemon::default_state_dir().ok()?;
    Some(ReapLog::new(crate::reap_log::session_reap_log_path(
        &state_dir,
        std::process::id(),
        crate::process_identity::self_start_time(),
    )))
}

/// A fixed, durable checkpoint complements the buffered event log. It is
/// intentionally separate so a watchdog reset leaves the last counters
/// available even though `ReapLog::Drop` never gets a chance to flush.
fn session_reap_flight_recorder() -> Option<crate::reap_log::ReapFlightRecorder> {
    let state_dir = crate::daemon::default_state_dir().ok()?;
    Some(crate::reap_log::ReapFlightRecorder::new(
        crate::reap_log::session_reap_health_path(
            &state_dir,
            std::process::id(),
            crate::process_identity::self_start_time(),
        ),
    ))
}

fn log_decision(decision: &ReapDecision) {
    let Ok(state_dir) = crate::daemon::default_state_dir() else {
        return;
    };
    crate::daemon::log_structured_event(
        &state_dir,
        "foreground_tool_shell_decision",
        crate::job_orphan_reaper::decision_log_fields(decision),
    );
}

/// A job notification arrived for a PID Toolhelp could not resolve before
/// it exited. The path fails closed and spares it; this is the record.
///
/// #689: this used to be one **synchronous** `log_structured_event` per
/// miss into the shared, rotating `daemon-events.jsonl`. Under the
/// process-churn workloads that make the reaper interesting that ran at
/// ~300 writes/min — 98.8% of the log — and evicted every other producer's
/// events, including the daemon's own orphan-sweep status, before anyone
/// could read them. It also reintroduced exactly the per-op synchronous
/// JSONL flush that #544 measured as an idle-CPU cost and that #673
/// principle 7 forbids; Phase 5 honoured that for `reap.jsonl` and this
/// path predates it.
///
/// Per-PID fidelity now goes to *this session's* buffered log, and the
/// aggregate reaches the exit summary through
/// [`ReapCounters::metadata_misses`] — the diagnostic is relocated and
/// summarized, not deleted.
fn record_metadata_miss(telemetry: &Mutex<Telemetry>, pid: u32) {
    let ts_ms = now_ms();
    with_telemetry(telemetry, |t| {
        t.epoch.metadata_misses += 1;
        t.record(ReapEvent {
            ts_ms,
            pid: Some(pid),
            parent_pid: None,
            start_time: None,
            image_name: None,
            // Fails closed: an unresolvable process is never a kill target.
            action: ReapAction::Spared,
            reason: "process_metadata_unavailable",
            phase: ReapPhase::Runtime,
        });
    });
}

/// One full host process-table enumeration.
///
/// **Every** call is counted into [`ReapCounters::host_scans`], from here
/// rather than from the call sites. #707 instrumented only the batch path,
/// which left three other callers — `register_backend`,
/// `sweep_abandoned_at_exit` and `retry_unresolved_new_processes` —
/// invisible. The last of those runs on the 200 ms quiet-period timeout
/// whenever any PID is still unresolved, so the counter that #706 exists
/// to drive down was under-reporting exactly the recurring caller a reader
/// would want to see.
/// Read one PID's metadata without materialising the whole host table.
///
/// #687 lists this call site first: the registration path wanted a single
/// PID and got it by building a `HashMap` of every process on the host and
/// then throwing all but one entry away. The Toolhelp32 walk itself is
/// unavoidable here — `parent_pid` has no cheap per-PID Win32 answer,
/// which is exactly why #687 wants a daemon-owned service and why
/// `tests/integration/diagnostics/tier_refresh_probe.rs` concluded a "targeted" sysinfo refresh
/// saves nothing on Windows — but the per-process `String` allocation and
/// map insert are pure waste when one entry is wanted. On a 500-process
/// host that is 500 UTF-16 decodes traded for at most one.
///
/// Counted as a host scan like `snapshot`, because it is one: the saving
/// is allocation, not the enumeration. Keeping the telemetry honest
/// matters more than making the number look better.
fn snapshot_pid(telemetry: &Mutex<Telemetry>, wanted: u32) -> Option<ProcessMeta> {
    with_telemetry(telemetry, |t| t.epoch.host_scans += 1);
    unsafe {
        let handle = CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0).ok()?;
        let mut entry: PROCESSENTRY32W = zeroed();
        entry.dwSize = size_of::<PROCESSENTRY32W>() as u32;
        let mut found = None;
        if Process32FirstW(handle, &mut entry).is_ok() {
            loop {
                if entry.th32ProcessID == wanted {
                    let end = entry
                        .szExeFile
                        .iter()
                        .position(|c| *c == 0)
                        .unwrap_or(entry.szExeFile.len());
                    found = Some(ProcessMeta {
                        pid: entry.th32ProcessID,
                        parent_pid: entry.th32ParentProcessID,
                        image_name: String::from_utf16_lossy(&entry.szExeFile[..end]),
                        alive: true,
                        start_time: crate::process_identity::UNKNOWN_START_TIME,
                        exited_at_ms: None,
                    });
                    break;
                }
                if Process32NextW(handle, &mut entry).is_err() {
                    break;
                }
            }
        }
        let _ = CloseHandle(handle);
        found
    }
}

fn snapshot(telemetry: &Mutex<Telemetry>) -> HashMap<u32, ProcessMeta> {
    with_telemetry(telemetry, |t| t.epoch.host_scans += 1);
    let mut out = HashMap::new();
    unsafe {
        let Ok(handle) = CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0) else {
            return out;
        };
        let mut entry: PROCESSENTRY32W = zeroed();
        entry.dwSize = size_of::<PROCESSENTRY32W>() as u32;
        if Process32FirstW(handle, &mut entry).is_ok() {
            loop {
                let end = entry
                    .szExeFile
                    .iter()
                    .position(|c| *c == 0)
                    .unwrap_or(entry.szExeFile.len());
                out.insert(
                    entry.th32ProcessID,
                    ProcessMeta {
                        pid: entry.th32ProcessID,
                        parent_pid: entry.th32ParentProcessID,
                        image_name: String::from_utf16_lossy(&entry.szExeFile[..end]),
                        alive: true,
                        start_time: crate::process_identity::UNKNOWN_START_TIME,
                        exited_at_ms: None,
                    },
                );
                if Process32NextW(handle, &mut entry).is_err() {
                    break;
                }
            }
        }
        let _ = CloseHandle(handle);
        out
    }
}

/// #687: the targeted read must agree with the full-table read.
///
/// The saving is allocation, not enumeration, so the only thing that can
/// regress is correctness — the targeted walk skipping the entry the
/// full walk would have found, or reporting different fields for it.
/// Asserting equivalence against `snapshot` pins exactly that.
#[cfg(test)]
mod snapshot_pid_tests {
    use super::{snapshot, snapshot_pid, Telemetry};
    use std::sync::Mutex;

    #[test]
    fn targeted_read_matches_the_full_table_for_self() {
        let telemetry = Mutex::new(Telemetry::default());
        let me = std::process::id();

        let targeted = snapshot_pid(&telemetry, me).expect("self must be in the process table");
        let full = snapshot(&telemetry);
        let expected = full.get(&me).expect("self must be in the full table");

        assert_eq!(targeted.pid, me);
        assert_eq!(targeted.pid, expected.pid);
        assert_eq!(targeted.parent_pid, expected.parent_pid);
        assert_eq!(targeted.image_name, expected.image_name);
        assert!(targeted.alive);
        assert!(!targeted.image_name.is_empty());
    }

    #[test]
    fn targeted_read_reports_a_missing_pid_as_none() {
        let telemetry = Mutex::new(Telemetry::default());
        // Windows PIDs are multiples of four and far below this; a value
        // the full walk cannot contain must come back as None rather than
        // as a defaulted row.
        assert!(snapshot_pid(&telemetry, u32::MAX - 1).is_none());
    }

    #[test]
    fn targeted_read_is_counted_as_a_host_scan() {
        let telemetry = Mutex::new(Telemetry::default());
        let _ = snapshot_pid(&telemetry, std::process::id());
        assert_eq!(
            telemetry.lock().expect("telemetry").epoch.host_scans,
            1,
            "the walk still happens, so it must still be counted"
        );
    }
}

#[cfg(test)]
mod scan_backoff_tests {
    use std::time::{Duration, Instant};

    use super::ScanBackoff;

    #[test]
    fn cooldown_defers_an_immediate_second_host_scan() {
        let now = Instant::now();
        let mut backoff = ScanBackoff {
            // Start at the first permitted slot so this is a deterministic
            // test of the post-scan cooldown.
            next_allowed: now,
            last_retry_scan: now,
        };
        assert!(!backoff.acquire(1));
        assert!(backoff.acquire(1));
    }

    #[test]
    fn retry_scan_is_gated_by_interval() {
        // Seed in the past so the first claim succeeds (cold start).
        let past = Instant::now() - super::RETRY_SCAN_INTERVAL;
        let mut backoff = ScanBackoff {
            next_allowed: past,
            last_retry_scan: past,
        };
        // First claim succeeds — seeded in the past.
        assert!(backoff.claim_retry_scan());
        // Immediate second claim is denied — interval hasn't elapsed.
        assert!(!backoff.claim_retry_scan());
    }

    #[test]
    fn retry_scan_is_granted_after_interval_elapses() {
        // Start far enough in the past that the interval has elapsed.
        let past = Instant::now() - super::RETRY_SCAN_INTERVAL - Duration::from_millis(1);
        let mut backoff = ScanBackoff {
            next_allowed: past,
            last_retry_scan: past,
        };
        assert!(backoff.claim_retry_scan());
    }
}
