//! Unix half of `crate::shim_telemetry`'s parent-process lookup.

/// The parent's process name when it costs one small file read (Linux
/// `/proc/<ppid>/comm`); `None` elsewhere.
#[cfg(target_os = "linux")]
pub fn parent_name() -> Option<String> {
    let ppid = std::os::unix::process::parent_id();
    std::fs::read_to_string(format!("/proc/{ppid}/comm"))
        .ok()
        .map(|name| name.trim().to_string())
        .filter(|name| !name.is_empty())
}

/// The parent's process name when it costs one small file read (Linux
/// `/proc/<ppid>/comm`); `None` elsewhere.
#[cfg(not(target_os = "linux"))]
pub fn parent_name() -> Option<String> {
    None
}
