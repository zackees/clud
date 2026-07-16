//! `clud settings` — a small, cross-platform TUI checkbox menu over the
//! boolean settings in `~/.clud/settings.json`.
//!
//! Split the same way `launch_setup.rs`'s `ScopeSelector` is: a pure,
//! unit-tested state machine (`Menu`) plus a thin impure terminal-I/O shell
//! (`run_interactive`/`run_interactive_inner`) built on the same crossterm
//! primitives (raw-mode RAII guard, raw-ANSI cursor hide/show, redraw via
//! cursor-up + clear-to-end) already proven cross-platform in this repo.
//! Only boolean settings exist today; `SettingItem` is a `Vec` so adding a
//! second one later is additive, not a rewrite.

use std::io::{self, IsTerminal, Write};
use std::time::Duration;

use crossterm::event::{self, Event, KeyCode, KeyModifiers};
use crossterm::terminal;

use crate::clud_settings;

#[derive(Clone)]
struct SettingItem {
    key: &'static str,
    label: &'static str,
    note: &'static str,
    value: bool,
    save: fn(bool) -> Result<(), clud_settings::SettingsError>,
}

fn setting_items() -> Vec<SettingItem> {
    vec![SettingItem {
        key: "git.pr_wait_fail_fast",
        label: "PR-wait fail-fast git commands",
        note: "Blocks raw `gh pr checks --watch`-style commands in favor of \
               a bundled fail-fast waiter script. Off by default; may \
               become the default later.",
        value: clud_settings::load_pr_wait_fail_fast_enabled().unwrap_or(false),
        save: clud_settings::save_pr_wait_fail_fast_enabled,
    }]
}

