use std::collections::{BTreeSet, HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use sysinfo::{Pid, ProcessRefreshKind, ProcessesToUpdate, System};

use super::io_helpers::read_json_file;
use super::paths::sessions_dir;
use super::types::{
    unix_millis_now, ProcRow, ProcTier, ProcTreeSnapshot, ProcTreeSummary, SessionSnapshot,
};

pub(super) const DEFAULT_SAMPLE_INTERVAL_MS: u64 = 2_000;
const ORIGINATOR_SCAN_INTERVAL_MS: u64 = 30_000;
const DEAD_ROW_RETENTION_MS: u64 = 60_000;
const MAX_TRACKED_PIDS: usize = 5_000;
const EWMA_ALPHA: f32 = 0.3;

#[derive(Clone)]
pub(super) struct ProcSamplerHandle {
    snapshot: Arc<Mutex<ProcTreeSnapshot>>,
}

impl ProcSamplerHandle {
    pub(super) fn empty(interval_ms: u64) -> Self {
        Self {
            snapshot: Arc::new(Mutex::new(ProcTreeSnapshot::empty(interval_ms))),
        }
    }

    pub(super) fn snapshot(&self, include_dead_since_ms: u64) -> ProcTreeSnapshot {
        let mut snapshot = self
            .snapshot
            .lock()
            .expect("proc sampler snapshot mutex poisoned")
            .clone();
        snapshot.refresh_age();
        if include_dead_since_ms == 0 {
            snapshot.rows.retain(|row| row.live);
        } else {
            let now = unix_millis_now();
            snapshot.rows.retain(|row| {
                row.live
                    || row
                        .exited_at_ms
                        .map(|exited| now.saturating_sub(exited) <= include_dead_since_ms)
                        .unwrap_or(false)
            });
        }
        snapshot.recompute_summary();
        snapshot
    }
}

pub(super) fn spawn_proc_sampler(
    state_dir: PathBuf,
    shutdown_requested: Arc<AtomicBool>,
) -> ProcSamplerHandle {
    let interval_ms = configured_interval_ms();
    let handle = ProcSamplerHandle::empty(interval_ms);
    let snapshot = Arc::clone(&handle.snapshot);
    let _ = thread::Builder::new()
        .name("clud-proc-sampler".to_string())
        .spawn(move || {
            let mut sampler = ProcSampler::new(state_dir, interval_ms);
            while !shutdown_requested.load(Ordering::SeqCst) {
                let next = sampler.sample();
                *snapshot
                    .lock()
                    .expect("proc sampler snapshot mutex poisoned") = next;
                sleep_until_next_tick(interval_ms, &shutdown_requested);
            }
        });
    handle
}

fn configured_interval_ms() -> u64 {
    let Ok(document) = crate::clud_settings::load_or_init_global_settings() else {
        return DEFAULT_SAMPLE_INTERVAL_MS;
    };
    document
        .pointer("/daemon/proc_sampler/interval_ms")
        .and_then(serde_json::Value::as_u64)
        .filter(|value| (250..=60_000).contains(value))
        .unwrap_or(DEFAULT_SAMPLE_INTERVAL_MS)
}

fn sleep_until_next_tick(interval_ms: u64, shutdown_requested: &AtomicBool) {
    let deadline = Instant::now() + Duration::from_millis(interval_ms);
    while Instant::now() < deadline {
        if shutdown_requested.load(Ordering::SeqCst) {
            return;
        }
        let remaining = deadline.saturating_duration_since(Instant::now());
        thread::sleep(remaining.min(Duration::from_millis(100)));
    }
}

struct ProcSampler {
    state_dir: PathBuf,
    system: System,
    ewma_by_pid: HashMap<u32, f32>,
    last_live_rows: HashMap<u32, ProcRow>,
    dead_rows: HashMap<u32, ProcRow>,
    originator_cache: HashMap<u32, OriginatorTag>,
    last_originator_scan: Option<Instant>,
    interval_ms: u64,
}

impl ProcSampler {
    fn new(state_dir: PathBuf, interval_ms: u64) -> Self {
        Self {
            state_dir,
            system: System::new(),
            ewma_by_pid: HashMap::new(),
            last_live_rows: HashMap::new(),
            dead_rows: HashMap::new(),
            originator_cache: HashMap::new(),
            last_originator_scan: None,
            interval_ms,
        }
    }

    fn sample(&mut self) -> ProcTreeSnapshot {
        self.system.refresh_processes_specifics(
            ProcessesToUpdate::All,
            true,
            ProcessRefreshKind::nothing()
                .with_cpu()
                .with_memory()
                .without_tasks(),
        );
        self.refresh_originator_cache_if_due();

        let now_ms = unix_millis_now();
        let parent_by_pid = parent_map(&self.system);
        let children_by_parent = children_map(&self.system);
        let sessions = SessionIndex::from_state(&self.state_dir, &self.system);
        let included = included_pids(
            &self.system,
            &children_by_parent,
            &self.originator_cache,
            &sessions,
        );

        let mut rows = Vec::new();
        let mut live_rows = HashMap::new();
        for pid_u32 in included.into_iter().take(MAX_TRACKED_PIDS) {
            let pid = Pid::from_u32(pid_u32);
            let Some(process) = self.system.process(pid) else {
                continue;
            };
            let Some(assignment) =
                resolve_assignment(pid_u32, &parent_by_pid, &self.originator_cache, &sessions)
            else {
                continue;
            };
            let cpu_pct = process.cpu_usage().max(0.0);
            let cpu_ewma_pct = ewma(self.ewma_by_pid.get(&pid_u32).copied(), cpu_pct);
            self.ewma_by_pid.insert(pid_u32, cpu_ewma_pct);
            let rss_bytes = process.memory();
            let command = self
                .originator_cache
                .get(&pid_u32)
                .map(|tag| tag.command.clone())
                .filter(|command| !command.is_empty())
                .unwrap_or_else(|| command_from_process(process));
            let row = ProcRow {
                pid: pid_u32,
                ppid: process.parent().map(Pid::as_u32),
                originator: assignment.originator,
                originator_pid: assignment.originator_pid,
                session_id: assignment.session_id,
                session_name: assignment.session_name,
                cpu_pct,
                cpu_ewma_pct,
                rss_bytes,
                age_secs: process.run_time(),
                command,
                depth: depth_from_originator(pid_u32, assignment.originator_pid, &parent_by_pid),
                tier: tier_for(cpu_pct, cpu_ewma_pct, rss_bytes),
                live: true,
                exited_at_ms: None,
            };
            live_rows.insert(pid_u32, row.clone());
            rows.push(row);
        }

        self.record_dead_rows(now_ms, &live_rows);
        rows.extend(self.dead_rows.values().cloned());
        rows.sort_by(|left, right| {
            left.originator
                .cmp(&right.originator)
                .then(left.depth.cmp(&right.depth))
                .then(left.pid.cmp(&right.pid))
        });
        self.last_live_rows = live_rows;

        let mut snapshot = ProcTreeSnapshot {
            schema_version: 1,
            sampled_at_ms: now_ms,
            sample_age_ms: 0,
            sampler_pid: std::process::id(),
            interval_ms: self.interval_ms,
            rows,
            summary: ProcTreeSummary::default(),
        };
        snapshot.recompute_summary();
        snapshot
    }

    fn refresh_originator_cache_if_due(&mut self) {
        let due = self
            .last_originator_scan
            .map(|last| last.elapsed() >= Duration::from_millis(ORIGINATOR_SCAN_INTERVAL_MS))
            .unwrap_or(true);
        if !due {
            return;
        }
        self.last_originator_scan = Some(Instant::now());
        self.originator_cache = running_process::originator::find_processes_by_originator("CLUD")
            .into_iter()
            .map(|process| {
                (
                    process.pid,
                    OriginatorTag {
                        originator: process.originator,
                        originator_pid: process.parent_pid,
                        command: process.command,
                    },
                )
            })
            .collect();
    }

    fn record_dead_rows(&mut self, now_ms: u64, live_rows: &HashMap<u32, ProcRow>) {
        for (pid, row) in &self.last_live_rows {
            if live_rows.contains_key(pid) {
                continue;
            }
            let mut dead = row.clone();
            dead.live = false;
            dead.exited_at_ms = Some(now_ms);
            dead.cpu_pct = 0.0;
            dead.cpu_ewma_pct = ewma(self.ewma_by_pid.get(pid).copied(), 0.0);
            dead.tier = ProcTier::Frozen;
            self.dead_rows.insert(*pid, dead);
        }

        for pid in live_rows.keys() {
            self.dead_rows.remove(pid);
        }
        self.dead_rows.retain(|_, row| {
            row.exited_at_ms
                .map(|exited| now_ms.saturating_sub(exited) <= DEAD_ROW_RETENTION_MS)
                .unwrap_or(false)
        });
        self.ewma_by_pid
            .retain(|pid, _| live_rows.contains_key(pid) || self.dead_rows.contains_key(pid));
    }
}

#[derive(Debug, Clone)]
struct OriginatorTag {
    originator: String,
    originator_pid: u32,
    command: String,
}

#[derive(Debug, Clone)]
struct Assignment {
    originator: String,
    originator_pid: Option<u32>,
    session_id: Option<String>,
    session_name: Option<String>,
}

#[derive(Debug, Clone)]
struct SessionRoot {
    id: String,
    name: Option<String>,
    worker_pid: u32,
    root_pid: Option<u32>,
}

#[derive(Debug, Default)]
struct SessionIndex {
    sessions: Vec<SessionRoot>,
    root_to_session: HashMap<u32, usize>,
}

impl SessionIndex {
    fn from_state(state_dir: &Path, system: &System) -> Self {
        let Ok(entries) = std::fs::read_dir(sessions_dir(state_dir)) else {
            return Self::default();
        };
        let mut sessions = Vec::new();
        let mut root_to_session = HashMap::new();
        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().and_then(|ext| ext.to_str()) != Some("json") {
                continue;
            }
            let Ok(session) = read_json_file::<SessionSnapshot>(&path) else {
                continue;
            };
            if session.exit_code.is_some() {
                continue;
            }
            if system.process(Pid::from_u32(session.worker_pid)).is_none() {
                continue;
            }
            if let Some(root_pid) = session.root_pid {
                if system.process(Pid::from_u32(root_pid)).is_none() {
                    continue;
                }
            }
            let index = sessions.len();
            root_to_session.insert(session.worker_pid, index);
            if let Some(root_pid) = session.root_pid {
                root_to_session.insert(root_pid, index);
            }
            sessions.push(SessionRoot {
                id: session.id,
                name: session.name,
                worker_pid: session.worker_pid,
                root_pid: session.root_pid,
            });
        }
        Self {
            sessions,
            root_to_session,
        }
    }

    fn session_for_chain(
        &self,
        pid: u32,
        parent_by_pid: &HashMap<u32, u32>,
    ) -> Option<&SessionRoot> {
        let mut current = Some(pid);
        let mut seen = HashSet::new();
        while let Some(cur) = current {
            if !seen.insert(cur) {
                return None;
            }
            if let Some(index) = self.root_to_session.get(&cur) {
                return self.sessions.get(*index);
            }
            current = parent_by_pid.get(&cur).copied();
        }
        None
    }
}

