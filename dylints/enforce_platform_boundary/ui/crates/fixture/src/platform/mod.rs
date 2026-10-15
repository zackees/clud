// The selector: allowed to name hosts.
#[cfg(unix)]
#[path = "unix/imp.rs"]
mod imp;
#[cfg(windows)]
#[path = "windows/imp.rs"]
mod imp;
#[cfg(not(any(unix, windows)))]
mod imp {}

pub use self::imp::*;
