//! Windows half of `crate::console_title`: the Win32 console-title calls
//! and the keeper thread that re-applies the title after drift.

use std::time::Instant;

use crate::console_title::{
    classify_snapshot, code_page_means_console, current_desired_title_locked, should_spawn_keeper,
    title_state_cell, update_cpu_alert_locked, KeeperCadence, SnapshotCheck,
    CPU_METRICS_FALLBACK_POLL_INTERVAL,
};

/// Does this process have a console whose title it could keep?
///
/// `GetConsoleCP` returns the console input code page, or **0** with
/// `ERROR_INVALID_HANDLE` when the process has no console at all. That is the
/// daemon and every worker: `main` starts the keeper before subcommand
/// dispatch, so console-less processes get one too.
///
/// **Not `GetConsoleWindow`.** That returns the console's *window handle*, and
/// it is documented to return null for a console that has no window — which
/// includes a pseudoconsole. A ConPTY client (clud's own `--pty` mode, and
/// Windows Terminal) genuinely *has* a console and can set its title, so
/// gating on a window handle would disable the keeper in exactly the
/// environment it exists for. Verified on Windows 10 19045: with a console
/// `GetConsoleCP` is 65001 and after `FreeConsole` it is 0 / `ERROR_INVALID_HANDLE`.
fn console_is_attached() -> bool {
    extern "system" {
        fn GetConsoleCP() -> u32;
    }
    // SAFETY: no arguments, no out-params, no handles retained.
    code_page_means_console(unsafe { GetConsoleCP() })
}

pub fn spawn_keeper_thread() -> bool {
    spawn_keeper_thread_with(console_is_attached)
}

/// Spawn the keeper unless `attached` reports no console. Returns whether a
/// thread was started.
fn spawn_keeper_thread_with(attached: impl FnOnce() -> bool) -> bool {
    // #706: without a console there is nothing to stamp *and* nothing to read
    // back, so `read_console_title()` returns `None` on every pass, the
    // `current != want` comparison is always true, `changed` is pinned true,
    // and `KeeperCadence` never backs off. The result was a 750 ms wake loop —
    // `SetConsoleTitleW` into the void plus a `stat` of the metrics snapshot —
    // running forever in exactly the processes that are supposed to be idle:
    // the long-lived daemon and every worker. #547's backoff was correct; it
    // just could never engage here.
    if !should_spawn_keeper(attached) {
        return false;
    }
    let _ = std::thread::Builder::new()
        .name("clud-title-keeper".into())
        .spawn(|| {
            let mut last_metrics_poll = None::<Instant>;
            let mut last_snapshot_mtime = None::<std::time::SystemTime>;
            let mut cadence = KeeperCadence::new();
            let mut last_title = None::<String>;
            loop {
                let now = Instant::now();
                // #547: one `stat` per pass. When the daemon publishes, this is
                // the whole cost of the alert -- no connection, no daemon work,
                // and no read at all unless the mtime moved.
                if !refresh_cpu_alert_from_snapshot(now, &mut last_snapshot_mtime)
                    && last_metrics_poll
                        .map(|last| last.elapsed() >= CPU_METRICS_FALLBACK_POLL_INTERVAL)
                        .unwrap_or(true)
                {
                    // Compatibility path only: an older daemon that publishes
                    // nothing still gets its alert, just at 15 s instead of 2 s.
                    last_metrics_poll = Some(now);
                    refresh_cpu_alert_from_daemon(now);
                }
                let want = current_desired_title(Instant::now());
                let mut changed = false;
                if !want.is_empty() {
                    let current = read_console_title();
                    if current.as_deref() != Some(want.as_str()) {
                        set_title(&want);
                        changed = true;
                    }
                }
                // The desired title moving is itself a change even when the
                // console already agreed — that is how the flash animation
                // advances, and backing off through it would make the alert
                // stutter.
                if last_title.as_deref() != Some(want.as_str()) {
                    changed = true;
                    last_title = Some(want);
                }
                cadence = cadence.record_pass(changed);
                std::thread::sleep(cadence.interval());
            }
        });
    true
}

