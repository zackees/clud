//! Compile-time facts about the host, as constants neutral code can branch on.

pub const WINDOWS: bool = true;
pub const UNIX: bool = false;
pub const LINUX: bool = false;
pub const MACOS: bool = false;
