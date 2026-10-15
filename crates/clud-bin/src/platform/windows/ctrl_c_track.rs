//! Windows half of `crate::ctrl_c_track`: the console/process forensics
//! snapshot taken when a Ctrl+C is observed.

use crate::ctrl_c_track::{unix_millis_now, CtrlCForensics, CtrlCProcessSnapshot};

#[expect(
    clippy::too_many_lines,
    reason = "complexity ratchet baseline (zackees/ci.yml#229); split this function"
)]
pub fn platform_forensics(child_root_pid: Option<u32>) -> Option<CtrlCForensics> {
    use std::collections::{HashMap, HashSet};

    use windows::Win32::Foundation::CloseHandle;
    use windows::Win32::System::Console::GetConsoleProcessList;
    use windows::Win32::System::Diagnostics::ToolHelp::{
        CreateToolhelp32Snapshot, Process32FirstW, Process32NextW, PROCESSENTRY32W,
        TH32CS_SNAPPROCESS,
    };
    use windows::Win32::System::Threading::GetCurrentProcessId;
    use windows::Win32::UI::WindowsAndMessaging::{GetForegroundWindow, GetWindowThreadProcessId};

    #[derive(Clone)]
    struct Entry {
        pid: u32,
        parent_pid: u32,
        exe: String,
    }

    fn process_entries() -> Vec<Entry> {
        let Ok(snapshot) = (unsafe { CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0) }) else {
            return Vec::new();
        };
        let mut entries = Vec::new();
        let mut entry = PROCESSENTRY32W {
            dwSize: std::mem::size_of::<PROCESSENTRY32W>() as u32,
            ..Default::default()
        };
        if unsafe { Process32FirstW(snapshot, &mut entry) }.is_ok() {
            loop {
                entries.push(Entry {
                    pid: entry.th32ProcessID,
                    parent_pid: entry.th32ParentProcessID,
                    exe: nul_terminated_wide_to_string(&entry.szExeFile),
                });
                if unsafe { Process32NextW(snapshot, &mut entry) }.is_err() {
                    break;
                }
            }
        }
        let _ = unsafe { CloseHandle(snapshot) };
        entries
    }

    fn nul_terminated_wide_to_string(buf: &[u16]) -> String {
        let len = buf.iter().position(|&unit| unit == 0).unwrap_or(buf.len());
        String::from_utf16_lossy(&buf[..len])
    }

    fn console_process_pids() -> Vec<u32> {
        let mut buf = vec![0u32; 128];
        let count = unsafe { GetConsoleProcessList(&mut buf) };
        if count == 0 {
            return Vec::new();
        }
        if count as usize > buf.len() {
            buf.resize(count as usize, 0);
            let count = unsafe { GetConsoleProcessList(&mut buf) };
            buf.truncate(count as usize);
        } else {
            buf.truncate(count as usize);
        }
        buf.sort_unstable();
        buf.dedup();
        buf
    }

    fn foreground_window_pid() -> Option<u32> {
        let hwnd = unsafe { GetForegroundWindow() };
        if hwnd.is_invalid() {
            return None;
        }
        let mut pid = 0u32;
        unsafe {
            GetWindowThreadProcessId(hwnd, Some(&mut pid));
        }
        (pid != 0).then_some(pid)
    }

    fn descendant_pids(entries: &[Entry], root: u32) -> Vec<u32> {
        let mut children: HashMap<u32, Vec<u32>> = HashMap::new();
        for entry in entries {
            children
                .entry(entry.parent_pid)
                .or_default()
                .push(entry.pid);
        }
        let mut stack = vec![root];
        let mut out = Vec::new();
        while let Some(pid) = stack.pop() {
            if let Some(next) = children.get(&pid) {
                for child in next {
                    out.push(*child);
                    stack.push(*child);
                }
            }
        }
        out
    }

    fn ancestor_pids(entries_by_pid: &HashMap<u32, Entry>, current_pid: u32) -> Vec<u32> {
        let mut out = Vec::new();
        let mut seen = HashSet::new();
        let mut pid = current_pid;
        for _ in 0..64 {
            let Some(entry) = entries_by_pid.get(&pid) else {
                break;
            };
            let parent = entry.parent_pid;
            if parent == 0 || parent == pid || !seen.insert(parent) {
                break;
            }
            out.push(parent);
            pid = parent;
        }
        out
    }

    let entries = process_entries();
    let by_pid: HashMap<u32, Entry> = entries.iter().map(|e| (e.pid, e.clone())).collect();
    let current_pid = unsafe { GetCurrentProcessId() };
    let current_parent_pid = by_pid.get(&current_pid).map(|e| e.parent_pid);
    let child_tree_pids = child_root_pid
        .map(|pid| descendant_pids(&entries, pid))
        .unwrap_or_default();
    let ancestor_pids = ancestor_pids(&by_pid, current_pid);
    let console_process_pids = console_process_pids();
    let foreground_window_pid = foreground_window_pid();

    let mut wanted = HashSet::new();
    wanted.insert(current_pid);
    if let Some(pid) = current_parent_pid {
        wanted.insert(pid);
    }
    if let Some(pid) = child_root_pid {
        wanted.insert(pid);
    }
    for pid in &child_tree_pids {
        wanted.insert(*pid);
    }
    for pid in &ancestor_pids {
        wanted.insert(*pid);
    }
    for pid in &console_process_pids {
        wanted.insert(*pid);
    }
    if let Some(pid) = foreground_window_pid {
        wanted.insert(pid);
    }

    let mut processes: Vec<CtrlCProcessSnapshot> = wanted
        .into_iter()
        .filter_map(|pid| {
            let entry = by_pid.get(&pid)?;
            let mut roles = Vec::new();
            if pid == current_pid {
                roles.push("clud".to_string());
            }
            if Some(pid) == current_parent_pid {
                roles.push("clud_parent".to_string());
            }
            if Some(pid) == child_root_pid {
                roles.push("child_root".to_string());
            }
            if child_tree_pids.contains(&pid) {
                roles.push("child_descendant".to_string());
            }
            if ancestor_pids.contains(&pid) {
                roles.push("clud_ancestor".to_string());
            }
            if console_process_pids.contains(&pid) {
                roles.push("same_console".to_string());
            }
            if Some(pid) == foreground_window_pid {
                roles.push("foreground_window_owner".to_string());
            }
            Some(CtrlCProcessSnapshot {
                pid,
                parent_pid: entry.parent_pid,
                exe: entry.exe.clone(),
                roles,
            })
        })
        .collect();
    processes.sort_by_key(|p| p.pid);

    Some(CtrlCForensics {
        captured_at_ms: unix_millis_now(),
        current_pid,
        current_parent_pid,
        child_root_pid,
        child_tree_pids,
        ancestor_pids,
        console_process_pids,
        foreground_window_pid,
        processes,
        source_limit: "win32_console_control_events_do_not_expose_sender_pid".to_string(),
    })
}

