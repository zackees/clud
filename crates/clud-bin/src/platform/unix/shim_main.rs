//! Unix half of `crate::shim_main`'s relay: `exec` replaces the shim with
//! the real binary, and a waited child gets shell-style signal handling.
//!
//! This file keeps the `shim_main.rs` basename on purpose:
//! `ci/banned_imports.py` exempts the relay's raw `std::process` use by
//! basename (#406 / #409).

use std::os::unix::process::{CommandExt, ExitStatusExt};
use std::path::Path;
use std::process::{Child, Command};

/// Replace this process with `command` (Unix `exec`). Only an exec failure
/// returns, as 126.
pub fn exec(label: &str, path: &Path, mut command: Command) -> i32 {
    let error = command.exec();
    eprintln!(
        "clud {label} shim: failed to exec {}: {error}",
        path.display()
    );
    126
}

/// Wait for `child` while ignoring SIGINT and SIGQUIT and forwarding SIGTERM
/// and SIGHUP to it. A child killed by signal N is recorded as `128 + N`,
/// then the shim re-raises N on itself.
pub fn wait_child(
    label: &str,
    path: &Path,
    mut child: Child,
    recorder: &crate::shim_telemetry::Recorder,
) -> i32 {
    let forwarding = unix_signals::Forwarding::install(child.id());
    let status = child.wait();
    drop(forwarding);
    match status {
        Ok(status) => match status.signal() {
            Some(signal) => {
                recorder.record(128 + signal);
                unix_signals::reraise(signal);
                128 + signal
            }
            None => {
                let code = status.code().unwrap_or(1);
                recorder.record(code);
                code
            }
        },
        Err(error) => {
            eprintln!(
                "clud {label} shim: failed to wait for {}: {error}",
                path.display()
            );
            recorder.record(1);
            1
        }
    }
}

mod unix_signals {
    //! Shell-style signal handling for [`super::wait_child`].

    use signal_hook::consts::{SIGHUP, SIGINT, SIGQUIT, SIGTERM};
    use signal_hook::SigId;

    pub struct Forwarding {
        saved: Vec<(i32, libc::sighandler_t)>,
        forwarders: Vec<SigId>,
    }

    impl Forwarding {
        /// Ignore SIGINT/SIGQUIT and forward SIGTERM/SIGHUP to `child`.
        /// Installed after the spawn, so the child keeps default dispositions.
        pub fn install(child: u32) -> Self {
            let pid = child as libc::pid_t;
            let mut saved = Vec::new();
            for signal in [SIGINT, SIGQUIT] {
                // SAFETY: setting a disposition to SIG_IGN is always valid.
                let old = unsafe { libc::signal(signal, libc::SIG_IGN) };
                saved.push((signal, old));
            }
            let mut forwarders = Vec::new();
            for signal in [SIGTERM, SIGHUP] {
                // SAFETY: the action only calls kill(2), which is
                // async-signal-safe, with values captured by copy.
                let id = unsafe {
                    signal_hook::low_level::register(signal, move || {
                        libc::kill(pid, signal);
                    })
                };
                if let Ok(id) = id {
                    forwarders.push(id);
                }
            }
            Forwarding { saved, forwarders }
        }
    }

    impl Drop for Forwarding {
        fn drop(&mut self) {
            for id in self.forwarders.drain(..) {
                signal_hook::low_level::unregister(id);
            }
            for (signal, old) in self.saved.drain(..) {
                // SAFETY: restoring the disposition this guard replaced.
                unsafe { libc::signal(signal, old) };
            }
        }
    }

    /// Die by `signal` with its default action, like the child did.
    pub fn reraise(signal: i32) {
        // SAFETY: default disposition, then raise(3) on this process.
        unsafe {
            libc::signal(signal, libc::SIG_DFL);
            libc::raise(signal);
        }
    }
}

#[cfg(test)]
mod tests {
    use crate::shim_main::gh_shim::watch_words;
    use std::ffi::OsString;
    use std::os::unix::ffi::OsStringExt;

    #[test]
    fn non_utf8_watch_selector_fails_closed() {
        let args = [
            OsString::from("pr"),
            OsString::from("checks"),
            OsString::from_vec(b"branch-\xff".to_vec()),
            OsString::from("--watch"),
        ];
        assert_eq!(watch_words(&args), Err(2));
    }
}
