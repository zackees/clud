//! `clud settings` — a small, cross-platform TUI checkbox menu over the
//! typed settings in `~/.clud/settings.json`.
//!
//! Split the same way `launch_setup.rs`'s `ScopeSelector` is: a pure,
//! unit-tested state machine (`Menu`) plus a thin impure terminal-I/O shell
//! (`run_interactive`/`run_interactive_inner`) built on the same crossterm
//! primitives (raw-mode RAII guard, raw-ANSI cursor hide/show, redraw via
//! cursor-up + clear-to-end) already proven cross-platform in this repo.
//! Provider and harness choice rows share `preference::ChoiceSelector` with
//! the inline launch-scope selector; all rows save in one atomic patch.

use std::io::{self, IsTerminal, Write};
use std::time::Duration;

use crossterm::event::{self, Event, KeyCode, KeyModifiers};
use crossterm::terminal;

use crate::backend::{HarnessSelection, ModelProvider};
use crate::clud_settings;
use crate::preference::{ChoiceOption, ChoiceSelector};
use crate::provider_catalog::{self, EffortLevel};

const MODEL_OPTIONS: [ChoiceOption<ModelProvider>; 3] = [
    ChoiceOption {
        value: ModelProvider::Claude,
        label: "claude",
        note: "",
    },
    ChoiceOption {
        value: ModelProvider::Codex,
        label: "codex",
        note: "",
    },
    ChoiceOption {
        value: ModelProvider::DeepSeek,
        label: "deepseek",
        note: "",
    },
];

const HARNESS_OPTIONS: [ChoiceOption<HarnessSelection>; 3] = [
    ChoiceOption {
        value: HarnessSelection::Default,
        label: "default",
        note: "",
    },
    ChoiceOption {
        value: HarnessSelection::Claude,
        label: "claude",
        note: "",
    },
    ChoiceOption {
        value: HarnessSelection::Codex,
        label: "codex",
        note: "",
    },
];

#[derive(Debug, Clone, PartialEq, Eq)]
enum SettingValue {
    Bool(bool),
    ModelProvider(ModelProvider),
    Harness(HarnessSelection),
    Model {
        provider: ModelProvider,
        value: &'static str,
    },
    Effort {
        provider: ModelProvider,
        value: Option<EffortLevel>,
    },
    ContextWindow {
        provider: ModelProvider,
        value: Option<&'static str>,
    },
}

impl SettingValue {
    fn cycle(&mut self) {
        match self {
            Self::Bool(value) => *value = !*value,
            Self::ModelProvider(value) => {
                let mut selector = ChoiceSelector::new(&MODEL_OPTIONS, *value, *value);
                selector.cycle();
                *value = selector.selected();
            }
            Self::Harness(value) => {
                let mut selector = ChoiceSelector::new(&HARNESS_OPTIONS, *value, *value);
                selector.cycle();
                *value = selector.selected();
            }
            Self::Model { provider, value } => {
                let options = provider_catalog::models_for_provider(*provider)
                    .map(|model| model.cli_id)
                    .collect::<Vec<_>>();
                let index = options
                    .iter()
                    .position(|candidate| candidate == value)
                    .unwrap_or(0);
                *value = options[(index + 1) % options.len()];
            }
            Self::Effort { provider, value } => {
                let options = provider_catalog::supported_efforts(*provider);
                *value = match value {
                    None => options.first().copied(),
                    Some(current) => options
                        .iter()
                        .position(|candidate| candidate == current)
                        .and_then(|index| options.get(index + 1).copied()),
                };
            }
            Self::ContextWindow { provider, value } => {
                let options = provider_catalog::supported_context_windows(*provider);
                *value = match value {
                    None => options.first().copied(),
                    Some(current) => options
                        .iter()
                        .position(|candidate| candidate == current)
                        .and_then(|index| options.get(index + 1).copied()),
                };
            }
        }
    }

