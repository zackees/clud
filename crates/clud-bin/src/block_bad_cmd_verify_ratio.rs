//! Verification-to-edit ratio watchdog (#1988).
//!
//! An agent can spend most of a session re-running workspace-wide lint,
//! release builds and full test suites after one-line edits, and nothing
//! tells it so. This hook classifies each PreToolUse call as an edit, a
//! verification (lint, build, test, CI replay) or neither, keeps the last
//! [`WINDOW`] classified calls per session, and when verification outnumbers
//! edits by more than the configured ratio it adds one advisory note to the
//! agent's context. It never denies. After a note it stays quiet for another
//! full window.
//!
//! **Fail open**, like the repeat guard (#1674): a missing session id or any
//! state error means no note. State holds only a short class string.

use std::path::{Path, PathBuf};
use std::time::SystemTime;

use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};

use super::block_bad_cmd_repeat::{prune, write_fast};

/// Classified calls kept per session.
pub(super) const WINDOW: usize = 30;
/// Verification calls in the window before the ratio is judged at all.
pub(super) const MIN_VERIFY: usize = 10;
/// Default ratio: more than this many verifications per edit trips the note.
pub(super) const DEFAULT_RATIO: u32 = 3;
/// Session-wide ratio override; `0` disables the watchdog.
pub(super) const RATIO_ENV: &str = "CLUD_VERIFY_EDIT_RATIO";
const DIR_NAME: &str = "verify-ratio";
const HASH_DOMAIN: &str = "clud-verify-ratio-v1\0";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Class {
    Edit,
    Verify,
}

impl Class {
    fn letter(self) -> char {
        match self {
            Self::Edit => 'e',
            Self::Verify => 'v',
        }
    }
}

const EDIT_TOOLS: &[&str] = &["Write", "Edit", "MultiEdit", "NotebookEdit", "apply_patch"];
const SHELL_EDITS: &[&str] = &[
    "sed -i",
    "perl -pi",
    "perl -i",
    "apply_patch",
    "git apply",
    "git mv",
];
const VERIFY_WORDS: &[&str] = &[
    "clippy",
    "dylint",
    "cargo fmt",
    "cargo build",
    "cargo check",
    "cargo test",
    "cargo nextest",
    "ruff",
    "eslint",
    "mypy",
    "pyright",
    "pytest",
    "npm test",
    "npm run lint",
    "./lint",
    "bash lint",
    "ci/lint.py",
    "./test",
    "bash test",
    "bosn ci run",
    "clud ci",
    "local-gate run",
];

/// The class of one call, from its tool name and shell command.
pub(super) fn classify(tool_name: &str, command: &str) -> Option<Class> {
    if EDIT_TOOLS.contains(&tool_name) {
        return Some(Class::Edit);
    }
    if command.trim().is_empty() {
        return None;
    }
    let text = command.to_ascii_lowercase();
    // An edit chained with a build counts as the edit: the agent did change
    // something, and #1988 asks for them to be split, not punished twice.
    if SHELL_EDITS.iter().any(|word| text.contains(word)) {
        return Some(Class::Edit);
    }
    VERIFY_WORDS
        .iter()
        .any(|word| text.contains(word))
        .then_some(Class::Verify)
}

/// The persisted window: one letter per classified call, newest last, and
/// how many classified calls remain before another note may be shown.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub(super) struct Ledger {
    pub window: String,
    pub quiet: u32,
}

/// Counts in a window, for the note.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct Counts {
    pub edits: usize,
    pub verifies: usize,
}

/// The decision table over injected facts: the ledger to persist and, when
/// the ratio trips, the counts to report. `ratio == 0` disables it.
pub(super) fn decide(previous: &Ledger, class: Class, ratio: u32) -> (Ledger, Option<Counts>) {
    let mut window = previous.window.clone();
    window.push(class.letter());
    if window.len() > WINDOW {
        window = window[window.len() - WINDOW..].to_string();
    }
    let counts = Counts {
        edits: window.chars().filter(|c| *c == 'e').count(),
        verifies: window.chars().filter(|c| *c == 'v').count(),
    };
    let quiet = previous.quiet.saturating_sub(1);
    let trips = ratio > 0
        && class == Class::Verify
        && quiet == 0
        && counts.verifies >= MIN_VERIFY
        && counts.verifies > counts.edits.max(1) * ratio as usize;
    if trips {
        let quiet = u32::try_from(WINDOW).unwrap_or(u32::MAX);
        return (Ledger { window, quiet }, Some(counts));
    }
    (Ledger { window, quiet }, None)
}

