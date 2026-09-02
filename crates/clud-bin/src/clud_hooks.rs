//! `.clud/hooks.json` — clud-owned hook declarations (zackees/clud#966 §4,
//! #967 Phase 2).
//!
//! A repo opts into clud-managed hooks by declaring them here instead of in
//! `.claude/settings.json`. clud then owns execution, which is what lets every
//! hook run rooted at the repo that declared it rather than at whatever cwd
//! the session happens to have drifted to — the failure that motivated
//! zackees/clud#965.
//!
//! ```json
//! {
//!   "hooks": {
//!     "PreToolUse": [
//!       { "matcher": "Bash", "command": "uv run python ci/hooks/check_cmd.py" }
//!     ],
//!     "Stop": [
//!       { "command": "uv run python ci/hooks/check_on_stop.py", "timeout": 120 }
//!     ]
//!   }
//! }
//! ```
//!
//! ## Why a separate file rather than reading `.claude/settings.json`
//!
//! A hook left in the harness's own settings fires natively *and* would fire
//! again through clud, and only clud's copy would be correctly rooted.
//! Declaring here is the explicit, reviewable act that moves a hook from the
//! harness's control to clud's. See #966 D3 — and #977 D16 for the
//! `--setting-sources` route that could one day absorb the harness's copy
//! instead, which is deliberately not taken yet.
//!
//! ## Parse posture
//!
//! Lenient, like `repo_clud_config`: one malformed entry is skipped with a
//! warning rather than taking the rest of the file's hooks down with it. A
//! settings file that silently disarms itself over a typo is worse than one
//! that runs everything it could still understand.
//!
//! ## Tier B source: frontend settings of non-opted-in sub-repos
//!
//! Phase 4 (#966 D4) also reads a sub-repo's hooks out of the frontend
//! settings the harness itself would load — `.claude/settings.json`,
//! `.claude/settings.local.json`, `.codex/hooks.json` — via
//! [`from_frontend_settings`]. That is safe for the *same* reason the
//! parent's frontend settings are off limits (D3): the harness never loads a
//! nested repo's settings, so a hook found there cannot fire natively and
//! again through clud. Callers use it only for extern and child roots, and
//! only after [`discover`] found no `.clud/hooks.json` opt-in.

use serde_json::Value;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

/// Where a repo declares its clud-managed hooks, relative to the repo root.
pub const HOOKS_FILE_REL: &[&str] = &[".clud", "hooks.json"];

/// Default wall-clock budget for one hook, when the entry does not set
/// `timeout`. Generous because project hooks routinely shell out to a
/// package manager on their first run.
pub const DEFAULT_TIMEOUT_SECS: u64 = 60;

/// Matcher that applies to every tool.
const MATCH_ALL: &str = "*";

/// One declared hook.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HookEntry {
    /// Tool-name pattern, harness-style. `None` or `"*"` matches every tool.
    /// Only meaningful for tool-scoped events; ignored for `Stop` and friends.
    pub matcher: Option<String>,
    pub command: String,
    pub timeout_secs: u64,
}

impl HookEntry {
    /// Whether this entry applies to `tool_name`.
    ///
    /// The harness treats a matcher as a regex over the tool name, so this
    /// does too, anchored so `Edit` cannot match `MultiEdit` by accident. A
    /// pattern that will not compile falls back to exact equality — a
    /// declaration clud cannot understand should under-match, never
    /// over-match, since over-matching would fire someone's guard against
    /// tools they never meant to guard.
    #[must_use]
    pub fn matches_tool(&self, tool_name: Option<&str>) -> bool {
        let Some(matcher) = self.matcher.as_deref() else {
            return true;
        };
        let matcher = matcher.trim();
        if matcher.is_empty() || matcher == MATCH_ALL {
            return true;
        }
        let Some(tool_name) = tool_name else {
            // A tool-scoped matcher on an event that carries no tool cannot
            // be satisfied; skip rather than guess.
            return false;
        };
        match regex::Regex::new(&format!("^(?:{matcher})$")) {
            Ok(regex) => regex.is_match(tool_name),
            Err(_) => matcher == tool_name,
        }
    }
}

/// Every hook a repo declares, indexed by event name.
///
/// Event names are kept as free-form strings rather than an enum: the harness
/// adds events regularly, and a declaration for an event clud has not heard of
/// should sit inert until clud learns to fire it, not fail the file.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CludHooks {
    events: BTreeMap<String, Vec<HookEntry>>,
    /// The file this came from, for diagnostics. `None` when parsed from text.
    pub source: Option<PathBuf>,
}

impl CludHooks {
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.events.values().all(Vec::is_empty)
    }

    /// Every event name that has at least one entry, in stable order.
    pub fn events(&self) -> impl Iterator<Item = &str> {
        self.events
            .iter()
            .filter(|(_, entries)| !entries.is_empty())
            .map(|(event, _)| event.as_str())
    }

    /// The entries that should run for `event` against `tool_name`, in
    /// declaration order.
    #[must_use]
    pub fn matching(&self, event: &str, tool_name: Option<&str>) -> Vec<&HookEntry> {
        self.events
            .get(event)
            .map(|entries| {
                entries
                    .iter()
                    .filter(|entry| entry.matches_tool(tool_name))
                    .collect()
            })
            .unwrap_or_default()
    }

    /// All entries for `event`, regardless of matcher — used when compiling
    /// the set the harness should be told about.
    #[must_use]
    pub fn for_event(&self, event: &str) -> &[HookEntry] {
        self.events.get(event).map_or(&[], Vec::as_slice)
    }
}

