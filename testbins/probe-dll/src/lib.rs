//! Windows-only injectable probe DLL. The body lives in the host
//! boundary: `platform/windows/dll_main.rs`, with a placeholder on Unix.

mod platform;

pub use platform::dll_main::*;
