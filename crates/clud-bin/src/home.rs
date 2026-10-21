//! The one place clud decides which directory is the user's home (#1836).
//!
//! Every `~/.clud`, `~/.claude`, `~/.codex` and managed-install path hangs off
//! this answer, so two subsystems that resolve it differently stop agreeing
//! about where a file lives. That happened in #1829: the managed DeepSeek
//! Harness installed under the Windows Known Folder profile while discovery
//! searched `USERPROFILE`, and the launch fell back to a bare `dsh`.
//!
//! Precedence, the long-standing behavior of `clud_settings`:
//! - Windows: `USERPROFILE`, then `HOME`, then the OS profile folder.
//! - Elsewhere: `HOME`, then the OS lookup.
//!
//! Environment variables come first so a launch with an isolated home (tests,
//! sandboxes, `HOME=... clud`) is honored everywhere at once. Empty values are
//! ignored. `dirs::home_dir` and `std::env::home_dir` are banned outside this
//! module by the `ban_dirs_home_dir` Dylint lint and `ci/banned_home_dir.py`.

use std::ffi::OsString;
use std::path::PathBuf;

/// The user's home directory, or `None` when nothing names one.
pub fn user_home() -> Option<PathBuf> {
    resolve(
        cfg!(windows),
        std::env::var_os("USERPROFILE"),
        std::env::var_os("HOME"),
        dirs::home_dir,
    )
}

/// Pure precedence rule behind [`user_home`], seamed for tests.
pub fn resolve(
    windows: bool,
    user_profile: Option<OsString>,
    home: Option<OsString>,
    os_fallback: impl FnOnce() -> Option<PathBuf>,
) -> Option<PathBuf> {
    let set = |value: Option<OsString>| value.filter(|value| !value.is_empty()).map(PathBuf::from);
    let from_env = if windows {
        set(user_profile).or_else(|| set(home))
    } else {
        set(home)
    };
    from_env.or_else(os_fallback)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn os(value: &str) -> Option<OsString> {
        Some(OsString::from(value))
    }

    #[test]
    fn windows_prefers_user_profile_then_home_then_os() {
        let fallback = || Some(PathBuf::from("known-folder"));
        assert_eq!(
            resolve(true, os("profile"), os("home"), fallback),
            Some(PathBuf::from("profile"))
        );
        assert_eq!(
            resolve(true, None, os("home"), fallback),
            Some(PathBuf::from("home"))
        );
        assert_eq!(
            resolve(true, os(""), os(""), fallback),
            Some(PathBuf::from("known-folder"))
        );
    }

    #[test]
    fn unix_uses_home_and_ignores_user_profile() {
        assert_eq!(
            resolve(false, os("profile"), os("home"), || None),
            Some(PathBuf::from("home"))
        );
        assert_eq!(
            resolve(false, os("profile"), None, || Some(PathBuf::from("passwd"))),
            Some(PathBuf::from("passwd"))
        );
    }

    #[test]
    fn nothing_set_and_no_os_answer_is_none() {
        assert_eq!(resolve(false, None, None, || None), None);
    }
}