/// Read `<repo_root>/.clud/hooks.json`, if it exists and declares anything.
///
/// A file that cannot be read or parsed is reported on stderr and treated as
/// absent: a broken declaration must not stop the tool call that triggered
/// the lookup.
#[must_use]
pub fn discover(repo_root: &Path) -> Option<CludHooks> {
    let mut path = repo_root.to_path_buf();
    for segment in HOOKS_FILE_REL {
        path.push(segment);
    }
    if !path.is_file() {
        return None;
    }
    let text = match std::fs::read_to_string(&path) {
        Ok(text) => text,
        Err(error) => {
            eprintln!("clud: failed to read {}: {error}; ignoring", path.display());
            return None;
        }
    };
    match parse(&text) {
        Ok(mut hooks) => {
            if hooks.is_empty() {
                return None;
            }
            hooks.source = Some(path);
            Some(hooks)
        }
        Err(error) => {
            eprintln!(
                "clud: failed to parse {}: {error}; ignoring",
                path.display()
            );
            None
        }
    }
}

/// Parse the declaration text. Returns `Err` only when the document itself is
/// not usable JSON; individual bad entries are skipped with a warning.
pub fn parse(text: &str) -> Result<CludHooks, String> {
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return Ok(CludHooks::default());
    }
    let root: Value = serde_json::from_str(trimmed).map_err(|error| error.to_string())?;
    let Some(object) = root.as_object() else {
        return Err("hooks.json must contain a JSON object".to_string());
    };
    // Unknown top-level keys are ignored rather than rejected, so a future
    // key (a schema version, say) does not break older clud builds.
    let Some(events) = object.get("hooks").and_then(Value::as_object) else {
        return Ok(CludHooks::default());
    };

    let mut parsed = CludHooks::default();
    for (event, entries) in events {
        let Some(entries) = entries.as_array() else {
            eprintln!("clud: hooks.json event {event:?} must be an array; ignoring it");
            continue;
        };
        let mut collected = Vec::new();
        for (index, entry) in entries.iter().enumerate() {
            match parse_entry(entry) {
                Ok(entry) => collected.push(entry),
                Err(reason) => eprintln!("clud: hooks.json {event}[{index}] ignored: {reason}"),
            }
        }
        parsed.events.insert(event.clone(), collected);
    }
    Ok(parsed)
}

fn parse_entry(value: &Value) -> Result<HookEntry, String> {
    let object = value
        .as_object()
        .ok_or_else(|| "entry must be an object".to_string())?;
    let command = object
        .get("command")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|command| !command.is_empty())
        .ok_or_else(|| "entry needs a non-empty string `command`".to_string())?
        .to_string();
    let matcher = object
        .get("matcher")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|matcher| !matcher.is_empty())
        .map(ToOwned::to_owned);
    let timeout_secs = match object.get("timeout") {
        None | Some(Value::Null) => DEFAULT_TIMEOUT_SECS,
        Some(value) => value
            .as_u64()
            .filter(|seconds| *seconds > 0)
            .ok_or_else(|| "`timeout` must be a positive whole number of seconds".to_string())?,
    };
    Ok(HookEntry {
        matcher,
        command,
        timeout_secs,
    })
}

/// Frontend settings files the harness itself would load for a repo rooted
/// at `repo_root`, in the harness's layering order: the shared Claude file,
/// then its gitignored local one, then the codex file.
const FRONTEND_SETTINGS_FILES: &[&[&str]] = &[
    &[".claude", "settings.json"],
    &[".claude", "settings.local.json"],
    &[".codex", "hooks.json"],
];

/// Read a sub-repo's hooks from its frontend settings — the Tier B source
/// for extern and child roots that have not opted into `.clud/hooks.json`
/// (zackees/clud#967 Phase 4, #966 D4).
///
/// `None` when nothing is declared, mirroring [`discover`]'s contract. The
/// same file missing, unreadable, or unparsable is reported on stderr and
/// treated as absent.
#[must_use]
pub fn from_frontend_settings(repo_root: &Path) -> Option<CludHooks> {
    let mut merged = CludHooks::default();
    for (index, segments) in FRONTEND_SETTINGS_FILES.iter().enumerate() {
        let mut path = repo_root.to_path_buf();
        for segment in *segments {
            path.push(segment);
        }
        let Ok(text) = std::fs::read_to_string(&path) else {
            continue;
        };
        // The last file in the list is the codex one, which also accepts the
        // root-level `<Event>` legacy shape; the Claude files never do.
        let codex = index == FRONTEND_SETTINGS_FILES.len() - 1;
        match parse_frontend(&text, codex) {
            Ok(hooks) => merged.merge(hooks),
            Err(error) => eprintln!(
                "clud: failed to parse {}: {error}; ignoring",
                path.display()
            ),
        }
    }
    if merged.is_empty() {
        return None;
    }
    merged.source = Some(repo_root.to_path_buf());
    Some(merged)
}

