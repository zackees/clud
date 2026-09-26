use super::*;

fn env_of(vars: &'static [(&'static str, &'static str)]) -> impl Fn(&str) -> Option<String> {
    move |name| {
        vars.iter()
            .find(|(key, _)| *key == name)
            .map(|(_, value)| (*value).to_string())
    }
}

/// Build a linear process chain, root first. Pids are 1-based positions.
fn chain(exes: &[&str]) -> Vec<ProcessEntry> {
    exes.iter()
        .enumerate()
        .map(|(index, exe)| ProcessEntry {
            pid: index as u32 + 1,
            parent_pid: index as u32,
            exe: (*exe).to_string(),
        })
        .collect()
}

fn resolve(vars: &'static [(&'static str, &'static str)], processes: &[ProcessEntry]) -> DropHost {
    let current_pid = processes.len() as u32;
    let snapshot = processes.to_vec();
    resolve_drop_host(&env_of(vars), current_pid, move || snapshot)
}

/// The chain VS Code builds for an integrated-terminal child: the main
/// `Code.exe` window process, its pty-host utility `Code.exe`, the shell.
/// Here VS Code itself was started from a Windows Terminal tab, so the
/// terminal inherits `WT_SESSION` and a `WindowsTerminal.exe` sits further up.
const VSCODE_UNDER_WINDOWS_TERMINAL: &[&str] = &[
    "explorer.exe",
    "WindowsTerminal.exe",
    "pwsh.exe",
    "Code.exe",
    "Code.exe",
    "pwsh.exe",
    "clud.exe",
];

// --- Windows Terminal: the walk #127 added ---------------------------------

#[test]
fn windows_terminal_parent_is_the_drop_host() {
    let processes = chain(&["explorer.exe", "WindowsTerminal.exe", "cmd.exe", "clud.exe"]);
    assert_eq!(
        resolve(&[("WT_SESSION", "abc")], &processes),
        DropHost::WindowsTerminal { pid: 2 }
    );
}

#[test]
fn windows_terminal_preview_parent_is_the_drop_host() {
    let processes = chain(&["explorer.exe", "windowsterminalpreview.exe", "clud.exe"]);
    assert_eq!(
        resolve(&[("WT_SESSION", "abc")], &processes),
        DropHost::WindowsTerminal { pid: 2 }
    );
}

#[test]
fn wt_session_without_a_windows_terminal_ancestor_uses_the_console_window() {
    let processes = chain(&["explorer.exe", "cmd.exe", "clud.exe"]);
    assert_eq!(
        resolve(&[("WT_SESSION", "abc")], &processes),
        DropHost::ConsoleWindowOnly
    );
}

#[test]
fn plain_conhost_never_takes_a_process_snapshot() {
    let host = resolve_drop_host(&env_of(&[]), 3, || {
        panic!("conhost needs no ancestor walk, so no snapshot")
    });
    assert_eq!(host, DropHost::ConsoleWindowOnly);
}

#[test]
fn ancestor_walk_stops_on_a_parent_cycle() {
    let processes = vec![
        ProcessEntry {
            pid: 1,
            parent_pid: 2,
            exe: "a.exe".to_string(),
        },
        ProcessEntry {
            pid: 2,
            parent_pid: 1,
            exe: "b.exe".to_string(),
        },
    ];
    assert!(ancestor_chain(1, &processes).len() <= MAX_ANCESTOR_HOPS + 1);
    assert_eq!(
        resolve_drop_host(&env_of(&[("WT_SESSION", "abc")]), 1, move || processes),
        DropHost::ConsoleWindowOnly
    );
}

// --- VS Code and other hosts that accept drops themselves (#1358) ----------

#[test]
fn vscode_terminal_never_walks_to_an_outer_windows_terminal() {
    let processes = chain(VSCODE_UNDER_WINDOWS_TERMINAL);
    assert_eq!(
        resolve(
            &[("WT_SESSION", "inherited"), ("TERM_PROGRAM", "vscode")],
            &processes
        ),
        DropHost::AcceptsDropsItself
    );
}

#[test]
fn code_exe_ancestor_is_the_nearest_host_even_without_term_program() {
    let processes = chain(VSCODE_UNDER_WINDOWS_TERMINAL);
    assert_eq!(
        resolve(&[("WT_SESSION", "inherited")], &processes),
        DropHost::AcceptsDropsItself
    );
}

#[test]
fn code_insiders_ancestor_is_recognized() {
    let processes = chain(&[
        "WindowsTerminal.exe",
        "pwsh.exe",
        "Code - Insiders.exe",
        "Code - Insiders.exe",
        "pwsh.exe",
        "clud.exe",
    ]);
    assert_eq!(
        resolve(&[("WT_SESSION", "inherited")], &processes),
        DropHost::AcceptsDropsItself
    );
}

#[test]
fn vscode_terminal_without_wt_session_is_classified_as_vscode() {
    let processes = chain(&[
        "explorer.exe",
        "Code.exe",
        "Code.exe",
        "pwsh.exe",
        "clud.exe",
    ]);
    assert_eq!(
        resolve(&[("TERM_PROGRAM", "vscode")], &processes),
        DropHost::AcceptsDropsItself
    );
}

#[test]
fn vscode_fork_is_recognized_by_term_program_alone() {
    // Cursor, VSCodium and Windsurf report TERM_PROGRAM=vscode under their
    // own executable names.
    let processes = chain(&[
        "WindowsTerminal.exe",
        "pwsh.exe",
        "Cursor.exe",
        "Cursor.exe",
        "pwsh.exe",
        "clud.exe",
    ]);
    assert_eq!(
        resolve(
            &[("WT_SESSION", "inherited"), ("TERM_PROGRAM", "vscode")],
            &processes
        ),
        DropHost::AcceptsDropsItself
    );
}

#[test]
fn wezterm_under_windows_terminal_is_its_own_host() {
    let processes = chain(&[
        "WindowsTerminal.exe",
        "pwsh.exe",
        "wezterm-gui.exe",
        "pwsh.exe",
        "clud.exe",
    ]);
    assert_eq!(
        resolve(&[("WT_SESSION", "inherited")], &processes),
        DropHost::AcceptsDropsItself
    );
}

#[test]
fn windows_terminal_nearest_to_clud_still_wins_over_an_outer_code_exe() {
    // Windows Terminal started from a VS Code terminal: WT is the host.
    let processes = chain(&[
        "Code.exe",
        "Code.exe",
        "pwsh.exe",
        "WindowsTerminal.exe",
        "pwsh.exe",
        "clud.exe",
    ]);
    assert_eq!(
        resolve(&[("WT_SESSION", "abc")], &processes),
        DropHost::WindowsTerminal { pid: 4 }
    );
}

#[test]
fn self_identified_host_needs_no_process_snapshot() {
    let host = resolve_drop_host(
        &env_of(&[("WT_SESSION", "inherited"), ("TERM_PROGRAM", "vscode")]),
        7,
        || panic!("TERM_PROGRAM=vscode decides without an ancestor walk"),
    );
    assert_eq!(host, DropHost::AcceptsDropsItself);
}

#[test]
fn wezterm_pane_env_is_recognized_without_term_program() {
    let processes = chain(&["WindowsTerminal.exe", "pwsh.exe", "clud.exe"]);
    assert_eq!(
        resolve(
            &[("WT_SESSION", "inherited"), ("WEZTERM_PANE", "0")],
            &processes
        ),
        DropHost::AcceptsDropsItself
    );
}
