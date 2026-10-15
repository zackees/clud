//! Compile-time facts about the host, as constants neutral code can branch on.

pub const WINDOWS: bool = false;
pub const UNIX: bool = true;
pub const LINUX: bool = cfg!(target_os = "linux");
pub const MACOS: bool = cfg!(target_os = "macos");
