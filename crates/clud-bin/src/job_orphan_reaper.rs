//! Windows-only foreground tool-shell lifecycle tracking (#569, #616).
//!
//! The Win32 listener is deliberately thin. Role selection and exit planning
//! live in the platform-neutral functions below so destructive decisions are
//! pinned by must-survive fixtures on every CI platform.

#[cfg(any(windows, test))]
#[rustfmt::skip]
mod model {
use std::collections::{HashMap, HashSet, VecDeque};

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ProcessMeta {
    pub(crate) pid: u32,
    pub(crate) parent_pid: u32,
    pub(crate) image_name: String,
    pub(crate) alive: bool,
    /// Process *creation* time. Pairs with `pid` to form the identity every
    /// per-process map here is keyed by.
    pub(crate) start_time: u64,
    /// When this process was observed to exit, in tracker-clock milliseconds;
    /// `None` while alive. `start_time` cannot serve here — it is creation, not
    /// exit — and the eviction sweep needs a grace window measured from exit.
    pub(crate) exited_at_ms: Option<u64>,
}

impl ProcessMeta {
    pub(crate) fn identity(&self) -> (u32, u64) {
        (self.pid, self.start_time)
    }
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
    /// Declared itself a daemon via `RUNNING_PROCESS_IS_DAEMON`. **Cooperative**
    /// — see [`ProcessFacts`] for why this is ranked below the OS signals.
    DeclaredDaemon,
    ConsoleHost,
    /// Broke away from the reaper's Job Object, so it is outside our
    /// containment and was never ours to kill.
    OutsideJobObject,
    /// Runs in session 0 — a Windows service, not a session descendant.
    ServiceSession,
    /// Called `setsid()`: it leads its own POSIX session.
    SessionLeader,
    /// Runs as a different token owner / euid than we do.
    ForeignTokenOwner,
    /// Owns a listening endpoint, so later unrelated invocations discover and
    /// reuse it. This is what a build-cache or language server *is*.
    ListeningEndpoint,
    /// Matched the operator's configured spare-list. Last resort, and data
    /// rather than code — see [`ProcessFacts::spare_listed`].
    ConfiguredSpareList,
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
            Self::OutsideJobObject => "outside_job_object",
            Self::ServiceSession => "service_session",
            Self::SessionLeader => "session_leader",
            Self::ForeignTokenOwner => "foreign_token_owner",
            Self::ListeningEndpoint => "listening_endpoint",
            Self::ConfiguredSpareList => "configured_spare_list",
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

/// The OS-authoritative spare machinery, owned by [`crate::reaper_facts`].
///
/// It used to live here, which is exactly why #688 happened: the
/// cross-platform reaper could not reach it and kept sparing by the
/// cooperative marker alone. One table now serves both reapers; this module
/// contributes the Job Object signal, which only it can answer.
pub(crate) use crate::reaper_facts::{
    build_spare_list as build_os_spare_list, configured_spare_images, image_basename,
    normalized_image, FactsSnapshot, ProcessFacts, Signal, SpareReason,
};
#[cfg(test)]
pub(crate) use crate::reaper_facts::spare_signal as os_spare_signal;

impl From<SpareReason> for ReapDecisionReason {
    fn from(reason: SpareReason) -> Self {
        match reason {
            SpareReason::OutsideJobObject => Self::OutsideJobObject,
            SpareReason::ServiceSession => Self::ServiceSession,
            SpareReason::SessionLeader => Self::SessionLeader,
            SpareReason::ForeignTokenOwner => Self::ForeignTokenOwner,
            SpareReason::DeclaredDaemon => Self::DeclaredDaemon,
            SpareReason::ListeningEndpoint => Self::ListeningEndpoint,
            SpareReason::ConfiguredSpareList => Self::ConfiguredSpareList,
        }
    }
}

/// The subtractive spare-list: PID -> why it must not be reaped.
///
/// Subtractive data may be stale (worst case: it spares too much). Additive
/// data - anything naming a kill *target* - may not.
pub(crate) type SpareList = HashMap<u32, ReapDecisionReason>;

/// [`crate::reaper_facts::spare_signal`] in this reaper's richer reason space.
///
/// Only the batched [`build_spare_list`] form is used in production; this
/// exists so the Tier 1 decision table can assert one PID's reason directly.
#[cfg(test)]
pub(crate) fn spare_signal(
    facts: &dyn ProcessFacts,
    pid: u32,
    image_name: &str,
) -> Option<ReapDecisionReason> {
    os_spare_signal(facts, pid, image_name).map(Into::into)
}

/// [`crate::reaper_facts::build_spare_list`] in this reaper's richer reason
/// space.
///
/// The candidate set is the reaper's *own* tracked processes, never the host -
/// that bound is what turned a 442-PEB-read full-host scan per process exit
/// into single-digit queries over the job's own membership.
pub(crate) fn build_spare_list(
    facts: &dyn ProcessFacts,
    candidates: impl Iterator<Item = (u32, String)>,
) -> SpareList {
    build_os_spare_list(facts, candidates)
        .into_iter()
        .map(|(pid, reason)| (pid, reason.into()))
        .collect()
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

/// The reap graph, indexed once per reconcile pass.
///
/// Before #673 Phase 3 every pending exit rebuilt all of this from a full deep
/// clone of the tracked process map — `known.values().cloned().collect()`, per
/// pending PID, per 200 ms tick, which is O(N×M) in backlog size × processes
/// ever spawned. The indices depend only on the process set and the registered
/// backends, so they are built once and shared across the whole pass.
pub(crate) struct ProcessGraph<'a> {
    by_pid: HashMap<u32, &'a ProcessMeta>,
    children: HashMap<u32, Vec<u32>>,
    roles: HashMap<u32, ProcessRole>,
}

impl<'a> ProcessGraph<'a> {
    pub(crate) fn build<I>(processes: I, backends: &[RegisteredBackend]) -> Self
    where
        I: IntoIterator<Item = &'a ProcessMeta>,
    {
        let mut by_pid: HashMap<u32, &'a ProcessMeta> = HashMap::new();
        let mut children = HashMap::<u32, Vec<u32>>::new();
        for process in processes {
            by_pid.insert(process.pid, process);
            children
                .entry(process.parent_pid)
                .or_default()
                .push(process.pid);
        }
        // Deterministic traversal: `known` is a HashMap, so without this the
        // decision order (and therefore the log order) would vary run to run.
        for siblings in children.values_mut() {
            siblings.sort_unstable();
        }
        let roles = classify_roles(&by_pid, &children, backends);
        Self {
            by_pid,
            children,
            roles,
        }
    }

    fn children_of(&self, pid: u32) -> &[u32] {
        self.children.get(&pid).map(Vec::as_slice).unwrap_or(&[])
    }

    pub(crate) fn meta(&self, pid: u32) -> Option<&'a ProcessMeta> {
        self.by_pid.get(&pid).copied()
    }

    /// Every tracked process as `(pid, image_name)` — the bounded candidate set
    /// the spare-list is evaluated over. Bounded by the reaper's *own* job
    /// membership, never by the host process table.
    pub(crate) fn spare_candidates(&self) -> impl Iterator<Item = (u32, String)> + '_ {
        self.by_pid
            .values()
            .map(|process| (process.pid, process.image_name.clone()))
    }
}

