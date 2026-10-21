//! Unix half of `crate::session`'s PTY plumbing: the library's resize, and a
//! `SIGWINCH` resize watcher.

use std::io;

use running_process::pty::NativePtyProcess;

/// The library's implementation does the right thing on POSIX; delegate.
pub fn resize_pty(process: &NativePtyProcess, rows: u16, cols: u16) -> io::Result<()> {
    process
        .resize_impl(rows, cols)
        .map_err(|e| io::Error::other(e.to_string()))
}

/// A `SIGWINCH` handler via `signal-hook`; zero-latency.
pub fn spawn_os_resize_watcher(resize_tx: std::sync::mpsc::Sender<(u16, u16)>) {
    use signal_hook::consts::signal::SIGWINCH;
    use signal_hook::iterator::Signals;
    std::thread::spawn(move || {
        let Ok(mut signals) = Signals::new([SIGWINCH]) else {
            return;
        };
        for _ in signals.forever() {
            if let Ok((cols, rows)) = crossterm::terminal::size() {
                if resize_tx.send((rows, cols)).is_err() {
                    break; // pump exited
                }
            }
        }
    });
}
