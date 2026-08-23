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

/// The hooks a repo declares to the *harness*, read as if they had been
/// declared to clud.
///
/// Sub-repos have not opted into `.clud/hooks.json` — they are somebody
/// else's repo, and asking them to migrate before clud will run their guards
/// would mean the feature does nothing for the case it was built for. The
/// harness never loads a nested repo's settings at all, so there is no
/// double-fire risk here: clud is the only thing that can run these.
#[must_use]
pub fn discover_frontend(repo_root: &Path) -> Option<CludHooks> {
    for relative in [
        [".claude", "settings.json"],
        [".claude", "settings.local.json"],
    ] {
        let mut path = repo_root.to_path_buf();
        for segment in relative {
            path.push(segment);
        }
        let Ok(text) = std::fs::read_to_string(&path) else {
            continue;
        };
        let Ok(document) = serde_json::from_str::<Value>(&text) else {
            continue;
        };
        let Some(hooks) = from_frontend_document(&document) else {
            continue;
        };
        if !hooks.is_empty() {
            let mut hooks = hooks;
            hooks.source = Some(path);
            return Some(hooks);
        }
    }
    None
}

/// Convert a frontend settings document's `hooks` section.
///
/// The frontend nests one level deeper than clud's own schema — each matcher
/// group holds a list of handlers — and only `type: "command"` handlers are
/// runnable here; an `http` handler is the harness's own business.
#[must_use]
pub fn from_frontend_document(document: &Value) -> Option<CludHooks> {
    let events = document.get("hooks")?.as_object()?;
    let mut parsed = CludHooks::default();
    for (event, groups) in events {
        let Some(groups) = groups.as_array() else {
            continue;
        };
        let mut collected = Vec::new();
        for group in groups {
            let matcher = group
                .get("matcher")
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|matcher| !matcher.is_empty())
                .map(ToOwned::to_owned);
            let Some(handlers) = group.get("hooks").and_then(Value::as_array) else {
                continue;
            };
            for handler in handlers {
                let is_command = handler
                    .get("type")
                    .and_then(Value::as_str)
                    .is_none_or(|kind| kind == "command");
                if !is_command {
                    continue;
                }
                let Some(command) = handler
                    .get("command")
                    .and_then(Value::as_str)
                    .map(str::trim)
                    .filter(|command| !command.is_empty())
                else {
                    continue;
                };
                collected.push(HookEntry {
                    matcher: matcher.clone(),
                    command: command.to_string(),
                    timeout_secs: handler
                        .get("timeout")
                        .and_then(Value::as_u64)
                        .filter(|seconds| *seconds > 0)
                        .unwrap_or(DEFAULT_TIMEOUT_SECS),
                });
            }
        }
        parsed.events.insert(event.clone(), collected);
    }
    Some(parsed)
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

#[cfg(test)]
#[path = "clud_hooks_tests.rs"]
mod clud_hooks_tests;
