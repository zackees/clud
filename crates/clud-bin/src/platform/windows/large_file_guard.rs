//! Windows half of the `crate::large_file_guard` test helpers (test builds only).

/// CPU time (kernel + user) consumed so far by the calling thread.
pub fn thread_cpu_time() -> std::time::Duration {
    use windows::Win32::Foundation::FILETIME;
    use windows::Win32::System::Threading::{GetCurrentThread, GetThreadTimes};
    let mut creation = FILETIME::default();
    let mut exit = FILETIME::default();
    let mut kernel = FILETIME::default();
    let mut user = FILETIME::default();
    // SAFETY: `GetCurrentThread` returns a pseudo-handle that needs no
    // closing, and all four out-pointers are stack `FILETIME`s valid for the
    // duration of the call.
    unsafe {
        GetThreadTimes(
            GetCurrentThread(),
            &mut creation,
            &mut exit,
            &mut kernel,
            &mut user,
        )
    }
    .expect("GetThreadTimes on the current thread");
    let ticks = |t: FILETIME| (u64::from(t.dwHighDateTime) << 32) | u64::from(t.dwLowDateTime);
    // FILETIME counts 100 ns intervals.
    std::time::Duration::from_nanos((ticks(kernel) + ticks(user)) * 100)
}
