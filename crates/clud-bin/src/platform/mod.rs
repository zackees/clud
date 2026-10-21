//! The host-platform boundary (#1953).
//!
//! This file is the only place clud selects a host. `windows/` and `unix/`
//! each implement the same per-area modules (`platform::subprocess`,
//! `platform::host`, ...) with identical public signatures, so code outside
//! this directory calls one neutral API and never names a host. Where a
//! concept exists on one host only, the other tree supplies a stub: an
//! uninhabited type, a no-op, or an `Unsupported` error.
//!
//! The `enforce_platform_boundary` Dylint lint denies host `cfg` keys and
//! native API crates (`libc`, `windows_sys`, ...) everywhere else, with no
//! baseline and no allowlist. Because it runs before cfg expansion, the
//! Linux Dylint run checks the Windows and macOS code too.

#[cfg(unix)]
mod unix;
#[cfg(unix)]
pub use unix::*;

#[cfg(windows)]
mod windows;
#[cfg(windows)]
pub use windows::*;
