//! Unix half of the `crate::large_file_guard` test helpers (test builds only).

/// CPU time consumed so far by the calling thread.
pub fn thread_cpu_time() -> std::time::Duration {
    let mut ts = libc::timespec {
        tv_sec: 0,
        tv_nsec: 0,
    };
    // SAFETY: `ts` is a valid, writable `timespec` for the duration of the
    // call, and CLOCK_THREAD_CPUTIME_ID is supported on every unix target.
    let rc = unsafe { libc::clock_gettime(libc::CLOCK_THREAD_CPUTIME_ID, &mut ts) };
    assert_eq!(rc, 0, "clock_gettime(CLOCK_THREAD_CPUTIME_ID) failed");
    std::time::Duration::new(ts.tv_sec as u64, ts.tv_nsec as u32)
}
