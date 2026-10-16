//! Unix-only coverage for `crate::process_scan`.

#[cfg(all(test, target_os = "linux"))]
mod tests {
    use crate::process_scan::scan_env;

    /// A thread is not a reapable unit, and `sysinfo` lists Linux tasks
    /// alongside processes.
    ///
    /// This is the invariant that broke in the wild: a leaked `__worker` with
    /// 19 threads was reported as "19 env-tagged descendant(s)", because every
    /// task under `/proc/<tgid>/task/` reads back the group leader's
    /// environment block and so matched the originator tag 19 times.
    ///
    /// Asserted two ways. The first is a host-wide sweep of whatever the scan
    /// happened to find. The second is deliberately self-referential and is
    /// what makes the case non-vacuous when the suite itself runs under clud
    /// (`bash test` does): the test binary is then tagged, this test forces it
    /// to be unambiguously multi-threaded, and the pre-fix code would have
    /// returned one row per thread of *this* process.
    #[test]
    fn a_multi_threaded_process_is_one_row_not_one_row_per_thread() {
        use std::sync::mpsc;

        let (release_tx, release_rx) = mpsc::channel::<()>();
        let (ready_tx, ready_rx) = mpsc::channel::<()>();
        let release_rx = std::sync::Arc::new(std::sync::Mutex::new(release_rx));
        let parked: Vec<_> = (0..4)
            .map(|_| {
                let ready = ready_tx.clone();
                let release = std::sync::Arc::clone(&release_rx);
                std::thread::spawn(move || {
                    ready.send(()).ok();
                    // Park until the scan has been taken, so the threads are
                    // provably alive across it.
                    let _ = release.lock().expect("release lock poisoned").recv();
                })
            })
            .collect();
        for _ in 0..4 {
            ready_rx
                .recv()
                .expect("spawned thread never reported ready");
        }

        let scan = scan_env("CLUD");

        // Enumerated while the threads are still parked, so this is the set
        // the scan above could have seen.
        let self_pid = std::process::id();
        let own_threads: Vec<u32> = std::fs::read_dir(format!("/proc/{self_pid}/task"))
            .expect("own task dir is readable")
            .filter_map(|entry| entry.ok()?.file_name().to_str()?.parse::<u32>().ok())
            .filter(|tid| *tid != self_pid)
            .collect();

        drop(release_tx);
        for handle in parked {
            handle.join().expect("parked thread panicked");
        }

        let tgid_of = |pid: u32| -> Option<u32> {
            let status = std::fs::read_to_string(format!("/proc/{pid}/status")).ok()?;
            status
                .lines()
                .find_map(|line| line.strip_prefix("Tgid:"))
                .and_then(|value| value.trim().parse::<u32>().ok())
        };

        for tagged in &scan.tagged {
            if let Some(tgid) = tgid_of(tagged.pid) {
                assert_eq!(
                    tgid, tagged.pid,
                    "scan returned TID {} of process {tgid}; threads are not reapable units",
                    tagged.pid
                );
            }
        }

        assert!(
            own_threads.len() >= 4,
            "expected the parked threads to be visible under /proc/self/task"
        );
        for tid in own_threads {
            assert!(
                !scan.tagged.iter().any(|tagged| tagged.pid == tid),
                "scan returned our own TID {tid} as a tagged process"
            );
            assert!(
                !scan.declared_daemons.contains(&tid),
                "scan returned our own TID {tid} as a declared daemon"
            );
        }
    }
}
