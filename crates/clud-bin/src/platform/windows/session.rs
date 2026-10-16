//! Windows half of `crate::session`'s PTY plumbing: resize through the
//! master directly, and a polling resize watcher.

use std::io;

use running_process::pty::NativePtyProcess;
use running_process::pty::PtySize;

/// `NativePtyProcess::resize_impl` is a deliberate no-op on Windows, so
/// resize the underlying master directly; see `crate::session::resize_pty`.
pub fn resize_pty(process: &NativePtyProcess, rows: u16, cols: u16) -> io::Result<()> {
    let guard = process
        .handles
        .lock()
        .map_err(|e| io::Error::other(e.to_string()))?;
    if let Some(handles) = guard.as_ref() {
        handles
            .master
            .resize(PtySize {
                rows,
                cols,
                pixel_width: 0,
                pixel_height: 0,
            })
            .map_err(|e| io::Error::other(e.to_string()))?;
    }
    Ok(())
}

/// 150 ms polling of `crossterm::terminal::size()`; see
/// `crate::session::spawn_os_resize_watcher` for why not `ReadConsoleInputW`.
pub fn spawn_os_resize_watcher(resize_tx: std::sync::mpsc::Sender<(u16, u16)>) {
    std::thread::spawn(move || {
        let mut last: Option<(u16, u16)> = None;
        while let Ok((cols, rows)) = crossterm::terminal::size() {
            let now = (rows, cols);
            if Some(now) != last {
                if resize_tx.send(now).is_err() {
                    break; // pump exited
                }
                last = Some(now);
            }
            std::thread::sleep(std::time::Duration::from_millis(150));
        }
    });
}