    fn marker(&self) -> String {
        match self {
            Self::Bool(true) => "[x]".to_string(),
            Self::Bool(false) => "[ ]".to_string(),
            Self::ModelProvider(value) => format!("[{}]", value.as_str()),
            Self::Harness(value) => format!("[{}]", value.as_str()),
            Self::Model { value, .. } => {
                let display_name = provider_catalog::model_by_cli_id(value)
                    .map_or(*value, |model| model.display_name);
                format!("[{display_name}]")
            }
            Self::Effort { value, .. } => {
                format!("[{}]", value.map(EffortLevel::as_str).unwrap_or("default"))
            }
            Self::ContextWindow { value, .. } => {
                format!("[{}]", value.unwrap_or("default"))
            }
        }
    }

    fn list_value(&self) -> String {
        match self {
            Self::Bool(true) => "true".to_string(),
            Self::Bool(false) => "false".to_string(),
            Self::ModelProvider(value) => value.as_str().to_string(),
            Self::Harness(value) => value.as_str().to_string(),
            Self::Model { value, .. } => (*value).to_string(),
            Self::Effort { value, .. } => value
                .map(EffortLevel::as_str)
                .unwrap_or("default")
                .to_string(),
            Self::ContextWindow { value, .. } => value.unwrap_or("default").to_string(),
        }
    }
}

#[derive(Clone)]
struct SettingItem {
    key: &'static str,
    label: &'static str,
    note: &'static str,
    value: SettingValue,
}

fn setting_items() -> Vec<SettingItem> {
    let launch = clud_settings::load_global_launch_preferences().unwrap_or_default();
    let snapshot = clud_settings::load_launch_preferences_read_only().unwrap_or_default();
    let mut items = vec![
        SettingItem {
            key: "backend.default",
            label: "Default model provider",
            note: "Used when neither --claude, --codex, nor --deepseek is supplied.",
            value: SettingValue::ModelProvider(
                launch.model_provider.unwrap_or(ModelProvider::Claude),
            ),
        },
        SettingItem {
            key: "harness.default",
            label: "Agent harness",
            note: "default follows the model provider; explicit overrides are announced.",
            value: SettingValue::Harness(launch.harness.unwrap_or_default()),
        },
    ];
    for provider in [
        ModelProvider::Claude,
        ModelProvider::Codex,
        ModelProvider::DeepSeek,
    ] {
        append_provider_profile_items(&mut items, &snapshot, provider);
    }
    items.push(SettingItem {
        key: "git.pr_wait_fail_fast",
        label: "PR-wait fail-fast git commands",
        note: "Blocks raw `gh pr checks --watch`-style commands in favor of \
               a bundled fail-fast waiter script. Off by default; may \
               become the default later.",
        value: SettingValue::Bool(clud_settings::load_pr_wait_fail_fast_enabled().unwrap_or(false)),
    });
    items
}

fn append_provider_profile_items(
    items: &mut Vec<SettingItem>,
    snapshot: &clud_settings::LaunchPreferencesSnapshot,
    provider: ModelProvider,
) {
    let profile = snapshot.profile(provider);
    let fallback_model = provider_catalog::reviewed_default_model(provider)
        .or_else(|| provider_catalog::models_for_provider(provider).next())
        .expect("every provider has catalog models");
    let selected_model = profile
        .and_then(|profile| profile.model.as_deref())
        .and_then(provider_catalog::model_by_cli_id)
        .unwrap_or(fallback_model);
    let context_window = profile
        .and_then(|profile| profile.context_window.as_deref())
        .and_then(|value| match value {
            "auto" => Some("auto"),
            "1m" => Some("1m"),
            _ => None,
        });
    let (
        model_key,
        harness_key,
        effort_key,
        context_key,
        model_label,
        harness_label,
        effort_label,
        context_label,
    ) = match provider {
        ModelProvider::Claude => (
            "providers.claude.model",
            "providers.claude.harness",
            "providers.claude.effort",
            "providers.claude.context_window",
            "Claude model",
            "Claude harness",
            "Claude effort",
            "Claude context window",
        ),
        ModelProvider::Codex => (
            "providers.codex.model",
            "providers.codex.harness",
            "providers.codex.effort",
            "providers.codex.context_window",
            "Codex model",
            "Codex harness",
            "Codex effort",
            "Codex context window",
        ),
        ModelProvider::DeepSeek => (
            "providers.deepseek.model",
            "providers.deepseek.harness",
            "providers.deepseek.effort",
            "providers.deepseek.context_window",
            "DeepSeek model",
            "DeepSeek harness",
            "DeepSeek effort",
            "DeepSeek context window",
        ),
    };
    items.extend([
        SettingItem {
            key: model_key,
            label: model_label,
            note: "Canonical catalog model for direct launches.",
            value: SettingValue::Model {
                provider,
                value: selected_model.cli_id,
            },
        },
        SettingItem {
            key: harness_key,
            label: harness_label,
            note: "Harness used when this provider is selected explicitly.",
            value: SettingValue::Harness(
                profile
                    .and_then(|profile| profile.harness)
                    .unwrap_or_default(),
            ),
        },
        SettingItem {
            key: effort_key,
            label: effort_label,
            note: "Provider-scoped launch effort; default uses catalog policy.",
            value: SettingValue::Effort {
                provider,
                value: profile.and_then(|profile| profile.effort),
            },
        },
        SettingItem {
            key: context_key,
            label: context_label,
            note: "Provider-scoped context; default uses catalog policy.",
            value: SettingValue::ContextWindow {
                provider,
                value: context_window,
            },
        },
    ]);
}

