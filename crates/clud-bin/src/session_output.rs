//! Output-side workers for the raw PTY session pump.

use std::io::{self, Write};
use std::sync::mpsc::{Receiver, RecvTimeoutError};
use std::time::Instant;

use crate::graphics::GraphicsConfig;
use crate::toast::compositor::{Compositor, TICK};
use crate::verbose_log;

/// One message for the writer thread.
pub(super) enum OutputMsg {
    /// Filtered child output, in order.
    Child(Vec<u8>),
    /// The PTY was resized; a toast compositor re-pins to the new size.
    Resize { rows: u16, cols: u16 },
}

impl From<Vec<u8>> for OutputMsg {
    fn from(bytes: Vec<u8>) -> Self {
        Self::Child(bytes)
    }
}

/// Writer thread without toasts. Kept for the #538 coalescing tests.
#[cfg(test)]
pub(super) fn run_output_writer<W: Write, M: Into<OutputMsg>>(rx: Receiver<M>, writer: W) {
    run_output_writer_composited(rx, writer, None);
}

/// Drains `rx`, coalescing every message already queued into one
/// `write_all` + one `flush` per wakeup (#538), until the channel disconnects.
///
/// With a [`Compositor`] (#1189) every coalesced child burst passes through
/// it, and the thread also wakes on [`TICK`] while a toast is pending,
/// visible, or may expire. Without one, child bytes are written verbatim and
/// resize messages are ignored — identical to the pre-toast writer.
pub(super) fn run_output_writer_composited<W: Write, M: Into<OutputMsg>>(
    rx: Receiver<M>,
    mut writer: W,
    mut compositor: Option<Compositor>,
) {
    loop {
        let wants_tick = compositor.as_ref().is_some_and(Compositor::wants_tick);
        let first = if wants_tick {
            match rx.recv_timeout(TICK) {
                Ok(msg) => Some(msg.into()),
                Err(RecvTimeoutError::Timeout) => None,
                Err(RecvTimeoutError::Disconnected) => break,
            }
        } else {
            match rx.recv() {
                Ok(msg) => Some(msg.into()),
                Err(_) => break,
            }
        };
        let now = Instant::now();
        let mut out = Vec::new();
        match first {
            None => {
                if let Some(compositor) = compositor.as_mut() {
                    out.extend(compositor.on_tick(now));
                }
            }
            Some(msg) => {
                let mut child = Vec::new();
                apply(msg, &mut child, &mut out, &mut compositor, now);
                while let Ok(more) = rx.try_recv() {
                    apply(more.into(), &mut child, &mut out, &mut compositor, now);
                }
                flush_child(&mut child, &mut out, &mut compositor, now);
            }
        }
        if !out.is_empty() {
            let _ = writer.write_all(&out);
            let _ = writer.flush();
        }
    }
    if let Some(compositor) = compositor.as_mut() {
        let bytes = compositor.finish();
        if !bytes.is_empty() {
            let _ = writer.write_all(&bytes);
            let _ = writer.flush();
        }
    }
}

fn apply(
    msg: OutputMsg,
    child: &mut Vec<u8>,
    out: &mut Vec<u8>,
    compositor: &mut Option<Compositor>,
    now: Instant,
) {
    match msg {
        OutputMsg::Child(bytes) => child.extend_from_slice(&bytes),
        OutputMsg::Resize { rows, cols } => {
            // Child bytes queued before the resize are laid out at the old
            // size; composite them first.
            flush_child(child, out, compositor, now);
            if let Some(compositor) = compositor.as_mut() {
                out.extend(compositor.on_resize(rows, cols, now));
            }
        }
    }
}

fn flush_child(
    child: &mut Vec<u8>,
    out: &mut Vec<u8>,
    compositor: &mut Option<Compositor>,
    now: Instant,
) {
    if child.is_empty() {
        return;
    }
    match compositor.as_mut() {
        Some(compositor) => out.extend(compositor.on_child(child, now)),
        None => out.extend_from_slice(child),
    }
    child.clear();
}

pub(super) fn redraw_graphics_header_for_resize(
    config: &GraphicsConfig,
    terminal_rows: u16,
    terminal_cols: u16,
    verbose: bool,
) -> u16 {
    match crate::graphics::render_header(config, terminal_rows, terminal_cols) {
        Ok(Some(header)) => {
            write_bytes(&header.bytes);
            header.text_rows
        }
        Ok(None) => {
            write_bytes(&crate::graphics::reset_layout_bytes(terminal_rows, true));
            terminal_rows
        }
        Err(err) => {
            if verbose {
                verbose_log::log(format_args!("[clud] graphics: resize redraw failed: {err}"));
            }
            write_bytes(&crate::graphics::reset_layout_bytes(terminal_rows, true));
            terminal_rows
        }
    }
}

fn write_bytes(bytes: &[u8]) {
    let mut out = io::stdout().lock();
    let _ = out.write_all(bytes);
    let _ = out.flush();
}
