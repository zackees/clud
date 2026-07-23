//! Windows-only foreground shell orphan reaping (#569).

#[cfg(not(windows))]
pub struct ForegroundJobTracker;

#[cfg(not(windows))]
impl ForegroundJobTracker {
    pub fn install() -> Option<Self> {
        None
    }
}

#[cfg(windows)]
mod imp {
    use std::collections::HashMap;
    use std::ffi::c_void;
    use std::mem::{size_of, zeroed};
    use std::ptr::null_mut;
    use std::sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
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

    const ACTIVE_PROCESS_ZERO: u32 = 4;
    const NEW_PROCESS: u32 = 6;
    const EXIT_PROCESS: u32 = 7;

    pub struct ForegroundJobTracker {
        job: HANDLE,
        port: HANDLE,
        stop: Arc<AtomicBool>,
        listener: Option<JoinHandle<()>>,
    }
    #[derive(Clone)]
    struct Process {
        parent_pid: u32,
        image_name: String,
    }

    impl ForegroundJobTracker {
        /// Installs after daemon startup. Failure is intentionally non-fatal:
        /// existing originator-tag exit cleanup remains the fallback.
        pub fn install() -> Option<Self> {
            unsafe {
                let job = CreateJobObjectW(None, PCWSTR::null()).ok()?;
                let port = CreateIoCompletionPort(INVALID_HANDLE_VALUE, None, 0, 1).ok()?;
                let assoc = JOBOBJECT_ASSOCIATE_COMPLETION_PORT {
                    CompletionKey: job.0 as *mut c_void,
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
                let listener = thread::spawn({
                    let stop = Arc::clone(&stop);
                    move || listen(port, stop)
                });
                Some(Self {
                    job,
                    port,
                    stop,
                    listener: Some(listener),
                })
            }
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

    fn listen(port: HANDLE, stop: Arc<AtomicBool>) {
        let mut known = HashMap::<u32, Process>::new();
        while !stop.load(Ordering::Acquire) {
            let (mut message, mut key, mut payload) = (0u32, 0usize, null_mut());
            if unsafe { GetQueuedCompletionStatus(port, &mut message, &mut key, &mut payload, 200) }
                .is_err()
            {
                if unsafe { GetLastError() } == WAIT_TIMEOUT {
                    continue;
                }
                break;
            }
            let pid = payload as usize as u32;
            match message {
                NEW_PROCESS => {
                    if let Some(process) = snapshot().remove(&pid) {
                        known.insert(pid, process);
                    }
                }
                EXIT_PROCESS if is_watched_shell(known.get(&pid)) => {
                    // The shell itself is gone. Its live direct children still
                    // retain its creator PID, so kill each child subtree.
                    let roots: Vec<u32> = known
                        .iter()
                        .filter_map(|(&child, process)| {
                            (process.parent_pid == pid).then_some(child)
                        })
                        .collect();
                    for root in roots {
                        crate::process_tree::kill_tree(root);
                    }
                }
                ACTIVE_PROCESS_ZERO => known.clear(),
                _ => {}
            }
            let _ = key;
        }
    }
    fn is_watched_shell(process: Option<&Process>) -> bool {
        matches!(
            process
                .map(|p| p.image_name.to_ascii_lowercase())
                .as_deref(),
            Some("cmd.exe" | "powershell.exe" | "pwsh.exe" | "bash.exe" | "git-bash.exe")
        )
    }
    fn snapshot() -> HashMap<u32, Process> {
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
                        Process {
                            parent_pid: entry.th32ParentProcessID,
                            image_name: String::from_utf16_lossy(&entry.szExeFile[..end]),
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
    #[cfg(test)]
    mod tests {
        use super::*;
        #[test]
        fn shell_names_are_case_insensitive() {
            assert!(is_watched_shell(Some(&Process {
                parent_pid: 0,
                image_name: "PoWeRsHeLl.ExE".into()
            })));
            assert!(!is_watched_shell(Some(&Process {
                parent_pid: 0,
                image_name: "node.exe".into()
            })));
        }
    }
}
#[cfg(windows)]
pub use imp::ForegroundJobTracker;
