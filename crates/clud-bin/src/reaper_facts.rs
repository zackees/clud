//! OS-authoritative daemon-sparing signals, shared by **both** reapers (#688).
//!
//! #673 Phase 1a introduced the signal table so that a build cache, a language
//! server or a container daemon started inside a clud session survives that
//! session — none of them set the cooperative `RUNNING_PROCESS_IS_DAEMON`
//! marker, so a marker-only spare-list gives them nothing.
//!
//! That machinery originally lived inside [`crate::job_orphan_reaper`], which
//! is Windows-only and whose blast radius is one tool shell. The *cross*-platform
//! reaper — [`crate::orphan_reaper`], which runs on every foreground `clud`
//! exit, on `clud slay`, and on the daemon's periodic sweep — still spared by
//! the marker alone, so an sccache-shaped daemon survived the shell that
//! started it and was then killed moments later by clud's own exit. This module
//! is that machinery lifted out so one table serves both.
//!
//! ## What lives here and what does not
//!
//! Everything here is a *pure function of injected data* ([`FactsSnapshot`]),
//! plus one clearly-fenced per-platform producer ([`collect_host_facts`]). No
//! code that decides *reap or spare* may call the OS directly; that is what
//! makes the decision table unit-testable on every platform without spawning
//! anything (#674).
//!
//! [`crate::job_orphan_reaper`] re-exports these under its own
//! `ReapDecisionReason`, which additionally carries the tool-shell-specific
//! reasons (leaked tool client, git-bash re-exec, …) that have no meaning here.

use std::collections::{HashMap, HashSet};

/// Environment variable naming the operator's spare-list.
///
/// The whitelist is the last resort in the precedence order and must be
/// **data, not code** — clud ships none.
pub const SPARE_IMAGES_ENV: &str = "CLUD_REAPER_SPARE_IMAGES";

/// Why a process must not be reaped.
///
/// Only OS-signal reasons live here. A reaper with additional, local reasons
/// (the Windows tool-shell reaper has several) converts these into its own
/// richer enum.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SpareReason {
    /// Broke away from the reaper's Job Object, so it is outside our
    /// containment and was never ours to kill.
    OutsideJobObject,
    /// Runs in session 0 — a Windows service, not a session descendant.
    ServiceSession,
    /// Called `setsid()`: it leads its own POSIX session.
    SessionLeader,
    /// Runs as a different token owner / euid than we do.
    ForeignTokenOwner,
    /// Declared itself a daemon via `RUNNING_PROCESS_IS_DAEMON`. **Cooperative**
    /// — see [`ProcessFacts`] for why this is ranked below the OS signals.
    DeclaredDaemon,
    /// Owns a listening endpoint, so later unrelated invocations discover and
    /// reuse it. This is what a build-cache or language server *is*.
    ListeningEndpoint,
    /// Matched the operator's configured spare-list. Last resort, and data
    /// rather than code — see [`ProcessFacts::spare_listed`].
    ConfiguredSpareList,
}

impl SpareReason {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::OutsideJobObject => "outside_job_object",
            Self::ServiceSession => "service_session",
            Self::SessionLeader => "session_leader",
            Self::ForeignTokenOwner => "foreign_token_owner",
            Self::DeclaredDaemon => "declared_daemon",
            Self::ListeningEndpoint => "listening_endpoint",
            Self::ConfiguredSpareList => "configured_spare_list",
        }
    }
}

/// Facts about a process that only the OS can answer.
///
/// `None` means **"this signal is unavailable"** — either the platform has no
/// such concept, or the query failed. A `None` never spares: absence of
/// evidence is not evidence of daemon-hood, which is the exact inversion #522
/// fixed.
pub trait ProcessFacts {
    /// Is `pid` inside the reaper's own Job Object?
    ///
    /// `Some(false)` means it broke away (`CREATE_BREAKAWAY_FROM_JOB`) and is
    /// outside our containment. Only the tool-shell reaper owns a job handle;
    /// [`collect_host_facts`] reports this signal as unavailable.
    fn in_reaper_job(&self, pid: u32) -> Option<bool>;