fn included_pids(
    system: &System,
    children_by_parent: &HashMap<u32, Vec<u32>>,
    originator_cache: &HashMap<u32, OriginatorTag>,
    sessions: &SessionIndex,
) -> BTreeSet<u32> {
    let mut included = BTreeSet::new();
    // #571: roots must include every live clud process, not merely backend
    // processes that happened to inherit RUNNING_PROCESS_ORIGINATOR.
    for (pid, process) in system.processes() {
        if is_clud_process(process) {
            include_subtree(pid.as_u32(), children_by_parent, &mut included);
        }
    }
    for pid in originator_cache.keys().copied() {
        if system.process(Pid::from_u32(pid)).is_some() {
            include_subtree(pid, children_by_parent, &mut included);
        }
    }
    for session in &sessions.sessions {
        include_subtree(session.worker_pid, children_by_parent, &mut included);
        if let Some(root_pid) = session.root_pid {
            include_subtree(root_pid, children_by_parent, &mut included);
        }
    }
    included
}

fn is_clud_process(process: &sysinfo::Process) -> bool {
    matches!(
        process.name().to_string_lossy().to_ascii_lowercase().as_str(),
        "clud" | "clud.exe"
    )
}

fn include_subtree(
    root: u32,
    children_by_parent: &HashMap<u32, Vec<u32>>,
    out: &mut BTreeSet<u32>,
) {
    let mut stack = vec![root];
    while let Some(pid) = stack.pop() {
        if !out.insert(pid) {
            continue;
        }
        if let Some(children) = children_by_parent.get(&pid) {
            stack.extend(children.iter().copied());
        }
    }
}

