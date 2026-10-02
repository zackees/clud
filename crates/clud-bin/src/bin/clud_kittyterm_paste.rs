//! WezTerm's Ctrl+V helper: one clipboard snapshot as one JSON line.
//! The logic and its tests live in `clud::paste_image` (#1726: this bin
//! builds no test harness of its own).

use clud::paste_image::{kitty_clipboard_payload, write_kitty_paste};
use std::io;

fn main() {
    let code = write_kitty_paste(
        kitty_clipboard_payload(),
        &mut io::stdout(),
        &mut io::stderr(),
    );
    std::process::exit(code);
}