    /// Does `pid` run in the Windows services session (session 0)?
    fn is_service_session(&self, pid: u32) -> Option<bool>;

    /// Did `pid` call `setsid()` — does it lead its own POSIX session?
    fn is_session_leader(&self, pid: u32) -> Option<bool>;

    /// Does `pid` run as a different token owner / euid than we do?
    fn owner_differs(&self, pid: u32) -> Option<bool>;

    /// Did `pid` set `RUNNING_PROCESS_IS_DAEMON`?
    ///
    /// **Cooperative**, which is why it is ranked below every OS signal: it is
    /// set by *other programs* through `running_process` (zccache and soldr do;
    /// sccache, dockerd and `FBuildWorker` do not). Grepping this repo tells
    /// you nothing about the runtime set.
    fn declared_daemon(&self, pid: u32) -> Option<bool>;

    /// Does `pid` own a listening endpoint (TCP listener or unix socket)?
    ///
    /// The most expensive signal, and the one that catches the hard case: a
    /// process discovered and reused by later unrelated invocations is a
    /// service, whatever it did or did not declare.
    fn owns_listening_endpoint(&self, pid: u32) -> Option<bool>;

    /// Did the operator name this image in a configured spare-list?
    ///
    /// A whitelist is a last resort and must be **data, not code** — nothing in
    /// clud hard-codes an image name here.
    fn spare_listed(&self, pid: u32, image_name: &str) -> bool;
}

/// The signals [`ProcessFacts`] can answer. Named so a snapshot can say
/// "I cannot answer this at all here" rather than silently answering "no".
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Signal {
    JobMembership,
    ServiceSession,
    SessionLeader,
    TokenOwner,
    DaemonMarker,
    ListeningEndpoint,
}

/// Process facts collected once per pass and then consulted as pure data.
///
/// This is the production carrier **and** the test fake. Production fills it
/// from the OS once per pass; a unit test builds one by hand. There is no
/// second implementation of [`ProcessFacts`] that could drift from the one
/// under test.
///
/// Each set holds only *positive* findings. `unavailable` names the signals
/// this snapshot could not evaluate at all.
#[derive(Clone, Debug, Default)]
pub struct FactsSnapshot {
    /// PIDs proven to be outside the reaper's Job Object.
    pub outside_job: HashSet<u32>,
    pub service_session: HashSet<u32>,
    pub session_leaders: HashSet<u32>,
    pub foreign_owner: HashSet<u32>,
    pub declared_daemons: HashSet<u32>,
    pub listening: HashSet<u32>,
    /// Operator-configured image names. Data, not code: nothing in clud
    /// hard-codes an entry here.
    pub spare_images: Vec<String>,
    pub unavailable: HashSet<Signal>,
}

impl FactsSnapshot {
    fn answer(&self, signal: Signal, present: bool) -> Option<bool> {
        (!self.unavailable.contains(&signal)).then_some(present)
    }
}

impl ProcessFacts for FactsSnapshot {
    fn in_reaper_job(&self, pid: u32) -> Option<bool> {
        self.answer(Signal::JobMembership, !self.outside_job.contains(&pid))
    }

    fn is_service_session(&self, pid: u32) -> Option<bool> {
        self.answer(Signal::ServiceSession, self.service_session.contains(&pid))
    }

    fn is_session_leader(&self, pid: u32) -> Option<bool> {
        self.answer(Signal::SessionLeader, self.session_leaders.contains(&pid))
    }

    fn owner_differs(&self, pid: u32) -> Option<bool> {
        self.answer(Signal::TokenOwner, self.foreign_owner.contains(&pid))
    }

    fn declared_daemon(&self, pid: u32) -> Option<bool> {
        self.answer(Signal::DaemonMarker, self.declared_daemons.contains(&pid))
    }

    fn owns_listening_endpoint(&self, pid: u32) -> Option<bool> {
        self.answer(Signal::ListeningEndpoint, self.listening.contains(&pid))
    }

    fn spare_listed(&self, _pid: u32, image_name: &str) -> bool {
        let image = normalized_image(image_name);
        self.spare_images
            .iter()
            .any(|configured| normalized_image(configured) == image)
    }
}

