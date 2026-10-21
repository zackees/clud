//! Unix half of `crate::console_input`: intentionally empty. The native
//! `ReadConsoleInputW` adapter exists on Windows only; POSIX reads stdin as a
//! byte stream.