pub fn run(list_only: bool) -> i32 {
    let items = setting_items();

    if list_only {
        for item in &items {
            println!(
                "{} = {}  # {}",
                item.key,
                item.value.list_value(),
                item.note
            );
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
    original: Vec<SettingValue>,
    cursor: usize,
}

impl Menu {
    fn new(items: Vec<SettingItem>) -> Self {
        let original = items.iter().map(|item| item.value.clone()).collect();
        Self {
            items,
            original,
            cursor: 0,
        }
    }

    fn is_dirty(&self) -> bool {
        self.items
            .iter()
            .map(|item| &item.value)
            .ne(self.original.iter())
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
                    item.value.cycle();
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
                item.value.marker(),
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
                    let patch = patch_from_menu(&menu);
                    clud_settings::save_settings_patch(patch)
                        .map_err(|error| io::Error::other(format!("saving settings: {error}")))?;
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

fn patch_from_menu(menu: &Menu) -> clud_settings::GlobalSettingsPatch {
    let mut patch = clud_settings::GlobalSettingsPatch::default();
    for (item, original) in menu.items.iter().zip(&menu.original) {
        if &item.value == original {
            continue;
        }
        match (item.key, &item.value) {
            ("backend.default", SettingValue::ModelProvider(value)) => {
                patch.model_provider = Some(*value);
            }
            ("harness.default", SettingValue::Harness(value)) => {
                patch.harness = Some(*value);
            }
            (key, SettingValue::Model { provider, value }) if key.starts_with("providers.") => {
                provider_patch(&mut patch, *provider).model = Some((*value).to_string());
            }
            (key, SettingValue::Harness(value)) if key.starts_with("providers.") => {
                if let Some(provider) = provider_from_profile_key(key) {
                    provider_patch(&mut patch, provider).harness = Some(*value);
                }
            }
            (key, SettingValue::Effort { provider, value }) if key.starts_with("providers.") => {
                provider_patch(&mut patch, *provider).effort = Some(*value);
            }
            (key, SettingValue::ContextWindow { provider, value })
                if key.starts_with("providers.") =>
            {
                provider_patch(&mut patch, *provider).context_window =
                    Some(value.map(str::to_string));
            }
            ("git.pr_wait_fail_fast", SettingValue::Bool(value)) => {
                patch.pr_wait_fail_fast = Some(*value);
            }
            _ => {}
        }
    }
    patch
}

fn provider_patch(
    patch: &mut clud_settings::GlobalSettingsPatch,
    provider: ModelProvider,
) -> &mut clud_settings::ProviderProfilePatch {
    if let Some(index) = patch
        .provider_profiles
        .iter()
        .position(|profile| profile.provider == Some(provider))
    {
        return &mut patch.provider_profiles[index];
    }
    patch
        .provider_profiles
        .push(clud_settings::ProviderProfilePatch {
            provider: Some(provider),
            ..clud_settings::ProviderProfilePatch::default()
        });
    patch.provider_profiles.last_mut().unwrap()
}

fn provider_from_profile_key(key: &str) -> Option<ModelProvider> {
    let provider = key.strip_prefix("providers.")?.split('.').next()?;
    ModelProvider::from_settings_str(provider)
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
            value: SettingValue::Bool(value),
        }
    }

    #[test]
    fn toggle_flips_value_and_marks_dirty() {
        let mut menu = Menu::new(vec![item(false)]);
        assert!(!menu.is_dirty());
        assert_eq!(menu.handle(MenuEvent::Toggle), MenuAction::Redraw);
        assert_eq!(menu.items[0].value, SettingValue::Bool(true));
        assert!(menu.is_dirty());
    }

    #[test]
    fn toggle_twice_returns_to_clean() {
        let mut menu = Menu::new(vec![item(false)]);
        menu.handle(MenuEvent::Toggle);
        menu.handle(MenuEvent::Toggle);
        assert_eq!(menu.items[0].value, SettingValue::Bool(false));
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
        assert_eq!(menu.items[0].value, SettingValue::Bool(false));
        assert_eq!(menu.items[1].value, SettingValue::Bool(true));
    }

    #[test]
    fn typed_model_and_harness_choices_share_cycle_behavior() {
        let mut model = SettingValue::ModelProvider(ModelProvider::Claude);
        model.cycle();
        assert_eq!(model, SettingValue::ModelProvider(ModelProvider::Codex));
        model.cycle();
        assert_eq!(model, SettingValue::ModelProvider(ModelProvider::DeepSeek));
        model.cycle();
        assert_eq!(model, SettingValue::ModelProvider(ModelProvider::Claude));

        let mut harness = SettingValue::Harness(HarnessSelection::Default);
        harness.cycle();
        assert_eq!(harness, SettingValue::Harness(HarnessSelection::Claude));
        harness.cycle();
        assert_eq!(harness, SettingValue::Harness(HarnessSelection::Codex));
        harness.cycle();
        assert_eq!(harness, SettingValue::Harness(HarnessSelection::Default));
    }

    #[test]
    fn model_marker_shows_checkpoint_while_list_value_stays_stable() {
        let model = SettingValue::Model {
            provider: ModelProvider::DeepSeek,
            value: "deepseek-v4-pro",
        };
        assert_eq!(model.marker(), "[DeepSeek V4 Pro 0813]");
        assert_eq!(model.list_value(), "deepseek-v4-pro");
    }

    #[test]
    fn typed_items_build_one_atomic_settings_patch() {
        let mut menu = Menu::new(vec![
            SettingItem {
                key: "backend.default",
                label: "",
                note: "",
                value: SettingValue::ModelProvider(ModelProvider::Claude),
            },
            SettingItem {
                key: "harness.default",
                label: "",
                note: "",
                value: SettingValue::Harness(HarnessSelection::Default),
            },
            SettingItem {
                key: "git.pr_wait_fail_fast",
                label: "",
                note: "",
                value: SettingValue::Bool(false),
            },
        ]);
        menu.items[0].value = SettingValue::ModelProvider(ModelProvider::Codex);
        menu.items[1].value = SettingValue::Harness(HarnessSelection::Claude);
        menu.items[2].value = SettingValue::Bool(true);
        assert_eq!(
            patch_from_menu(&menu),
            clud_settings::GlobalSettingsPatch {
                model_provider: Some(ModelProvider::Codex),
                harness: Some(HarnessSelection::Claude),
                pr_wait_fail_fast: Some(true),
                provider_profiles: Vec::new(),
            }
        );
    }

    #[test]
    fn unrelated_edit_does_not_materialize_launch_preferences() {
        let mut menu = Menu::new(vec![
            SettingItem {
                key: "backend.default",
                label: "",
                note: "",
                value: SettingValue::ModelProvider(ModelProvider::Claude),
            },
            SettingItem {
                key: "harness.default",
                label: "",
                note: "",
                value: SettingValue::Harness(HarnessSelection::Default),
            },
            SettingItem {
                key: "git.pr_wait_fail_fast",
                label: "",
                note: "",
                value: SettingValue::Bool(false),
            },
        ]);
        menu.items[2].value = SettingValue::Bool(true);
        assert_eq!(
            patch_from_menu(&menu),
            clud_settings::GlobalSettingsPatch {
                model_provider: None,
                harness: None,
                pr_wait_fail_fast: Some(true),
                provider_profiles: Vec::new(),
            }
        );
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