/// Operator-configured spare-list, read from [`SPARE_IMAGES_ENV`].
///
/// Comma- or semicolon-separated image names, matched on basename,
/// case-insensitively.
pub fn configured_spare_images(raw: Option<&str>) -> Vec<String> {
    raw.unwrap_or_default()
        .split([',', ';'])
        .map(str::trim)
        .filter(|entry| !entry.is_empty())
        .map(str::to_string)
        .collect()
}

/// [`configured_spare_images`] applied to the live environment.
pub fn configured_spare_images_from_env() -> Vec<String> {
    configured_spare_images(std::env::var(SPARE_IMAGES_ENV).ok().as_deref())
}

/// The subtractive spare-list: PID → why it must not be reaped.
///
/// Subtractive data may be stale (worst case: it spares too much). Additive
/// data — anything naming a kill *target* — may not.
pub type SpareList = HashMap<u32, SpareReason>;

/// Why `pid` must not be reaped, or `None` if nothing protects it.
///
/// # Precedence
///
/// Cheap and authoritative first, expensive last, cooperative in between:
///
/// 1. **Job-object membership** — free, and removes most candidates before any
///    other work. A process outside our job was never ours to kill.
/// 2. **Service session** — a session-0 process is a Windows service. It is
///    spared even if it somehow carries a `CLUD:` tag.
/// 3. **Session leader** — the primary POSIX signal; a double-fork daemon.
/// 4. **Foreign token owner** — a kill would generally fail anyway.
/// 5. **Declared daemon** — the cooperative marker, ranked below every OS
///    signal precisely because opting in is optional.
/// 6. **Listening endpoint** — evaluated only for PIDs that survived all of the
///    above, because it is the costly one.
/// 7. **Configured spare-list** — operator data, last resort.
///
/// Console attachment is deliberately **not** ranked. Once the trigger shell
/// has exited its console goes with it, which makes "no console"
/// indistinguishable between a detached daemon and an ordinary leaked client —
/// the signal would over-spare into uselessness exactly when it is consulted.
pub fn spare_signal(facts: &dyn ProcessFacts, pid: u32, image_name: &str) -> Option<SpareReason> {
    if facts.in_reaper_job(pid) == Some(false) {
        return Some(SpareReason::OutsideJobObject);
    }
    if facts.is_service_session(pid) == Some(true) {
        return Some(SpareReason::ServiceSession);
    }
    if facts.is_session_leader(pid) == Some(true) {
        return Some(SpareReason::SessionLeader);
    }
    if facts.owner_differs(pid) == Some(true) {
        return Some(SpareReason::ForeignTokenOwner);
    }
    if facts.declared_daemon(pid) == Some(true) {
        return Some(SpareReason::DeclaredDaemon);
    }
    if facts.owns_listening_endpoint(pid) == Some(true) {
        return Some(SpareReason::ListeningEndpoint);
    }
    if facts.spare_listed(pid, image_name) {
        return Some(SpareReason::ConfiguredSpareList);
    }
    None
}

/// Evaluate [`spare_signal`] over a bounded candidate set.
///
/// The candidate set is always the reaper's own tracked or tagged processes,
/// never the host — that bound is what keeps the cost proportional to the
/// sweep rather than to the machine.
pub fn build_spare_list(
    facts: &dyn ProcessFacts,
    candidates: impl Iterator<Item = (u32, String)>,
) -> SpareList {
    candidates
        .filter_map(|(pid, image_name)| {
            spare_signal(facts, pid, &image_name).map(|reason| (pid, reason))
        })
        .collect()
}

pub fn image_basename(image: &str) -> &str {
    image.rsplit(['\\', '/']).next().unwrap_or(image)
}

pub fn normalized_image(image: &str) -> String {
    image_basename(image).to_ascii_lowercase()
}

