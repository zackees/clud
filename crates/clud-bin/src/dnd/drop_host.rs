//! Which terminal host clud's `IDropTarget` should cover, decided from
//! injected facts so the rule runs in every CI host's unit tests.
//!
//! `console_drop_target` always registers on `GetConsoleWindow()`. Under a
//! ConPTY host that window is an invisible `PseudoConsoleWindow`, and
//! Explorer hover-tests the host's visible top-level window instead. This
//! module decides whether clud should also register on that host window.
//! The Win32 glue (process snapshot, `EnumWindows`) stays in
//! `console_drop_target::win`.

/// One row of a process snapshot (`CreateToolhelp32Snapshot` on Windows).
#[derive(Clone, Debug)]
pub(crate) struct ProcessEntry {
    pub(crate) pid: u32,
    pub(crate) parent_pid: u32,
    pub(crate) exe: String,
}

/// The host decision for the drop-target window walk.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum DropHost {
    /// Windows Terminal at `pid` hosts the session. Its visible top-level
    /// windows are what Explorer hover-tests, so clud registers on them.
    WindowsTerminal { pid: u32 },
    /// The host's own UI accepts Explorer drops and types the path into
    /// the terminal (VS Code and its forks, WezTerm). Its top-level window
    /// is shared with the rest of the application, so clud must not take
    /// it over: register on `GetConsoleWindow()` only.
    AcceptsDropsItself,
    /// Legacy conhost, or a host clud does not recognize: register on
    /// `GetConsoleWindow()` only.
    ConsoleWindowOnly,
}

/// Executable names of Windows Terminal builds.
const WINDOWS_TERMINAL_EXES: &[&str] = &["WindowsTerminal.exe", "WindowsTerminalPreview.exe"];

/// Executable names of GUI hosts that accept Explorer drops themselves:
/// VS Code (main and pty-host processes share the name) and WezTerm.
const SELF_DROP_HOST_EXES: &[&str] = &["Code.exe", "Code - Insiders.exe", "wezterm-gui.exe"];

/// Upper bound on ancestor hops, so a corrupt snapshot cannot loop.
const MAX_ANCESTOR_HOPS: usize = 64;

/// Decide which host window, if any, clud registers its drop target on.
///
/// `env` reads an environment variable. `processes` takes the process
/// snapshot; it is only called when the decision needs the ancestor chain.
///
/// 1. A host that identifies itself in the environment and accepts drops
///    itself (`TERM_PROGRAM=vscode`, which VS Code forks report too, or
///    WezTerm) wins outright: no host-window walk.
/// 2. Otherwise the walk is only possible under Windows Terminal, so
///    without `WT_SESSION` no snapshot is taken.
/// 3. With `WT_SESSION`, the nearest recognised host in the ancestor chain
///    decides. `WT_SESSION` is inherited by everything a Windows Terminal
///    tab starts, including a VS Code launched from it, so the first
///    `WindowsTerminal.exe` ancestor is not proof that it hosts clud.
pub(crate) fn resolve_drop_host(
    env: &dyn Fn(&str) -> Option<String>,
    current_pid: u32,
    processes: impl FnOnce() -> Vec<ProcessEntry>,
) -> DropHost {
    if self_drop_host_env(env) {
        return DropHost::AcceptsDropsItself;
    }
    if env("WT_SESSION").is_none() {
        return DropHost::ConsoleWindowOnly;
    }
    let entries = processes();
    for entry in ancestor_chain(current_pid, &entries) {
        if exe_is_any(&entry.exe, WINDOWS_TERMINAL_EXES) {
            return DropHost::WindowsTerminal { pid: entry.pid };
        }
        if exe_is_any(&entry.exe, SELF_DROP_HOST_EXES) {
            return DropHost::AcceptsDropsItself;
        }
    }
    DropHost::ConsoleWindowOnly
}

/// Environment set by a host whose own UI accepts Explorer drops.
fn self_drop_host_env(env: &dyn Fn(&str) -> Option<String>) -> bool {
    env("TERM_PROGRAM").is_some_and(|program| {
        program.eq_ignore_ascii_case("vscode") || program.eq_ignore_ascii_case("wezterm")
    }) || env("WEZTERM_PANE").is_some_and(|pane| !pane.is_empty())
}

/// `current_pid` and its ancestors, nearest first. Stops at a missing
/// parent, a self-parented entry, or after [`MAX_ANCESTOR_HOPS`].
fn ancestor_chain(current_pid: u32, entries: &[ProcessEntry]) -> Vec<&ProcessEntry> {
    let mut chain = Vec::new();
    let mut pid = current_pid;
    while let Some(entry) = entries.iter().find(|entry| entry.pid == pid) {
        chain.push(entry);
        if entry.parent_pid == 0 || entry.parent_pid == pid || chain.len() > MAX_ANCESTOR_HOPS {
            break;
        }
        pid = entry.parent_pid;
    }
    chain
}

fn exe_is_any(exe: &str, names: &[&str]) -> bool {
    names.iter().any(|name| exe.eq_ignore_ascii_case(name))
}

#[cfg(test)]
#[path = "drop_host_tests.rs"]
mod tests;
