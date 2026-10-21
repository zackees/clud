//! Windows half of `crate::crash_report`: decode a structured exception
//! from `crash_handler::CrashContext` into report metadata.

use crate::crash_report::{hex_addr, windows_exception_name, NativeCrashMeta};

#[cfg(target_os = "windows")]
pub(crate) fn extract_native_meta(cc: &crash_handler::CrashContext) -> NativeCrashMeta {
    // crash-context exposes the exception_code directly so we don't need
    // to dereference exception_pointers just to read it. The faulting
    // address still requires reading the ExceptionRecord.
    let code = cc.exception_code as i64;
    let addr = unsafe {
        if cc.exception_pointers.is_null() {
            0
        } else {
            let record = (*cc.exception_pointers).ExceptionRecord;
            if record.is_null() {
                0
            } else {
                (*record).ExceptionAddress as usize
            }
        }
    };
    NativeCrashMeta {
        signal_or_exception: Some(windows_exception_name(code).to_string()),
        signal_number: None,
        exception_code: Some(code),
        faulting_address: hex_addr(addr),
    }
}
