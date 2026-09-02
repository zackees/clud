//! Report, once per launch, when the harness does not have the launch
//! directory trusted (issue #1102).
//!
//! Claude Code gates a project's `.claude/settings*.json` behind a per-project
//! trust decision recorded as `projects["<abs cwd>"].hasTrustDialogAccepted`
//! in `~/.claude.json`. Until that flag is set, it loads none of that file and
//! says so on every start:
//!
//! ```text
//! Ignoring 10 permissions.allow entries from .claude/settings.local.json:
//! this workspace has not been trusted.
//! ```
//!
//! Under `clud grind` / `clud loop` that banner scrolls past at the top of
//! every one of up to 200 unattended iterations, and it is easy to read as
//! cosmetic — clud already injects `--dangerously-skip-permissions`
//! ([DD-002]), so the dropped `permissions.allow` entries change nothing for
//! tool gating. What does change is everything else the file configures: the
//! repo gets a materially different run under grind than the one its author
//! gets interactively, with only the harness's own scrolled-away banner to
//! say so.
//!
//! So clud says it once, up front, and only when it can actually matter:
//! the run has more than one iteration, the backend is Claude, the workspace
//! is not trusted, **and** the repo really has a `.claude/settings*.json` to
//! ignore.
//!
//! The iteration gate is the point of the whole thing. On a single interactive
//! launch the harness's own banner is right there on screen and perfectly
//! readable; adding a second one would just be a double banner in the one case
//! that never needed help. It is the unattended multi-iteration run — where
//! the banner repeats past the scrollback and nobody is watching — that has no
//! other way to surface this.
//!
//! Trust is a security boundary, so the notice never writes the flag and never
//! coaches anyone into writing it by hand. Note the deliberate asymmetry with
//! `hook_health::codex_trust`, which *does* write Codex project trust: that is
//! a reported, `--no-fix-hooks`-able repair taken while installing clud's own
//! hooks, not a banner being silenced. See [DD-066] for the full argument and
//! for the shape a symmetric repair would take if one is ever wanted.
//!
//! [DD-002]: ../../../docs/DESIGN_DECISIONS.md
//! [DD-066]: ../../../docs/DESIGN_DECISIONS.md

use std::path::{Path, PathBuf};

use crate::backend::Backend;

/// Claude Code's per-user state file, which owns the trust decisions.
const CLAUDE_JSON: &str = ".claude.json";
/// The per-project key that records an accepted trust dialog.
const TRUST_KEY: &str = "hasTrustDialogAccepted";
/// Project settings files whose contents the trust decision gates.
const PROJECT_SETTINGS: [&str; 2] = ["settings.local.json", "settings.json"];

/// What `~/.claude.json` says about one workspace.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WorkspaceTrust {
    /// The project entry exists and has accepted the trust dialog.
    Trusted,
    /// The file parsed, but this project is absent or not accepted.
    Untrusted,
    /// No readable/parseable state file — say nothing rather than guess.
    /// Covers a fresh machine, a relocated config, and a corrupt file
    /// alike; none of them are grounds for warning a user about trust.
    Unknown,
}

/// Locate Claude Code's state file.
///
/// Two layouts are probed, first existing wins: `CLAUDE_CONFIG_DIR` relocates
/// the harness's config root and newer versions keep `.claude.json` inside it,
/// while older ones keep it beside `~/.claude`. Guessing one and missing would
/// read as "untrusted" and warn a user whose workspace is fine.
///
/// Home resolution goes through `hook_health::hook_home_dir`, i.e.
/// `CLUD_HOOK_HOME` before `dirs::home_dir()`. That override matters on
/// Windows, where `dirs::home_dir()` asks `SHGetKnownFolderPath` and ignores a
/// `USERPROFILE` set by a test harness or a sandbox — without it a process
/// pointed at a temp home reads the developer's real state file instead.
///
/// When neither candidate exists the first is still returned, so the read
/// fails and `read_workspace_trust` answers `Unknown` rather than guessing.
pub fn claude_json_path() -> Option<PathBuf> {
    let mut candidates = Vec::with_capacity(2);
    if let Some(dir) = std::env::var_os("CLAUDE_CONFIG_DIR") {
        candidates.push(PathBuf::from(dir).join(CLAUDE_JSON));
    }
    if let Some(home) = crate::hook_health::hook_home_dir() {
        candidates.push(home.join(CLAUDE_JSON));
    }
    candidates
        .iter()
        .find(|path| path.is_file())
        .cloned()
        .or_else(|| candidates.into_iter().next())
}

