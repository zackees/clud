//! Issue #183: `clud ui` subcommand — open the local web dashboard.
//!
//! Three modes, all gated through the same `run` entry point so the
//! daemon-up / port-discovery preamble is shared:
//!
//! 1. Default (`clud ui`) — ensure the daemon is up, then launch the
//!    user's default browser at the dashboard URL.
//! 2. `--no-open` — same preamble but only print the URL.
//! 3. `--json` — fetch `/state.json` from the dashboard and dump it to
//!    stdout, no browser. Mirrors the `clud gc list --json` convention.

use crate::daemon::{
    self, dashboard_url_from_info, ensure_daemon, fetch_state_json, read_dashboard_info,
    DashboardInfo,
};

pub(crate) fn dashboard_listener_missing_message(pid: u32) -> String {
    format!(
        "error: this daemon (pid {pid}) was started without a dashboard listener. \
         Restart it with `clud daemon restart`, then re-run `clud ui`."
    )
}

pub fn run(json: bool, no_open: bool) -> i32 {
    let state_dir = match daemon::default_state_dir() {
        Ok(p) => p,
        Err(e) => {
            eprintln!("error: cannot resolve clud state dir: {e}");
            return 1;
        }
    };

    if let Err(e) = ensure_daemon(&state_dir) {
        eprintln!("error: daemon unavailable: {e}");
        return 1;
    }

    let info = match read_dashboard_info(&state_dir) {
        Ok(info) => info,
        Err(e) => {
            eprintln!("error: cannot read daemon info: {e}");
            return 1;
        }
    };

    let Some(port) = info.dashboard_port else {
        eprintln!("{}", dashboard_listener_missing_message(info.pid));
        return 1;
    };

    if json {
        return print_state_json(port);
    }

    let url = dashboard_url_from_info(port);
    println!("{}", url);

    if no_open {
        return 0;
    }

    open_browser(&url, &info)
}

fn print_state_json(port: u16) -> i32 {
    match fetch_state_json(port) {
        Ok(body) => {
            println!("{}", body);
            0
        }
        Err(e) => {
            eprintln!("error: fetch /state.json failed: {e}");
            1
        }
    }
}

fn open_browser(url: &str, info: &DashboardInfo) -> i32 {
    // The `open` crate handles macOS `open`, Linux `xdg-open`, and
    // Windows `cmd /c start` under the hood. Failures are non-fatal — we
    // still printed the URL above and the user can paste it manually.
    if let Err(e) = open::that_detached(url) {
        eprintln!(
            "note: could not auto-open browser ({e}); paste the URL above. \
             (daemon pid {})",
            info.pid
        );
        return 1;
    }
    0
}

#[cfg(test)]
mod tests {
    //! Most of `ui.rs` is wrapper plumbing around the daemon HTTP layer
    //! that is exercised by `daemon/http.rs` tests; the bits worth
    //! pinning here are the surface the CLI promises.

    #[test]
    fn dashboard_listener_missing_message_includes_pid() {
        let msg = super::dashboard_listener_missing_message(142500);
        assert!(msg.contains("142500"));
    }

    #[test]
    fn dashboard_listener_missing_message_does_not_recommend_kill_all() {
        let msg = super::dashboard_listener_missing_message(1);
        assert!(!msg.contains("kill --all"));
        assert!(!msg.contains("will respawn"));
    }

    #[test]
    fn dashboard_listener_missing_message_points_at_daemon_restart() {
        let msg = super::dashboard_listener_missing_message(1);
        assert!(msg.contains("clud daemon restart"));
    }
}