/// The ratio: [`RATIO_ENV`] wins, then `hooks.verify_edit_ratio` in clud's
/// global settings, then [`DEFAULT_RATIO`].
pub(super) fn ratio_from(env: Option<&str>, settings: Option<&Value>) -> u32 {
    if let Some(value) = env.and_then(|v| v.trim().parse::<u32>().ok()) {
        return value;
    }
    settings
        .and_then(|doc| doc.get("hooks"))
        .and_then(|hooks| hooks.get("verify_edit_ratio"))
        .and_then(Value::as_u64)
        .map_or(DEFAULT_RATIO, |v| u32::try_from(v).unwrap_or(u32::MAX))
}

fn effective_ratio() -> u32 {
    let env = std::env::var(RATIO_ENV).ok();
    if env
        .as_deref()
        .is_some_and(|v| v.trim().parse::<u32>().is_ok())
    {
        return ratio_from(env.as_deref(), None);
    }
    let settings = crate::clud_settings::home_dir_path()
        .ok()
        .and_then(|home| std::fs::read(crate::clud_settings::settings_path_at(&home)).ok())
        .and_then(|bytes| serde_json::from_slice::<Value>(&bytes).ok());
    ratio_from(None, settings.as_ref())
}

fn session_file(state_dir: &Path, session_id: &str) -> PathBuf {
    let digest = Sha256::digest(format!("{HASH_DOMAIN}session\0{session_id}").as_bytes());
    let hash: String = digest[..8].iter().map(|b| format!("{b:02x}")).collect();
    state_dir.join(DIR_NAME).join(format!("{hash}.json"))
}

/// Record `class` for the session in `state_dir`; the counts when a note is
/// due. Every failure returns `None`.
pub(super) fn observe_at(
    state_dir: &Path,
    session_id: &str,
    class: Class,
    ratio: u32,
) -> Option<Counts> {
    if ratio == 0 || session_id.is_empty() {
        return None;
    }
    let path = session_file(state_dir, session_id);
    let existing = std::fs::read(&path).ok();
    let previous = existing
        .as_deref()
        .and_then(|bytes| serde_json::from_slice::<Ledger>(bytes).ok())
        .unwrap_or_default();
    let (ledger, note) = decide(&previous, class, ratio);
    let bytes = serde_json::to_vec(&ledger).ok()?;
    write_fast(&path, &bytes, existing.is_none()).ok()?;
    if existing.is_none() {
        if let Some(dir) = path.parent() {
            prune(dir, SystemTime::now());
        }
    }
    note
}

/// Hook entry: the advisory note for this call, or `None`.
pub(super) fn note(tool_name: &str, command: &str, session_id: Option<&str>) -> Option<String> {
    let session_id = session_id?;
    let class = classify(tool_name, command)?;
    let ratio = effective_ratio();
    if ratio == 0 {
        return None;
    }
    let state_dir = crate::daemon::default_state_dir().ok()?;
    observe_at(&state_dir, session_id, class, ratio).map(|counts| note_text(counts, ratio))
}

pub(super) fn note_text(counts: Counts, ratio: u32) -> String {
    format!(
        "[clud verify-ratio] Verification is crowding out editing: {} lint/build/test/CI calls \
         against {} edits in the last {WINDOW} classified calls (limit {ratio} per edit, #1988). \
         Scope checks to what changed (`-p <crate>`, one test file), drop `--release` unless the \
         test needs optimized code, batch edits, and run the full gate once per commit or batch. \
         Keep edits and builds in separate tool calls. This is advice, not a refusal; set \
         {RATIO_ENV}=0 or `hooks.verify_edit_ratio` in clud's settings to change it.",
        counts.verifies, counts.edits
    )
}

/// PreToolUse output that adds `note` to the agent's context without making
/// a permission decision, so the harness's normal prompt still applies.
pub(super) fn context_json(note: &str) -> Value {
    serde_json::json!({
        "hookSpecificOutput": {
            "hookEventName": "PreToolUse",
            "additionalContext": note,
        }
    })
}

#[cfg(test)]
#[path = "block_bad_cmd_verify_ratio_tests.rs"]
mod tests;