fn resolve_assignment(
    pid: u32,
    parent_by_pid: &HashMap<u32, u32>,
    originator_cache: &HashMap<u32, OriginatorTag>,
    sessions: &SessionIndex,
) -> Option<Assignment> {
    if let Some(tag) = tag_for_chain(pid, parent_by_pid, originator_cache) {
        let session = sessions.session_for_chain(pid, parent_by_pid).or_else(|| {
            sessions
                .sessions
                .iter()
                .find(|session| session.worker_pid == tag.originator_pid)
        });
        return Some(Assignment {
            originator: tag.originator.clone(),
            originator_pid: Some(tag.originator_pid),
            session_id: session.map(|session| session.id.clone()),
            session_name: session.and_then(|session| session.name.clone()),
        });
    }
    let session = sessions.session_for_chain(pid, parent_by_pid)?;
    Some(Assignment {
        originator: format!("CLUD:{}", session.worker_pid),
        originator_pid: Some(session.worker_pid),
        session_id: Some(session.id.clone()),
        session_name: session.name.clone(),
    })
}

fn tag_for_chain<'a>(
    pid: u32,
    parent_by_pid: &HashMap<u32, u32>,
    originator_cache: &'a HashMap<u32, OriginatorTag>,
) -> Option<&'a OriginatorTag> {
    let mut current = Some(pid);
    let mut seen = HashSet::new();
    while let Some(cur) = current {
        if !seen.insert(cur) {
            return None;
        }
        if let Some(tag) = originator_cache.get(&cur) {
            return Some(tag);
        }
        current = parent_by_pid.get(&cur).copied();
    }
    None
}

