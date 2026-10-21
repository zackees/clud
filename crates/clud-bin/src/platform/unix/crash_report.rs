//! Unix half of `crate::crash_report`: decode a `crash_handler::CrashContext`
//! (a Linux `signalfd_siginfo` or a macOS Mach exception) into report metadata.

use crate::crash_report::{hex_addr, NativeCrashMeta};

#[cfg(target_os = "macos")]
use crate::crash_report::mac_exception_kind_name;
#[cfg(target_os = "linux")]
use crate::crash_report::signal_name;

#[cfg(target_os = "linux")]
#[cfg(target_os = "linux")]
pub(crate) fn extract_native_meta(cc: &crash_handler::CrashContext) -> NativeCrashMeta {
    // `signalfd_siginfo` is plain-old-data populated by the kernel.
    let signo = cc.siginfo.ssi_signo as i32;
    let addr = cc.siginfo.ssi_addr as usize;
    NativeCrashMeta {
        signal_or_exception: Some(signal_name(signo).to_string()),
        signal_number: Some(signo),
        exception_code: None,
        faulting_address: hex_addr(addr),
    }
}

#[cfg(target_os = "macos")]
#[cfg(target_os = "macos")]
pub(crate) fn extract_native_meta(cc: &crash_handler::CrashContext) -> NativeCrashMeta {
    // mac uses Mach exceptions; `exception` is `Option<ExceptionInfo>`.
    match &cc.exception {
        Some(info) => NativeCrashMeta {
            signal_or_exception: Some(mac_exception_kind_name(info.kind).to_string()),
            signal_number: Some(info.kind as i32),
            exception_code: Some(info.code as i64),
            faulting_address: info
                .subcode
                .map(|s| format!("0x{s:x}"))
                .or_else(|| hex_addr(info.code as usize)),
        },
        None => NativeCrashMeta::default(),
    }
}
