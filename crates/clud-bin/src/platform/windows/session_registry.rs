//! Windows half of `crate::session_registry`: the
//! `OpenProcess + GetExitCodeProcess` liveness probe.

pub fn os_probe_is_alive(pid: u32) -> bool {
    use std::ffi::c_void;

    // Windows reserves PID 0 for the System Idle Process; treating it as
    // a clud sibling is nonsense.
    if pid == 0 {
        return false;
    }

    const PROCESS_QUERY_LIMITED_INFORMATION: u32 = 0x1000;
    const STILL_ACTIVE: u32 = 259; // STATUS_PENDING

    extern "system" {
        fn OpenProcess(desired_access: u32, inherit_handle: i32, process_id: u32) -> *mut c_void;
        fn CloseHandle(handle: *mut c_void) -> i32;
        fn GetExitCodeProcess(handle: *mut c_void, exit_code: *mut u32) -> i32;
    }

    unsafe {
        let handle = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid);
        if handle.is_null() {
            // Either the PID never existed, or we lack rights. We can't
            // distinguish here, so treat as dead — the worst case is one
            // stale row that the *next* GC pass will catch.
            return false;
        }
        let mut exit_code: u32 = 0;
        let ok = GetExitCodeProcess(handle, &mut exit_code);
        CloseHandle(handle);
        if ok == 0 {
            return false;
        }
        exit_code == STILL_ACTIVE
    }
}