fn parent_map(system: &System) -> HashMap<u32, u32> {
    system
        .processes()
        .iter()
        .filter_map(|(pid, process)| {
            process
                .parent()
                .map(|parent| (pid.as_u32(), parent.as_u32()))
        })
        .collect()
}

fn children_map(system: &System) -> HashMap<u32, Vec<u32>> {
    let mut children: HashMap<u32, Vec<u32>> = HashMap::new();
    for (pid, process) in system.processes() {
        if let Some(parent) = process.parent() {
            children
                .entry(parent.as_u32())
                .or_default()
                .push(pid.as_u32());
        }
    }
    children
}

fn depth_from_originator(
    pid: u32,
    originator_pid: Option<u32>,
    parent_by_pid: &HashMap<u32, u32>,
) -> u32 {
    let Some(originator_pid) = originator_pid else {
        return 0;
    };
    let mut depth = 0_u32;
    let mut current = Some(pid);
    let mut seen = HashSet::new();
    while let Some(cur) = current {
        if !seen.insert(cur) {
            return depth;
        }
        if cur == originator_pid {
            return depth;
        }
        current = parent_by_pid.get(&cur).copied();
        if current.is_some() {
            depth = depth.saturating_add(1);
        }
        if depth > 128 {
            return depth;
        }
    }
    0
}