/// Collect OS facts for a bounded candidate set, on whatever platform we are.
///
/// This is the producer [`crate::orphan_reaper`] uses. It is **not** the
/// tool-shell reaper's producer: that one additionally owns a Job Object
/// handle and can therefore answer [`Signal::JobMembership`], which is both its
/// cheapest and its strongest signal. Here there is no job, so that signal is
/// reported unavailable rather than silently answered "inside".
///
/// `declared_daemons` is passed in rather than re-derived: `orphan_reaper`'s
/// caller already paid for one full-host environment pass and got the marker
/// set out of it (#673 Phase 7b). Reading `environ` again here would double the
/// most expensive part of the sweep.
///
/// Ordered cheapest-first so the expensive queries run over the smallest
/// possible set.
pub fn collect_host_facts(
    candidates: &[u32],
    declared_daemons: &HashSet<u32>,
    spare_images: Vec<String>,
) -> FactsSnapshot {
    let mut snapshot = FactsSnapshot {
        spare_images,
        declared_daemons: candidates
            .iter()
            .copied()
            .filter(|pid| declared_daemons.contains(pid))
            .collect(),
        // No Job Object on this path — say so rather than answering "inside",
        // which would read as a positive finding of containment.
        unavailable: HashSet::from([Signal::JobMembership]),
        ..FactsSnapshot::default()
    };
    if candidates.is_empty() {
        // Still mark the platform's structurally-absent signals so a caller
        // inspecting the snapshot sees the same shape either way.
        platform::mark_unavailable(&mut snapshot);
        return snapshot;
    }
    platform::fill(candidates, &mut snapshot);
    snapshot
}

#[cfg(windows)]
mod platform {
    use super::{FactsSnapshot, Signal};

    pub(super) fn mark_unavailable(snapshot: &mut FactsSnapshot) {
        // Windows has no POSIX session leader.
        snapshot.unavailable.insert(Signal::SessionLeader);
    }

    pub(super) fn fill(candidates: &[u32], snapshot: &mut FactsSnapshot) {
        mark_unavailable(snapshot);

        for &pid in candidates {
            match win32::can_open(pid) {
                // Overwhelmingly an access-denied on a higher-privilege
                // process. A kill would fail anyway.
                Some(false) => {
                    snapshot.foreign_owner.insert(pid);
                    continue;
                }
                None => continue,
                Some(true) => {}
            }
            if win32::session_id(pid) == Some(0) {
                snapshot.service_session.insert(pid);
            }
        }

        let survivors: Vec<u32> = candidates
            .iter()
            .copied()
            .filter(|pid| {
                !snapshot.foreign_owner.contains(pid) && !snapshot.service_session.contains(pid)
            })
            .collect();
        if survivors.is_empty() {
            return;
        }

        let listening = win32::listening_pids();
        snapshot.listening = survivors
            .into_iter()
            .filter(|pid| listening.contains(pid))
            .collect();
    }

    /// The Win32 half of the [`super::ProcessFacts`] signals for the
    /// cross-platform reaper.
    mod win32 {
        use std::collections::HashSet;

        use windows::Win32::Foundation::CloseHandle;
        use windows::Win32::NetworkManagement::IpHelper::{
            GetExtendedTcpTable, MIB_TCP6TABLE_OWNER_PID, MIB_TCPTABLE_OWNER_PID,
            TCP_TABLE_OWNER_PID_LISTENER,
        };
        use windows::Win32::Networking::WinSock::{AF_INET, AF_INET6};
        use windows::Win32::System::RemoteDesktop::ProcessIdToSessionId;
        use windows::Win32::System::Threading::{
            OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION, PROCESS_TERMINATE,
        };

