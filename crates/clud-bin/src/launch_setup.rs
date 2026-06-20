//! Launch-scope selection and per-backend persistent setup.
//!
//! Session-only launches are the default for automation and one-shot prompt
//! paths. Interactive TUI launches can opt into global setup before the backend
//! starts.

use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::time::Duration;

use crossterm::event::{self, Event, KeyCode};
use crossterm::terminal;

use crate::args::Args;
use crate::backend::Backend;
use crate::{codex_hook_normalize, skill_install, skills};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LaunchSetupScope {
    SessionOnly,
    Global,
}

impl LaunchSetupScope {
    pub fn as_str(self) -> &'static str {
        match self {
            LaunchSetupScope::SessionOnly => "session-only",
            LaunchSetupScope::Global => "global",
        }
    }

    pub fn from_settings_str(value: &str) -> Option<Self> {
        match value {
            "session-only" | "session_only" => Some(LaunchSetupScope::SessionOnly),
            "global" => Some(LaunchSetupScope::Global),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SelectorEvent {
    Up,
    Down,
    Enter,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ScopeSelector {
    selected: LaunchSetupScope,
}

impl Default for ScopeSelector {
    fn default() -> Self {
        Self {
            selected: LaunchSetupScope::SessionOnly,
        }
    }
}

impl ScopeSelector {
    pub fn selected(self) -> LaunchSetupScope {
        self.selected
    }

    pub fn handle(&mut self, event: SelectorEvent) -> Option<LaunchSetupScope> {
        match event {
            SelectorEvent::Up => self.selected = LaunchSetupScope::SessionOnly,
            SelectorEvent::Down => self.selected = LaunchSetupScope::Global,
            SelectorEvent::Enter => return Some(self.selected),
        }
        None
    }

    pub fn render<W: Write>(&self, out: &mut W) -> io::Result<()> {
        writeln!(out, "Launch setup scope (Up/Down, Enter):")?;
        writeln!(
            out,
            "{} Session only",
            marker(self.selected == LaunchSetupScope::SessionOnly)
        )?;
        writeln!(
            out,
            "{} Globally",
            marker(self.selected == LaunchSetupScope::Global)
        )?;
        out.flush()
    }
}

fn marker(selected: bool) -> &'static str {
    if selected {
        "[x]"
    } else {
        "[ ]"
    }
}

pub fn should_prompt_for_scope(args: &Args, interactive_terminal: bool) -> bool {
    interactive_terminal
        && (args.claude || args.codex)
        && !args.dry_run
        && args.prompt.is_none()
        && args.message.is_none()
        && !args.continue_session
        && args.resume.is_none()
        && args.command.is_none()
}

pub fn scope_for_non_prompting_launch(
    args: &Args,
    interactive_terminal: bool,
) -> Option<LaunchSetupScope> {
    (!should_prompt_for_scope(args, interactive_terminal)).then_some(LaunchSetupScope::SessionOnly)
}

pub fn scope_for_configured_launch(
    args: &Args,
    interactive_terminal: bool,
    configured_scope: Option<LaunchSetupScope>,
) -> Option<LaunchSetupScope> {
    if !args.dry_run {
        if let Some(scope) = configured_scope {
            return Some(scope);
        }
    }
    scope_for_non_prompting_launch(args, interactive_terminal)
}

pub fn prompt_scope<W: Write>(out: &mut W) -> io::Result<LaunchSetupScope> {
    let _raw = RawModeGuard::enable()?;
    let mut selector = ScopeSelector::default();
    selector.render(out)?;
    let _ = drain_pending_terminal_events();

    loop {
        let Event::Key(key) = event::read()? else {
            continue;
        };
        let event = match key.code {
            KeyCode::Up => SelectorEvent::Up,
            KeyCode::Down => SelectorEvent::Down,
            KeyCode::Enter => SelectorEvent::Enter,
            _ => continue,
        };
        if let Some(scope) = selector.handle(event) {
            writeln!(out)?;
            out.flush()?;
            return Ok(scope);
        }
        write!(out, "\x1b[3A")?;
        selector.render(out)?;
    }
}

fn drain_pending_terminal_events() -> io::Result<usize> {
    drain_pending_events(|| event::poll(Duration::from_millis(0)), event::read)
}

fn drain_pending_events<P, R>(mut poll: P, mut read: R) -> io::Result<usize>
where
    P: FnMut() -> io::Result<bool>,
    R: FnMut() -> io::Result<Event>,
{
    let mut drained = 0;
    while poll()? {
        let _ = read()?;
        drained += 1;
    }
    Ok(drained)
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

#[derive(Debug)]
pub enum SetupError {
    NoHomeDir,
    Skills(skills::InstallError),
    Io(io::Error),
}

impl std::fmt::Display for SetupError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SetupError::NoHomeDir => write!(f, "could not resolve user home directory"),
            SetupError::Skills(error) => write!(f, "{error}"),
            SetupError::Io(error) => write!(f, "{error}"),
        }
    }
}