fn read_console_title() -> Option<String> {
    extern "system" {
        fn GetConsoleTitleW(buf: *mut u16, size: u32) -> u32;
    }
    let mut buf: Vec<u16> = vec![0; 1024];
    // SAFETY: `buf` is a valid, mutable, aligned u16 buffer of length
    // 1024. GetConsoleTitleW writes at most `size` u16s to `buf`.
    let n = unsafe { GetConsoleTitleW(buf.as_mut_ptr(), buf.len() as u32) };
    if n == 0 {
        // 0 = empty title or error (e.g. no console attached). Treat as
        // unknown so the keeper doesn't try to re-stamp.
        return None;
    }
    Some(String::from_utf16_lossy(&buf[..n as usize]))
}

fn current_desired_title(now: Instant) -> String {
    let mut state = title_state_cell()
        .lock()
        .expect("title state mutex poisoned");
    current_desired_title_locked(&mut state, now)
}

/// Refresh the CPU alert, preferring the published snapshot over an RPC (#547).
///
/// Returns `true` when the snapshot path answered, so the caller knows not to
/// run the fallback timer.
fn refresh_cpu_alert_from_snapshot(
    now: Instant,
    last_seen: &mut Option<std::time::SystemTime>,
) -> bool {
    let Ok(state_dir) = crate::daemon::default_state_dir() else {
        clear_cpu_alert();
        return false;
    };
    let path = crate::daemon::metrics_snapshot_path(&state_dir);
    let mtime = std::fs::metadata(&path)
        .and_then(|meta| meta.modified())
        .ok();
    match classify_snapshot(mtime, last_seen) {
        SnapshotCheck::Absent => false,
        SnapshotCheck::Unchanged => true,
        SnapshotCheck::Changed => {
            match std::fs::read_to_string(&path)
                .ok()
                .and_then(|text| serde_json::from_str::<crate::daemon::MetricsSnapshot>(&text).ok())
            {
                Some(snapshot) => update_cpu_alert(snapshot.cpu_pct, now),
                // Torn or malformed read: treat as no alert, same as the RPC
                // error path always has.
                None => clear_cpu_alert(),
            }
            true
        }
    }
}

fn refresh_cpu_alert_from_daemon(now: Instant) {
    let Ok(state_dir) = crate::daemon::default_state_dir() else {
        clear_cpu_alert();
        return;
    };
    match crate::daemon::daemon_client_metrics(&state_dir) {
        Ok((_pid, cpu_pct)) => update_cpu_alert(cpu_pct, now),
        Err(_) => clear_cpu_alert(),
    }
}

fn update_cpu_alert(cpu_pct: f32, now: Instant) {
    let mut state = title_state_cell()
        .lock()
        .expect("title state mutex poisoned");
    update_cpu_alert_locked(&mut state, cpu_pct, now);
}

fn clear_cpu_alert() {
    title_state_cell()
        .lock()
        .expect("title state mutex poisoned")
        .cpu_alert = None;
}

pub fn set_title(title: &str) {
    // SetConsoleTitleW writes to the console window owning this
    // process — which is the cmd.exe / Windows Terminal we want. Wide
    // (UTF-16) form so non-ASCII cwd names render correctly.
    extern "system" {
        fn SetConsoleTitleW(lp_console_title: *const u16) -> i32;
    }
    let mut wide: Vec<u16> = title.encode_utf16().collect();
    wide.push(0);
    // SAFETY: `wide` is a properly null-terminated UTF-16 buffer with
    // a stable address until the unsafe block ends.
    unsafe {
        let _ = SetConsoleTitleW(wide.as_ptr());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn spawn_keeper_thread_with_no_console_returns_without_spawning() {
        assert!(!spawn_keeper_thread_with(|| false));
    }
}