#[cfg(test)]
mod tests {
    use crate::ctrl_c_track::{double_tap_enabled, test_state_lock, ENV_DISABLE_DOUBLE_TAP};

    /// On Windows the guard is engaged by default and the opt-out env
    /// var disables it. Truthy values match the common conventions
    /// ("1"/"true"/"yes"/"on"); anything else (including the empty
    /// string) leaves the guard engaged.
    #[test]
    fn double_tap_enabled_respects_opt_out_env_var() {
        let _guard = test_state_lock().lock().unwrap_or_else(|e| e.into_inner());
        std::env::remove_var(ENV_DISABLE_DOUBLE_TAP);
        assert!(double_tap_enabled(), "default on Windows must be enabled");
        for truthy in ["1", "true", "TRUE", "Yes", "on"] {
            std::env::set_var(ENV_DISABLE_DOUBLE_TAP, truthy);
            assert!(!double_tap_enabled(), "{truthy:?} must disable the guard");
        }
        for falsy in ["", "0", "no", "off", "false", "garbage"] {
            std::env::set_var(ENV_DISABLE_DOUBLE_TAP, falsy);
            assert!(
                double_tap_enabled(),
                "{falsy:?} must leave the guard engaged"
            );
        }
        std::env::remove_var(ENV_DISABLE_DOUBLE_TAP);
    }
}