        /// `Some(false)` = the process exists but we may not terminate it.
        /// `None` = it is gone, or the answer is not usable.
        pub(super) fn can_open(pid: u32) -> Option<bool> {
            unsafe {
                match OpenProcess(
                    PROCESS_QUERY_LIMITED_INFORMATION | PROCESS_TERMINATE,
                    false,
                    pid,
                ) {
                    Ok(handle) => {
                        let _ = CloseHandle(handle);
                        Some(true)
                    }
                    Err(_) => Some(false),
                }
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

        /// PIDs owning a TCP listener, over **both** address families.
        ///
        /// One syscall per family for the whole table, not one per PID. IPv6 is
        /// not optional trivia here: a daemon bound only to `::1` is exactly the
        /// sccache-class process this signal exists to protect, and querying
        /// `AF_INET` alone reported it as owning nothing (#688).
        ///
        /// Known gap: named pipes. Windows exposes no documented pipe-name →
        /// owning-PID mapping, so a daemon whose only endpoint is a named pipe
        /// still needs the cooperative marker or the operator spare-list.
        pub(super) fn listening_pids() -> HashSet<u32> {
            let mut pids = tcp_listener_pids(AF_INET.0 as u32);
            pids.extend(tcp_listener_pids(AF_INET6.0 as u32));
            pids
        }

        fn tcp_listener_pids(family: u32) -> HashSet<u32> {
            unsafe {
                let mut size = 0u32;
                // First call sizes the buffer; ERROR_INSUFFICIENT_BUFFER is the
                // documented success path here.
                let _ = GetExtendedTcpTable(
                    None,
                    &mut size,
                    false,
                    family,
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
                    family,
                    TCP_TABLE_OWNER_PID_LISTENER,
                    0,
                ) != 0
                {
                    return HashSet::new();
                }
                if family == AF_INET6.0 as u32 {
                    let table = &*(buffer.as_ptr() as *const MIB_TCP6TABLE_OWNER_PID);
                    let rows = std::slice::from_raw_parts(
                        table.table.as_ptr(),
                        table.dwNumEntries as usize,
                    );
                    rows.iter().map(|row| row.dwOwningPid).collect()
                } else {
                    let table = &*(buffer.as_ptr() as *const MIB_TCPTABLE_OWNER_PID);
                    let rows = std::slice::from_raw_parts(
                        table.table.as_ptr(),
                        table.dwNumEntries as usize,
                    );
                    rows.iter().map(|row| row.dwOwningPid).collect()
                }
            }
        }
    }
}

#[cfg(unix)]
mod platform {
    use super::{FactsSnapshot, Signal};

    pub(super) fn mark_unavailable(snapshot: &mut FactsSnapshot) {
        // No Windows services session anywhere on Unix.
        snapshot.unavailable.insert(Signal::ServiceSession);
        #[cfg(not(target_os = "linux"))]
        {
            // `/proc` is the only interface here that answers these without
            // linking a platform-specific process-inspection library. On
            // non-Linux Unix the honest answer is "cannot evaluate".
            snapshot.unavailable.insert(Signal::TokenOwner);
            snapshot.unavailable.insert(Signal::ListeningEndpoint);
        }
    }

    pub(super) fn fill(candidates: &[u32], snapshot: &mut FactsSnapshot) {
        mark_unavailable(snapshot);

        // The cheap gate, and the one that exists on every Unix: a daemon that
        // called `setsid()` has left our session, which is the POSIX statement
        // of "I intend to outlive whatever started me".
        let own_session = unsafe { libc::getsid(0) };
        if own_session >= 0 {
            for &pid in candidates {
                let session = unsafe { libc::getsid(pid as libc::pid_t) };
                if session >= 0 && session != own_session {
                    snapshot.session_leaders.insert(pid);
                }
            }
        }

        #[cfg(target_os = "linux")]
        linux::fill(candidates, snapshot);
    }

    #[cfg(target_os = "linux")]
    mod linux {
        use std::collections::HashSet;

        use super::FactsSnapshot;

        pub(super) fn fill(candidates: &[u32], snapshot: &mut FactsSnapshot) {
            let own_euid = unsafe { libc::geteuid() };
            for &pid in candidates {
                if let Some(euid) = effective_uid(pid) {
                    if euid != own_euid {
                        snapshot.foreign_owner.insert(pid);
                    }
                }
            }

            let survivors: Vec<u32> = candidates
                .iter()
                .copied()
                .filter(|pid| !snapshot.foreign_owner.contains(pid))
                .collect();
            if survivors.is_empty() {
                return;
            }

            let listening = listening_inodes();
            if listening.is_empty() {
                return;
            }
            snapshot.listening = survivors
                .into_iter()
                .filter(|pid| owns_any_socket(*pid, &listening))
                .collect();
        }

