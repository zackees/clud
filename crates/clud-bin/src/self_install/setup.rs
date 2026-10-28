//! Interactive post-install provider setup and launch choice.

use std::io::{self, Write};
use std::time::Duration;

use crate::args::{Args, DeepseekAuthSubcommand};
use crate::backend::{HarnessSelection, ModelProvider};
use crate::selector::{self, Key, Note, Row, Selector, Step, View};

const DEEPSEEK: &str = "deepseek/deepseek-v4.1-flash";
const GLM: &str = "z-ai/glm-5.3-flash";

struct Choice {
    title: &'static str,
    labels: &'static [&'static str],
    selected: usize,
}

impl Selector for Choice {
    type Outcome = Option<usize>;

    fn view(&self, _: Duration) -> View {
        View {
            title: self.title.into(),
            hints: vec!["Up/Down choose, Enter confirm, Esc cancel".into()],
            gap: true,
            rows: self
                .labels
                .iter()
                .enumerate()
                .map(|(index, label)| Row {
                    current: self.selected == index,
                    marker: if self.selected == index { "[*]" } else { "[ ]" }.into(),
                    label: (*label).into(),
                    note: Note::None,
                })
                .collect(),
            footer: Vec::new(),
        }
    }

    fn on_key(&mut self, key: Key) -> Step<Self::Outcome> {
        match key {
            Key::Up => {
                self.selected = self.selected.saturating_sub(1);
                Step::Redraw
            }
            Key::Down => {
                self.selected = (self.selected + 1).min(self.labels.len() - 1);
                Step::Redraw
            }
            Key::Enter => Step::Done(Some(self.selected)),
            Key::Escape => Step::Done(None),
            Key::Space | Key::Char(_) => Step::Stay,
        }
    }
}

fn choose(title: &'static str, labels: &'static [&'static str]) -> Result<usize, i32> {
    selector::run(
        &mut io::stderr(),
        &mut Choice {
            title,
            labels,
            selected: 0,
        },
    )
    .map_err(|error| {
        if error.kind() == io::ErrorKind::Interrupted {
            130
        } else {
            eprintln!("clud setup: {error}");
            1
        }
    })?
    .ok_or(130)
}

fn other_model() -> Result<String, i32> {
    eprint!("OpenRouter model ID: ");
    io::stderr().flush().map_err(|_| 1)?;
    let mut model = String::new();
    if io::stdin().read_line(&mut model).map_err(|_| 1)? == 0 {
        return Err(130);
    }
    let model = model.trim();
    if model.is_empty() || model.chars().any(char::is_control) {
        eprintln!("clud setup: model ID cannot be empty or contain control characters");
        return Err(1);
    }
    Ok(model.to_owned())
}

fn setup_openrouter() -> Result<String, i32> {
    let descriptor = crate::provider_registry::descriptor_for(ModelProvider::OpenRouter)
        .expect("OpenRouter descriptor");
    let code = crate::provider_auth::run_for(descriptor, &DeepseekAuthSubcommand::Login);
    if code != 0 {
        return Err(code);
    }
    let model = match choose("Select a model:", &[DEEPSEEK, GLM, "Other:"])? {
        0 => DEEPSEEK.to_owned(),
        1 => GLM.to_owned(),
        _ => other_model()?,
    };
    crate::clud_settings::save_openrouter_model(model.clone()).map_err(|error| {
        eprintln!("clud setup: could not save OpenRouter model: {error}");
        1
    })?;
    Ok(model)
}

fn launch_args(provider: ModelProvider, model: Option<String>, start: bool) -> Option<Args> {
    if !start {
        return None;
    }
    let mut argv = vec!["clud".to_string()];
    match provider {
        ModelProvider::OpenRouter => {
            argv.extend([
                "--openrouter".into(),
                "--model".into(),
                model.expect("model"),
            ]);
        }
        ModelProvider::Claude => argv.push("--claude".into()),
        ModelProvider::Codex => argv.push("--codex".into()),
        _ => unreachable!("setup offers only OpenRouter, Claude, and Codex"),
    }
    argv.extend(["--harness".into(), "default".into()]);
    Some(Args::parse_from_raw(argv))
}

/// Return launch arguments only when setup and the start confirmation succeed.
pub(super) fn run() -> Result<Option<Args>, i32> {
    let choice = choose("Setup for:", &["OpenRouter", "Claude", "Codex"])?;
    let (provider, model) = match choice {
        0 => (ModelProvider::OpenRouter, Some(setup_openrouter()?)),
        1 => (ModelProvider::Claude, None),
        _ => (ModelProvider::Codex, None),
    };
    crate::clud_settings::save_settings_patch(crate::clud_settings::GlobalSettingsPatch {
        model_provider: Some(provider),
        harness: Some(HarnessSelection::Default),
        provider_profiles: vec![crate::clud_settings::ProviderProfilePatch {
            provider: Some(provider),
            harness: Some(HarnessSelection::Default),
            ..crate::clud_settings::ProviderProfilePatch::default()
        }],
        ..crate::clud_settings::GlobalSettingsPatch::default()
    })
    .map_err(|error| {
        eprintln!("clud setup: could not save provider: {error}");
        1
    })?;
    let start = choose("Start right now?", &["Yes", "No (exits)"])? == 0;
    Ok(launch_args(provider, model, start))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn choices_default_to_first_and_support_cancel() {
        for (title, labels) in [
            ("Setup for:", &["OpenRouter", "Claude", "Codex"][..]),
            ("Select a model:", &[DEEPSEEK, GLM, "Other:"][..]),
            ("Start right now?", &["Yes", "No (exits)"][..]),
        ] {
            let mut choice = Choice {
                title,
                labels,
                selected: 0,
            };
            assert_eq!(choice.on_key(Key::Enter), Step::Done(Some(0)));
            assert_eq!(choice.on_key(Key::Down), Step::Redraw);
            assert_eq!(choice.on_key(Key::Enter), Step::Done(Some(1)));
            assert_eq!(choice.on_key(Key::Escape), Step::Done(None));
            assert_eq!(choice.view(Duration::ZERO).rows[1].marker, "[*]");
        }
    }

    #[test]
    fn start_choice_hands_the_selected_provider_and_model_to_normal_launch() {
        for model in [DEEPSEEK, GLM, "example/custom"] {
            let args = launch_args(ModelProvider::OpenRouter, Some(model.into()), true).unwrap();
            assert!(args.openrouter);
            assert_eq!(args.model.as_deref(), Some(model));
            assert!(launch_args(ModelProvider::OpenRouter, Some(model.into()), false).is_none());
        }
        assert!(
            launch_args(ModelProvider::Claude, None, true)
                .unwrap()
                .claude
        );
        assert!(launch_args(ModelProvider::Codex, None, true).unwrap().codex);
        assert!(launch_args(ModelProvider::Claude, None, false).is_none());
        assert!(launch_args(ModelProvider::Codex, None, false).is_none());
    }
}