fn classify_roles(
    by_pid: &HashMap<u32, &ProcessMeta>,
    children: &HashMap<u32, Vec<u32>>,
    backends: &[RegisteredBackend],
) -> HashMap<u32, ProcessRole> {
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
    graph: &ProcessGraph<'_>,
    spares: &SpareList,
    exited_pid: u32,
) -> Vec<ReapDecision> {
    let by_pid = &graph.by_pid;
    let roles = &graph.roles;
    let Some(trigger) = by_pid.get(&exited_pid) else {
        return Vec::new();
    };
    let Some(trigger_role @ (ProcessRole::ToolShellRoot | ProcessRole::ShellHandoff)) =
        roles.get(&exited_pid).copied()
    else {
        return Vec::new();
    };

    if is_bash_image(&trigger.image_name) {
        if let Some(handoff) = graph
            .children_of(exited_pid)
            .iter()
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

    let mut pending: Vec<Pending> = graph
        .children_of(exited_pid)
        .iter()
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
        let own_spare = if let Some(reason) = spares.get(&item.pid).copied() {
            Some(reason)
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
        for child_pid in graph.children_of(item.pid) {
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

impl ReapDecision {
    /// The reason string, shared verbatim with the daemon event stream so the
    /// reap log and that stream name the same thing the same way.
    // Consumed by the Win32 listener in `imp`, which does not exist off
    // Windows. `model` still compiles under `cfg(test)` everywhere so the
    // decision table is asserted on every platform — which leaves this
    // genuinely unreferenced in a non-Windows `lib test` build.
    #[cfg_attr(not(windows), allow(dead_code))]
    pub(crate) fn reason_str(&self) -> &'static str {
        self.reason.as_str()
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
    pid: u32,
    final_observation: Option<ProcessMeta>,
    now_ms: u64,
) -> bool {
    if unresolved_new_pids.contains(&pid) {
        record_new_process_observation(known, unresolved_new_pids, pid, final_observation);
    }
    // A PID still unresolved at exit exited before either Toolhelp observation
    // succeeded and can never be resolved (retry has nothing live to read). It
    // is therefore absent from `known`, so `plan_shell_exit`'s tree walk — which
    // only reaches processes linked through `known` — can never reach anything
    // beneath it: its absence can only *disconnect* (spare) a subtree, never
    // expand the reap set. Report the miss for telemetry, but do NOT let a dead,
    // un-retryable PID gate finalization; only live `unresolved_new_pids` (which
    // a retry can still resolve into a real boundary) are allowed to block.
    let metadata_failed = unresolved_new_pids.remove(&pid);
    if let Some(process) = known.get_mut(&pid) {
        process.alive = false;
        // Creation time cannot serve as the eviction clock, so record when the
        // exit was *observed*. Only the first observation counts: a replayed
        // notification must not restart the grace window.
        process.exited_at_ms.get_or_insert(now_ms);
    }
    metadata_failed
}

// ---------------------------------------------------------------------------
// #673 Phase 2 — the tracked keyspace, and the one sweep that bounds it.
//
// Every per-process map here is keyed by `(pid, creation_time)` and evicted by
// a single sweep. Before this, `known` was cleared only at
// `ACTIVE_PROCESS_ZERO`, which never fires while the backend lives, so all four
// maps grew monotonically for the session's life and the reaper's cost tracked
// **session age** rather than activity.
// ---------------------------------------------------------------------------

/// Grace after a process is observed to exit before its identity may be
/// evicted. Covers completion-port reordering, which is tens of milliseconds.
pub(crate) const EVICTION_GRACE_MS: u64 = 5_000;

/// TTL for the `decisions.is_empty()` deferral.
///
/// The plan produced **nothing to kill**, so finalizing loses only the chance
/// that late metadata would have produced a decision. Aggressive.
pub(crate) const EMPTY_PLAN_DEFER_MS: u64 = 3_000;

/// TTL for the provisional-empty deferral.
///
/// That branch is a possible *real leak*, so this is the conservative one. It
/// is a backstop, not the normal path: provisional-empty normally finalizes
/// after a single completion-port quiet period.
pub(crate) const PROVISIONAL_DEFER_MS: u64 = 60_000;

/// Quiet-period retries an unresolved PID gets before it is moved to the
/// unkeyable holding pen and stops gating finalization.
pub(crate) const MAX_METADATA_RETRIES: u32 = 10;

/// Hard cap on the unkeyable holding pen, and on the abandoned retry list.
/// Neither can be drained by retrying, so both need a cap as well as a TTL.
pub(crate) const MAX_UNKEYABLE: usize = 256;

/// How long an unkeyable PID is remembered before it is dropped outright.
pub(crate) const UNKEYABLE_TTL_MS: u64 = 300_000;

/// Everything the tracker remembers about the processes in its job.
///
/// Lives in the platform-neutral model, not in the Win32 listener, so the
/// bounding rules are unit-testable on every platform (#674).
#[derive(Default)]
pub(crate) struct TrackerProcesses {
    pub(crate) known: HashMap<u32, ProcessMeta>,
    pub(crate) processed_exits: HashSet<(u32, u64)>,
    pub(crate) provisional_empty_exits: HashSet<(u32, u64)>,
    pub(crate) unresolved_new_pids: HashSet<u32>,
    /// Quiet periods charged against each still-unresolved PID. Bounded by
    /// `unresolved_new_pids`, which this is what bounds.
    unresolved_retries: HashMap<u32, u32>,
    /// PIDs that can *never* be keyed — they died before a creation time could
    /// be read. No amount of retrying produces a key, so this gets a hard TTL
    /// and a hard cap rather than an eviction rule tied to liveness.
    #[cfg(test)]
    pub(crate) unkeyable: VecDeque<(u32, u64)>,
    #[cfg(not(test))]
    unkeyable: VecDeque<(u32, u64)>,
    /// First time each pending exit identity was deferred. The TTL that
    /// eventually abandons it is measured from here.
    deferred_since: HashMap<(u32, u64), u64>,
    /// Exit triggers abandoned at runtime, retried once at foreground exit.
    abandoned: VecDeque<(u32, u64)>,
}

/// The mutable exit bookkeeping, handed to [`claim_exit_replay`] while the
/// process graph holds an immutable borrow of `known`.
pub(crate) struct ExitLedger<'a> {
    pub(crate) processed_exits: &'a mut HashSet<(u32, u64)>,
    pub(crate) provisional_empty_exits: &'a mut HashSet<(u32, u64)>,
    pub(crate) deferred_since: &'a mut HashMap<(u32, u64), u64>,
    pub(crate) abandoned: &'a mut VecDeque<(u32, u64)>,
}

impl<'a> ExitLedger<'a> {
    /// Has `identity` been deferred for longer than `ttl_ms`?
    ///
    /// The first call records the deferral; later calls measure from it.
    fn defer_expired(&mut self, identity: (u32, u64), now_ms: u64, ttl_ms: u64) -> bool {
        let since = *self.deferred_since.entry(identity).or_insert(now_ms);
        now_ms.saturating_sub(since) >= ttl_ms
    }

    /// Stop tracking `identity` as pending and queue it for the exit sweep.
    ///
    /// There is no periodic backstop for these: the daemon sweep filters
    /// `!parent_alive`, so it catches only orphans whose originating clud is
    /// **dead**, while this reaper exists for leaks of a *live* clud. The two
    /// sets are disjoint by construction. Without the retry list an abandoned
    /// orphan would hold its port or file lock for the rest of the session.
    fn abandon(&mut self, identity: (u32, u64)) {
        self.processed_exits.insert(identity);
        self.provisional_empty_exits.remove(&identity);
        self.deferred_since.remove(&identity);
        if !self.abandoned.contains(&identity) {
            self.abandoned.push_back(identity);
            while self.abandoned.len() > MAX_UNKEYABLE {
                self.abandoned.pop_front();
            }
        }
    }

    fn finalize(&mut self, identity: (u32, u64)) {
        self.provisional_empty_exits.remove(&identity);
        self.deferred_since.remove(&identity);
        self.processed_exits.insert(identity);
    }
}

impl TrackerProcesses {
    /// Borrow the graph inputs immutably and the exit bookkeeping mutably, in
    /// one step, so a reconcile pass can hold both at once.
    // Consumed by the Win32 listener in `imp`, which does not exist off
    // Windows. `model` still compiles under `cfg(test)` everywhere so the
    // decision table is asserted on every platform — which leaves this
    // genuinely unreferenced in a non-Windows `lib test` build.
    #[cfg_attr(not(windows), allow(dead_code))]
    pub(crate) fn split(&mut self) -> (&HashMap<u32, ProcessMeta>, ExitLedger<'_>) {
        (
            &self.known,
            ExitLedger {
                processed_exits: &mut self.processed_exits,
                provisional_empty_exits: &mut self.provisional_empty_exits,
                deferred_since: &mut self.deferred_since,
                abandoned: &mut self.abandoned,
            },
        )
    }

    /// Only *live* unresolved PIDs gate finalization; see
    /// [`bump_unresolved_retries`](Self::bump_unresolved_retries) for why the
    /// set is now bounded.
    pub(crate) fn metadata_complete(&self) -> bool {
        self.unresolved_new_pids.is_empty()
    }

    /// Charge one quiet period against every still-unresolved PID, moving the
    /// ones that have exhausted their retries into the unkeyable holding pen.
    ///
    /// The finalization gate is `unresolved_new_pids.is_empty()` — a **global**
    /// condition. One PID whose metadata never resolves used to block
    /// finalization of *every* pending exit for the rest of the session while
    /// the reconcile pass kept paying its costs. A per-identity TTL on the
    /// backlog does not fix that; it only mass-abandons entries that were
    /// genuinely reapable. Bounding the retry bounds the gate itself.
    ///
    /// A PID abandoned here is absent from `known`, and `plan_shell_exit`'s
    /// walk only reaches processes linked through `known`, so its absence can
    /// only *disconnect* (spare) a subtree — never expand the reap set.
    pub(crate) fn bump_unresolved_retries(&mut self, now_ms: u64) -> Vec<u32> {
        let mut abandoned = Vec::new();
        for pid in self.unresolved_new_pids.iter().copied().collect::<Vec<_>>() {
            let attempts = self.unresolved_retries.entry(pid).or_insert(0);
            *attempts += 1;
            if *attempts >= MAX_METADATA_RETRIES {
                self.unresolved_new_pids.remove(&pid);
                self.unresolved_retries.remove(&pid);
                self.push_unkeyable(pid, now_ms);
                abandoned.push(pid);
            }
        }
        self.unresolved_retries
            .retain(|pid, _| self.unresolved_new_pids.contains(pid));
        abandoned
    }

    fn push_unkeyable(&mut self, pid: u32, now_ms: u64) {
        self.unkeyable.push_back((pid, now_ms));
        while self.unkeyable.len() > MAX_UNKEYABLE {
            self.unkeyable.pop_front();
        }
    }

    /// Identities abandoned at runtime, drained for the exit sweep.
    // Consumed by the Win32 listener in `imp`, which does not exist off
    // Windows. `model` still compiles under `cfg(test)` everywhere so the
    // decision table is asserted on every platform — which leaves this
    // genuinely unreferenced in a non-Windows `lib test` build.
    #[cfg_attr(not(windows), allow(dead_code))]
    pub(crate) fn take_abandoned(&mut self) -> Vec<(u32, u64)> {
        self.abandoned.drain(..).collect()
    }

    /// The **one** purge sweep over the tracked keyspace.
    ///
    /// An identity is evictable when it is dead, its grace window has elapsed,
    /// it is no longer pending, and **no live descendant remains beneath it**.
    /// That last clause is load-bearing: `plan_shell_exit` walks `parent_pid`
    /// links *through* `known`, so evicting an interior dead node disconnects
    /// its subtree. The disconnection is spare-biased — `has_non_shell_client`
    /// and `inherited_spare` both propagate through the walk, so removal can
    /// only make descendants *less* reapable — but it would silently disable
    /// reaping of that subtree, which is not a trade worth making for a node
    /// whose descendants are still running.
    ///
    /// The predicate is deliberately independent of `processed_exits`: that set
    /// only ever receives shell-role entries with non-empty decisions, while
    /// `known` is dominated by dead `git.exe`/`rg.exe`/`node.exe`/`conhost.exe`
    /// that never reach it. A predicate keyed on it would prune almost nothing.
    ///
    /// Companions are evicted with `known` in the same step, never before it:
    /// dropping `processed_exits[(pid, st)]` while a dead `known[pid]` survives
    /// would make `pending_exit_pids` resurrect the identity and re-finalize it
    /// forever.
    pub(crate) fn purge(&mut self, now_ms: u64) -> usize {
        let protected = self.identities_with_live_descendants();
        let evictable: Vec<(u32, u64)> = self
            .known
            .values()
            .filter(|process| !process.alive && !protected.contains(&process.pid))
            .filter(|process| !self.is_pending(process))
            .filter(|process| {
                process
                    .exited_at_ms
                    .is_some_and(|at| now_ms.saturating_sub(at) >= EVICTION_GRACE_MS)
            })
            .map(ProcessMeta::identity)
            .collect();

        for identity in &evictable {
            self.known.remove(&identity.0);
            self.processed_exits.remove(identity);
            self.provisional_empty_exits.remove(identity);
            self.deferred_since.remove(identity);
        }

        self.unkeyable
            .retain(|(_, seen)| now_ms.saturating_sub(*seen) < UNKEYABLE_TTL_MS);
        // A deferral for an identity that is no longer tracked has nothing left
        // to age out.
        let known = &self.known;
        self.deferred_since
            .retain(|(pid, start_time), _| known.get(pid).is_some_and(|p| p.start_time == *start_time));

        evictable.len()
    }

    fn is_pending(&self, process: &ProcessMeta) -> bool {
        is_shell_image(&process.image_name)
            && !self.processed_exits.contains(&process.identity())
    }

    /// PIDs that still have a live process somewhere beneath them in `known`.
    fn identities_with_live_descendants(&self) -> HashSet<u32> {
        let mut protected = HashSet::new();
        for process in self.known.values().filter(|process| process.alive) {
            let mut cursor = process.parent_pid;
            // `insert` returning false both dedupes shared ancestry and breaks
            // any cycle a recycled PID could have created.
            while let Some(parent) = self.known.get(&cursor) {
                if !protected.insert(parent.pid) {
                    break;
                }
                cursor = parent.parent_pid;
            }
        }
        protected
    }

    /// Total tracked entries across every map — the number #673 says must stop
    /// tracking session age.
    #[cfg(test)]
    pub(crate) fn tracked_len(&self) -> usize {
        self.known.len()
            + self.processed_exits.len()
            + self.provisional_empty_exits.len()
            + self.unresolved_new_pids.len()
            + self.unresolved_retries.len()
            + self.unkeyable.len()
            + self.deferred_since.len()
            + self.abandoned.len()
    }
}

// ---------------------------------------------------------------------------
// #706 — completion-port message kinds, and the batch-folding rule.
//
// The constants live here rather than in the Win32 listener so the folding
// rule below is unit-testable on every platform, the same way #674 moved the
// decision table out of `imp`.
// ---------------------------------------------------------------------------

/// `JOB_OBJECT_MSG_ACTIVE_PROCESS_ZERO`.
pub(crate) const ACTIVE_PROCESS_ZERO: u32 = 4;
/// `JOB_OBJECT_MSG_NEW_PROCESS`.
pub(crate) const NEW_PROCESS: u32 = 6;
/// `JOB_OBJECT_MSG_EXIT_PROCESS`.
pub(crate) const EXIT_PROCESS: u32 = 7;

/// Does a drained completion-port batch leave anything to reconcile?
///
/// One reconcile pass per *batch* replaces one per *message* (#706). The pass
/// re-plans the entire pending set, so folding is safe: N passes over a
/// backlog that only grew produce the same end state as one pass after the
/// last message.
///
/// `ACTIVE_PROCESS_ZERO` is the exception that makes this worth a function.
/// It empties the tracker outright, so everything queued before it in the same
/// batch has nothing left to reconcile — but a message *after* it re-arms the
/// pass against the fresh state.
pub(crate) fn batch_needs_reconcile(messages: &[u32]) -> bool {
    let mut needs = false;
    for &message in messages {
        match message {
            NEW_PROCESS | EXIT_PROCESS => needs = true,
            ACTIVE_PROCESS_ZERO => needs = false,
            _ => {}
        }
    }
    needs
}

#[derive(Clone, Copy)]
pub(super) struct ReplayControl {
    pub(super) finalize_provisional_empty: bool,
    pub(super) metadata_complete: bool,
    pub(super) now_ms: u64,
}

/// What one pending exit resolved to this pass.
#[derive(Debug)]
pub(crate) enum ExitClaim {
    /// Finalized: these decisions are the caller's to execute.
    Execute(Vec<ReapDecision>),
    /// Still pending; a later event may resolve it.
    Deferred,
    /// Gave up. Queued for the exit sweep rather than dropped.
    Abandoned,
}

impl ExitClaim {
    #[cfg(test)]
    pub(crate) fn into_decisions(self) -> Option<Vec<ReapDecision>> {
        match self {
            Self::Execute(decisions) => Some(decisions),
            _ => None,
        }
    }
}

/// Claim one pending shell exit against an **already-built** graph.
///
/// The graph is built once per reconcile pass and shared across every pending
/// PID in it. It used to be rebuilt here from `known.values().cloned()` — a
/// full deep clone of every process the session ever spawned, per pending PID,
/// per 200 ms tick (#673 Phase 3).
pub(super) fn claim_exit_replay(
    graph: &ProcessGraph<'_>,
    spares: &SpareList,
    ledger: &mut ExitLedger<'_>,
    exited_pid: u32,
    control: ReplayControl,
) -> ExitClaim {
    if !control.metadata_complete {
        // Missing metadata can hide a nested shell / conhost / daemon
        // boundary beneath an otherwise reapable ancestor. Never plan from an
        // incomplete graph: ambiguity is a false-negative cleanup, not a
        // destructive false positive.
        return ExitClaim::Deferred;
    }
    let Some(trigger) = graph.meta(exited_pid) else {
        return ExitClaim::Deferred;
    };
    let exit_identity = trigger.identity();
    if ledger.processed_exits.contains(&exit_identity) {
        return ExitClaim::Deferred;
    }

    let decisions = plan_shell_exit(graph, spares, exited_pid);
    if decisions.is_empty() {
        // Registration or descendant metadata may arrive after the exit
        // notification. Leave this identity pending so either event can
        // replay it — but not forever: nothing else ages this branch out, and
        // before #673 Phase 2c an identity that landed here stayed in the
        // backlog for the session's life, paying reconcile cost every tick.
        //
        // Finalizing loses only the chance that late metadata would have
        // produced a decision, so the TTL is the aggressive one.
        if ledger.defer_expired(exit_identity, control.now_ms, EMPTY_PLAN_DEFER_MS) {
            ledger.abandon(exit_identity);
            return ExitClaim::Abandoned;
        }
        return ExitClaim::Deferred;
    }
    if decisions.len() == 1 && decisions[0].reason == ReapDecisionReason::NoLiveClients {
        // Job notifications can precede Toolhelp metadata publication. "No
        // clients yet" is therefore provisional until one completion-port
        // quiet period has elapsed. A NEW_PROCESS event during that period
        // can expose and reap the leaked child; genuinely empty tools are then
        // finalized instead of accumulating forever.
        if !control.finalize_provisional_empty
            || !ledger.provisional_empty_exits.contains(&exit_identity)
        {
            ledger.provisional_empty_exits.insert(exit_identity);
            // This branch is a possible *real leak*, so its TTL is the
            // conservative one and exists only as a backstop for the case
            // where the quiet period somehow never arrives.
            if ledger.defer_expired(exit_identity, control.now_ms, PROVISIONAL_DEFER_MS) {
                ledger.abandon(exit_identity);
                return ExitClaim::Abandoned;
            }
            return ExitClaim::Deferred;
        }
    }
    ledger.finalize(exit_identity);
    ExitClaim::Execute(decisions)
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

    pub fn sweep_abandoned_at_exit(&self) -> usize {
        0
    }

    pub fn finish_and_report(&self, _measure: bool) -> Vec<String> {
        Vec::new()
    }
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

    use super::{
        DecisionAction, FactsSnapshot, ProcessMeta, ReapDecision, RegisteredBackend, Signal,
    };

    use super::{batch_needs_reconcile, ACTIVE_PROCESS_ZERO, EXIT_PROCESS, NEW_PROCESS};

    /// Upper bound on completion-port messages folded into one batch (#706).
    ///
    /// The drain is non-blocking, so this is not a latency knob — it only
    /// bounds the work done between two `stop` checks, so a session that is
    /// spawning processes faster than the reaper can classify them still
    /// shuts down promptly. Anything left in the queue is picked up by the
    /// next iteration.
    const MAX_DRAIN_BATCH: usize = 256;

    use super::TrackerProcesses;

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
                let collector = Arc::new(Mutex::new(FactsCollector {
                    daemon_marker: crate::process_scan::DaemonMarkerCache::new(),
                    spare_images: super::configured_spare_images(
                        std::env::var("CLUD_REAPER_SPARE_IMAGES").ok().as_deref(),
                    ),
                }));
                let telemetry = Arc::new(Mutex::new(Telemetry {
                    log: session_reap_log(),
                    ..Telemetry::default()
                }));
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
                        move || {
                            listen(
                                HANDLE(port_value as *mut c_void),
                                job_value,
                                stop,
                                backends,
                                processes,
                                collector,
                                telemetry,
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
            if let Some(mut process) = snapshot().remove(&pid) {
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
            let live = snapshot();

            let Ok(mut tracker) = self.processes.lock() else {
                return 0;
            };
            // Refresh liveness from the fresh table without disturbing the
            // recorded identities: a PID absent from it has exited, and a PID
            // present under a *different* identity is a recycled number that
            // must not inherit the old process's place in the tree.
            for process in tracker.known.values_mut() {
                if !live.contains_key(&process.pid) {
                    process.alive = false;
                } else if process.alive {
                    process.alive =
                        crate::process_identity::start_time_of(process.pid) == process.start_time;
                }
            }

            let (known, _ledger) = tracker.split();
            let graph = super::ProcessGraph::build(known.values(), &registered);
            let facts = {
                let Ok(mut collector) = self.collector.lock() else {
                    return 0;
                };
                collect_facts(self.job.0 as usize, &graph, &mut collector)
            };
            let spares = super::build_spare_list(&facts, graph.spare_candidates());

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
                let decisions = super::plan_shell_exit(&graph, &spares, pid);
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
        job_value: usize,
        stop: Arc<AtomicBool>,
        backends: Arc<Mutex<Vec<RegisteredBackend>>>,
        processes: Arc<Mutex<TrackerProcesses>>,
        collector: Arc<Mutex<FactsCollector>>,
        telemetry: Arc<Mutex<Telemetry>>,
    ) {
        while !stop.load(Ordering::Acquire) {
            with_telemetry(&telemetry, |t| t.epoch.ticks += 1);
            let (mut message, mut key, mut payload) = (0u32, 0usize, null_mut());
            if unsafe { GetQueuedCompletionStatus(port, &mut message, &mut key, &mut payload, 200) }
                .is_err()
            {
                if unsafe { GetLastError().0 } == WAIT_TIMEOUT.0 {
                    let metadata_complete = retry_unresolved_new_processes(&processes, &telemetry);
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
                        job_value,
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
                if unsafe { GetQueuedCompletionStatus(port, &mut m, &mut k, &mut p, 0) }.is_err() {
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
                &batch, &processes, &backends, &collector, &telemetry, job_value,
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
        job_value: usize,
    ) {
        let mut table = BatchTable::new(telemetry);

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
                        super::record_new_process_observation(
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
                        super::record_process_exit(
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
                            super::pending_exit_pids(&processes.known, &processes.processed_exits)
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
    }

    impl<'a> BatchTable<'a> {
        fn new(telemetry: &'a Arc<Mutex<Telemetry>>) -> Self {
            Self {
                table: None,
                telemetry,
            }
        }

        /// Observe `pid` against this batch's table, stamping the authoritative
        /// creation time from the process handle.
        ///
        /// `start_time_of` is a per-PID `OpenProcess` + `GetProcessTimes` —
        /// microseconds, and unlike the table it cannot be stale — so it stays
        /// per-call rather than being folded into the shared read.
        fn observe(&mut self, pid: u32) -> Option<ProcessMeta> {
            let telemetry = self.telemetry;
            let table = self.table.get_or_insert_with(|| {
                with_telemetry(telemetry, |t| t.epoch.host_scans += 1);
                snapshot()
            });
            let mut process = table.get(&pid).cloned()?;
            process.start_time = crate::process_identity::start_time_of(pid);
            Some(process)
        }
    }

    fn retry_unresolved_new_processes(
        processes: &Mutex<TrackerProcesses>,
        telemetry: &Mutex<Telemetry>,
    ) -> bool {
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
            return true;
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
        let control = super::ReplayControl {
            finalize_provisional_empty,
            metadata_complete: tracker.metadata_complete(),
            now_ms: now,
        };

        let (known, mut ledger) = tracker.split();
        let pending = super::pending_exit_pids(known, ledger.processed_exits);
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

        let graph = super::ProcessGraph::build(known.values(), &registered);
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
        let spares = super::build_spare_list(&facts, graph.spare_candidates());

        let mut claimed = Vec::new();
        let mut abandoned = Vec::new();
        for pid in pending {
            match super::claim_exit_replay(&graph, &spares, &mut ledger, pid, control) {
                super::ExitClaim::Execute(decisions) => claimed.push(decisions),
                super::ExitClaim::Abandoned => abandoned.push((
                    pid,
                    graph.meta(pid).map(|meta| meta.start_time),
                    graph
                        .meta(pid)
                        .map(|meta| meta.image_name.clone())
                        .unwrap_or_default(),
                )),
                super::ExitClaim::Deferred => {}
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
        graph: &super::ProcessGraph<'_>,
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

        snapshot
    }

    /// The Win32 half of the [`super::ProcessFacts`] signals.
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
        spares: &super::SpareList,
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
                && !super::candidate_identity_is_live(&decision)
            {
                // A downgrade is still a decision, and it counts as a spare in
                // the `decisions_emitted == reaped + spared` identity.
                decision.action = DecisionAction::Spare;
                decision.reason = super::ReapDecisionReason::CandidateIdentityChanged;
                log_decision(&decision);
                census(telemetry, &decision, phase, ts_ms);
                continue;
            }

            log_decision(&decision);
            census(telemetry, &decision, phase, ts_ms);
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
                start_time: None,
                image_name: None,
                // Fails closed: an unresolvable process is never a kill target.
                action: ReapAction::Spared,
                reason: "process_metadata_unavailable",
                phase: ReapPhase::Runtime,
            });
        });
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
}
#[cfg(windows)]
pub use imp::ForegroundJobTracker;

/// #706 — the completion-port batch-folding rule.
///
/// Tier 1 (per `agents/docs`): pure, platform-free, no Job Object required.
/// The listener that consumes this is Windows-only, but the rule that decides
/// how many reconcile passes a drained batch costs is asserted everywhere.
#[cfg(test)]
mod batch_tests {
    use super::{batch_needs_reconcile, ACTIVE_PROCESS_ZERO, EXIT_PROCESS, NEW_PROCESS};

    #[test]
    fn an_empty_batch_reconciles_nothing() {
        assert!(!batch_needs_reconcile(&[]));
    }

    #[test]
    fn a_spawn_or_exit_arms_one_pass() {
        assert!(batch_needs_reconcile(&[NEW_PROCESS]));
        assert!(batch_needs_reconcile(&[EXIT_PROCESS]));
    }

    #[test]
    fn many_messages_still_cost_exactly_one_pass() {
        // The whole point of #706: this batch used to be 200 reconcile
        // passes and 200 full host process-table enumerations.
        let batch = [NEW_PROCESS; 200];
        assert!(batch_needs_reconcile(&batch));
    }

    #[test]
    fn active_process_zero_alone_reconciles_nothing() {
        // The reset empties the tracker; there is no backlog left to plan.
        assert!(!batch_needs_reconcile(&[ACTIVE_PROCESS_ZERO]));
    }

    #[test]
    fn active_process_zero_cancels_only_what_preceded_it() {
        assert!(!batch_needs_reconcile(&[
            NEW_PROCESS,
            EXIT_PROCESS,
            ACTIVE_PROCESS_ZERO,
        ]));
    }

    #[test]
    fn a_message_after_active_process_zero_rearms_the_pass() {
        // Ordering matters: the job emptied, then something new spawned into
        // it. That spawn must still be reconciled against the fresh state.
        assert!(batch_needs_reconcile(&[
            NEW_PROCESS,
            ACTIVE_PROCESS_ZERO,
            NEW_PROCESS,
        ]));
    }

    #[test]
    fn unknown_message_kinds_are_inert() {
        // The listener ignores kinds it does not model; they must not arm a
        // pass on their own, nor cancel one.
        assert!(!batch_needs_reconcile(&[0, 1, 2, 3, 5, 8, 9]));
        assert!(batch_needs_reconcile(&[NEW_PROCESS, 9]));
        assert!(!batch_needs_reconcile(&[ACTIVE_PROCESS_ZERO, 9]));
    }
}

#[cfg(test)]
mod lifecycle_tests {
    use std::collections::{HashMap, HashSet};

    use super::{
        DecisionAction, ProcessMeta, ProcessRole, ReapDecisionReason, RegisteredBackend, SpareList,
    };

    fn process(pid: u32, parent_pid: u32, image_name: &str, alive: bool) -> ProcessMeta {
        ProcessMeta {
            pid,
            parent_pid,
            image_name: image_name.to_string(),
            alive,
            start_time: pid as u64,
            exited_at_ms: (!alive).then_some(0),
        }
    }

    /// The declared-daemon set these cases were written against, expressed in
    /// the spare-list the planner now takes. The reason is asserted by the
    /// cases themselves, so this must map to the same one the marker produces.
    fn declared(daemons: &HashSet<u32>) -> SpareList {
        daemons
            .iter()
            .map(|pid| (*pid, ReapDecisionReason::DeclaredDaemon))
            .collect()
    }

    /// Adapter preserving the pre-#673 call shape. Production builds the graph
    /// once per reconcile pass and shares it across the whole backlog; a unit
    /// case is a single exit over a synthetic process list, so building it per
    /// call keeps each case a one-liner.
    fn plan_shell_exit(
        processes: &[ProcessMeta],
        backends: &[RegisteredBackend],
        daemons: &HashSet<u32>,
        exited_pid: u32,
    ) -> Vec<super::ReapDecision> {
        let graph = super::ProcessGraph::build(processes.iter(), backends);
        super::plan_shell_exit(&graph, &declared(daemons), exited_pid)
    }

    fn claim_exit_replay(
        known: &HashMap<u32, ProcessMeta>,
        backends: &[RegisteredBackend],
        daemons: &HashSet<u32>,
        processed_exits: &mut HashSet<(u32, u64)>,
        provisional_empty_exits: &mut HashSet<(u32, u64)>,
        exited_pid: u32,
        control: super::ReplayControl,
    ) -> Option<Vec<super::ReapDecision>> {
        let mut deferred_since = HashMap::new();
        let mut abandoned = std::collections::VecDeque::new();
        let graph = super::ProcessGraph::build(known.values(), backends);
        super::claim_exit_replay(
            &graph,
            &declared(daemons),
            &mut super::ExitLedger {
                processed_exits,
                provisional_empty_exits,
                deferred_since: &mut deferred_since,
                abandoned: &mut abandoned,
            },
            exited_pid,
            control,
        )
        .into_decisions()
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
            now_ms: 0,
            finalize_provisional_empty,
            metadata_complete,
        }
    }

    // ---- #673 Phase 2: the tracked keyspace stays bounded ----

    fn tracker_of(processes: Vec<ProcessMeta>) -> super::TrackerProcesses {
        let mut tracker = super::TrackerProcesses::default();
        for process in processes {
            tracker.known.insert(process.pid, process);
        }
        tracker
    }

    fn exited(mut process: ProcessMeta, at_ms: u64) -> ProcessMeta {
        process.alive = false;
        process.exited_at_ms = Some(at_ms);
        process
    }

    /// The headline bound. 500 short-lived **non-shell** exits are the traffic
    /// `known` is actually dominated by — `git.exe`/`node.exe` that never reach
    /// `processed_exits` at all, which is why a predicate keyed on that set
    /// would have pruned nothing.
    #[test]
    fn five_hundred_non_shell_exits_leave_every_map_bounded() {
        let mut tracker = tracker_of(vec![process(10, 1, "codex.exe", true)]);

        for round in 0..500u32 {
            let pid = 1000 + round;
            let mut unresolved = HashSet::new();
            super::record_new_process_observation(
                &mut tracker.known,
                &mut unresolved,
                pid,
                Some(process(pid, 10, "git.exe", true)),
            );
            super::record_process_exit(
                &mut tracker.known,
                &mut tracker.unresolved_new_pids,
                pid,
                None,
                u64::from(round),
            );
            tracker.purge(u64::from(round) + super::EVICTION_GRACE_MS);
        }

        // Only the live backend root survives; nothing accumulated anywhere.
        assert_eq!(tracker.known.len(), 1);
        assert!(tracker.known.contains_key(&10));
        assert_eq!(
            tracker.tracked_len(),
            1,
            "every map must be bounded, not just `known`"
        );
    }

    /// The grace window is real: an exit observed a moment ago is still there,
    /// because completion-port notifications can arrive out of order.
    #[test]
    fn a_freshly_exited_identity_survives_its_grace_window() {
        let mut tracker = tracker_of(vec![
            process(10, 1, "codex.exe", true),
            exited(process(21, 10, "git.exe", false), 1_000),
        ]);

        tracker.purge(1_000 + super::EVICTION_GRACE_MS - 1);
        assert!(tracker.known.contains_key(&21));

        tracker.purge(1_000 + super::EVICTION_GRACE_MS);
        assert!(!tracker.known.contains_key(&21));
    }

    /// The load-bearing clause: `plan_shell_exit` walks parent links *through*
    /// `known`, so evicting an interior dead node would disconnect — and
    /// silently stop reaping — everything beneath it.
    #[test]
    fn an_interior_dead_node_with_live_descendants_is_never_evicted() {
        let mut tracker = tracker_of(vec![
            process(10, 1, "codex.exe", true),
            exited(process(20, 10, "powershell.exe", false), 0),
            exited(process(21, 20, "wrapper.exe", false), 0),
            process(22, 21, "node.exe", true), // still running
        ]);
        tracker.processed_exits.insert((20, 20));

        tracker.purge(super::EVICTION_GRACE_MS * 10);

        assert!(tracker.known.contains_key(&20), "shell root must stay");
        assert!(tracker.known.contains_key(&21), "interior node must stay");
        assert!(tracker.known.contains_key(&22));
    }

    /// ...and once the subtree is genuinely gone, the whole chain goes with it.
    #[test]
    fn a_fully_dead_subtree_is_evicted_together_with_its_companions() {
        let mut tracker = tracker_of(vec![
            process(10, 1, "codex.exe", true),
            exited(process(20, 10, "powershell.exe", false), 0),
            exited(process(21, 20, "wrapper.exe", false), 0),
            exited(process(22, 21, "node.exe", false), 0),
        ]);
        tracker.processed_exits.insert((20, 20));
        tracker.provisional_empty_exits.insert((20, 20));

        assert_eq!(tracker.purge(super::EVICTION_GRACE_MS), 3);
        assert_eq!(tracker.known.len(), 1);
        assert!(
            tracker.processed_exits.is_empty() && tracker.provisional_empty_exits.is_empty(),
            "companions must be evicted with `known`, in the same step"
        );
    }

    /// A dead shell that has not been finalized is still *pending*. Evicting it
    /// would drop the exit silently; abandonment is the only way out, and it is
    /// what makes the identity evictable afterwards.
    #[test]
    fn a_still_pending_shell_exit_is_not_evicted_out_from_under_the_backlog() {
        let mut tracker = tracker_of(vec![
            process(10, 1, "codex.exe", true),
            exited(process(20, 10, "powershell.exe", false), 0),
        ]);

        tracker.purge(super::EVICTION_GRACE_MS * 10);
        assert!(tracker.known.contains_key(&20));
        assert_eq!(
            super::pending_exit_pids(&tracker.known, &tracker.processed_exits),
            vec![20]
        );

        tracker.processed_exits.insert((20, 20));
        tracker.purge(super::EVICTION_GRACE_MS * 10);
        assert!(!tracker.known.contains_key(&20));
    }

    /// #673 Phase 2b. The finalization gate is global, so before this one
    /// permanently-unresolvable PID blocked *every* pending exit for the
    /// session's life while the reconcile pass kept paying its costs.
    #[test]
    fn one_unresolvable_pid_stops_blocking_finalization_after_bounded_retries() {
        let mut tracker = super::TrackerProcesses::default();
        tracker.unresolved_new_pids.insert(999);
        assert!(!tracker.metadata_complete());

        for round in 1..super::MAX_METADATA_RETRIES {
            assert!(tracker.bump_unresolved_retries(u64::from(round)).is_empty());
            assert!(!tracker.metadata_complete(), "round {round} gave up early");
        }

        assert_eq!(
            tracker.bump_unresolved_retries(u64::from(super::MAX_METADATA_RETRIES)),
            vec![999]
        );
        assert!(tracker.metadata_complete());
        assert!(tracker.unresolved_new_pids.is_empty());
    }

    /// The unkeyable holding pen can never be drained by retrying, so it gets
    /// both a hard cap and a hard TTL.
    #[test]
    fn the_unkeyable_holding_pen_is_capped_and_expires() {
        let mut tracker = super::TrackerProcesses::default();
        for pid in 0..(super::MAX_UNKEYABLE as u32 * 2) {
            tracker.unresolved_new_pids.insert(pid);
            for _ in 0..super::MAX_METADATA_RETRIES {
                tracker.bump_unresolved_retries(0);
            }
        }
        assert_eq!(tracker.unkeyable.len(), super::MAX_UNKEYABLE);

        tracker.purge(super::UNKEYABLE_TTL_MS);
        assert!(tracker.unkeyable.is_empty());
    }

    /// #673 Phase 2c. Nothing else ages the "plan produced nothing to kill"
    /// branch out, so before this an identity that landed there stayed in the
    /// backlog forever, costing a tree walk every tick.
    #[test]
    fn an_empty_plan_is_abandoned_on_the_aggressive_ttl() {
        // A shell exit under no registered backend never produces a plan.
        let known: HashMap<u32, ProcessMeta> = [
            process(10, 1, "codex.exe", true),
            exited(process(20, 10, "powershell.exe", false), 0),
        ]
        .into_iter()
        .map(|process| (process.pid, process))
        .collect();
        let graph = super::ProcessGraph::build(known.values(), &[]);
        let spares = SpareList::new();

        let mut processed = HashSet::new();
        let mut provisional = HashSet::new();
        let mut deferred = HashMap::new();
        let mut abandoned = std::collections::VecDeque::new();
        let mut ledger = super::ExitLedger {
            processed_exits: &mut processed,
            provisional_empty_exits: &mut provisional,
            deferred_since: &mut deferred,
            abandoned: &mut abandoned,
        };

        let deferred_claim = super::claim_exit_replay(
            &graph,
            &spares,
            &mut ledger,
            20,
            super::ReplayControl {
                now_ms: 0,
                finalize_provisional_empty: false,
                metadata_complete: true,
            },
        );
        assert!(matches!(deferred_claim, super::ExitClaim::Deferred));
        assert!(ledger.abandoned.is_empty());

        let expired = super::claim_exit_replay(
            &graph,
            &spares,
            &mut ledger,
            20,
            super::ReplayControl {
                now_ms: super::EMPTY_PLAN_DEFER_MS,
                finalize_provisional_empty: false,
                metadata_complete: true,
            },
        );
        assert!(matches!(expired, super::ExitClaim::Abandoned));
        // #673 Phase 2d: abandonment is a hand-off to the exit sweep, not a
        // silent drop -- a leaked node.exe holding a port for six hours is the
        // bug this reaper exists to prevent.
        assert_eq!(abandoned.into_iter().collect::<Vec<_>>(), vec![(20, 20)]);
        // ...and it stops being pending, so the backlog actually drains.
        assert!(super::pending_exit_pids(&known, &processed).is_empty());
    }

    /// The two deferral branches must not share a TTL. "The plan produced
    /// nothing to kill" is cheap to give up on; "no live clients yet" is a
    /// possible real leak, so it must still be pending at the point the other
    /// branch would already have been abandoned.
    #[test]
    fn the_possible_leak_branch_outlives_the_empty_plan_ttl() {
        let known: HashMap<u32, ProcessMeta> = [
            process(10, 1, "codex.exe", true),
            exited(process(20, 10, "powershell.exe", false), 0),
        ]
        .into_iter()
        .map(|process| (process.pid, process))
        .collect();
        let registered = [RegisteredBackend::new(10, "codex.exe", 10)];
        let graph = super::ProcessGraph::build(known.values(), &registered);
        let spares = SpareList::new();

        let mut processed = HashSet::new();
        let mut provisional = HashSet::new();
        let mut deferred = HashMap::new();
        let mut abandoned = std::collections::VecDeque::new();
        let mut ledger = super::ExitLedger {
            processed_exits: &mut processed,
            provisional_empty_exits: &mut provisional,
            deferred_since: &mut deferred,
            abandoned: &mut abandoned,
        };
        let claim = |ledger: &mut super::ExitLedger<'_>, now_ms| {
            super::claim_exit_replay(
                &graph,
                &spares,
                ledger,
                20,
                super::ReplayControl {
                    now_ms,
                    finalize_provisional_empty: false,
                    metadata_complete: true,
                },
            )
        };

        assert!(matches!(claim(&mut ledger, 0), super::ExitClaim::Deferred));
        assert!(
            matches!(
                claim(&mut ledger, super::EMPTY_PLAN_DEFER_MS),
                super::ExitClaim::Deferred
            ),
            "the aggressive TTL must not apply to the possible-leak branch"
        );
        assert!(matches!(
            claim(&mut ledger, super::PROVISIONAL_DEFER_MS),
            super::ExitClaim::Abandoned
        ));
    }

    // ---- #673 Phase 1a: the OS-signal spare-list, as pure data ----

    fn facts_with(f: impl FnOnce(&mut super::FactsSnapshot)) -> super::FactsSnapshot {
        let mut facts = super::FactsSnapshot {
            unavailable: HashSet::from([super::Signal::SessionLeader]),
            ..super::FactsSnapshot::default()
        };
        f(&mut facts);
        facts
    }

    fn signal_for(
        facts: &super::FactsSnapshot,
        pid: u32,
        image: &str,
    ) -> Option<ReapDecisionReason> {
        super::spare_signal(facts, pid, image)
    }

    /// Nothing protects an ordinary leaked client. Guard against the fix
    /// over-sparing into uselessness.
    #[test]
    fn a_process_with_no_signal_is_not_spared() {
        let facts = facts_with(|_| {});
        assert_eq!(signal_for(&facts, 21, "node.exe"), None);
    }

    /// A process that broke away from our Job Object was never ours to kill,
    /// and that is the cheapest and strongest thing we can know.
    #[test]
    fn job_breakaway_is_spared_before_anything_else_is_consulted() {
        let facts = facts_with(|facts| {
            facts.outside_job.insert(21);
            // Deliberately *also* declared: the assertion is about which
            // reason wins, not merely that it is spared.
            facts.declared_daemons.insert(21);
        });
        assert_eq!(
            signal_for(&facts, 21, "zccache.exe"),
            Some(ReapDecisionReason::OutsideJobObject)
        );
    }

    /// dockerd runs as a Windows service in session 0. It is spared for that
    /// reason even if it somehow carries our tag.
    #[test]
    fn a_session_zero_service_is_spared_as_a_service() {
        let facts = facts_with(|facts| {
            facts.service_session.insert(30);
        });
        assert_eq!(
            signal_for(&facts, 30, "dockerd.exe"),
            Some(ReapDecisionReason::ServiceSession)
        );
    }

    /// The sccache-shaped case: no marker, no breakaway, but it owns a
    /// listening endpoint, which is what makes it discoverable and reusable by
    /// later unrelated invocations. This is the row that had no protection at
    /// all before #673.
    #[test]
    fn marker_absence_is_not_permission_to_kill_a_listening_server() {
        let facts = facts_with(|facts| {
            facts.listening.insert(40);
        });
        assert!(facts.declared_daemons.is_empty());
        assert_eq!(
            signal_for(&facts, 40, "sccache.exe"),
            Some(ReapDecisionReason::ListeningEndpoint)
        );
    }

    /// The cooperative marker still protects the daemons that do opt in, and
    /// it ranks below every OS signal precisely because opting in is optional.
    #[test]
    fn the_cooperative_marker_ranks_below_every_os_signal() {
        let marked = facts_with(|facts| {
            facts.declared_daemons.insert(50);
        });
        assert_eq!(
            signal_for(&marked, 50, "zccache.exe"),
            Some(ReapDecisionReason::DeclaredDaemon)
        );

        for (build, expected) in [
            (
                Box::new(|f: &mut super::FactsSnapshot| {
                    f.service_session.insert(50);
                }) as Box<dyn FnOnce(&mut super::FactsSnapshot)>,
                ReapDecisionReason::ServiceSession,
            ),
            (
                Box::new(|f: &mut super::FactsSnapshot| {
                    f.foreign_owner.insert(50);
                }),
                ReapDecisionReason::ForeignTokenOwner,
            ),
        ] {
            let facts = facts_with(|f| {
                f.declared_daemons.insert(50);
                build(f);
            });
            assert_eq!(signal_for(&facts, 50, "zccache.exe"), Some(expected));
        }
    }

    /// A signal the platform cannot answer must never spare. Absence of
    /// evidence is not evidence of daemon-hood — the inversion #522 fixed.
    #[test]
    fn an_unavailable_signal_never_spares() {
        let mut facts = facts_with(|facts| {
            facts.session_leaders.insert(60);
        });
        assert!(facts.unavailable.contains(&super::Signal::SessionLeader));
        assert_eq!(signal_for(&facts, 60, "sccache"), None);

        // ...and when the platform *can* answer it, it does spare.
        facts.unavailable.remove(&super::Signal::SessionLeader);
        assert_eq!(
            signal_for(&facts, 60, "sccache"),
            Some(ReapDecisionReason::SessionLeader)
        );
    }

    /// The whitelist is the last resort, is matched on basename
    /// case-insensitively, and is empty unless an operator supplies it.
    #[test]
    fn the_configured_spare_list_is_data_and_ranks_last() {
        assert!(super::configured_spare_images(None).is_empty());
        assert!(super::configured_spare_images(Some("  ,; ")).is_empty());
        assert_eq!(
            super::configured_spare_images(Some("FBuildWorker.exe, my-daemon.exe")),
            vec!["FBuildWorker.exe", "my-daemon.exe"]
        );

        let facts = facts_with(|facts| {
            facts.spare_images = super::configured_spare_images(Some("FBuildWorker.exe"));
        });
        assert_eq!(
            signal_for(&facts, 70, "C:\\tools\\FBUILDWORKER.EXE"),
            Some(ReapDecisionReason::ConfiguredSpareList)
        );
        assert_eq!(signal_for(&facts, 70, "node.exe"), None);
    }

    /// The spare-list is built over a *bounded* candidate set and carries the
    /// reason with it, so a later log says why a process survived.
    #[test]
    fn the_spare_list_carries_the_reason_for_each_candidate() {
        let facts = facts_with(|facts| {
            facts.outside_job.insert(21);
            facts.listening.insert(22);
        });
        let spares = super::build_spare_list(
            &facts,
            [
                (21u32, "zccache.exe".to_string()),
                (22, "sccache.exe".to_string()),
                (23, "node.exe".to_string()),
            ]
            .into_iter(),
        );
        assert_eq!(spares.len(), 2);
        assert_eq!(spares[&21], ReapDecisionReason::OutsideJobObject);
        assert_eq!(spares[&22], ReapDecisionReason::ListeningEndpoint);
        assert!(!spares.contains_key(&23));
    }

    // ---- #674 Tier 1: every daemon archetype, spare *and* reason ----

    /// The table #674 is about. Each row is a daemon that must survive a clud
    /// session, the OS signal that actually protects it, and the reason the
    /// decision must carry.
    ///
    /// The **reason** is asserted, not just the outcome: sparing zccache via
    /// "outside our job" is correct, while sparing it via "no decisions
    /// produced" would be an accident that regresses silently the next time
    /// the planner changes. That distinction is the whole point of this table.
    ///
    /// Runs on every platform, over synthetic graphs and injected facts. No
    /// spawning, no Job Object, no dependency on a real sccache or docker
    /// being installed.
    #[test]
    fn every_daemon_archetype_is_spared_for_the_right_reason() {
        struct Archetype {
            image: &'static str,
            /// How it detaches, expressed as the facts the OS would report.
            signal: fn(&mut super::FactsSnapshot, u32),
            expected: ReapDecisionReason,
        }

        let archetypes = [
            // zccache: detaches through the running-process daemon API and
            // breaks away from the job. Job membership is the strongest thing
            // we can know about it.
            Archetype {
                image: "zccache.exe",
                signal: |facts, pid| {
                    facts.outside_job.insert(pid);
                    facts.declared_daemons.insert(pid);
                },
                expected: ReapDecisionReason::OutsideJobObject,
            },
            // soldr-daemon: same API, but staying inside the job. The
            // cooperative marker is the only thing protecting it, which is the
            // load-bearing case #673 rev 3.0 nearly deleted.
            Archetype {
                image: "soldr-daemon.exe",
                signal: |facts, pid| {
                    facts.declared_daemons.insert(pid);
                },
                expected: ReapDecisionReason::DeclaredDaemon,
            },
            // sccache: own double-fork, no marker, no breakaway. It owns a
            // listening endpoint, which is what makes it discoverable and
            // reusable by later unrelated invocations. Before #673 this row
            // had no protection at all.
            Archetype {
                image: "sccache.exe",
                signal: |facts, pid| {
                    facts.listening.insert(pid);
                },
                expected: ReapDecisionReason::ListeningEndpoint,
            },
            // FBuildWorker: detached spawn, no marker, listens for work.
            Archetype {
                image: "FBuildWorker.exe",
                signal: |facts, pid| {
                    facts.listening.insert(pid);
                },
                expected: ReapDecisionReason::ListeningEndpoint,
            },
            // dockerd: a Windows service in session 0. Spared for that reason
            // even though it is not really a descendant at all.
            Archetype {
                image: "dockerd.exe",
                signal: |facts, pid| {
                    facts.service_session.insert(pid);
                },
                expected: ReapDecisionReason::ServiceSession,
            },
            // Language servers (rust-analyzer, tsserver): spawned by tooling
            // under clud, no marker, but they hold a socket for their client.
            Archetype {
                image: "rust-analyzer.exe",
                signal: |facts, pid| {
                    facts.listening.insert(pid);
                },
                expected: ReapDecisionReason::ListeningEndpoint,
            },
            // A double-fork daemon on POSIX, where session leadership is the
            // primary signal and the job object does not exist.
            Archetype {
                image: "sccache",
                signal: |facts, pid| {
                    facts.unavailable.remove(&super::Signal::SessionLeader);
                    facts.unavailable.insert(super::Signal::JobMembership);
                    facts.session_leaders.insert(pid);
                },
                expected: ReapDecisionReason::SessionLeader,
            },
        ];

        const DAEMON_PID: u32 = 40;
        for archetype in archetypes {
            let facts = facts_with(|facts| (archetype.signal)(facts, DAEMON_PID));

            // The signal alone, as data.
            assert_eq!(
                signal_for(&facts, DAEMON_PID, archetype.image),
                Some(archetype.expected),
                "{} must be spared as {:?}",
                archetype.image,
                archetype.expected
            );

            // ...and the same reason must survive into the planner's decision,
            // with the daemon's whole subtree pruned along with it.
            let processes = [
                process(10, 1, "cmd.exe", true),
                process(11, 10, "node.exe", true),
                process(12, 11, "codex.exe", true),
                process(20, 12, "powershell.exe", false),
                {
                    let mut daemon = process(DAEMON_PID, 20, archetype.image, true);
                    daemon.start_time = u64::from(DAEMON_PID);
                    daemon
                },
                process(41, DAEMON_PID, "worker.exe", true),
            ];
            let graph = super::ProcessGraph::build(
                processes.iter(),
                &[RegisteredBackend::new(10, "codex.exe", 10)],
            );
            let spares = super::build_spare_list(&facts, graph.spare_candidates());
            let decisions = super::plan_shell_exit(&graph, &spares, 20);

            assert_eq!(
                decisions.len(),
                1,
                "{}: one decision describes the whole pruned subtree",
                archetype.image
            );
            assert_eq!(decisions[0].action, DecisionAction::Spare);
            assert_eq!(
                decisions[0].reason, archetype.expected,
                "{}: the reason must be the signal that actually protected it, \
                 not an accident of the plan being empty",
                archetype.image
            );
            assert_eq!(decisions[0].candidate_pid, Some(DAEMON_PID));
        }
    }

    /// The counterweight to the table above. If sparing were over-broad every
    /// row would still pass while the reaper had quietly stopped working, so a
    /// leaked client with **none** of the daemon signals must still be reaped.
    #[test]
    fn a_leaked_client_with_no_daemon_signal_is_still_reaped() {
        let facts = facts_with(|_| {});
        let processes = [
            process(10, 1, "cmd.exe", true),
            process(11, 10, "node.exe", true),
            process(12, 11, "codex.exe", true),
            process(20, 12, "powershell.exe", false),
            process(40, 20, "node.exe", true),
        ];
        let graph = super::ProcessGraph::build(
            processes.iter(),
            &[RegisteredBackend::new(10, "codex.exe", 10)],
        );
        let spares = super::build_spare_list(&facts, graph.spare_candidates());
        assert!(spares.is_empty(), "nothing here deserves protection");

        let decisions = super::plan_shell_exit(&graph, &spares, 20);
        assert_eq!(decisions.len(), 1);
        assert_eq!(decisions[0].action, DecisionAction::Reap);
        assert_eq!(decisions[0].reason, ReapDecisionReason::LeakedToolClient);
        assert_eq!(decisions[0].candidate_pid, Some(40));
    }

    /// Session isolation, as a decision-table property: a process that is not
    /// in *our* job is spared for that reason, whatever else is true of it.
    /// The live two-tracker version is in `reaper_daemon_survival_windows.rs`;
    /// this is the part that does not need a real Job Object.
    #[test]
    fn another_sessions_descendant_is_out_of_scope_for_this_reaper() {
        let facts = facts_with(|facts| {
            facts.outside_job.insert(40);
        });
        assert_eq!(
            signal_for(&facts, 40, "node.exe"),
            Some(ReapDecisionReason::OutsideJobObject),
            "a sibling session's descendant is not ours to kill"
        );
    }

    /// The whole point of the seam: a spare-list built from OS signals prunes
    /// the daemon's subtree in the planner exactly as the marker set did, and
    /// the *reason* survives into the decision. Sparing zccache via
    /// "outside our job" is correct; sparing it via "no decisions produced"
    /// would be an accident that regresses silently.
    #[test]
    fn an_os_derived_spare_prunes_the_daemon_subtree_with_its_reason() {
        let processes = [
            process(10, 1, "cmd.exe", true),
            process(11, 10, "node.exe", true),
            process(12, 11, "codex.exe", true),
            process(20, 12, "powershell.exe", false),
            process(40, 20, "sccache.exe", true),
            process(41, 40, "cl.exe", true),
        ];
        let facts = facts_with(|facts| {
            facts.listening.insert(40);
        });
        let graph = super::ProcessGraph::build(
            processes.iter(),
            &[RegisteredBackend::new(10, "codex.exe", 10)],
        );
        let spares = super::build_spare_list(&facts, graph.spare_candidates());
        let decisions = super::plan_shell_exit(&graph, &spares, 20);

        assert_eq!(decisions.len(), 1);
        assert_eq!(decisions[0].action, DecisionAction::Spare);
        assert_eq!(decisions[0].reason, ReapDecisionReason::ListeningEndpoint);
        assert_eq!(decisions[0].candidate_pid, Some(40));
    }

    /// #673 Phase 3: the graph is an input, not something each pending exit
    /// rebuilds. One graph must serve the whole backlog with identical results.
    #[test]
    fn one_graph_serves_every_pending_exit_in_the_backlog() {
        let processes = vec![
            process(10, 1, "codex.exe", true),
            process(20, 10, "powershell.exe", false),
            process(21, 20, "git.exe", true),
            process(30, 10, "cmd.exe", false),
            process(31, 30, "rg.exe", true),
        ];
        let backends = [RegisteredBackend::new(10, "codex.exe", 10)];
        let graph = super::ProcessGraph::build(processes.iter(), &backends);
        let spares = SpareList::new();

        for (trigger, expected_candidate) in [(20u32, 21u32), (30, 31)] {
            let shared = super::plan_shell_exit(&graph, &spares, trigger);
            let standalone = plan_shell_exit(&processes, &backends, &HashSet::new(), trigger);
            assert_eq!(shared, standalone);
            assert_eq!(shared.len(), 1);
            assert_eq!(shared[0].action, DecisionAction::Reap);
            assert_eq!(shared[0].candidate_pid, Some(expected_candidate));
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
        assert!(claim_exit_replay(
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
        assert!(claim_exit_replay(
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
        let replayed = claim_exit_replay(
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
        assert!(claim_exit_replay(
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

        assert!(claim_exit_replay(
            &known,
            &registered,
            &HashSet::new(),
            &mut processed,
            &mut provisional,
            20,
            replay_control(false, true),
        )
        .is_none());
        let finalized = claim_exit_replay(
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
    fn unresolved_process_exit_takes_final_observation_or_reports_miss() {
        let mut known = std::collections::HashMap::new();
        let mut unresolved = HashSet::new();
        super::record_new_process_observation(&mut known, &mut unresolved, 21, None);

        // A final observation resolves the PID: no miss reported, and it lands
        // in `known` as a dead node.
        assert!(!super::record_process_exit(
            &mut known,
            &mut unresolved,
            21,
            Some(process(21, 20, "git.exe", true)),
            0,
        ));
        assert!(!known[&21].alive);
        assert!(unresolved.is_empty());

        // A PID that exits still unresolved reports a miss (for telemetry) and
        // is dropped from the live unresolved set. It is absent from `known`,
        // so it can never gate finalization or appear in the reap graph.
        super::record_new_process_observation(&mut known, &mut unresolved, 22, None);
        assert!(super::record_process_exit(
            &mut known,
            &mut unresolved,
            22,
            None,
            0,
        ));
        assert!(unresolved.is_empty());
        assert!(!known.contains_key(&22));
    }

    #[test]
    fn dead_unresolved_pid_does_not_wedge_finalization() {
        // Regression for #651: a short-lived process that exits before its
        // metadata resolves must not permanently block reaping. It is dropped
        // from `unresolved_new_pids` on exit and never enters `known`, so the
        // finalization gate (`unresolved_new_pids.is_empty()`) stays satisfied
        // and a well-understood shell exit can still be reaped.
        let mut known: std::collections::HashMap<_, _> = vec![
            process(10, 1, "codex.exe", true),        // backend, alive
            process(20, 10, "powershell.exe", false), // tool shell root, exited
            process(21, 20, "git.exe", true),         // leaked live client
        ]
        .into_iter()
        .map(|process| (process.pid, process))
        .collect();
        let mut unresolved = HashSet::new();

        // A ghost PID appears and exits before Toolhelp can resolve it.
        super::record_new_process_observation(&mut known, &mut unresolved, 99, None);
        assert_eq!(unresolved, HashSet::from([99]));
        assert!(super::record_process_exit(
            &mut known,
            &mut unresolved,
            99,
            None,
            0,
        ));
        // The ghost is gone from the live gate and never entered the graph.
        assert!(unresolved.is_empty());
        assert!(!known.contains_key(&99));

        // Finalization proceeds: the leaked client under the exited shell is reaped.
        let registered = [RegisteredBackend::new(10, "codex.exe", 10)];
        let mut processed = HashSet::new();
        let mut provisional = HashSet::new();
        let metadata_complete = unresolved.is_empty();
        let decisions = claim_exit_replay(
            &known,
            &registered,
            &HashSet::new(),
            &mut processed,
            &mut provisional,
            20,
            super::ReplayControl {
                now_ms: 0,
                finalize_provisional_empty: true,
                metadata_complete,
            },
        )
        .expect("dead ghost PID must not block finalization");
        assert!(decisions.iter().any(|decision| {
            decision.action == DecisionAction::Reap
                && decision.candidate_pid == Some(21)
                && decision.reason == ReapDecisionReason::LeakedToolClient
        }));
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

        assert!(claim_exit_replay(
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
        let decisions = claim_exit_replay(
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