/// Read the trust decision for `cwd` out of the state file at `claude_json`.
///
/// Project keys are absolute path strings as the harness saw them, so a
/// symlinked or non-canonical `cwd` would miss on a plain string compare.
/// Both sides are canonicalized before matching, with the raw string kept as
/// a fallback for the case where `cwd` no longer resolves.
pub fn read_workspace_trust(claude_json: &Path, cwd: &Path) -> WorkspaceTrust {
    let Ok(body) = std::fs::read_to_string(claude_json) else {
        return WorkspaceTrust::Unknown;
    };
    let Ok(root) = serde_json::from_str::<serde_json::Value>(&body) else {
        return WorkspaceTrust::Unknown;
    };
    // A state file with no `projects` map is a harness that has never opened
    // a project. That is a real "not trusted", not a parse failure.
    let Some(projects) = root.get("projects").and_then(|value| value.as_object()) else {
        return WorkspaceTrust::Untrusted;
    };

    let target = std::fs::canonicalize(cwd).unwrap_or_else(|_| cwd.to_path_buf());
    let accepted = projects.iter().any(|(key, entry)| {
        let key_path = Path::new(key);
        let same = std::fs::canonicalize(key_path).unwrap_or_else(|_| key_path.to_path_buf())
            == target
            || key_path == cwd;
        same && entry.get(TRUST_KEY).and_then(|v| v.as_bool()) == Some(true)
    });

    if accepted {
        WorkspaceTrust::Trusted
    } else {
        WorkspaceTrust::Untrusted
    }
}

/// The project settings file that the untrusted state would suppress, if the
/// repo has one at all. `settings.local.json` wins when both exist — it is
/// the one the harness names in its own banner.
pub fn project_settings_file(cwd: &Path) -> Option<PathBuf> {
    PROJECT_SETTINGS
        .iter()
        .map(|name| cwd.join(".claude").join(name))
        .find(|path| path.is_file())
}

/// The notice itself. Separate from the printing so a test can assert on the
/// text without capturing stderr.
pub fn untrusted_workspace_notice(settings: &Path, iterations: u32) -> String {
    format!(
        "[clud] note: claude does not have this workspace trusted, so it will ignore\n\
         [clud]       {} for all {} iterations of this run.\n\
         [clud]       Run `claude` here once and accept the trust prompt to fix it.",
        settings.display(),
        iterations
    )
}

/// Print the notice at most once, immediately before a launch runs.
///
/// Silent unless every condition holds: the run is multi-iteration (a single
/// interactive launch shows the harness's own banner on screen, where it is
/// perfectly readable), the backend is Claude (no other harness reads
/// `~/.claude.json`), the state file is readable and says the workspace is
/// untrusted, and the repo actually ships a `.claude/settings*.json` for that
/// decision to suppress.
pub fn warn_if_workspace_untrusted(backend: Backend, cwd: Option<&str>, iterations: u32) {
    if iterations <= 1 || backend != Backend::Claude {
        return;
    }
    let cwd = match cwd {
        Some(path) => PathBuf::from(path),
        None => match std::env::current_dir() {
            Ok(dir) => dir,
            Err(_) => return,
        },
    };
    let Some(settings) = project_settings_file(&cwd) else {
        return;
    };
    let Some(claude_json) = claude_json_path() else {
        return;
    };
    if read_workspace_trust(&claude_json, &cwd) != WorkspaceTrust::Untrusted {
        return;
    }
    eprintln!("{}", untrusted_workspace_notice(&settings, iterations));
}

#[cfg(test)]
#[path = "workspace_trust_tests.rs"]
mod tests;