impl std::error::Error for SetupError {}

impl From<skills::InstallError> for SetupError {
    fn from(error: skills::InstallError) -> Self {
        SetupError::Skills(error)
    }
}

impl From<io::Error> for SetupError {
    fn from(error: io::Error) -> Self {
        SetupError::Io(error)
    }
}

#[derive(Debug, Default, PartialEq, Eq)]
pub struct SetupReport {
    pub ran: Vec<&'static str>,
}

pub trait HarnessSetupAction {
    fn name(&self) -> &'static str;
    fn backend(&self) -> Backend;
    fn supports(&self, scope: LaunchSetupScope) -> bool;
    fn run(&self, ctx: &mut SetupContext<'_>) -> Result<(), SetupError>;
}

pub struct SetupContext<'a> {
    pub home: &'a Path,
    pub verbose: bool,
    pub out: &'a mut dyn Write,
}

struct BundledSkillsAction {
    backend: Backend,
}

impl HarnessSetupAction for BundledSkillsAction {
    fn name(&self) -> &'static str {
        "bundled-skills"
    }

    fn backend(&self) -> Backend {
        self.backend
    }

    fn supports(&self, scope: LaunchSetupScope) -> bool {
        matches!(scope, LaunchSetupScope::Global)
    }

    fn run(&self, ctx: &mut SetupContext<'_>) -> Result<(), SetupError> {
        let _ = skills::ensure_installed_for_backend_at(ctx.home, self.backend)?;
        Ok(())
    }
}

struct ClaudeDriftSkillsAction;

impl HarnessSetupAction for ClaudeDriftSkillsAction {
    fn name(&self) -> &'static str {
        "claude-drift-skills"
    }

    fn backend(&self) -> Backend {
        Backend::Claude
    }

    fn supports(&self, scope: LaunchSetupScope) -> bool {
        matches!(scope, LaunchSetupScope::Global)
    }

    fn run(&self, ctx: &mut SetupContext<'_>) -> Result<(), SetupError> {
        skill_install::ensure_installed_at(ctx.home);
        Ok(())
    }
}

struct CodexHookNormalizeAction;

impl HarnessSetupAction for CodexHookNormalizeAction {
    fn name(&self) -> &'static str {
        "codex-hook-normalize"
    }

    fn backend(&self) -> Backend {
        Backend::Codex
    }

    fn supports(&self, scope: LaunchSetupScope) -> bool {
        matches!(scope, LaunchSetupScope::Global)
    }

    fn run(&self, ctx: &mut SetupContext<'_>) -> Result<(), SetupError> {
        let clud_dir = ctx.home.join(".clud");
        let hooks_path = ctx.home.join(".codex").join("hooks.json");
        if let Err(error) =
            codex_hook_normalize::run_at(&clud_dir, &hooks_path, ctx.out, ctx.verbose)
        {
            if ctx.verbose {
                let _ = writeln!(ctx.out, "[clud] codex hook normalize: {error}");
            }
        }
        Ok(())
    }
}

pub fn setup_actions() -> Vec<Box<dyn HarnessSetupAction>> {
    // Note: bundled Python tools (~/.clud/tools/*) are installed by the
    // daemon at startup (see `daemon/server.rs::run_daemon`), not as part
    // of this launch-setup pipeline. `clud tool run` bootstraps the
    // daemon when needed so first-run hooks bypass NotFound. The launch
    // setup actions here are limited to backend-specific skills, drift
    // tracking, and codex hook normalization.
    vec![
        Box::new(BundledSkillsAction {
            backend: Backend::Claude,
        }),
        Box::new(BundledSkillsAction {
            backend: Backend::Codex,
        }),
        Box::new(ClaudeDriftSkillsAction),
        Box::new(CodexHookNormalizeAction),
    ]
}