fn command_from_process(process: &sysinfo::Process) -> String {
    let parts: Vec<String> = process
        .cmd()
        .iter()
        .map(|part| part.to_string_lossy().into_owned())
        .filter(|part| !part.is_empty())
        .collect();
    if parts.is_empty() {
        process.name().to_string_lossy().into_owned()
    } else {
        parts.join(" ")
    }
}

fn ewma(previous: Option<f32>, sample: f32) -> f32 {
    match previous {
        Some(prev) => (EWMA_ALPHA * sample) + ((1.0 - EWMA_ALPHA) * prev),
        None => sample,
    }
}

fn tier_for(cpu_pct: f32, cpu_ewma_pct: f32, rss_bytes: u64) -> ProcTier {
    const WARM_RSS_BYTES: u64 = 100 * 1024 * 1024;
    if cpu_pct >= 5.0 || cpu_ewma_pct >= 5.0 {
        ProcTier::Hot
    } else if cpu_pct >= 0.5 || cpu_ewma_pct >= 0.5 || rss_bytes >= WARM_RSS_BYTES {
        ProcTier::Warm
    } else {
        ProcTier::Cold
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn session_index(session: SessionRoot) -> SessionIndex {
        let mut index = SessionIndex::default();
        index.root_to_session.insert(session.worker_pid, 0);
        if let Some(root_pid) = session.root_pid {
            index.root_to_session.insert(root_pid, 0);
        }
        index.sessions.push(session);
        index
    }

    #[test]
    fn ewma_applies_expected_decay() {
        let first = ewma(None, 10.0);
        assert!((first - 10.0).abs() < f32::EPSILON);
        let second = ewma(Some(first), 0.0);
        assert!((second - 7.0).abs() < 0.001);
        let third = ewma(Some(second), 10.0);
        assert!((third - 7.9).abs() < 0.001);
    }

    #[test]
    fn originator_tag_wins_over_parent_chain_fallback() {
        let parents = HashMap::from([(300_u32, 200_u32), (200, 100)]);
        let tags = HashMap::from([(
            300_u32,
            OriginatorTag {
                originator: "CLUD:900".to_string(),
                originator_pid: 900,
                command: "tagged".to_string(),
            },
        )]);
        let sessions = session_index(SessionRoot {
            id: "sess-a".to_string(),
            name: Some("build".to_string()),
            worker_pid: 100,
            root_pid: Some(200),
        });

        let assignment = resolve_assignment(300, &parents, &tags, &sessions).unwrap();
        assert_eq!(assignment.originator, "CLUD:900");
        assert_eq!(assignment.originator_pid, Some(900));
    }

    #[test]
    fn parent_chain_falls_back_to_session_worker_originator() {
        let parents = HashMap::from([(300_u32, 200_u32), (200, 100)]);
        let tags = HashMap::new();
        let sessions = session_index(SessionRoot {
            id: "sess-a".to_string(),
            name: Some("build".to_string()),
            worker_pid: 100,
            root_pid: Some(200),
        });

        let assignment = resolve_assignment(300, &parents, &tags, &sessions).unwrap();
        assert_eq!(assignment.originator, "CLUD:100");
        assert_eq!(assignment.originator_pid, Some(100));
        assert_eq!(assignment.session_id.as_deref(), Some("sess-a"));
        assert_eq!(
            depth_from_originator(300, assignment.originator_pid, &parents),
            2
        );
    }

    #[test]
    fn dead_rows_are_retained_and_marked_frozen() {
        let tmp = tempfile::tempdir().unwrap();
        let mut sampler = ProcSampler::new(tmp.path().to_path_buf(), DEFAULT_SAMPLE_INTERVAL_MS);
        let live = ProcRow {
            pid: 10,
            ppid: Some(1),
            originator: "CLUD:1".to_string(),
            originator_pid: Some(1),
            session_id: None,
            session_name: None,
            cpu_pct: 12.0,
            cpu_ewma_pct: 12.0,
            rss_bytes: 10,
            age_secs: 1,
            command: "x".to_string(),
            depth: 1,
            tier: ProcTier::Hot,
            live: true,
            exited_at_ms: None,
        };
        sampler.last_live_rows.insert(10, live);
        sampler.ewma_by_pid.insert(10, 12.0);

        sampler.record_dead_rows(1_000, &HashMap::new());

        let dead = sampler.dead_rows.get(&10).unwrap();
        assert!(!dead.live);
        assert_eq!(dead.exited_at_ms, Some(1_000));
        assert_eq!(dead.cpu_pct, 0.0);
        assert_eq!(dead.tier, ProcTier::Frozen);
    }

    #[test]
    fn proc_snapshot_filters_dead_rows_by_requested_window() {
        let handle = ProcSamplerHandle::empty(DEFAULT_SAMPLE_INTERVAL_MS);
        {
            let mut snapshot = handle.snapshot.lock().unwrap();
            snapshot.sampled_at_ms = unix_millis_now();
            snapshot.rows.push(ProcRow {
                pid: 10,
                ppid: None,
                originator: "CLUD:1".to_string(),
                originator_pid: Some(1),
                session_id: None,
                session_name: None,
                cpu_pct: 0.0,
                cpu_ewma_pct: 0.0,
                rss_bytes: 0,
                age_secs: 0,
                command: "dead".to_string(),
                depth: 0,
                tier: ProcTier::Frozen,
                live: false,
                exited_at_ms: Some(unix_millis_now()),
            });
        }

        assert!(handle.snapshot(0).rows.is_empty());
        assert_eq!(handle.snapshot(5_000).rows.len(), 1);
    }

    #[test]
    fn proc_snapshot_serde_roundtrips() {
        let mut snapshot = ProcTreeSnapshot::empty(DEFAULT_SAMPLE_INTERVAL_MS);
        snapshot.rows.push(ProcRow {
            pid: 42,
            ppid: Some(1),
            originator: "CLUD:1".to_string(),
            originator_pid: Some(1),
            session_id: Some("sess".to_string()),
            session_name: None,
            cpu_pct: 1.25,
            cpu_ewma_pct: 0.75,
            rss_bytes: 4096,
            age_secs: 9,
            command: "worker".to_string(),
            depth: 1,
            tier: ProcTier::Warm,
            live: true,
            exited_at_ms: None,
        });
        snapshot.recompute_summary();

        let wire = serde_json::to_string(&snapshot).unwrap();
        let parsed: ProcTreeSnapshot = serde_json::from_str(&wire).unwrap();
        assert_eq!(parsed, snapshot);
    }

    #[test]
    fn summary_counts_unique_originators() {
        let mut snapshot = ProcTreeSnapshot::empty(DEFAULT_SAMPLE_INTERVAL_MS);
        for pid in [1_u32, 2, 3] {
            snapshot.rows.push(ProcRow {
                pid,
                ppid: None,
                originator: if pid == 3 { "CLUD:9" } else { "CLUD:1" }.to_string(),
                originator_pid: Some(1),
                session_id: None,
                session_name: None,
                cpu_pct: 1.0,
                cpu_ewma_pct: 1.0,
                rss_bytes: 10,
                age_secs: 0,
                command: "x".to_string(),
                depth: 0,
                tier: ProcTier::Cold,
                live: true,
                exited_at_ms: None,
            });
        }
        snapshot.recompute_summary();
        assert_eq!(snapshot.summary.process_count, 3);
        assert_eq!(snapshot.summary.originator_count, 2);
        assert_eq!(snapshot.summary.total_rss_bytes, 30);
    }
}