pub fn run(list_only: bool) -> i32 {
    let items = setting_items();

    if list_only {
        for item in &items {
            println!("{} = {}  # {}", item.key, item.value, item.note);
        }
        return 0;
    }

    if !io::stdin().is_terminal() || !io::stdout().is_terminal() {
        eprintln!(
            "clud settings requires an interactive terminal. Use `clud settings --list` to view current values."
        );
        return 1;
    }

    match run_interactive(items) {
        Ok(()) => 0,
        Err(error) => {
            eprintln!("clud settings: {error}");
            1
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum MenuEvent {
    Up,
    Down,
    Toggle,
    Quit,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum MenuAction {
    Redraw,
    RequestSaveDecision,
    ExitClean,
}

struct Menu {
    items: Vec<SettingItem>,
    original: Vec<bool>,
    cursor: usize,
}

impl Menu {
    fn new(items: Vec<SettingItem>) -> Self {
        let original = items.iter().map(|item| item.value).collect();
        Self {
            items,
            original,
            cursor: 0,
        }
    }

    fn is_dirty(&self) -> bool {
        self.items
            .iter()
            .map(|item| item.value)
            .ne(self.original.iter().copied())
    }

    fn handle(&mut self, event: MenuEvent) -> MenuAction {
        match event {
            MenuEvent::Up => {
                self.cursor = self.cursor.saturating_sub(1);
                MenuAction::Redraw
            }
            MenuEvent::Down => {
                if self.cursor + 1 < self.items.len() {
                    self.cursor += 1;
                }
                MenuAction::Redraw
            }
            MenuEvent::Toggle => {
                if let Some(item) = self.items.get_mut(self.cursor) {
                    item.value = !item.value;
                }
                MenuAction::Redraw
            }
            MenuEvent::Quit => {
                if self.is_dirty() {
                    MenuAction::RequestSaveDecision
                } else {
                    MenuAction::ExitClean
                }
            }
        }
    }

    /// Title + hint + blank separator, then a fixed 2-line unit per item
    /// (label line + always-visible note line) — keeping this a trivial
    /// constant is what makes the cursor-up-N redraw trick work as more
    /// settings are added later.
    fn rendered_lines(&self) -> usize {
        3 + self.items.len() * 2
    }

    fn render<W: Write>(&self, out: &mut W) -> io::Result<()> {
        writeln!(out, "clud settings")?;
        writeln!(out, "  Space toggle, q quit")?;
        writeln!(out)?;
        for (index, item) in self.items.iter().enumerate() {
            writeln!(
                out,
                "{} {} {}",
                cursor_marker(index == self.cursor),
                checkbox(item.value),
                item.label
            )?;
            writeln!(out, "      {}", item.note)?;
        }
        out.flush()
    }
}

fn cursor_marker(selected: bool) -> &'static str {
    if selected {
        ">"
    } else {
        " "
    }
}

fn checkbox(value: bool) -> &'static str {
    if value {
        "[x]"
    } else {
        "[ ]"
    }
}

fn menu_event_for_key(code: KeyCode) -> Option<MenuEvent> {
    match code {
        KeyCode::Up | KeyCode::Char('k') => Some(MenuEvent::Up),
        KeyCode::Down | KeyCode::Char('j') => Some(MenuEvent::Down),
        KeyCode::Char(' ') => Some(MenuEvent::Toggle),
        KeyCode::Char('q') => Some(MenuEvent::Quit),
        _ => None,
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SaveDecision {
    Save,
    Discard,
    Cancel,
}

fn save_decision_for_key(code: KeyCode) -> Option<SaveDecision> {
    match code {
        KeyCode::Char('y' | 'Y') | KeyCode::Enter => Some(SaveDecision::Save),
        KeyCode::Char('n' | 'N') => Some(SaveDecision::Discard),
        KeyCode::Esc => Some(SaveDecision::Cancel),
        _ => None,
    }
}

fn is_ctrl_c_or_d(code: KeyCode, modifiers: KeyModifiers) -> bool {
    matches!(code, KeyCode::Char('c' | 'd')) && modifiers.contains(KeyModifiers::CONTROL)
}

struct RawModeGuard;

impl RawModeGuard {
    fn enable() -> io::Result<Self> {
        terminal::enable_raw_mode()?;
        Ok(Self)
    }
}

impl Drop for RawModeGuard {
    fn drop(&mut self) {
        let _ = terminal::disable_raw_mode();
    }
}

fn run_interactive(items: Vec<SettingItem>) -> io::Result<()> {
    let mut out = io::stdout();
    let _raw = RawModeGuard::enable()?;
    write!(out, "\x1b[?25l")?;
    out.flush()?;

    let result = run_interactive_inner(&mut out, items);

    let restore_result = write!(out, "\x1b[?25h").and_then(|_| out.flush());
    match result {
        Ok(()) => restore_result,
        Err(error) => {
            let _ = restore_result;
            Err(error)
        }
    }
}

fn run_interactive_inner<W: Write>(out: &mut W, items: Vec<SettingItem>) -> io::Result<()> {
    let mut menu = Menu::new(items);
    menu.render(out)?;
    let _ = drain_pending_terminal_events();

    loop {
        let Event::Key(key) = event::read()? else {
            continue;
        };
        if is_ctrl_c_or_d(key.code, key.modifiers) {
            return Err(io::Error::new(
                io::ErrorKind::Interrupted,
                "clud settings cancelled",
            ));
        }
        let Some(event) = menu_event_for_key(key.code) else {
            continue;
        };
        match menu.handle(event) {
            MenuAction::Redraw => redraw(out, &menu)?,
            MenuAction::ExitClean => {
                writeln!(out)?;
                return Ok(());
            }
            MenuAction::RequestSaveDecision => match prompt_save_decision(out)? {
                SaveDecision::Save => {
                    for item in &menu.items {
                        (item.save)(item.value).map_err(|error| {
                            io::Error::other(format!("saving {}: {error}", item.key))
                        })?;
                    }
                    writeln!(out)?;
                    return Ok(());
                }
                SaveDecision::Discard => {
                    writeln!(out)?;
                    return Ok(());
                }
                SaveDecision::Cancel => {
                    // `prompt_save_decision` already erased its own prompt
                    // line; the menu above it is untouched, nothing to redraw.
                }
            },
        }
    }
}

fn redraw<W: Write>(out: &mut W, menu: &Menu) -> io::Result<()> {
    write!(out, "\x1b[{}A\x1b[J", menu.rendered_lines())?;
    menu.render(out)
}

fn prompt_save_decision<W: Write>(out: &mut W) -> io::Result<SaveDecision> {
    writeln!(out, "Unsaved changes. Save before exiting? [Y/n]")?;
    out.flush()?;
    loop {
        let Event::Key(key) = event::read()? else {
            continue;
        };
        if is_ctrl_c_or_d(key.code, key.modifiers) {
            return Err(io::Error::new(
                io::ErrorKind::Interrupted,
                "clud settings cancelled",
            ));
        }
        if let Some(decision) = save_decision_for_key(key.code) {
            if decision == SaveDecision::Cancel {
                write!(out, "\x1b[1A\x1b[J")?;
            }
            return Ok(decision);
        }
    }
}

fn drain_pending_terminal_events() -> io::Result<usize> {
    let mut drained = 0;
    while event::poll(Duration::from_millis(0))? {
        let _ = event::read()?;
        drained += 1;
    }
    Ok(drained)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn item(value: bool) -> SettingItem {
        SettingItem {
            key: "test.key",
            label: "Test setting",
            note: "note",
            value,
            save: |_| Ok(()),
        }
    }

    #[test]
    fn toggle_flips_value_and_marks_dirty() {
        let mut menu = Menu::new(vec![item(false)]);
        assert!(!menu.is_dirty());
        assert_eq!(menu.handle(MenuEvent::Toggle), MenuAction::Redraw);
        assert!(menu.items[0].value);
        assert!(menu.is_dirty());
    }

    #[test]
    fn toggle_twice_returns_to_clean() {
        let mut menu = Menu::new(vec![item(false)]);
        menu.handle(MenuEvent::Toggle);
        menu.handle(MenuEvent::Toggle);
        assert!(!menu.items[0].value);
        assert!(!menu.is_dirty());
    }

    #[test]
    fn quit_with_no_changes_exits_clean() {
        let mut menu = Menu::new(vec![item(false)]);
        assert_eq!(menu.handle(MenuEvent::Quit), MenuAction::ExitClean);
    }

    #[test]
    fn quit_with_changes_requests_save_decision() {
        let mut menu = Menu::new(vec![item(false)]);
        menu.handle(MenuEvent::Toggle);
        assert_eq!(
            menu.handle(MenuEvent::Quit),
            MenuAction::RequestSaveDecision
        );
    }

    #[test]
    fn cursor_clamps_at_list_ends() {
        let mut menu = Menu::new(vec![item(false), item(true)]);
        assert_eq!(menu.cursor, 0);
        menu.handle(MenuEvent::Up);
        assert_eq!(menu.cursor, 0, "cannot move above the first row");
        menu.handle(MenuEvent::Down);
        assert_eq!(menu.cursor, 1);
        menu.handle(MenuEvent::Down);
        assert_eq!(menu.cursor, 1, "cannot move below the last row");
    }

    #[test]
    fn toggle_only_affects_the_highlighted_row() {
        let mut menu = Menu::new(vec![item(false), item(false)]);
        menu.handle(MenuEvent::Down);
        menu.handle(MenuEvent::Toggle);
        assert!(!menu.items[0].value);
        assert!(menu.items[1].value);
    }

    #[test]
    fn rendered_lines_matches_actual_render_output() {
        let menu = Menu::new(vec![item(false), item(true)]);
        let mut buf = Vec::new();
        menu.render(&mut buf).unwrap();
        let text = String::from_utf8(buf).unwrap();
        assert_eq!(
            text.lines().count(),
            menu.rendered_lines(),
            "rendered_lines() must track render()'s actual line count for the redraw math"
        );
    }

    #[test]
    fn key_mapping_covers_navigation_toggle_and_quit() {
        assert_eq!(menu_event_for_key(KeyCode::Up), Some(MenuEvent::Up));
        assert_eq!(menu_event_for_key(KeyCode::Char('k')), Some(MenuEvent::Up));
        assert_eq!(menu_event_for_key(KeyCode::Down), Some(MenuEvent::Down));
        assert_eq!(
            menu_event_for_key(KeyCode::Char('j')),
            Some(MenuEvent::Down)
        );
        assert_eq!(
            menu_event_for_key(KeyCode::Char(' ')),
            Some(MenuEvent::Toggle)
        );
        assert_eq!(
            menu_event_for_key(KeyCode::Char('q')),
            Some(MenuEvent::Quit)
        );
        assert_eq!(menu_event_for_key(KeyCode::Char('x')), None);
    }

    #[test]
    fn save_decision_key_mapping() {
        assert_eq!(
            save_decision_for_key(KeyCode::Char('y')),
            Some(SaveDecision::Save)
        );
        assert_eq!(
            save_decision_for_key(KeyCode::Char('Y')),
            Some(SaveDecision::Save)
        );
        assert_eq!(
            save_decision_for_key(KeyCode::Enter),
            Some(SaveDecision::Save)
        );
        assert_eq!(
            save_decision_for_key(KeyCode::Char('n')),
            Some(SaveDecision::Discard)
        );
        assert_eq!(
            save_decision_for_key(KeyCode::Esc),
            Some(SaveDecision::Cancel)
        );
        assert_eq!(save_decision_for_key(KeyCode::Char('z')), None);
    }

    #[test]
    fn ctrl_c_and_ctrl_d_are_detected_regardless_of_other_keys() {
        assert!(is_ctrl_c_or_d(KeyCode::Char('c'), KeyModifiers::CONTROL));
        assert!(is_ctrl_c_or_d(KeyCode::Char('d'), KeyModifiers::CONTROL));
        assert!(!is_ctrl_c_or_d(KeyCode::Char('c'), KeyModifiers::NONE));
        assert!(!is_ctrl_c_or_d(KeyCode::Char('x'), KeyModifiers::CONTROL));
    }
}
