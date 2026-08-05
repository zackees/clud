//! Output-side workers for the raw PTY session pump.

use std::io::{self, Write};
use std::sync::mpsc::Receiver;

use crate::graphics::GraphicsConfig;
use crate::verbose_log;

pub(super) fn run_output_writer<W: Write>(rx: Receiver<Vec<u8>>, mut writer: W) {
    while let Ok(mut buf) = rx.recv() {
        while let Ok(more) = rx.try_recv() {
            buf.extend_from_slice(&more);
        }
        if !buf.is_empty() {
            let _ = writer.write_all(&buf);
            let _ = writer.flush();
        }
    }
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
