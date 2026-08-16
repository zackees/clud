//! Installed-harness discovery and the bare-launch countdown picker.

use std::io::{self, Write};
use std::time::{Duration, Instant};

use crossterm::event::{self, Event, KeyCode, KeyEventKind, KeyModifiers};
use crossterm::terminal;

use crate::args::Args;
use crate::backend::Backend;

pub const DEFAULT_COUNTDOWN: Duration = Duration::from_secs(3);

pub fn should_select(args: &Args, stdin_is_terminal: bool, stderr_is_terminal: bool) -> bool {
    stdin_is_terminal
        && stderr_is_terminal
        && !args.dry_run
        && args.command.is_none()
        && args.explicit_model_provider().is_none()
        && args.harness.is_none()
        && args.model.is_none()
        && args.effort.is_none()
        && args.context_window.is_none()
        && args.prompt.is_none()
        && args.message.is_none()
        && !args.continue_session
        && args.resume.is_none()
        && !args.detach
        && !args.detachable
        && args.transcript.is_none()
        && !args.experimental_daemon_centralized
        && args.daemon_mode.is_none()
        && args.passthrough.is_empty()
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SelectionFlow {
    NoneInstalled,
    Immediate(Backend),
    Prompt(Backend),
}

pub fn default_harness(installed: &[Backend], saved: Option<Backend>) -> Backend {
    saved
        .filter(|saved| installed.contains(saved))
        .unwrap_or(installed[0])
}

pub fn selection_flow(installed: &[Backend], saved: Option<Backend>) -> SelectionFlow {
    match installed {
        [] => SelectionFlow::NoneInstalled,
        [only] => SelectionFlow::Immediate(*only),
        many => SelectionFlow::Prompt(default_harness(many, saved)),
    }
}

pub fn discover_installed_with<F>(mut locate: F) -> Vec<Backend>
where
    F: FnMut(Backend) -> bool,
{
    Backend::ALL
        .into_iter()
        .filter(|backend| locate(*backend))
        .collect()
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PickerEvent {
    Up,
    Down,
    Enter,
    Cancel,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PickerOutcome {
    Selected(Backend),
    Cancelled,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PickerModel {
    options: Vec<Backend>,
    selected: usize,
    countdown: Duration,
    countdown_active: bool,
}

impl PickerModel {
    pub fn new(options: Vec<Backend>, default: Backend, countdown: Duration) -> Self {
        assert!(options.len() >= 2, "picker requires at least two harnesses");
        let selected = options
            .iter()
            .position(|backend| *backend == default)
            .unwrap_or(0);
        Self {
            options,
            selected,
            countdown,
            countdown_active: true,
        }
    }

    pub fn selected(&self) -> Backend {
        self.options[self.selected]
    }

    pub fn countdown_active(&self) -> bool {
        self.countdown_active
    }

    pub fn remaining_seconds(&self, elapsed: Duration) -> u64 {
        if !self.countdown_active {
            return 0;
        }
        let remaining = self.countdown.saturating_sub(elapsed);
        remaining.as_millis().div_ceil(1_000) as u64
    }

    pub fn handle(&mut self, event: PickerEvent) -> Option<PickerOutcome> {
        match event {
            PickerEvent::Up => {
                self.countdown_active = false;
                self.selected = self.selected.saturating_sub(1);
                None
            }
            PickerEvent::Down => {
                self.countdown_active = false;
                if self.selected + 1 < self.options.len() {
                    self.selected += 1;
                }
                None
            }
            PickerEvent::Enter => Some(PickerOutcome::Selected(self.selected())),
            PickerEvent::Cancel => Some(PickerOutcome::Cancelled),
        }
    }

    pub fn tick(&self, elapsed: Duration) -> Option<PickerOutcome> {
        (self.countdown_active && elapsed >= self.countdown)
            .then(|| PickerOutcome::Selected(self.selected()))
    }

    fn rendered_lines(&self) -> usize {
        self.options.len() + 4
    }

    fn render<W: Write>(&self, out: &mut W, elapsed: Duration) -> io::Result<()> {
        writeln!(out, "Select an agent harness")?;
        if self.countdown_active {
            writeln!(
                out,
                "  Auto-launching in {}s  |  Up/Down choose, Enter launch",
                self.remaining_seconds(elapsed)
            )?;
        } else {
            writeln!(out, "  Up/Down choose, Enter launch, Esc cancel")?;
        }
        writeln!(out)?;
        for backend in &self.options {
            let selected = *backend == self.selected();
            writeln!(
                out,
                "{} {} {}",
                if selected { ">" } else { " " },
                if selected { "[x]" } else { "[ ]" },
                display_name(*backend)
            )?;
        }
        writeln!(out, "  Last choice is remembered")?;
        out.flush()
    }
}

pub fn display_name(backend: Backend) -> &'static str {
    match backend {
        Backend::Claude => "Claude Code",
        Backend::Codex => "Codex CLI",
        Backend::DeepSeek => "DeepSeek Harness",
    }
}

pub fn prompt<W: Write>(
    out: &mut W,
    options: Vec<Backend>,
    default: Backend,
) -> io::Result<PickerOutcome> {
    let _raw = RawModeGuard::enable()?;
    write!(out, "\x1b[?25l")?;
    out.flush()?;

    let result = prompt_inner(out, options, default);
    let restore = write!(out, "\x1b[?25h").and_then(|_| out.flush());
    match result {
        Ok(value) => restore.map(|_| value),
        Err(error) => {
            let _ = restore;
            Err(error)
        }
    }
}

fn prompt_inner<W: Write>(
    out: &mut W,
    options: Vec<Backend>,
    default: Backend,
) -> io::Result<PickerOutcome> {
    drain_pending_terminal_events()?;
    let started = Instant::now();
    let mut picker = PickerModel::new(options, default, DEFAULT_COUNTDOWN);
    picker.render(out, Duration::ZERO)?;
    let mut rendered_seconds = picker.remaining_seconds(Duration::ZERO);

    loop {
        let elapsed = started.elapsed();
        if let Some(outcome) = picker.tick(elapsed) {
            clear_render(out, picker.rendered_lines())?;
            return Ok(outcome);
        }

        if !event::poll(Duration::from_millis(100))? {
            let remaining = picker.remaining_seconds(elapsed);
            if picker.countdown_active() && remaining != rendered_seconds {
                redraw(out, &picker, elapsed)?;
                rendered_seconds = remaining;
            }
            continue;
        }
        let Event::Key(key) = event::read()? else {
            continue;
        };
        if key.kind == KeyEventKind::Release {
            continue;
        }
        let input = match key.code {
            KeyCode::Up | KeyCode::Char('k') => Some(PickerEvent::Up),
            KeyCode::Down | KeyCode::Char('j') => Some(PickerEvent::Down),
            KeyCode::Enter => Some(PickerEvent::Enter),
            KeyCode::Esc => Some(PickerEvent::Cancel),
            KeyCode::Char('c' | 'd') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                Some(PickerEvent::Cancel)
            }
            _ => None,
        };
        let Some(input) = input else {
            continue;
        };
        if let Some(outcome) = picker.handle(input) {
            clear_render(out, picker.rendered_lines())?;
            return Ok(outcome);
        }
        redraw(out, &picker, elapsed)?;
        rendered_seconds = picker.remaining_seconds(elapsed);
    }
}

fn redraw<W: Write>(out: &mut W, picker: &PickerModel, elapsed: Duration) -> io::Result<()> {
    write!(out, "\x1b[{}A\x1b[J", picker.rendered_lines())?;
    picker.render(out, elapsed)
}

fn clear_render<W: Write>(out: &mut W, rendered_lines: usize) -> io::Result<()> {
    write!(out, "\x1b[{}A\x1b[J", rendered_lines)?;
    out.flush()
}

fn drain_pending_terminal_events() -> io::Result<()> {
    while event::poll(Duration::ZERO)? {
        let _ = event::read()?;
    }
    Ok(())
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::backend::Backend;
    use std::time::Duration;

    fn parse(argv: &[&str]) -> crate::args::Args {
        crate::args::Args::parse_from_raw(argv.iter().map(|arg| (*arg).to_string()).collect())
    }

    #[test]
    fn discovery_is_stably_ordered() {
        let installed = discover_installed_with(|backend| backend != Backend::Codex);
        assert_eq!(installed, vec![Backend::Claude, Backend::DeepSeek]);
    }

    #[test]
    fn only_bare_interactive_launches_offer_the_picker() {
        assert!(should_select(&parse(&["clud"]), true, true));
        for argv in [
            vec!["clud", "--codex"],
            vec!["clud", "--harness", "deepseek"],
            vec!["clud", "--dry-run"],
            vec!["clud", "-p", "hello"],
            vec!["clud", "--transcript", "session.log"],
            vec!["clud", "--experimental-daemon-centralized"],
            vec!["clud", "loop", "task"],
        ] {
            assert!(!should_select(&parse(&argv), true, true), "argv={argv:?}");
        }
        assert!(!should_select(&parse(&["clud"]), false, true));
        assert!(!should_select(&parse(&["clud"]), true, false));
    }

    #[test]
    fn stable_order_and_saved_default_fallback() {
        let installed = vec![Backend::Claude, Backend::Codex, Backend::DeepSeek];
        assert_eq!(
            default_harness(&installed, Some(Backend::Codex)),
            Backend::Codex
        );
        assert_eq!(default_harness(&installed, None), Backend::Claude);
        assert_eq!(
            default_harness(&[Backend::Codex, Backend::DeepSeek], Some(Backend::Claude)),
            Backend::Codex
        );
    }

    #[test]
    fn zero_one_and_many_installed_harnesses_choose_the_right_flow() {
        assert_eq!(selection_flow(&[], None), SelectionFlow::NoneInstalled);
        assert_eq!(
            selection_flow(&[Backend::Codex], Some(Backend::Claude)),
            SelectionFlow::Immediate(Backend::Codex)
        );
        assert_eq!(
            selection_flow(&[Backend::Claude, Backend::Codex], Some(Backend::Codex)),
            SelectionFlow::Prompt(Backend::Codex)
        );
    }

    #[test]
    fn timeout_confirms_default_without_input() {
        let picker = PickerModel::new(
            vec![Backend::Claude, Backend::Codex],
            Backend::Codex,
            Duration::from_secs(3),
        );
        assert_eq!(picker.tick(Duration::from_millis(2_999)), None);
        assert_eq!(
            picker.tick(Duration::from_secs(3)),
            Some(PickerOutcome::Selected(Backend::Codex))
        );
    }

    #[test]
    fn navigation_changes_selection_and_cancels_countdown() {
        let mut picker = PickerModel::new(
            vec![Backend::Claude, Backend::Codex, Backend::DeepSeek],
            Backend::Claude,
            Duration::from_secs(3),
        );
        assert_eq!(picker.handle(PickerEvent::Down), None);
        assert_eq!(picker.selected(), Backend::Codex);
        assert!(!picker.countdown_active());
        assert_eq!(picker.tick(Duration::from_secs(30)), None);
        assert_eq!(
            picker.handle(PickerEvent::Enter),
            Some(PickerOutcome::Selected(Backend::Codex))
        );
    }

    #[test]
    fn cancel_never_selects() {
        let mut picker = PickerModel::new(
            vec![Backend::Claude, Backend::Codex],
            Backend::Claude,
            Duration::from_secs(3),
        );
        assert_eq!(
            picker.handle(PickerEvent::Cancel),
            Some(PickerOutcome::Cancelled)
        );
    }
}