pub fn run_setup(
    scope: LaunchSetupScope,
    backend: Backend,
    verbose: bool,
    out: &mut dyn Write,
) -> Result<SetupReport, SetupError> {
    let home = home_dir().ok_or(SetupError::NoHomeDir)?;
    run_setup_at(&home, scope, backend, verbose, out)
}

pub fn run_setup_at(
    home: &Path,
    scope: LaunchSetupScope,
    backend: Backend,
    verbose: bool,
    out: &mut dyn Write,
) -> Result<SetupReport, SetupError> {
    if matches!(scope, LaunchSetupScope::SessionOnly) {
        return Ok(SetupReport::default());
    }

    let mut report = SetupReport::default();
    let mut ctx = SetupContext { home, verbose, out };
    for action in setup_actions() {
        if action.backend() == backend && action.supports(scope) {
            action.run(&mut ctx)?;
            report.ran.push(action.name());
        }
    }
    Ok(report)
}

fn home_dir() -> Option<PathBuf> {
    #[cfg(windows)]
    {
        if let Some(p) = std::env::var_os("USERPROFILE") {
            if !p.is_empty() {
                return Some(PathBuf::from(p));
            }
        }
    }
    if let Some(p) = std::env::var_os("HOME") {
        if !p.is_empty() {
            return Some(PathBuf::from(p));
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::args::Args;
    use std::fs;
    use tempfile::tempdir;

    fn parse(argv: &[&str]) -> Args {
        Args::parse_from_raw(argv.iter().map(|s| s.to_string()).collect())
    }

    #[test]
    fn selector_defaults_to_session_only() {
        let selector = ScopeSelector::default();
        assert_eq!(selector.selected(), LaunchSetupScope::SessionOnly);

        let mut out = Vec::new();
        selector.render(&mut out).unwrap();
        assert_eq!(
            String::from_utf8(out).unwrap(),
            "Launch setup scope (Up/Down, Enter):\n[x] Session only\n[ ] Globally\n"
        );
    }

    #[test]
    fn selector_navigation_and_enter() {
        let mut selector = ScopeSelector::default();
        assert_eq!(selector.handle(SelectorEvent::Down), None);
        assert_eq!(selector.selected(), LaunchSetupScope::Global);
        assert_eq!(selector.handle(SelectorEvent::Up), None);
        assert_eq!(selector.selected(), LaunchSetupScope::SessionOnly);
        assert_eq!(
            selector.handle(SelectorEvent::Enter),
            Some(LaunchSetupScope::SessionOnly)
        );
    }

    #[test]
    fn pending_enter_is_drained_before_prompt_accepts_input() {
        use crossterm::event::{KeyEvent, KeyModifiers};
        use std::cell::RefCell;
        use std::collections::VecDeque;

        let events = RefCell::new(VecDeque::from([Event::Key(KeyEvent::new(
            KeyCode::Enter,
            KeyModifiers::NONE,
        ))]));

        let drained = drain_pending_events(
            || Ok(!events.borrow().is_empty()),
            || Ok(events.borrow_mut().pop_front().unwrap()),
        )
        .unwrap();

        assert_eq!(drained, 1);
        assert!(events.borrow().is_empty());
    }

    #[test]
    fn prompt_scope_only_for_interactive_bare_launches() {
        assert!(should_prompt_for_scope(&parse(&["clud", "--codex"]), true));
        assert!(should_prompt_for_scope(&parse(&["clud", "--claude"]), true));
        assert!(!should_prompt_for_scope(&parse(&["clud"]), true));
        assert!(!should_prompt_for_scope(
            &parse(&["clud", "--codex"]),
            false
        ));
        assert!(!should_prompt_for_scope(
            &parse(&["clud", "--codex", "--dry-run"]),
            true
        ));
        assert!(!should_prompt_for_scope(
            &parse(&["clud", "--codex", "-p", "hello"]),
            true
        ));
        assert!(!should_prompt_for_scope(
            &parse(&["clud", "--codex", "loop"]),
            true
        ));
    }

    #[test]
    fn non_prompting_launches_default_to_session_only() {
        let args = parse(&["clud", "--codex", "--dry-run"]);
        assert_eq!(
            scope_for_non_prompting_launch(&args, true),
            Some(LaunchSetupScope::SessionOnly)
        );
    }

    #[test]
    fn configured_global_scope_skips_prompt_for_bare_launches() {
        let args = parse(&["clud", "--codex"]);
        assert_eq!(
            scope_for_configured_launch(&args, true, Some(LaunchSetupScope::Global)),
            Some(LaunchSetupScope::Global)
        );
    }

    #[test]
    fn configured_global_scope_applies_to_one_shot_launches() {
        let args = parse(&["clud", "--codex", "-p", "hello"]);
        assert_eq!(
            scope_for_configured_launch(&args, true, Some(LaunchSetupScope::Global)),
            Some(LaunchSetupScope::Global)
        );
    }

    #[test]
    fn dry_run_ignores_configured_global_scope() {
        let args = parse(&["clud", "--codex", "--dry-run"]);
        assert_eq!(
            scope_for_configured_launch(&args, true, Some(LaunchSetupScope::Global)),
            Some(LaunchSetupScope::SessionOnly)
        );
    }

    #[test]
    fn session_only_setup_does_not_write_agent_home_files() {
        let home = tempdir().unwrap();
        fs::create_dir_all(home.path().join(".claude")).unwrap();
        fs::create_dir_all(home.path().join(".codex")).unwrap();

        let mut out = Vec::new();
        let report = run_setup_at(
            home.path(),
            LaunchSetupScope::SessionOnly,
            Backend::Codex,
            false,
            &mut out,
        )
        .unwrap();

        assert!(report.ran.is_empty());
        assert!(!home.path().join(".agents").exists());
        assert!(!home.path().join(".claude/skills").exists());
        assert!(!home.path().join(".clud").exists());
    }

    #[test]
    fn codex_global_setup_is_selected_backend_only() {
        let home = tempdir().unwrap();
        fs::create_dir_all(home.path().join(".claude")).unwrap();
        fs::create_dir_all(home.path().join(".codex")).unwrap();
        fs::write(
            home.path().join(".codex/hooks.json"),
            r#"{"hooks":{"PreToolUse":[{"matcher":"Bash","hooks":[{"timeout":5}]}]}}"#,
        )
        .unwrap();

        let mut out = Vec::new();
        let report = run_setup_at(
            home.path(),
            LaunchSetupScope::Global,
            Backend::Codex,
            false,
            &mut out,
        )
        .unwrap();

        assert_eq!(report.ran, vec!["bundled-skills", "codex-hook-normalize"]);
        assert!(home.path().join(".codex/skills/clud-pr/SKILL.md").exists());
        assert!(!home.path().join(".agents").exists());
        assert!(!home.path().join(".claude/skills").exists());
        let hooks = fs::read_to_string(home.path().join(".codex/hooks.json")).unwrap();
        assert!(hooks.contains(r#""timeout": 30"#), "{hooks}");
        assert!(home.path().join(".clud/settings.json").exists());
    }

    #[test]
    fn claude_global_setup_is_selected_backend_only() {
        let home = tempdir().unwrap();
        fs::create_dir_all(home.path().join(".claude")).unwrap();
        fs::create_dir_all(home.path().join(".codex")).unwrap();
        fs::write(
            home.path().join(".codex/hooks.json"),
            r#"{"hooks":{"PreToolUse":[{"matcher":"Bash","hooks":[{"timeout":5}]}]}}"#,
        )
        .unwrap();

        let mut out = Vec::new();
        let report = run_setup_at(
            home.path(),
            LaunchSetupScope::Global,
            Backend::Claude,
            false,
            &mut out,
        )
        .unwrap();

        assert_eq!(report.ran, vec!["bundled-skills", "claude-drift-skills"]);
        assert!(home.path().join(".claude/skills/clud-pr/SKILL.md").exists());
        assert!(!home.path().join(".agents").exists());
        // Launch setup no longer installs bundled tools — the daemon owns
        // that. `.clud/` is created by the bundled-skills action for
        // settings.json under codex setup, but the claude path does not
        // touch it, so it stays absent here.
        assert!(!home.path().join(".clud").exists());
        let hooks = fs::read_to_string(home.path().join(".codex/hooks.json")).unwrap();
        assert!(hooks.contains(r#""timeout":5"#), "{hooks}");
    }
}
