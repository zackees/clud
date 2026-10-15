//! The host boundary of `probe-dll` (#1953): the only place there that selects a
//! host. `unix/` and `windows/` expose the same items with the same
//! signatures, so neutral code never names a host.

#[cfg(unix)]
mod unix;
#[cfg(unix)]
pub use unix::*;

#[cfg(windows)]
mod windows;
#[cfg(windows)]
pub use windows::*;