        /// Effective uid from `/proc/<pid>/status`'s `Uid:` line, whose four
        /// fields are real / effective / saved-set / filesystem.
        fn effective_uid(pid: u32) -> Option<u32> {
            let status = std::fs::read_to_string(format!("/proc/{pid}/status")).ok()?;
            let line = status.lines().find(|line| line.starts_with("Uid:"))?;
            line.split_whitespace().nth(2)?.parse().ok()
        }

        /// Inodes of every listening socket on the host: TCP over both address
        /// families, plus unix-domain sockets (the POSIX analogue of a Windows
        /// named pipe, and how a language server or build cache is typically
        /// reached).
        fn listening_inodes() -> HashSet<u64> {
            let mut inodes = HashSet::new();
            for family in ["/proc/net/tcp", "/proc/net/tcp6"] {
                collect_tcp_listeners(family, &mut inodes);
            }
            collect_unix_listeners(&mut inodes);
            inodes
        }

        /// `/proc/net/tcp{,6}` columns: `sl local rem st ... uid timeout inode`.
        /// State `0A` is `TCP_LISTEN`.
        fn collect_tcp_listeners(path: &str, out: &mut HashSet<u64>) {
            let Ok(text) = std::fs::read_to_string(path) else {
                return;
            };
            for line in text.lines().skip(1) {
                let fields: Vec<&str> = line.split_whitespace().collect();
                if fields.len() > 9 && fields[3] == "0A" {
                    if let Ok(inode) = fields[9].parse::<u64>() {
                        out.insert(inode);
                    }
                }
            }
        }

        /// `/proc/net/unix` columns end with `... st inode path`. `st` is `01`
        /// for a listening (unconnected, accepting) socket.
        fn collect_unix_listeners(out: &mut HashSet<u64>) {
            let Ok(text) = std::fs::read_to_string("/proc/net/unix") else {
                return;
            };
            for line in text.lines().skip(1) {
                let fields: Vec<&str> = line.split_whitespace().collect();
                if fields.len() > 6 && fields[5] == "01" {
                    if let Ok(inode) = fields[6].parse::<u64>() {
                        out.insert(inode);
                    }
                }
            }
        }

        /// Does `pid` hold a descriptor onto any of `listening`?
        ///
        /// Bounded by the candidate set, not the host: we read one process's
        /// `fd` directory per candidate, never the whole `/proc` tree.
        fn owns_any_socket(pid: u32, listening: &HashSet<u64>) -> bool {
            let Ok(entries) = std::fs::read_dir(format!("/proc/{pid}/fd")) else {
                return false;
            };
            for entry in entries.flatten() {
                let Ok(target) = std::fs::read_link(entry.path()) else {
                    continue;
                };
                let Some(inode) = target
                    .to_str()
                    .and_then(|link| link.strip_prefix("socket:["))
                    .and_then(|rest| rest.strip_suffix(']'))
                    .and_then(|digits| digits.parse::<u64>().ok())
                else {
                    continue;
                };
                if listening.contains(&inode) {
                    return true;
                }
            }
            false
        }
    }
}

#[cfg(not(any(windows, unix)))]
mod platform {
    use super::{FactsSnapshot, Signal};

    pub(super) fn mark_unavailable(snapshot: &mut FactsSnapshot) {
        snapshot.unavailable.extend([
            Signal::ServiceSession,
            Signal::SessionLeader,
            Signal::TokenOwner,
            Signal::ListeningEndpoint,
        ]);
    }

