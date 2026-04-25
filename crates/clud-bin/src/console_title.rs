//! Set the console window title to `clud <cwd-name>` on launch.
//!
//! On Windows, when `clud` runs in cmd.exe / Windows Terminal, the title
//! bar otherwise shows the host shell's title — usually a generic
//! `Command Prompt` or the path to cmd.exe. Stamping `clud <cwd-name>`
//! makes it obvious at a glance which window is the active session and
//! which directory it's working in (especially helpful with multiple
//! windows open at once).
//!
//! POSIX terminals are out of scope for this module; the issue
//! requesting it was Windows-only. The cross-platform stub here is a
//! no-op so the call site in `main.rs` doesn't need a `cfg`.

/// Set the console title to `clud <cwd-basename>` for the current
/// working directory. Best-effort — failures are silent.
///
/// Called once near the top of `main`. Does not attempt to restore the
/// original title on exit; conhost / Windows Terminal generally clear
/// or replace the title themselves when the foreground process exits.
pub fn set_for_current_cwd() {
    let basename = current_cwd_basename().unwrap_or_else(|| "?".to_string());
    let title = title_for_cwd_name(&basename);
    set_title(&title);
}

/// Format `<cwd-name>` into the canonical title string. Pure helper —
/// unit-tested on every platform.
pub fn title_for_cwd_name(cwd_name: &str) -> String {
    format!("clud {}", cwd_name)
}

/// Best-effort lookup of the current working directory's leaf name.
/// Returns `None` if `current_dir()` fails or the path has no final
/// component (e.g. the filesystem root on Windows like `C:\`).
fn current_cwd_basename() -> Option<String> {
    let cwd = std::env::current_dir().ok()?;
    cwd.file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .or_else(|| {
            // On `C:\` (drive root) there's no `file_name`; fall back to
            // the drive letter so the title is still informative.
            cwd.to_string_lossy()
                .split(':')
                .next()
                .map(|s| s.to_string())
        })
}

#[cfg(windows)]
fn set_title(title: &str) {
    // SetConsoleTitleW writes to the console window owning this
    // process — which is the cmd.exe / Windows Terminal we want. Wide
    // (UTF-16) form so non-ASCII cwd names render correctly.
    extern "system" {
        fn SetConsoleTitleW(lp_console_title: *const u16) -> i32;
    }
    let mut wide: Vec<u16> = title.encode_utf16().collect();
    wide.push(0);
    // SAFETY: `wide` is a properly null-terminated UTF-16 buffer with
    // a stable address until the unsafe block ends.
    unsafe {
        let _ = SetConsoleTitleW(wide.as_ptr());
    }
}

#[cfg(not(windows))]
fn set_title(_title: &str) {
    // Out of scope per the issue; intentional no-op.
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn title_uses_clud_prefix_with_cwd_name() {
        assert_eq!(title_for_cwd_name("clud"), "clud clud");
        assert_eq!(title_for_cwd_name("my-app"), "clud my-app");
    }

    #[test]
    fn title_handles_non_ascii_cwd_name() {
        // Cyrillic + emoji to verify the formatter doesn't choke on
        // non-ASCII paths (which then flow through SetConsoleTitleW's
        // wide-string conversion on Windows).
        assert_eq!(title_for_cwd_name("проект"), "clud проект");
        assert_eq!(title_for_cwd_name("🚀"), "clud 🚀");
    }

    #[test]
    fn title_passes_through_empty_name() {
        // Defensive: even an empty cwd name produces a well-formed
        // title rather than panicking.
        assert_eq!(title_for_cwd_name(""), "clud ");
    }

    #[test]
    fn title_does_not_trim_or_normalize_input() {
        // We're explicit about what we format — no surprise trims, no
        // case changes. The cwd basename is shown verbatim.
        assert_eq!(title_for_cwd_name(" spaced "), "clud  spaced ");
        assert_eq!(title_for_cwd_name("MIXED-Case"), "clud MIXED-Case");
    }

    #[test]
    fn current_cwd_basename_returns_some_in_test_env() {
        // Cargo runs tests with the manifest dir as cwd, which always
        // has a leaf component (`clud-bin`). Smoke test the helper
        // without asserting the specific value.
        let got = current_cwd_basename();
        assert!(got.is_some(), "expected Some(_) cwd basename in test env");
        assert!(!got.unwrap().is_empty(), "basename should not be empty");
    }

    #[test]
    fn set_for_current_cwd_does_not_panic() {
        // Smoke test on every platform — the POSIX stub is a no-op,
        // and on Windows SetConsoleTitleW returns silently when there
        // is no console (e.g. inside `cargo test` under a CI runner).
        set_for_current_cwd();
    }
}
