//! The injectable probe DLL's entry point: on attach it appends one
//! `INJECTED pid=<pid>` line to `CLUD_PROBE_DLL_SINK`.

const DLL_PROCESS_ATTACH: u32 = 1;

#[no_mangle]
pub extern "system" fn DllMain(
    _module: *mut core::ffi::c_void,
    reason: u32,
    _reserved: *mut core::ffi::c_void,
) -> i32 {
    if reason == DLL_PROCESS_ATTACH {
        if let Ok(path) = std::env::var("CLUD_PROBE_DLL_SINK") {
            let _ = std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(path)
                .and_then(|mut file| {
                    use std::io::Write;
                    writeln!(file, "INJECTED pid={}", std::process::id())
                });
        }
    }
    1
}