    pub(super) fn fill(_candidates: &[u32], snapshot: &mut FactsSnapshot) {
        mark_unavailable(snapshot);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn facts_with(f: impl FnOnce(&mut FactsSnapshot)) -> FactsSnapshot {
        let mut facts = FactsSnapshot::default();
        f(&mut facts);
        facts
    }

    #[test]
    fn an_unavailable_signal_never_spares() {
        let facts = facts_with(|f| {
            f.listening.insert(7);
            f.unavailable.insert(Signal::ListeningEndpoint);
        });
        assert_eq!(spare_signal(&facts, 7, "sccache"), None);
    }

    #[test]
    fn os_signals_outrank_the_cooperative_marker() {
        let facts = facts_with(|f| {
            f.session_leaders.insert(9);
            f.declared_daemons.insert(9);
        });
        assert_eq!(
            spare_signal(&facts, 9, "sccache"),
            Some(SpareReason::SessionLeader)
        );
    }

    #[test]
    fn a_listening_endpoint_spares_an_undeclared_daemon() {
        let facts = facts_with(|f| {
            f.listening.insert(11);
        });
        assert_eq!(
            spare_signal(&facts, 11, "sccache"),
            Some(SpareReason::ListeningEndpoint)
        );
    }

    #[test]
    fn nothing_protects_an_ordinary_leaked_process() {
        let facts = facts_with(|f| {
            f.listening.insert(11);
        });
        assert_eq!(spare_signal(&facts, 12, "node"), None);
    }

    #[test]
    fn the_operator_spare_list_matches_on_basename_case_insensitively() {
        let facts = facts_with(|f| {
            f.spare_images = configured_spare_images(Some("FBuildWorker.exe; my-daemon"));
        });
        assert_eq!(
            spare_signal(&facts, 3, "C:\\tools\\fbuildworker.EXE"),
            Some(SpareReason::ConfiguredSpareList)
        );
        assert_eq!(
            spare_signal(&facts, 3, "/opt/bin/my-daemon"),
            Some(SpareReason::ConfiguredSpareList)
        );
        assert_eq!(spare_signal(&facts, 3, "node"), None);
    }

    #[test]
    fn build_spare_list_reports_a_reason_per_spared_pid() {
        let facts = facts_with(|f| {
            f.listening.insert(21);
            f.declared_daemons.insert(22);
        });
        let spares = build_spare_list(
            &facts,
            [
                (21u32, "sccache".to_string()),
                (22, "zccache".to_string()),
                (23, "node".to_string()),
            ]
            .into_iter(),
        );
        assert_eq!(spares[&21], SpareReason::ListeningEndpoint);
        assert_eq!(spares[&22], SpareReason::DeclaredDaemon);
        assert!(!spares.contains_key(&23));
    }

    #[test]
    fn host_facts_never_claim_job_membership_it_cannot_observe() {
        let facts = collect_host_facts(&[std::process::id()], &HashSet::new(), Vec::new());
        assert!(facts.unavailable.contains(&Signal::JobMembership));
        assert_eq!(facts.in_reaper_job(std::process::id()), None);
    }

    #[test]
    fn host_facts_carry_the_declared_marker_for_candidates_only() {
        let declared = HashSet::from([41u32, 99]);
        let facts = collect_host_facts(&[41], &declared, Vec::new());
        assert!(facts.declared_daemons.contains(&41));
        assert!(
            !facts.declared_daemons.contains(&99),
            "a non-candidate must not enter the snapshot"
        );
    }

    /// The producer must answer at least one OS signal on every platform CI
    /// builds for — that is the whole point of #688's second half. Windows
    /// answers service-session and listening-endpoint; Unix answers session
    /// leadership.
    #[test]
    fn some_os_signal_is_available_on_this_platform() {
        let facts = collect_host_facts(&[std::process::id()], &HashSet::new(), Vec::new());
        let answerable = [
            Signal::ServiceSession,
            Signal::SessionLeader,
            Signal::TokenOwner,
            Signal::ListeningEndpoint,
        ]
        .into_iter()
        .filter(|signal| !facts.unavailable.contains(signal))
        .count();
        assert!(
            answerable > 0,
            "no OS-authoritative signal is available here: {:?}",
            facts.unavailable
        );
    }

    /// Our own process is in our own session, so it must not be mistaken for a
    /// detached daemon — the signal has to distinguish, not blanket-spare.
    #[cfg(unix)]
    #[test]
    fn our_own_process_is_not_a_session_leader_by_this_signal() {
        let facts = collect_host_facts(&[std::process::id()], &HashSet::new(), Vec::new());
        assert_eq!(facts.is_session_leader(std::process::id()), Some(false));
    }
}
