//! Windows-only foreground tool-shell lifecycle tracking (#569, #616).
//!
//! The Win32 listener is deliberately thin. Role selection and exit planning
//! live in the platform-neutral functions below so destructive decisions are
//! pinned by must-survive fixtures on every CI platform.

#[cfg(any(windows, test))]
#[rustfmt::skip]
mod model {
use std::collections::{HashMap, HashSet};

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ProcessMeta {
    pub(crate) pid: u32,
    pub(crate) parent_pid: u32,
    pub(crate) image_name: String,
    pub(crate) alive: bool,
    pub(crate) start_time: u64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct RegisteredBackend {
    pub(super) pid: u32,
    agent_images: Vec<String>,
    start_time: u64,
}

impl RegisteredBackend {
    pub(crate) fn new(pid: u32, executable_name: &str, start_time: u64) -> Self {
        let mut image = image_basename(executable_name).to_ascii_lowercase();
        if !image.ends_with(".exe") {
            image.push_str(".exe");
        }
        let mut agent_images = vec![image.clone()];
        // The npm Claude launcher is cmd.exe -> node.exe; unlike Codex it
        // does not hand off to a separately named native claude.exe. The
        // first node reached during bootstrap is therefore the exact agent
        // authority boundary. Native Claude installs still match claude.exe.
        if image == "claude.exe" {
            agent_images.push("node.exe".to_string());
        }
        Self {
            pid,
            agent_images,
            start_time,
        }
    }

    fn is_agent_image(&self, image: &str) -> bool {
        let normalized = normalized_image(image);
        self.agent_images
            .iter()
            .any(|candidate| candidate == &normalized)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ProcessRole {
    BackendRoot,
    BackendBootstrap,
    AgentHost,
    ToolShellRoot,
    ShellHandoff,
    Client,
}

impl ProcessRole {
    fn as_str(self) -> &'static str {
        match self {
            Self::BackendRoot => "backend_root",
            Self::BackendBootstrap => "backend_bootstrap",
            Self::AgentHost => "agent_host",
            Self::ToolShellRoot => "tool_shell_root",
            Self::ShellHandoff => "shell_handoff",
            Self::Client => "client",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum DecisionAction {
    Reap,
    Spare,
    Handoff,
}

impl DecisionAction {
    fn as_str(self) -> &'static str {
        match self {
            Self::Reap => "reap",
            Self::Spare => "spare",
            Self::Handoff => "handoff",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ReapDecisionReason {
    LeakedToolClient,
    GitBashReexec,
    NestedShellDetach,
    DeclaredDaemon,
    ConsoleHost,
    #[cfg(windows)]
    CandidateIdentityChanged,
    NoLiveClients,
}

impl ReapDecisionReason {
    fn as_str(self) -> &'static str {
        match self {
            Self::LeakedToolClient => "leaked_tool_client",
            Self::GitBashReexec => "git_bash_reexec",
            Self::NestedShellDetach => "nested_shell_detach",
            Self::DeclaredDaemon => "declared_daemon",
            Self::ConsoleHost => "console_host_never_targeted",
            #[cfg(windows)]
            Self::CandidateIdentityChanged => "candidate_identity_changed",
            Self::NoLiveClients => "no_live_clients",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ReapDecision {
    pub(crate) trigger_pid: u32,
    pub(crate) trigger_image: String,
    pub(crate) trigger_role: ProcessRole,
    pub(crate) candidate_pid: Option<u32>,
    pub(crate) candidate_image: Option<String>,
    pub(crate) candidate_start_time: Option<u64>,
    pub(crate) action: DecisionAction,
    pub(crate) reason: ReapDecisionReason,
}

fn image_basename(image: &str) -> &str {
    image.rsplit(['\\', '/']).next().unwrap_or(image)
}

fn normalized_image(image: &str) -> String {
    image_basename(image).to_ascii_lowercase()
}

fn is_shell_image(image: &str) -> bool {
    matches!(
        normalized_image(image).as_str(),
        "cmd.exe" | "powershell.exe" | "pwsh.exe" | "bash.exe" | "git-bash.exe"
    )
}

fn is_bash_image(image: &str) -> bool {
    matches!(
        normalized_image(image).as_str(),
        "bash.exe" | "git-bash.exe"
    )
}

fn is_conhost_image(image: &str) -> bool {
    normalized_image(image) == "conhost.exe"
}

fn classify_roles(
    processes: &[ProcessMeta],
    backends: &[RegisteredBackend],
) -> HashMap<u32, ProcessRole> {
    let by_pid: HashMap<u32, &ProcessMeta> = processes.iter().map(|p| (p.pid, p)).collect();
    let mut children = HashMap::<u32, Vec<u32>>::new();
    for process in processes {
        children
            .entry(process.parent_pid)
            .or_default()
            .push(process.pid);
    }

    let mut roles = HashMap::new();
    for backend in backends {
        let Some(root) = by_pid.get(&backend.pid) else {
            continue;
        };
        if backend.start_time == crate::process_identity::UNKNOWN_START_TIME
            || root.start_time == crate::process_identity::UNKNOWN_START_TIME
            || backend.start_time != root.start_time
        {
            // A PID is not authority. If the root identity is missing or was
            // recycled, fail closed (no automatic kill) rather than granting
            // an unrelated process the stale backend's role.
            continue;
        }
        let root_role = if backend.is_agent_image(&root.image_name) {
            ProcessRole::AgentHost
        } else {
            ProcessRole::BackendRoot
        };
        roles.entry(root.pid).or_insert(root_role);

        let mut stack = vec![root.pid];
        while let Some(parent_pid) = stack.pop() {
            let Some(parent) = by_pid.get(&parent_pid) else {
                continue;
            };
            let Some(parent_role) = roles.get(&parent_pid).copied() else {
                continue;
            };
            for child_pid in children.get(&parent_pid).into_iter().flatten() {
                let Some(child) = by_pid.get(child_pid) else {
                    continue;
                };
                let child_role = match parent_role {
                    ProcessRole::BackendRoot | ProcessRole::BackendBootstrap => {
                        if backend.is_agent_image(&child.image_name) {
                            ProcessRole::AgentHost
                        } else {
                            // Before the exact agent executable appears, this
                            // is launcher/bootstrap structure, not a tool call.
                            ProcessRole::BackendBootstrap
                        }
                    }
                    ProcessRole::AgentHost => {
                        if is_shell_image(&child.image_name) {
                            ProcessRole::ToolShellRoot
                        } else {
                            // The exact agent image is an authority boundary.
                            // Even node/python below it is an ordinary client;
                            // never continue wrapper recognition into a tool.
                            ProcessRole::Client
                        }
                    }
                    ProcessRole::ToolShellRoot | ProcessRole::ShellHandoff
                        if is_bash_image(&parent.image_name)
                            && is_bash_image(&child.image_name) =>
                    {
                        ProcessRole::ShellHandoff
                    }
                    ProcessRole::ToolShellRoot
                    | ProcessRole::ShellHandoff
                    | ProcessRole::Client => ProcessRole::Client,
                };
                if roles.insert(*child_pid, child_role).is_none() {
                    stack.push(*child_pid);
                }
            }
        }
    }
    roles
}

pub(crate) fn plan_shell_exit(
    processes: &[ProcessMeta],
    backends: &[RegisteredBackend],
    declared_daemons: &HashSet<u32>,
    exited_pid: u32,
) -> Vec<ReapDecision> {
    let by_pid: HashMap<u32, &ProcessMeta> = processes.iter().map(|p| (p.pid, p)).collect();
    let Some(trigger) = by_pid.get(&exited_pid) else {
        return Vec::new();
    };
    let roles = classify_roles(processes, backends);
    let Some(trigger_role @ (ProcessRole::ToolShellRoot | ProcessRole::ShellHandoff)) =
        roles.get(&exited_pid).copied()
    else {
        return Vec::new();
    };

    let mut children = HashMap::<u32, Vec<u32>>::new();
    for process in processes {
        children
            .entry(process.parent_pid)
            .or_default()
            .push(process.pid);
    }

    if is_bash_image(&trigger.image_name) {
        if let Some(handoff) = children
            .get(&exited_pid)
            .into_iter()
            .flatten()
            .filter_map(|pid| by_pid.get(pid))
            .find(|process| {
                process.alive
                    && roles.get(&process.pid) == Some(&ProcessRole::ShellHandoff)
                    && is_bash_image(&process.image_name)
            })
        {
            return vec![decision(
                trigger,
                trigger_role,
                Some(handoff),
                DecisionAction::Handoff,
                ReapDecisionReason::GitBashReexec,
            )];
        }
    }

    #[derive(Clone, Copy)]
    struct Pending {
        pid: u32,
        has_non_shell_client: bool,
        reap_ancestor: bool,
        inherited_spare: Option<ReapDecisionReason>,
    }

    let mut pending: Vec<Pending> = children
        .get(&exited_pid)
        .into_iter()
        .flatten()
        .map(|pid| Pending {
            pid: *pid,
            has_non_shell_client: false,
            reap_ancestor: false,
            inherited_spare: None,
        })
        .collect();
    let mut decisions = Vec::new();

    while let Some(item) = pending.pop() {
        let Some(process) = by_pid.get(&item.pid) else {
            continue;
        };
        let role = roles.get(&item.pid).copied().unwrap_or(ProcessRole::Client);
        let own_spare = if declared_daemons.contains(&item.pid) {
            Some(ReapDecisionReason::DeclaredDaemon)
        } else if is_conhost_image(&process.image_name) {
            Some(ReapDecisionReason::ConsoleHost)
        } else if role == ProcessRole::Client
            && is_shell_image(&process.image_name)
            && item.has_non_shell_client
        {
            Some(ReapDecisionReason::NestedShellDetach)
        } else {
            None
        };
        let spare = item.inherited_spare.or(own_spare);

        if process.alive {
            if let Some(reason) = spare {
                decisions.push(decision(
                    trigger,
                    trigger_role,
                    Some(process),
                    DecisionAction::Spare,
                    reason,
                ));
                // This live process is the protected subtree root; one
                // structured decision describes the whole pruned subtree.
                continue;
            }
            if !item.reap_ancestor {
                decisions.push(decision(
                    trigger,
                    trigger_role,
                    Some(process),
                    DecisionAction::Reap,
                    ReapDecisionReason::LeakedToolClient,
                ));
            }
        }

        let non_shell_client = role == ProcessRole::Client && !is_shell_image(&process.image_name);
        let reap_ancestor = item.reap_ancestor || (process.alive && spare.is_none());
        for child_pid in children.get(&item.pid).into_iter().flatten() {
            pending.push(Pending {
                pid: *child_pid,
                has_non_shell_client: item.has_non_shell_client || non_shell_client,
                reap_ancestor,
                inherited_spare: spare,
            });
        }
    }

    if decisions.is_empty() {
        decisions.push(decision(
            trigger,
            trigger_role,
            None,
            DecisionAction::Spare,
            ReapDecisionReason::NoLiveClients,
        ));
    }
    decisions
}

fn decision(
    trigger: &ProcessMeta,
    trigger_role: ProcessRole,
    candidate: Option<&&ProcessMeta>,
    action: DecisionAction,
    reason: ReapDecisionReason,
) -> ReapDecision {
    ReapDecision {
        trigger_pid: trigger.pid,
        trigger_image: trigger.image_name.clone(),
        trigger_role,
        candidate_pid: candidate.map(|process| process.pid),
        candidate_image: candidate.map(|process| process.image_name.clone()),
        candidate_start_time: candidate.map(|process| process.start_time),
        action,
        reason,
    }
}

pub(super) fn decision_log_fields(
    decision: &ReapDecision,
) -> Vec<(&'static str, serde_json::Value)> {
    use serde_json::json;

    vec![
        ("foreground_pid", json!(std::process::id())),
        ("trigger_shell_pid", json!(decision.trigger_pid)),
        ("trigger_shell_image", json!(decision.trigger_image)),
        ("trigger_role", json!(decision.trigger_role.as_str())),
        ("candidate_root_pid", json!(decision.candidate_pid)),
        ("candidate_root_image", json!(decision.candidate_image)),
        (
            "candidate_root_start_time",
            json!(decision.candidate_start_time),
        ),
        ("action", json!(decision.action.as_str())),
        ("reason", json!(decision.reason.as_str())),
    ]
}

#[cfg(any(windows, test))]
pub(super) fn candidate_identity(
    decision: &ReapDecision,
) -> Option<crate::process_identity::ProcessIdentity> {
    let (pid, start_time) = decision.candidate_pid.zip(decision.candidate_start_time)?;
    let identity = crate::process_identity::ProcessIdentity::new(pid, start_time);
    identity.has_start_time().then_some(identity)
}

#[cfg(windows)]
pub(super) fn candidate_identity_is_live(decision: &ReapDecision) -> bool {
    let Some(recorded) = candidate_identity(decision) else {
        return false;
    };
    crate::process_identity::ProcessIdentity::observe(recorded.pid)
        .is_some_and(|observed| crate::process_tree::automatic_identity_matches(recorded, observed))
}

pub(super) fn pending_exit_pids(
    known: &HashMap<u32, ProcessMeta>,
    processed_exits: &HashSet<(u32, u64)>,
) -> Vec<u32> {
    known
        .values()
        .filter(|process| {
            !process.alive
                && is_shell_image(&process.image_name)
                && !processed_exits.contains(&(process.pid, process.start_time))
        })
        .map(|process| process.pid)
        .collect()
}

pub(super) fn record_new_process_observation(
    known: &mut HashMap<u32, ProcessMeta>,
    unresolved_new_pids: &mut HashSet<u32>,
    pid: u32,
    observed: Option<ProcessMeta>,
) {
    if let Some(process) =
        observed.filter(|process| process.start_time != crate::process_identity::UNKNOWN_START_TIME)
    {
        known.insert(pid, process);
        unresolved_new_pids.remove(&pid);
    } else {
        // Job notifications can beat Toolhelp/process-handle publication.
        // Retain the PID so a quiet-period retry can resolve its identity.
        unresolved_new_pids.insert(pid);
    }
}

pub(super) fn record_process_exit(
    known: &mut HashMap<u32, ProcessMeta>,
    unresolved_new_pids: &mut HashSet<u32>,
    metadata_failed_pids: &mut HashSet<u32>,
    pid: u32,
    final_observation: Option<ProcessMeta>,
) -> bool {
    if unresolved_new_pids.contains(&pid) {
        record_new_process_observation(known, unresolved_new_pids, pid, final_observation);
    }
    let metadata_failed = unresolved_new_pids.remove(&pid);
    if metadata_failed {
        // The process exited before either Toolhelp observation succeeded.
        // Its role cannot be reconstructed safely from a bare PID, so record
        // a fail-closed metadata gap and never finalize an empty-shell
        // decision from this incomplete graph.
        metadata_failed_pids.insert(pid);
    }
    if let Some(process) = known.get_mut(&pid) {
        process.alive = false;
    }
    metadata_failed
}

#[derive(Clone, Copy)]
pub(super) struct ReplayControl {
    pub(super) finalize_provisional_empty: bool,
    pub(super) metadata_complete: bool,
}

pub(super) fn claim_exit_replay(
    known: &HashMap<u32, ProcessMeta>,
    registered: &[RegisteredBackend],
    declared_daemons: &HashSet<u32>,
    processed_exits: &mut HashSet<(u32, u64)>,
    provisional_empty_exits: &mut HashSet<(u32, u64)>,
    exited_pid: u32,
    control: ReplayControl,
) -> Option<Vec<ReapDecision>> {
    if !control.metadata_complete {
        // Missing metadata can hide a nested shell / conhost / daemon
        // boundary beneath an otherwise reapable ancestor. Never plan from an
        // incomplete graph: ambiguity is a false-negative cleanup, not a
        // destructive false positive.
        return None;
    }
    let trigger = known.get(&exited_pid)?;
    let exit_identity = (trigger.pid, trigger.start_time);
    if processed_exits.contains(&exit_identity) {
        return None;
    }

    let processes: Vec<ProcessMeta> = known.values().cloned().collect();
    let decisions = plan_shell_exit(&processes, registered, declared_daemons, exited_pid);
    if decisions.is_empty() {
        // Registration or descendant metadata may arrive after the exit
        // notification. Leave this identity pending so either event can
        // replay it.
        return None;
    }
    if decisions.len() == 1 && decisions[0].reason == ReapDecisionReason::NoLiveClients {
        // Job notifications can precede Toolhelp metadata publication. "No
        // clients yet" is therefore provisional until one completion-port
        // quiet period has elapsed. A NEW_PROCESS event during that period
        // can expose and reap the leaked child; genuinely empty tools are then
        // finalized instead of accumulating forever.
        if !control.finalize_provisional_empty || !provisional_empty_exits.contains(&exit_identity)
        {
            provisional_empty_exits.insert(exit_identity);
            return None;
        }
    }
    provisional_empty_exits.remove(&exit_identity);
    processed_exits.insert(exit_identity);
    Some(decisions)
}
}

#[cfg(any(windows, test))]
use model::*;

#[cfg(not(windows))]
pub struct ForegroundJobTracker;

#[cfg(not(windows))]
impl ForegroundJobTracker {
    pub fn install() -> Option<Self> {
        None
    }

    pub fn register_backend(&self, _pid: u32, _executable_name: &str) {}
}

#[cfg(windows)]
mod imp {
    use std::collections::{HashMap, HashSet};
    use std::ffi::c_void;
    use std::mem::{size_of, zeroed};
    use std::ptr::null_mut;
    use std::sync::{
        atomic::{AtomicBool, Ordering},
        Arc, Mutex,
    };
    use std::thread::{self, JoinHandle};

    use windows::core::PCWSTR;
    use windows::Win32::Foundation::{
        CloseHandle, GetLastError, HANDLE, INVALID_HANDLE_VALUE, WAIT_TIMEOUT,
    };
    use windows::Win32::System::Diagnostics::ToolHelp::{
        CreateToolhelp32Snapshot, Process32FirstW, Process32NextW, PROCESSENTRY32W,
        TH32CS_SNAPPROCESS,
    };
    use windows::Win32::System::JobObjects::{
        AssignProcessToJobObject, CreateJobObjectW, JobObjectAssociateCompletionPortInformation,
        SetInformationJobObject, JOBOBJECT_ASSOCIATE_COMPLETION_PORT,
    };
    use windows::Win32::System::Threading::GetCurrentProcess;
    use windows::Win32::System::IO::{CreateIoCompletionPort, GetQueuedCompletionStatus};

    use super::{DecisionAction, ProcessMeta, ReapDecision, RegisteredBackend};

    const ACTIVE_PROCESS_ZERO: u32 = 4;
    const NEW_PROCESS: u32 = 6;
    const EXIT_PROCESS: u32 = 7;

    #[derive(Default)]
    struct TrackerProcesses {
        known: HashMap<u32, ProcessMeta>,
        processed_exits: HashSet<(u32, u64)>,
        provisional_empty_exits: HashSet<(u32, u64)>,
        unresolved_new_pids: HashSet<u32>,
        metadata_failed_pids: HashSet<u32>,
    }

    pub struct ForegroundJobTracker {
        job: HANDLE,
        port: HANDLE,
        stop: Arc<AtomicBool>,
        backends: Arc<Mutex<Vec<RegisteredBackend>>>,
        processes: Arc<Mutex<TrackerProcesses>>,
        listener: Option<JoinHandle<()>>,
    }

    impl ForegroundJobTracker {
        /// Installs after daemon startup. Failure is intentionally non-fatal:
        /// existing originator-tag exit cleanup remains the fallback.
        pub fn install() -> Option<Self> {
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
                    || AssignProcessToJobObject(job, GetCurrentProcess()).is_err()
                {
                    let _ = CloseHandle(port);
                    let _ = CloseHandle(job);
                    return None;
                }
                let stop = Arc::new(AtomicBool::new(false));
                let backends = Arc::new(Mutex::new(Vec::new()));
                let processes = Arc::new(Mutex::new(TrackerProcesses::default()));
                // windows::Win32::Foundation::HANDLE intentionally does not
                // implement Send because it wraps a raw pointer. The kernel
                // handle value itself is process-wide and remains owned by
                // ForegroundJobTracker, so pass only its integer value across
                // the thread boundary and reconstruct the typed wrapper there.
                let port_value = port.0 as usize;
                let listener = thread::spawn({
                    let stop = Arc::clone(&stop);
                    let backends = Arc::clone(&backends);
                    let processes = Arc::clone(&processes);
                    move || listen(HANDLE(port_value as *mut c_void), stop, backends, processes)
                });
                Some(Self {
                    job,
                    port,
                    stop,
                    backends,
                    processes,
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
            if let Some(mut process) = snapshot().remove(&pid) {
                process.start_time = start_time;
                if let Ok(mut processes) = self.processes.lock() {
                    processes.known.entry(pid).or_insert(process);
                    if start_time != crate::process_identity::UNKNOWN_START_TIME {
                        processes.unresolved_new_pids.remove(&pid);
                    }
                }
            }
            reconcile_pending(&self.processes, &self.backends, false);
        }
    }

    impl Drop for ForegroundJobTracker {
        fn drop(&mut self) {
            self.stop.store(true, Ordering::Release);
            if let Some(listener) = self.listener.take() {
                let _ = listener.join();
            }
            unsafe {
                let _ = CloseHandle(self.port);
                let _ = CloseHandle(self.job);
            }
        }
    }

    fn listen(
        port: HANDLE,
        stop: Arc<AtomicBool>,
        backends: Arc<Mutex<Vec<RegisteredBackend>>>,
        processes: Arc<Mutex<TrackerProcesses>>,
    ) {
        while !stop.load(Ordering::Acquire) {
            let (mut message, mut key, mut payload) = (0u32, 0usize, null_mut());
            if unsafe { GetQueuedCompletionStatus(port, &mut message, &mut key, &mut payload, 200) }
                .is_err()
            {
                if unsafe { GetLastError().0 } == WAIT_TIMEOUT.0 {
                    let metadata_complete = retry_unresolved_new_processes(&processes);
                    reconcile_pending(&processes, &backends, metadata_complete);
                    continue;
                }
                break;
            }
            let pid = payload as usize as u32;
            match message {
                NEW_PROCESS => {
                    let mut observed = snapshot().remove(&pid);
                    if let Some(process) = observed.as_mut() {
                        process.start_time = crate::process_identity::start_time_of(pid);
                    }
                    if let Ok(mut processes) = processes.lock() {
                        let TrackerProcesses {
                            known,
                            unresolved_new_pids,
                            ..
                        } = &mut *processes;
                        super::record_new_process_observation(
                            known,
                            unresolved_new_pids,
                            pid,
                            observed,
                        );
                    }
                    reconcile_pending(&processes, &backends, false);
                }
                EXIT_PROCESS => {
                    let needs_final_observation = processes
                        .lock()
                        .map(|processes| processes.unresolved_new_pids.contains(&pid))
                        .unwrap_or(false);
                    let mut final_observation = needs_final_observation
                        .then(|| snapshot().remove(&pid))
                        .flatten();
                    if let Some(process) = final_observation.as_mut() {
                        process.start_time = crate::process_identity::start_time_of(pid);
                    }
                    let metadata_failed = if let Ok(mut processes) = processes.lock() {
                        let TrackerProcesses {
                            known,
                            unresolved_new_pids,
                            metadata_failed_pids,
                            ..
                        } = &mut *processes;
                        super::record_process_exit(
                            known,
                            unresolved_new_pids,
                            metadata_failed_pids,
                            pid,
                            final_observation,
                        )
                    } else {
                        false
                    };
                    if metadata_failed {
                        log_metadata_miss(pid);
                    }
                    reconcile_exit(&processes, &backends, pid, false);
                }
                ACTIVE_PROCESS_ZERO => {
                    if let Ok(mut processes) = processes.lock() {
                        processes.known.clear();
                        processes.processed_exits.clear();
                        processes.provisional_empty_exits.clear();
                        processes.unresolved_new_pids.clear();
                        processes.metadata_failed_pids.clear();
                    }
                }
                _ => {}
            }
            let _ = key;
        }
    }

    fn retry_unresolved_new_processes(processes: &Mutex<TrackerProcesses>) -> bool {
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
        if unresolved.is_empty() {
            return processes
                .lock()
                .map(|processes| processes.metadata_failed_pids.is_empty())
                .unwrap_or(false);
        }

        let mut current = snapshot();
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
            super::record_new_process_observation(known, unresolved_new_pids, pid, observed);
        }
        processes.unresolved_new_pids.is_empty() && processes.metadata_failed_pids.is_empty()
    }

    fn reconcile_pending(
        processes: &Mutex<TrackerProcesses>,
        backends: &Mutex<Vec<RegisteredBackend>>,
        finalize_provisional_empty: bool,
    ) {
        let pending = processes
            .lock()
            .map(|processes| super::pending_exit_pids(&processes.known, &processes.processed_exits))
            .unwrap_or_default();
        for pid in pending {
            reconcile_exit(processes, backends, pid, finalize_provisional_empty);
        }
    }

    fn reconcile_exit(
        processes: &Mutex<TrackerProcesses>,
        backends: &Mutex<Vec<RegisteredBackend>>,
        exited_pid: u32,
        finalize_provisional_empty: bool,
    ) {
        let registered = backends
            .lock()
            .map(|backends| backends.clone())
            .unwrap_or_default();
        let daemons: HashSet<u32> = running_process::originator::find_declared_daemon_pids();
        let decisions = {
            let Ok(mut tracker) = processes.lock() else {
                return;
            };
            let TrackerProcesses {
                known,
                processed_exits,
                provisional_empty_exits,
                unresolved_new_pids,
                metadata_failed_pids,
                ..
            } = &mut *tracker;
            let metadata_complete =
                unresolved_new_pids.is_empty() && metadata_failed_pids.is_empty();
            let Some(decisions) = super::claim_exit_replay(
                known,
                &registered,
                &daemons,
                processed_exits,
                provisional_empty_exits,
                exited_pid,
                super::ReplayControl {
                    finalize_provisional_empty,
                    metadata_complete,
                },
            ) else {
                return;
            };
            decisions
        };

        execute_decisions(decisions, &daemons);
    }

    fn execute_decisions(decisions: Vec<ReapDecision>, daemons: &HashSet<u32>) {
        if decisions.is_empty() {
            return;
        }

        let spared: HashSet<u32> = decisions
            .iter()
            .filter(|decision| decision.action == DecisionAction::Spare)
            .filter_map(|decision| decision.candidate_pid)
            .chain(daemons.iter().copied())
            .collect();

        for mut decision in decisions {
            if decision.action == DecisionAction::Reap
                && !super::candidate_identity_is_live(&decision)
            {
                decision.action = DecisionAction::Spare;
                decision.reason = super::ReapDecisionReason::CandidateIdentityChanged;
                log_decision(&decision);
                continue;
            }

            log_decision(&decision);
            if decision.action != DecisionAction::Reap {
                continue;
            }
            let identity =
                super::candidate_identity(&decision).expect("validated reap decision identity");
            crate::process_tree::kill_tree_filtered_automatic(identity, &|pid| {
                !spared.contains(&pid)
            });
        }
    }

    fn log_decision(decision: &ReapDecision) {
        let Ok(state_dir) = crate::daemon::default_state_dir() else {
            return;
        };
        crate::daemon::log_structured_event(
            &state_dir,
            "foreground_tool_shell_decision",
            super::decision_log_fields(decision),
        );
    }

    fn log_metadata_miss(pid: u32) {
        use serde_json::json;

        let Ok(state_dir) = crate::daemon::default_state_dir() else {
            return;
        };
        crate::daemon::log_structured_event(
            &state_dir,
            "foreground_tool_shell_metadata_miss",
            vec![
                ("foreground_pid", json!(std::process::id())),
                ("process_pid", json!(pid)),
                ("action", json!("spare")),
                ("reason", json!("process_metadata_unavailable")),
            ],
        );
    }

    fn snapshot() -> HashMap<u32, ProcessMeta> {
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
}
#[cfg(windows)]
pub use imp::ForegroundJobTracker;

#[cfg(test)]
mod lifecycle_tests {
    use std::collections::HashSet;

    use super::{
        plan_shell_exit, DecisionAction, ProcessMeta, ProcessRole, ReapDecisionReason,
        RegisteredBackend,
    };

    fn process(pid: u32, parent_pid: u32, image_name: &str, alive: bool) -> ProcessMeta {
        ProcessMeta {
            pid,
            parent_pid,
            image_name: image_name.to_string(),
            alive,
            start_time: pid as u64,
        }
    }

    fn fixture(processes: Vec<ProcessMeta>, exited_pid: u32) -> Vec<super::ReapDecision> {
        plan_shell_exit(
            &processes,
            &[RegisteredBackend::new(10, "codex.exe", 10)],
            &HashSet::new(),
            exited_pid,
        )
    }

    fn replay_control(
        finalize_provisional_empty: bool,
        metadata_complete: bool,
    ) -> super::ReplayControl {
        super::ReplayControl {
            finalize_provisional_empty,
            metadata_complete,
        }
    }

    #[test]
    fn direct_agent_tool_shell_reaps_a_leaked_git_client() {
        let decisions = fixture(
            vec![
                process(10, 1, "cmd.exe", true),
                process(11, 10, "node.exe", true),
                process(12, 11, "codex.exe", true),
                process(20, 12, "powershell.exe", false),
                process(21, 20, "gh.exe", true),
                process(22, 21, "git.exe", true),
            ],
            20,
        );

        assert_eq!(decisions.len(), 1);
        assert_eq!(decisions[0].action, DecisionAction::Reap);
        assert_eq!(decisions[0].reason, ReapDecisionReason::LeakedToolClient);
        assert_eq!(decisions[0].candidate_pid, Some(21));
        assert_eq!(decisions[0].trigger_role, ProcessRole::ToolShellRoot);
    }

    #[test]
    fn nested_shell_below_non_shell_tool_is_spared_as_detached() {
        // agent -> PowerShell tool root -> new.exe -> cmd wrappers -> terminal
        let decisions = fixture(
            vec![
                process(10, 1, "cmd.exe", true),
                process(11, 10, "node.exe", true),
                process(12, 11, "codex.exe", true),
                process(20, 12, "powershell.exe", false),
                process(21, 20, "new.exe", false),
                process(22, 21, "cmd.exe", false),
                process(23, 22, "cmd.exe", true),
            ],
            20,
        );

        assert_eq!(decisions.len(), 1);
        assert_eq!(decisions[0].action, DecisionAction::Spare);
        assert_eq!(decisions[0].reason, ReapDecisionReason::NestedShellDetach);
        assert_eq!(decisions[0].candidate_pid, Some(23));
    }

    #[test]
    fn conhost_is_never_an_automatic_kill_target() {
        let decisions = fixture(
            vec![
                process(10, 1, "cmd.exe", true),
                process(11, 10, "node.exe", true),
                process(12, 11, "codex.exe", true),
                process(20, 12, "powershell.exe", false),
                process(30, 20, "ConHost.EXE", true),
                process(31, 30, "cmd.exe", true),
            ],
            20,
        );

        assert_eq!(decisions.len(), 1);
        assert_eq!(decisions[0].action, DecisionAction::Spare);
        assert_eq!(decisions[0].reason, ReapDecisionReason::ConsoleHost);
        assert_eq!(decisions[0].candidate_pid, Some(30));
    }

    #[test]
    fn git_bash_reexec_is_a_handoff_until_inner_shell_exits() {
        let processes = vec![
            process(10, 1, "cmd.exe", true),
            process(12, 10, "claude.exe", true),
            process(20, 12, "bash.exe", false),
            process(21, 20, "bash.exe", true),
            process(22, 21, "git.exe", true),
        ];

        let claude = [RegisteredBackend::new(10, "claude.exe", 10)];
        let decisions = plan_shell_exit(&processes, &claude, &HashSet::new(), 20);
        assert_eq!(decisions.len(), 1);
        assert_eq!(decisions[0].action, DecisionAction::Handoff);
        assert_eq!(decisions[0].reason, ReapDecisionReason::GitBashReexec);
        assert_eq!(decisions[0].candidate_pid, Some(21));

        let mut after_inner_exit = processes;
        after_inner_exit
            .iter_mut()
            .find(|process| process.pid == 21)
            .unwrap()
            .alive = false;
        let decisions = plan_shell_exit(&after_inner_exit, &claude, &HashSet::new(), 21);
        assert_eq!(decisions.len(), 1);
        assert_eq!(decisions[0].action, DecisionAction::Reap);
        assert_eq!(decisions[0].candidate_pid, Some(22));
        assert_eq!(decisions[0].trigger_role, ProcessRole::ShellHandoff);
    }

    #[test]
    fn npm_claude_node_process_is_the_agent_boundary() {
        let processes = vec![
            process(10, 1, "cmd.exe", true),
            process(11, 10, "node.exe", true),
            process(20, 11, "powershell.exe", false),
            process(21, 20, "git.exe", true),
        ];
        let claude = [RegisteredBackend::new(10, "claude", 10)];
        let decisions = plan_shell_exit(&processes, &claude, &HashSet::new(), 20);

        assert_eq!(decisions.len(), 1);
        assert_eq!(decisions[0].action, DecisionAction::Reap);
        assert_eq!(decisions[0].candidate_pid, Some(21));
    }

    #[test]
    fn declared_daemon_and_its_subtree_are_spared() {
        let processes = vec![
            process(10, 1, "cmd.exe", true),
            process(11, 10, "node.exe", true),
            process(12, 11, "codex.exe", true),
            process(20, 12, "powershell.exe", false),
            process(40, 20, "docker.exe", true),
            process(41, 40, "docker-helper.exe", true),
        ];
        let daemons = HashSet::from([40]);
        let decisions = plan_shell_exit(
            &processes,
            &[RegisteredBackend::new(10, "codex.exe", 10)],
            &daemons,
            20,
        );

        assert_eq!(decisions.len(), 1);
        assert_eq!(decisions[0].action, DecisionAction::Spare);
        assert_eq!(decisions[0].reason, ReapDecisionReason::DeclaredDaemon);
        assert_eq!(decisions[0].candidate_pid, Some(40));
    }

    #[test]
    fn unmarked_detached_docker_helper_below_conhost_is_spared() {
        let processes = vec![
            process(10, 1, "cmd.exe", true),
            process(11, 10, "node.exe", true),
            process(12, 11, "codex.exe", true),
            process(20, 12, "powershell.exe", false),
            process(40, 20, "docker.exe", false),
            process(41, 40, "conhost.exe", true),
            process(42, 41, "com.docker.helper.exe", true),
        ];
        let decisions = fixture(processes, 20);

        assert_eq!(decisions.len(), 1);
        assert_eq!(decisions[0].action, DecisionAction::Spare);
        assert_eq!(decisions[0].reason, ReapDecisionReason::ConsoleHost);
        assert_eq!(decisions[0].candidate_pid, Some(41));
    }

    #[test]
    fn nested_shell_exit_is_not_promoted_to_tool_completion() {
        let processes = vec![
            process(10, 1, "cmd.exe", true),
            process(11, 10, "node.exe", true),
            process(12, 11, "codex.exe", true),
            process(20, 12, "powershell.exe", true),
            process(21, 20, "python.exe", true),
            process(22, 21, "cmd.exe", false),
            process(23, 22, "child.exe", true),
        ];

        assert!(fixture(processes, 22).is_empty());
    }

    #[test]
    fn unregistered_shell_exit_is_ignored() {
        let processes = vec![
            process(20, 99, "powershell.exe", false),
            process(21, 20, "git.exe", true),
        ];
        assert!(plan_shell_exit(&processes, &[], &HashSet::new(), 20).is_empty());
    }

    #[test]
    fn non_shell_child_after_agent_boundary_starts_client_subtree() {
        let processes = vec![
            process(10, 1, "cmd.exe", true),
            process(11, 10, "node.exe", true),
            process(12, 11, "codex.exe", true),
            process(20, 12, "node.exe", true),
            process(21, 20, "powershell.exe", false),
            process(22, 21, "git.exe", true),
        ];

        assert!(fixture(processes, 21).is_empty());
    }

    #[test]
    fn recycled_backend_pid_does_not_inherit_stale_authority() {
        let mut processes = vec![
            process(10, 1, "cmd.exe", true),
            process(12, 10, "codex.exe", true),
            process(20, 12, "powershell.exe", false),
            process(21, 20, "git.exe", true),
        ];
        processes[0].start_time = 99;
        let stale_registration = [RegisteredBackend::new(10, "codex.exe", 10)];

        assert!(plan_shell_exit(&processes, &stale_registration, &HashSet::new(), 20).is_empty());
    }

    #[test]
    fn pending_exit_replays_once_after_backend_registration_arrives() {
        let mut known: std::collections::HashMap<_, _> = vec![
            process(10, 1, "codex.exe", true),
            process(20, 10, "powershell.exe", false),
            process(30, 10, "node.exe", false),
        ]
        .into_iter()
        .map(|process| (process.pid, process))
        .collect();
        let mut processed = HashSet::new();
        let mut provisional = HashSet::new();

        assert_eq!(super::pending_exit_pids(&known, &processed), vec![20]);
        assert!(super::claim_exit_replay(
            &known,
            &[],
            &HashSet::new(),
            &mut processed,
            &mut provisional,
            20,
            replay_control(false, true),
        )
        .is_none());
        assert_eq!(super::pending_exit_pids(&known, &processed), vec![20]);

        let registered = [RegisteredBackend::new(10, "codex.exe", 10)];
        assert!(super::claim_exit_replay(
            &known,
            &registered,
            &HashSet::new(),
            &mut processed,
            &mut provisional,
            20,
            replay_control(false, true),
        )
        .is_none());
        assert_eq!(super::pending_exit_pids(&known, &processed), vec![20]);

        let late_client = process(21, 20, "git.exe", true);
        known.insert(late_client.pid, late_client);
        let replayed = super::claim_exit_replay(
            &known,
            &registered,
            &HashSet::new(),
            &mut processed,
            &mut provisional,
            20,
            replay_control(false, true),
        )
        .unwrap();
        assert_eq!(replayed.len(), 1);
        assert_eq!(replayed[0].action, DecisionAction::Reap);
        assert_eq!(replayed[0].candidate_pid, Some(21));
        assert!(super::pending_exit_pids(&known, &processed).is_empty());
        assert!(super::claim_exit_replay(
            &known,
            &registered,
            &HashSet::new(),
            &mut processed,
            &mut provisional,
            20,
            replay_control(false, true),
        )
        .is_none());
    }

    #[test]
    fn empty_shell_exit_finalizes_after_one_quiet_period() {
        let known: std::collections::HashMap<_, _> = vec![
            process(10, 1, "codex.exe", true),
            process(20, 10, "powershell.exe", false),
        ]
        .into_iter()
        .map(|process| (process.pid, process))
        .collect();
        let registered = [RegisteredBackend::new(10, "codex.exe", 10)];
        let mut processed = HashSet::new();
        let mut provisional = HashSet::new();

        assert!(super::claim_exit_replay(
            &known,
            &registered,
            &HashSet::new(),
            &mut processed,
            &mut provisional,
            20,
            replay_control(false, true),
        )
        .is_none());
        let finalized = super::claim_exit_replay(
            &known,
            &registered,
            &HashSet::new(),
            &mut processed,
            &mut provisional,
            20,
            replay_control(true, true),
        )
        .unwrap();

        assert_eq!(finalized.len(), 1);
        assert_eq!(finalized[0].reason, ReapDecisionReason::NoLiveClients);
        assert!(super::pending_exit_pids(&known, &processed).is_empty());
    }

    #[test]
    fn missed_new_process_metadata_is_retained_until_retry_succeeds() {
        let mut known = std::collections::HashMap::new();
        let mut unresolved = HashSet::new();

        super::record_new_process_observation(&mut known, &mut unresolved, 21, None);
        assert_eq!(unresolved, HashSet::from([21]));
        assert!(!known.contains_key(&21));

        super::record_new_process_observation(
            &mut known,
            &mut unresolved,
            21,
            Some(process(21, 20, "git.exe", true)),
        );
        assert!(unresolved.is_empty());
        assert_eq!(known[&21].parent_pid, 20);
        assert_eq!(known[&21].start_time, 21);
    }

    #[test]
    fn unresolved_process_exit_takes_final_observation_or_fails_closed() {
        let mut known = std::collections::HashMap::new();
        let mut unresolved = HashSet::new();
        let mut failed = HashSet::new();
        super::record_new_process_observation(&mut known, &mut unresolved, 21, None);

        assert!(!super::record_process_exit(
            &mut known,
            &mut unresolved,
            &mut failed,
            21,
            Some(process(21, 20, "git.exe", true)),
        ));
        assert!(!known[&21].alive);
        assert!(unresolved.is_empty());
        assert!(failed.is_empty());

        super::record_new_process_observation(&mut known, &mut unresolved, 22, None);
        assert!(super::record_process_exit(
            &mut known,
            &mut unresolved,
            &mut failed,
            22,
            None,
        ));
        assert!(unresolved.is_empty());
        assert_eq!(failed, HashSet::from([22]));
        assert!(!known.contains_key(&22));
    }

    #[test]
    fn incomplete_graph_cannot_claim_reap_before_nested_shell_resolves() {
        let mut known: std::collections::HashMap<_, _> = vec![
            process(10, 1, "codex.exe", true),
            process(20, 10, "powershell.exe", false),
            process(21, 20, "new.exe", true),
        ]
        .into_iter()
        .map(|process| (process.pid, process))
        .collect();
        let registered = [RegisteredBackend::new(10, "codex.exe", 10)];
        let mut processed = HashSet::new();
        let mut provisional = HashSet::new();

        assert!(super::claim_exit_replay(
            &known,
            &registered,
            &HashSet::new(),
            &mut processed,
            &mut provisional,
            20,
            replay_control(false, false),
        )
        .is_none());
        assert!(processed.is_empty());

        let nested_shell = process(22, 21, "cmd.exe", true);
        known.insert(nested_shell.pid, nested_shell);
        let decisions = super::claim_exit_replay(
            &known,
            &registered,
            &HashSet::new(),
            &mut processed,
            &mut provisional,
            20,
            replay_control(false, true),
        )
        .unwrap();
        assert!(decisions.iter().any(|decision| {
            decision.action == DecisionAction::Spare
                && decision.reason == ReapDecisionReason::NestedShellDetach
                && decision.candidate_pid == Some(22)
        }));
    }

    #[test]
    fn reap_candidate_identity_rejects_a_recycled_pid() {
        let decisions = fixture(
            vec![
                process(10, 1, "cmd.exe", true),
                process(12, 10, "codex.exe", true),
                process(20, 12, "powershell.exe", false),
                process(21, 20, "git.exe", true),
            ],
            20,
        );
        let recorded = super::candidate_identity(&decisions[0]).unwrap();
        let replacement =
            crate::process_identity::ProcessIdentity::new(recorded.pid, recorded.start_time + 1);

        assert!(!recorded.matches(&replacement));
    }

    #[test]
    fn structured_decision_fields_include_trigger_candidate_action_and_reason() {
        let decisions = fixture(
            vec![
                process(10, 1, "cmd.exe", true),
                process(12, 10, "codex.exe", true),
                process(20, 12, "powershell.exe", false),
                process(21, 20, "git.exe", true),
            ],
            20,
        );
        let fields: std::collections::HashMap<_, _> = super::decision_log_fields(&decisions[0])
            .into_iter()
            .collect();

        assert_eq!(fields["trigger_shell_pid"], 20);
        assert_eq!(fields["trigger_shell_image"], "powershell.exe");
        assert_eq!(fields["candidate_root_pid"], 21);
        assert_eq!(fields["candidate_root_image"], "git.exe");
        assert_eq!(fields["candidate_root_start_time"], 21);
        assert_eq!(fields["action"], "reap");
        assert_eq!(fields["reason"], "leaked_tool_client");
    }
}
