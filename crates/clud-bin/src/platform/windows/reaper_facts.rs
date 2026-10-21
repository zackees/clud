//! Windows producer for `crate::reaper_facts::collect_host_facts`.

use crate::reaper_facts::{FactsSnapshot, Signal};

pub fn mark_unavailable(snapshot: &mut FactsSnapshot) {
    // Windows has no POSIX session leader.
    snapshot.unavailable.insert(Signal::SessionLeader);
}

pub fn fill(candidates: &[u32], snapshot: &mut FactsSnapshot) {
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
        .iter()
        .copied()
        .filter(|pid| listening.contains(pid))
        .collect();
    snapshot.active_desktop_shells = win32::active_desktop_shell_pid()
        .filter(|pid| survivors.contains(pid))
        .into_iter()
        .collect();
}

/// The Win32 half of the [`crate::reaper_facts::ProcessFacts`] signals for the
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
    use windows::Win32::UI::WindowsAndMessaging::{GetShellWindow, GetWindowThreadProcessId};

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
                let rows =
                    std::slice::from_raw_parts(table.table.as_ptr(), table.dwNumEntries as usize);
                rows.iter().map(|row| row.dwOwningPid).collect()
            } else {
                let table = &*(buffer.as_ptr() as *const MIB_TCPTABLE_OWNER_PID);
                let rows =
                    std::slice::from_raw_parts(table.table.as_ptr(), table.dwNumEntries as usize);
                rows.iter().map(|row| row.dwOwningPid).collect()
            }
        }
    }
}