impl CludHooks {
    /// Merge `other` into `self`, dropping a hook already declared — the
    /// shared and local Claude files layer in the harness, so the same hook
    /// in both must fire once, not twice.
    fn merge(&mut self, other: CludHooks) {
        for (event, entries) in other.events {
            let target = self.events.entry(event).or_default();
            for entry in entries {
                if !target
                    .iter()
                    .any(|seen| seen.matcher == entry.matcher && seen.command == entry.command)
                {
                    target.push(entry);
                }
            }
        }
    }
}

/// Parse hooks out of a frontend settings body. Same lenient posture as
/// [`parse`]: a document that is not JSON at all is an error; anything else
/// malformed is skipped with a warning.
///
/// Recognized shapes:
///
/// - `hooks.<Event>` — an array of groups, or a single group object, in both
///   frontends' current shapes (`{ "matcher": …, "hooks": [{ "type":
///   "command", "command": …, "timeout": … }] }` and the older direct
///   `{ "matcher": …, "command": … }`).
/// - root-level `<Event>` — codex's legacy shape, which the codex file holds
///   nothing else at the root to confuse with.
fn parse_frontend(text: &str, codex: bool) -> Result<CludHooks, String> {
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return Ok(CludHooks::default());
    }
    let root: Value = serde_json::from_str(trimmed).map_err(|error| error.to_string())?;
    let Some(object) = root.as_object() else {
        return Err("frontend settings must contain a JSON object".to_string());
    };
    let mut parsed = CludHooks::default();
    if let Some(hooks) = object.get("hooks").and_then(Value::as_object) {
        for (event, entries) in hooks {
            if codex && event == "state" {
                // Codex's per-hook trust table (`hooks.state`), not an event.
                continue;
            }
            add_frontend_event(&mut parsed, event, entries);
        }
    }
    if codex {
        for (event, entries) in object {
            if event != "hooks" {
                add_frontend_event(&mut parsed, event, entries);
            }
        }
    }
    Ok(parsed)
}

fn add_frontend_event(parsed: &mut CludHooks, event: &str, value: &Value) {
    let groups: Vec<&Value> = match value {
        Value::Array(groups) => groups.iter().collect(),
        Value::Object(_) => vec![value],
        _ => {
            eprintln!(
                "clud: frontend hook event {event:?} must be an array or a group object; ignoring it"
            );
            return;
        }
    };
    let mut collected = Vec::new();
    for (index, group) in groups.iter().enumerate() {
        match parse_frontend_group(group) {
            Ok(entries) => collected.extend(entries),
            Err(reason) => eprintln!("clud: frontend hook {event}[{index}] ignored: {reason}"),
        }
    }
    parsed
        .events
        .entry(event.to_string())
        .or_default()
        .extend(collected);
}

/// One item of a frontend `hooks.<Event>` array (or a lone group object).
///
/// Either the current group shape — `{ "matcher": …, "hooks": [{
/// "type": "command", "command": …, "timeout": … }] }`, which can expand to
/// several entries — or the older direct `{ "matcher": …, "command": …,
/// "timeout": … }` entry.
fn parse_frontend_group(value: &Value) -> Result<Vec<HookEntry>, String> {
    let object = value
        .as_object()
        .ok_or_else(|| "entry must be an object".to_string())?;
    let matcher = object
        .get("matcher")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|matcher| !matcher.is_empty())
        .map(ToOwned::to_owned);

    if let Some(handlers) = object.get("hooks") {
        let handlers = handlers
            .as_array()
            .ok_or_else(|| "`hooks` must be an array".to_string())?;
        let mut entries = Vec::new();
        for (index, handler) in handlers.iter().enumerate() {
            if !handler_type_is_command(handler) {
                continue;
            }
            match parse_entry(handler) {
                Ok(mut entry) => {
                    if entry.matcher.is_none() {
                        entry.matcher = matcher.clone();
                    }
                    entries.push(entry);
                }
                Err(reason) => eprintln!("clud: frontend hook handler[{index}] ignored: {reason}"),
            }
        }
        return Ok(entries);
    }

    let mut entry = parse_entry(value)?;
    if entry.matcher.is_none() {
        entry.matcher = matcher;
    }
    Ok(vec![entry])
}

/// Whether a frontend handler is something clud executes. The harness tags
/// handlers with `type` (`"command"`, `"stdin"`, …); only an explicit
/// `"command"` — or an untagged handler in the older shape — is run.
fn handler_type_is_command(handler: &Value) -> bool {
    match handler.get("type").and_then(Value::as_str) {
        None => true,
        Some("command") => true,
        Some(_) => false,
    }
}

#[cfg(test)]
#[path = "clud_hooks_tests.rs"]
mod clud_hooks_tests;
