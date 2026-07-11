//! `.clud/settings.json` discovery + parser (repo-level AND user-level).
//!
//! Mirrors the `.claude/settings.json` convention so the two repo-scoped
//! config systems read symmetrically. When clud starts a session inside
//! a repo that ships a `.clud/settings.json` declaring
//! `"rust": { "use_soldr": true }`, clud transparently routes Rust
//! toolchain calls through soldr by prepending soldr's shim dir to the
//! session `PATH` (see [`crate::soldr_activate`] and zackees/clud#343).
//!
//! ## Two-level layout (DD-014)
//!
//! - **User-level** `~/.clud/settings.json` — defaults that apply to
//!   every repo the user opens. Lives next to the existing
//!   `~/.clud/settings.toml` (DD'd separately as the user-edited dev
//!   settings, owned by [`crate::clud_settings`]).
//! - **Repo-level** `<repo-root>/.clud/settings.json` — per-repo
//!   overrides. Lands in version control alongside other repo configs.
//!
//! Merge semantics: **repo wins per-field**. A field unset at the repo
//! level falls through to the user-level value; a field unset at both
//! levels uses the baked-in default. This is identical to how
//! `.claude/settings.json` layers with `~/.claude/settings.json` in
//! Claude Code.
//!
//! Schema (v1):
//!
//! ```json
//! {
//!   "rust": {
//!     "use_soldr": true,        // route cargo/rustc/rustfmt/clippy-driver/
//!                               // rustdoc through soldr (default: true when
//!                               // a settings file is present).
//!     "install":   true,        // auto-install soldr if missing (default: true).
//!     "version":   "0.7.55"     // optional pinned version; absent = latest.
//!   }
//! }
//! ```
//!
//! The current `clud optimize rust` command writes the equivalent shape under
//! `"optimize": { "rust": { "use_soldr_shims": ..., "install_soldr": ...,
//! "soldr_version": ... } }`. This parser accepts both forms. Direct `rust`
//! keys win over `optimize.rust` keys within a file.

use regex::Regex;
use serde::Deserialize;
use std::collections::HashSet;
use std::path::{Path, PathBuf};

// ---------------------------------------------------------------------
// Raw parse types — every field is Option so the merge step can tell
// "user set this to false" from "user didn't set this".
// ---------------------------------------------------------------------

#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
#[serde(default)]
pub struct RawRepoCludConfig {
    pub rust: RawRustConfig,
    pub optimize: RawOptimizeConfig,
    #[serde(deserialize_with = "deserialize_bad_commands")]
    pub bad_commands: Vec<BadCommandRule>,
    #[serde(deserialize_with = "deserialize_bad_pipelines")]
    pub bad_pipelines: Vec<BadPipelineRule>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
#[serde(default)]
pub struct RawRustConfig {
    pub use_soldr: Option<bool>,
    pub install: Option<bool>,
    pub version: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
#[serde(default)]
pub struct RawOptimizeConfig {
    pub rust: RawOptimizeRustConfig,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
#[serde(default)]
pub struct RawOptimizeRustConfig {
    pub use_soldr_shims: Option<bool>,
    pub install_soldr: Option<bool>,
    pub soldr_version: Option<String>,
}

// ---------------------------------------------------------------------
// `bad_commands` — generic "bad command -> blessed replacement" rules
// (zackees/clud#519). Each entry is fully validated at parse time: a
// rule with a bad shape or an invalid glob/regex pattern is skipped
// with a warning rather than failing the whole file, mirroring
// `read_and_parse_raw`'s malformed-JSON handling.
// ---------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MatchMode {
    Glob,
    Regex,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BadCommandRule {
    pub id: Option<String>,
    pub pattern: String,
    pub match_mode: MatchMode,
    pub replacement: String,
    pub reason: String,
    pub passthrough_prefixes: Vec<String>,
    pub allow_override: bool,
    pub through_wrappers: Vec<String>,
    pub arguments: Option<ArgumentMatcher>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MatchPattern {
    pub pattern: String,
    pub match_mode: MatchMode,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ArgumentMatcher {
    pub prefix: Vec<MatchPattern>,
    pub ordered: Vec<MatchPattern>,
    pub contiguous: Vec<MatchPattern>,
    pub any: Vec<MatchPattern>,
    pub all: Vec<MatchPattern>,
    pub none: Vec<MatchPattern>,
    pub short_flags_any: Vec<char>,
    pub short_flags_all: Vec<char>,
    pub any_of: Vec<ArgumentMatcher>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommandMatcher {
    pub pattern: String,
    pub match_mode: MatchMode,
    pub through_wrappers: Vec<String>,
    pub arguments: Option<ArgumentMatcher>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BadPipelineRule {
    pub id: Option<String>,
    pub stages: Vec<CommandMatcher>,
    pub replacement: String,
    pub reason: String,
    pub allow_override: bool,
}

/// Compile a rule's `pattern` (glob or regex, per `mode`) into a
/// `Regex` anchored to match the *whole* normalized program-name
/// token, never a substring/prefix. Both syntaxes are auto-anchored:
/// callers never need to write `^`/`$` themselves.
pub fn compile_match_pattern(pattern: &str, mode: MatchMode) -> Result<Regex, String> {
    let body = match mode {
        MatchMode::Regex => pattern.to_string(),
        MatchMode::Glob => glob_to_regex_source(pattern)?,
    };
    Regex::new(&format!("(?i)^(?:{body})$")).map_err(|e| e.to_string())
}

fn glob_to_regex_source(glob: &str) -> Result<String, String> {
    let mut out = String::new();
    let mut bracket_depth = 0i32;
    let mut chars = glob.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '*' if bracket_depth == 0 => out.push_str(".*"),
            '?' if bracket_depth == 0 => out.push('.'),
            '[' => {
                bracket_depth += 1;
                out.push('[');
                if let Some('!') = chars.peek() {
                    out.push('^');
                    chars.next();
                }
            }
            ']' => {
                if bracket_depth == 0 {
                    return Err("unmatched ']' in glob pattern".to_string());
                }
                bracket_depth -= 1;
                out.push(']');
            }
            c if bracket_depth > 0 => out.push(c),
            c => {
                if "\\.+^$(){}|".contains(c) {
                    out.push('\\');
                }
                out.push(c);
            }
        }
    }
    if bracket_depth != 0 {
        return Err("unmatched '[' in glob pattern".to_string());
    }
    Ok(out)
}

fn parse_bad_command_rule(value: &serde_json::Value) -> Result<BadCommandRule, String> {
    let object = value
        .as_object()
        .ok_or_else(|| "bad_commands entry is not a JSON object".to_string())?;
    let pattern = object
        .get("match")
        .and_then(serde_json::Value::as_str)
        .ok_or_else(|| "bad_commands entry missing required string field \"match\"".to_string())?
        .to_string();
    let replacement = object
        .get("replacement")
        .and_then(serde_json::Value::as_str)
        .ok_or_else(|| {
            "bad_commands entry missing required string field \"replacement\"".to_string()
        })?
        .to_string();
    let reason = object
        .get("reason")
        .and_then(serde_json::Value::as_str)
        .unwrap_or("")
        .to_string();
    let id = object
        .get("id")
        .and_then(serde_json::Value::as_str)
        .map(str::to_string);
    let match_mode_raw = object.get("match_mode").and_then(serde_json::Value::as_str);
    let match_mode = match match_mode_raw {
        None | Some("glob") => MatchMode::Glob,
        Some("regex") => MatchMode::Regex,
        Some(other) => {
            let msg = format!(
                "bad_commands entry has unknown match_mode {other:?}; expected \"glob\" or \"regex\""
            );
            return Err(msg);
        }
    };
    let passthrough_prefixes = object
        .get("passthrough_prefixes")
        .and_then(serde_json::Value::as_array)
        .map(|items| {
            items
                .iter()
                .filter_map(serde_json::Value::as_str)
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default();
    let allow_override = object
        .get("allow_override")
        .and_then(serde_json::Value::as_bool)
        .unwrap_or(false);
    let through_wrappers = parse_wrappers(object.get("through_wrappers"))?;
    let arguments = object
        .get("arguments")
        .map(|value| parse_argument_matcher(value, 0))
        .transpose()?;

    compile_match_pattern(&pattern, match_mode)?;

    Ok(BadCommandRule {
        id,
        pattern,
        match_mode,
        replacement,
        reason,
        passthrough_prefixes,
        allow_override,
        through_wrappers,
        arguments,
    })
}

const MAX_ARGUMENT_MATCHER_DEPTH: usize = 8;

fn parse_match_mode(value: Option<&serde_json::Value>, context: &str) -> Result<MatchMode, String> {
    match value.and_then(serde_json::Value::as_str) {
        None | Some("glob") => Ok(MatchMode::Glob),
        Some("regex") => Ok(MatchMode::Regex),
        Some(other) => Err(format!(
            "{context} has unknown match_mode {other:?}; expected \"glob\" or \"regex\""
        )),
    }
}

fn parse_match_pattern(value: &serde_json::Value, context: &str) -> Result<MatchPattern, String> {
    let (pattern, match_mode) = if let Some(pattern) = value.as_str() {
        (pattern.to_string(), MatchMode::Glob)
    } else {
        let object = value
            .as_object()
            .ok_or_else(|| format!("{context} pattern must be a string or JSON object"))?;
        for key in object.keys() {
            if !["match", "match_mode"].contains(&key.as_str()) {
                return Err(format!("{context} pattern has unknown field {key:?}"));
            }
        }
        let pattern = object
            .get("match")
            .and_then(serde_json::Value::as_str)
            .ok_or_else(|| format!("{context} pattern missing required string field \"match\""))?
            .to_string();
        let mode = parse_match_mode(object.get("match_mode"), context)?;
        (pattern, mode)
    };
    compile_match_pattern(&pattern, match_mode)
        .map_err(|err| format!("{context} has invalid pattern {pattern:?}: {err}"))?;
    Ok(MatchPattern {
        pattern,
        match_mode,
    })
}

fn parse_pattern_array(
    object: &serde_json::Map<String, serde_json::Value>,
    field: &str,
) -> Result<Vec<MatchPattern>, String> {
    let Some(value) = object.get(field) else {
        return Ok(Vec::new());
    };
    let array = value
        .as_array()
        .ok_or_else(|| format!("arguments.{field} must be an array"))?;
    array
        .iter()
        .map(|item| parse_match_pattern(item, &format!("arguments.{field}")))
        .collect()
}

fn parse_short_flags(
    object: &serde_json::Map<String, serde_json::Value>,
    field: &str,
) -> Result<Vec<char>, String> {
    let Some(value) = object.get(field) else {
        return Ok(Vec::new());
    };
    let array = value
        .as_array()
        .ok_or_else(|| format!("arguments.{field} must be an array"))?;
    array
        .iter()
        .map(|item| {
            let raw = item
                .as_str()
                .ok_or_else(|| format!("arguments.{field} entries must be strings"))?;
            let mut chars = raw.chars();
            let flag = chars
                .next()
                .ok_or_else(|| format!("arguments.{field} entries must be one flag character"))?;
            if chars.next().is_some() || flag == '-' {
                return Err(format!(
                    "arguments.{field} entry {raw:?} must be one flag character without '-'"
                ));
            }
            Ok(flag)
        })
        .collect()
}

fn parse_argument_matcher(
    value: &serde_json::Value,
    depth: usize,
) -> Result<ArgumentMatcher, String> {
    if depth > MAX_ARGUMENT_MATCHER_DEPTH {
        return Err(format!(
            "arguments.any_of nesting exceeds {MAX_ARGUMENT_MATCHER_DEPTH} levels"
        ));
    }
    let object = value
        .as_object()
        .ok_or_else(|| "arguments must be a JSON object".to_string())?;
    const FIELDS: &[&str] = &[
        "prefix",
        "ordered",
        "contiguous",
        "any",
        "all",
        "none",
        "short_flags_any",
        "short_flags_all",
        "any_of",
    ];
    for key in object.keys() {
        if !FIELDS.contains(&key.as_str()) {
            return Err(format!("arguments has unknown field {key:?}"));
        }
    }
    let any_of = match object.get("any_of") {
        None => Vec::new(),
        Some(value) => value
            .as_array()
            .ok_or_else(|| "arguments.any_of must be an array".to_string())?
            .iter()
            .map(|branch| parse_argument_matcher(branch, depth + 1))
            .collect::<Result<Vec<_>, _>>()?,
    };
    let matcher = ArgumentMatcher {
        prefix: parse_pattern_array(object, "prefix")?,
        ordered: parse_pattern_array(object, "ordered")?,
        contiguous: parse_pattern_array(object, "contiguous")?,
        any: parse_pattern_array(object, "any")?,
        all: parse_pattern_array(object, "all")?,
        none: parse_pattern_array(object, "none")?,
        short_flags_any: parse_short_flags(object, "short_flags_any")?,
        short_flags_all: parse_short_flags(object, "short_flags_all")?,
        any_of,
    };
    let has_predicate = !matcher.prefix.is_empty()
        || !matcher.ordered.is_empty()
        || !matcher.contiguous.is_empty()
        || !matcher.any.is_empty()
        || !matcher.all.is_empty()
        || !matcher.none.is_empty()
        || !matcher.short_flags_any.is_empty()
        || !matcher.short_flags_all.is_empty()
        || !matcher.any_of.is_empty();
    if !has_predicate {
        return Err("arguments must contain at least one non-empty predicate".to_string());
    }
    Ok(matcher)
}

fn parse_wrappers(value: Option<&serde_json::Value>) -> Result<Vec<String>, String> {
    let Some(value) = value else {
        return Ok(Vec::new());
    };
    let array = value
        .as_array()
        .ok_or_else(|| "through_wrappers must be an array".to_string())?;
    let wrappers = array
        .iter()
        .map(|item| {
            item.as_str()
                .map(|value| value.to_ascii_lowercase())
                .ok_or_else(|| "through_wrappers entries must be strings".to_string())
        })
        .collect::<Result<Vec<_>, _>>()?;
    for wrapper in &wrappers {
        if !["sudo", "env", "command", "exec"].contains(&wrapper.as_str()) {
            return Err(format!(
                "unsupported through_wrappers entry {wrapper:?}; expected sudo, env, command, or exec"
            ));
        }
    }
    Ok(wrappers)
}

fn parse_command_matcher(
    value: &serde_json::Value,
    context: &str,
) -> Result<CommandMatcher, String> {
    let object = value
        .as_object()
        .ok_or_else(|| format!("{context} must be a JSON object"))?;
    let pattern = object
        .get("match")
        .and_then(serde_json::Value::as_str)
        .ok_or_else(|| format!("{context} missing required string field \"match\""))?
        .to_string();
    let match_mode = parse_match_mode(object.get("match_mode"), context)?;
    compile_match_pattern(&pattern, match_mode)
        .map_err(|err| format!("{context} has invalid pattern {pattern:?}: {err}"))?;
    Ok(CommandMatcher {
        pattern,
        match_mode,
        through_wrappers: parse_wrappers(object.get("through_wrappers"))?,
        arguments: object
            .get("arguments")
            .map(|value| parse_argument_matcher(value, 0))
            .transpose()?,
    })
}

fn parse_bad_pipeline_rule(value: &serde_json::Value) -> Result<BadPipelineRule, String> {
    let object = value
        .as_object()
        .ok_or_else(|| "bad_pipelines entry is not a JSON object".to_string())?;
    let stages = object
        .get("stages")
        .and_then(serde_json::Value::as_array)
        .ok_or_else(|| "bad_pipelines entry missing required array field \"stages\"".to_string())?
        .iter()
        .enumerate()
        .map(|(index, stage)| parse_command_matcher(stage, &format!("bad_pipelines stage {index}")))
        .collect::<Result<Vec<_>, _>>()?;
    if stages.len() < 2 {
        return Err("bad_pipelines entry must contain at least two stages".to_string());
    }
    let replacement = object
        .get("replacement")
        .and_then(serde_json::Value::as_str)
        .ok_or_else(|| {
            "bad_pipelines entry missing required string field \"replacement\"".to_string()
        })?
        .to_string();
    Ok(BadPipelineRule {
        id: object
            .get("id")
            .and_then(serde_json::Value::as_str)
            .map(str::to_string),
        stages,
        replacement,
        reason: object
            .get("reason")
            .and_then(serde_json::Value::as_str)
            .unwrap_or("")
            .to_string(),
        allow_override: object
            .get("allow_override")
            .and_then(serde_json::Value::as_bool)
            .unwrap_or(false),
    })
}

fn deserialize_bad_commands<'de, D>(deserializer: D) -> Result<Vec<BadCommandRule>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let raw: Vec<serde_json::Value> = Vec::deserialize(deserializer)?;
    let mut rules = Vec::with_capacity(raw.len());
    for entry in raw {
        match parse_bad_command_rule(&entry) {
            Ok(rule) => rules.push(rule),
            Err(err) => {
                eprintln!("clud: skipping malformed bad_commands rule: {err}; ignoring");
            }
        }
    }
    Ok(rules)
}

fn deserialize_bad_pipelines<'de, D>(deserializer: D) -> Result<Vec<BadPipelineRule>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let raw: Vec<serde_json::Value> = Vec::deserialize(deserializer)?;
    let mut rules = Vec::with_capacity(raw.len());
    for entry in raw {
        match parse_bad_pipeline_rule(&entry) {
            Ok(rule) => rules.push(rule),
            Err(err) => {
                eprintln!("clud: skipping malformed bad_pipelines rule: {err}; ignoring");
            }
        }
    }
    Ok(rules)
}

/// Concatenate `upper` (e.g. repo-level) over `lower` (e.g. user-level)
/// rules. Unlike the scalar rust-config fields, arrays add rather than
/// override: every rule from both levels is active, except that a
/// `lower` rule sharing an `id` with an `upper` rule is dropped in
/// favor of the `upper` definition (id-less rules never dedupe).
fn concat_dedupe_bad_commands(
    upper: Vec<BadCommandRule>,
    lower: Vec<BadCommandRule>,
) -> Vec<BadCommandRule> {
    let upper_ids: HashSet<&str> = upper.iter().filter_map(|r| r.id.as_deref()).collect();
    let mut result: Vec<BadCommandRule> = lower
        .into_iter()
        .filter(|r| match &r.id {
            Some(id) => !upper_ids.contains(id.as_str()),
            None => true,
        })
        .collect();
    result.extend(upper);
    result
}

fn concat_dedupe_bad_pipelines(
    upper: Vec<BadPipelineRule>,
    lower: Vec<BadPipelineRule>,
) -> Vec<BadPipelineRule> {
    let upper_ids: HashSet<&str> = upper.iter().filter_map(|r| r.id.as_deref()).collect();
    let mut result: Vec<BadPipelineRule> = lower
        .into_iter()
        .filter(|r| match &r.id {
            Some(id) => !upper_ids.contains(id.as_str()),
            None => true,
        })
        .collect();
    result.extend(upper);
    result
}

// ---------------------------------------------------------------------
// Resolved types — what the rest of the binary actually consumes.
// ---------------------------------------------------------------------

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RepoCludConfig {
    pub rust: RustConfig,
    pub bad_commands: Vec<BadCommandRule>,
    pub bad_pipelines: Vec<BadPipelineRule>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RustConfig {
    pub use_soldr: bool,
    pub install: bool,
    pub version: Option<String>,
}

impl Default for RustConfig {
    fn default() -> Self {
        Self {
            use_soldr: true,
            install: true,
            version: None,
        }
    }
}

// ---------------------------------------------------------------------
// Public discovery API.
// ---------------------------------------------------------------------

/// Resolve the effective config for a session starting at `start`.
///
/// Loads user-level `~/.clud/settings.json` first, then repo-level
/// `<repo-root>/.clud/settings.json` (walking up from `start` to the
/// `.git/` boundary). Merges with repo winning per-field. Returns
/// `None` when neither file exists.
pub fn discover_effective_clud_config(start: &Path) -> Option<RepoCludConfig> {
    let user = discover_user_clud_config_raw();
    let repo = discover_repo_clud_config_raw(start);
    resolve_effective_config(repo, user)
}

/// Public single-source variant used by tests + future direct
/// callers that don't want the merge. Walks up from `start` looking
/// for a repo-level `.clud/settings.json`. See module docs for the
/// resolution rules.
pub fn discover_repo_clud_config(start: &Path) -> Option<RepoCludConfig> {
    discover_repo_clud_config_raw(start)
        .map(|raw| resolve(merge(raw, RawRepoCludConfig::default())))
}

/// Read user-level `~/.clud/settings.json`, if present.
pub fn discover_user_clud_config() -> Option<RepoCludConfig> {
    discover_user_clud_config_raw()
        .filter(has_directive)
        .map(|raw| resolve(merge(raw, RawRepoCludConfig::default())))
}

fn resolve_effective_config(
    repo: Option<RawRepoCludConfig>,
    user: Option<RawRepoCludConfig>,
) -> Option<RepoCludConfig> {
    match (repo, user) {
        (None, None) => None,
        (None, Some(user)) if !has_directive(&user) => None,
        (None, Some(user)) => Some(resolve(merge(user, RawRepoCludConfig::default()))),
        (Some(repo), None) => Some(resolve(merge(repo, RawRepoCludConfig::default()))),
        (Some(repo), Some(user)) => {
            let user = if has_directive(&user) {
                user
            } else {
                RawRepoCludConfig::default()
            };
            Some(resolve(merge(repo, user)))
        }
    }
}

fn has_directive(raw: &RawRepoCludConfig) -> bool {
    raw.rust.use_soldr.is_some()
        || raw.rust.install.is_some()
        || raw.rust.version.is_some()
        || raw.optimize.rust.use_soldr_shims.is_some()
        || raw.optimize.rust.install_soldr.is_some()
        || raw.optimize.rust.soldr_version.is_some()
        || !raw.bad_commands.is_empty()
        || !raw.bad_pipelines.is_empty()
}

// ---------------------------------------------------------------------
// Raw discovery (Option-shaped) — used by the merge.
// ---------------------------------------------------------------------

fn discover_repo_clud_config_raw(start: &Path) -> Option<RawRepoCludConfig> {
    let mut cursor: PathBuf = if start.is_absolute() {
        start.to_path_buf()
    } else {
        std::env::current_dir().ok()?.join(start)
    };
    if let Ok(real) = cursor.canonicalize() {
        cursor = real;
    }

    loop {
        let candidate = cursor.join(".clud").join("settings.json");
        if candidate.is_file() {
            return read_and_parse_raw(&candidate, "repo-level");
        }
        if cursor.join(".git").exists() {
            return None;
        }
        if !cursor.pop() {
            return None;
        }
    }
}

fn discover_user_clud_config_raw() -> Option<RawRepoCludConfig> {
    let home = dirs::home_dir()?;
    let candidate = home.join(".clud").join("settings.json");
    if !candidate.is_file() {
        return None;
    }
    read_and_parse_raw(&candidate, "user-level")
}

fn read_and_parse_raw(path: &Path, scope: &str) -> Option<RawRepoCludConfig> {
    let text = match std::fs::read_to_string(path) {
        Ok(s) => s,
        Err(err) => {
            eprintln!(
                "clud: failed to read {} ({scope}): {err}; ignoring",
                path.display()
            );
            return None;
        }
    };
    match parse_raw_repo_clud_config(&text) {
        Ok(raw) => Some(raw),
        Err(err) => {
            eprintln!(
                "clud: {scope} settings file at {} is malformed: {err}; ignoring",
                path.display()
            );
            None
        }
    }
}

// ---------------------------------------------------------------------
// Parsing.
// ---------------------------------------------------------------------

/// Parse a `.clud/settings.json` body into the raw (Option-shaped) form.
///
/// Empty file = all-None (= all-defaults at resolve time).
/// Empty / whitespace-only `version` is normalized to `None`.
pub fn parse_raw_repo_clud_config(text: &str) -> Result<RawRepoCludConfig, String> {
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return Ok(RawRepoCludConfig::default());
    }
    let mut parsed: RawRepoCludConfig =
        serde_json::from_str(text).map_err(|e: serde_json::Error| e.to_string())?;
    if let Some(v) = parsed.rust.version.as_deref() {
        if v.trim().is_empty() {
            parsed.rust.version = None;
        }
    }
    if let Some(v) = parsed.optimize.rust.soldr_version.as_deref() {
        if v.trim().is_empty() {
            parsed.optimize.rust.soldr_version = None;
        }
    }
    Ok(parsed)
}

/// Convenience wrapper used by tests that want the resolved form
/// straight from a string.
pub fn parse_repo_clud_config(text: &str) -> Result<RepoCludConfig, String> {
    parse_raw_repo_clud_config(text).map(|raw| resolve(merge(raw, RawRepoCludConfig::default())))
}

// ---------------------------------------------------------------------
// Merge + resolve.
// ---------------------------------------------------------------------

/// Layer `lower` (e.g. user-level) under `upper` (e.g. repo-level).
/// `upper` wins per-field where set for the scalar rust fields;
/// `bad_commands` concatenates instead (see `concat_dedupe_bad_commands`).
pub fn merge(upper: RawRepoCludConfig, lower: RawRepoCludConfig) -> RawRepoCludConfig {
    let upper_bad_commands = upper.bad_commands.clone();
    let lower_bad_commands = lower.bad_commands.clone();
    let upper_bad_pipelines = upper.bad_pipelines.clone();
    let lower_bad_pipelines = lower.bad_pipelines.clone();
    let upper_rust = normalize_raw_rust(upper);
    let lower_rust = normalize_raw_rust(lower);
    RawRepoCludConfig {
        rust: RawRustConfig {
            use_soldr: upper_rust.use_soldr.or(lower_rust.use_soldr),
            install: upper_rust.install.or(lower_rust.install),
            version: upper_rust.version.or(lower_rust.version),
        },
        optimize: RawOptimizeConfig::default(),
        bad_commands: concat_dedupe_bad_commands(upper_bad_commands, lower_bad_commands),
        bad_pipelines: concat_dedupe_bad_pipelines(upper_bad_pipelines, lower_bad_pipelines),
    }
}

fn normalize_raw_rust(raw: RawRepoCludConfig) -> RawRustConfig {
    let RawRepoCludConfig {
        rust,
        optimize,
        bad_commands: _,
        bad_pipelines: _,
    } = raw;
    RawRustConfig {
        use_soldr: rust.use_soldr.or(optimize.rust.use_soldr_shims),
        install: rust.install.or(optimize.rust.install_soldr),
        version: rust.version.or(optimize.rust.soldr_version),
    }
}

/// Apply baked-in defaults to any remaining None fields.
pub fn resolve(raw: RawRepoCludConfig) -> RepoCludConfig {
    let RawRustConfig {
        use_soldr,
        install,
        version,
    } = raw.rust;
    RepoCludConfig {
        rust: RustConfig {
            use_soldr: use_soldr.unwrap_or(true),
            install: install.unwrap_or(true),
            version,
        },
        bad_commands: raw.bad_commands,
        bad_pipelines: raw.bad_pipelines,
    }
}

// ---------------------------------------------------------------------
// Tests.
// ---------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    fn write_settings(root: &Path, body: &str) {
        let dir = root.join(".clud");
        fs::create_dir_all(&dir).expect("mkdir .clud");
        fs::write(dir.join("settings.json"), body).expect("write settings.json");
    }

    fn mark_repo_root(root: &Path) {
        fs::create_dir_all(root.join(".git")).expect("mkdir .git");
    }

    // -----------------------------------------------------------------
    // Parser tests.
    // -----------------------------------------------------------------

    #[test]
    fn empty_body_returns_default_resolved_config() {
        let cfg = parse_repo_clud_config("").expect("empty body parses");
        assert_eq!(cfg, RepoCludConfig::default());
        assert!(cfg.rust.use_soldr);
        assert!(cfg.rust.install);
        assert_eq!(cfg.rust.version, None);
    }

    #[test]
    fn empty_body_returns_all_none_raw() {
        let raw = parse_raw_repo_clud_config("").expect("parses");
        assert_eq!(raw.rust.use_soldr, None);
        assert_eq!(raw.rust.install, None);
        assert_eq!(raw.rust.version, None);
    }

    #[test]
    fn empty_object_resolves_to_defaults() {
        let cfg = parse_repo_clud_config("{}").expect("parses");
        assert_eq!(cfg, RepoCludConfig::default());
    }

    #[test]
    fn missing_rust_key_resolves_to_defaults() {
        let cfg = parse_repo_clud_config(r#"{"python":{}}"#).expect("parses");
        assert_eq!(cfg, RepoCludConfig::default());
    }

    #[test]
    fn full_rust_object_parses() {
        let cfg = parse_repo_clud_config(
            r#"{"rust":{"use_soldr":true,"install":true,"version":"0.7.55"}}"#,
        )
        .expect("parses");
        assert!(cfg.rust.use_soldr);
        assert!(cfg.rust.install);
        assert_eq!(cfg.rust.version.as_deref(), Some("0.7.55"));
    }

    #[test]
    fn optimize_rust_object_parses_as_activation_config() {
        let cfg = parse_repo_clud_config(
            r#"{"optimize":{"rust":{"use_soldr_shims":false,"install_soldr":false,"soldr_version":"0.7.11"}}}"#,
        )
        .expect("parses");
        assert!(!cfg.rust.use_soldr);
        assert!(!cfg.rust.install);
        assert_eq!(cfg.rust.version.as_deref(), Some("0.7.11"));
    }

    #[test]
    fn direct_rust_keys_win_over_optimize_rust_keys_in_same_file() {
        let cfg = parse_repo_clud_config(
            r#"{"rust":{"use_soldr":false,"version":"2.0.0"},"optimize":{"rust":{"use_soldr_shims":true,"soldr_version":"1.0.0"}}}"#,
        )
        .expect("parses");
        assert!(!cfg.rust.use_soldr);
        assert_eq!(cfg.rust.version.as_deref(), Some("2.0.0"));
    }

    #[test]
    fn explicit_use_soldr_false_is_honored() {
        let cfg = parse_repo_clud_config(r#"{"rust":{"use_soldr":false}}"#).expect("parses");
        assert!(!cfg.rust.use_soldr);
    }

    #[test]
    fn explicit_install_false_is_honored() {
        let cfg = parse_repo_clud_config(r#"{"rust":{"install":false}}"#).expect("parses");
        assert!(!cfg.rust.install);
        assert!(cfg.rust.use_soldr, "use_soldr should default to true");
    }

    #[test]
    fn empty_version_string_is_treated_as_unset() {
        let cfg = parse_repo_clud_config(r#"{"rust":{"version":""}}"#).expect("parses");
        assert_eq!(cfg.rust.version, None);
    }

    #[test]
    fn whitespace_version_string_is_treated_as_unset() {
        let cfg = parse_repo_clud_config(r#"{"rust":{"version":"   "}}"#).expect("parses");
        assert_eq!(cfg.rust.version, None);
    }

    #[test]
    fn unknown_rust_field_is_ignored_for_forward_compat() {
        let cfg = parse_repo_clud_config(r#"{"rust":{"use_soldr":true,"gc_after_install":true}}"#)
            .expect("parses");
        assert!(cfg.rust.use_soldr);
    }

    #[test]
    fn malformed_json_returns_err() {
        let err = parse_repo_clud_config("{\"rust\":{").unwrap_err();
        assert!(!err.is_empty(), "non-empty error message");
    }

    // -----------------------------------------------------------------
    // Merge tests.
    // -----------------------------------------------------------------

    #[test]
    fn merge_repo_overrides_user_per_field() {
        let user = parse_raw_repo_clud_config(
            r#"{"rust":{"use_soldr":true,"install":true,"version":"1.0.0"}}"#,
        )
        .unwrap();
        let repo = parse_raw_repo_clud_config(r#"{"rust":{"use_soldr":false,"version":"2.0.0"}}"#)
            .unwrap();

        let merged = resolve(merge(repo, user));
        assert!(!merged.rust.use_soldr, "repo wins");
        assert!(merged.rust.install, "repo unset → user wins");
        assert_eq!(merged.rust.version.as_deref(), Some("2.0.0"), "repo wins");
    }

    #[test]
    fn merge_repo_optimize_overrides_user_rust_per_field() {
        let user = parse_raw_repo_clud_config(
            r#"{"rust":{"use_soldr":false,"install":false,"version":"1.0.0"}}"#,
        )
        .unwrap();
        let repo = parse_raw_repo_clud_config(
            r#"{"optimize":{"rust":{"use_soldr_shims":true,"soldr_version":"2.0.0"}}}"#,
        )
        .unwrap();

        let merged = resolve(merge(repo, user));
        assert!(merged.rust.use_soldr, "repo optimize wins");
        assert!(
            !merged.rust.install,
            "repo unset falls through to user rust field"
        );
        assert_eq!(merged.rust.version.as_deref(), Some("2.0.0"));
    }

    #[test]
    fn unrelated_user_settings_do_not_enable_global_soldr_activation() {
        let user = parse_raw_repo_clud_config(r#"{"shell":{"disable_powershell":true}}"#).unwrap();
        assert_eq!(resolve_effective_config(None, Some(user)), None);
    }

    #[test]
    fn merge_user_only_provides_defaults_when_repo_missing() {
        let user =
            parse_raw_repo_clud_config(r#"{"rust":{"install":false,"version":"3.0.0"}}"#).unwrap();
        let repo = RawRepoCludConfig::default();

        let merged = resolve(merge(repo, user));
        assert!(merged.rust.use_soldr, "neither set → default true");
        assert!(!merged.rust.install, "user wins when repo unset");
        assert_eq!(merged.rust.version.as_deref(), Some("3.0.0"));
    }

    #[test]
    fn merge_both_empty_resolves_to_baked_defaults() {
        let merged = resolve(merge(
            RawRepoCludConfig::default(),
            RawRepoCludConfig::default(),
        ));
        assert_eq!(merged, RepoCludConfig::default());
    }

    // -----------------------------------------------------------------
    // Repo-level discovery.
    // -----------------------------------------------------------------

    #[test]
    fn discover_finds_at_marked_repo_root() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path();
        mark_repo_root(root);
        write_settings(root, r#"{"rust":{"use_soldr":true,"version":"1.2.3"}}"#);

        let cfg = discover_repo_clud_config(root).expect("found");
        assert!(cfg.rust.use_soldr);
        assert_eq!(cfg.rust.version.as_deref(), Some("1.2.3"));
    }

    #[test]
    fn discover_finds_from_subdirectory() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path();
        mark_repo_root(root);
        write_settings(root, r#"{"rust":{"use_soldr":true}}"#);
        let sub = root.join("crates").join("clud-bin").join("src");
        fs::create_dir_all(&sub).unwrap();

        let cfg = discover_repo_clud_config(&sub).expect("found from subdir");
        assert!(cfg.rust.use_soldr);
    }

    #[test]
    fn missing_settings_returns_none() {
        let tmp = TempDir::new().unwrap();
        mark_repo_root(tmp.path());
        assert!(discover_repo_clud_config(tmp.path()).is_none());
    }

    #[test]
    fn discover_stops_at_git_root_boundary() {
        let tmp = TempDir::new().unwrap();
        let outer = tmp.path();
        let repo = outer.join("repo");
        fs::create_dir_all(&repo).unwrap();
        mark_repo_root(&repo);
        write_settings(outer, r#"{"rust":{"use_soldr":true}}"#);

        assert!(
            discover_repo_clud_config(&repo).is_none(),
            "must not bleed across repo boundary"
        );
    }

    // Note: a `walk-without-git-dir-anywhere` test was considered but is
    // fundamentally fragile — the OS temp dir's ancestors may contain a
    // real user-level `~/.clud/settings.json` on the test host, which the
    // walk would legitimately pick up. The `missing_settings_returns_none`
    // case (which plants a `.git/` boundary explicitly) already covers
    // the "no settings found" branch; the no-`.git`-anywhere edge is
    // exercised in production by behavior, not by a test.

    #[test]
    fn malformed_settings_is_warned_and_skipped() {
        let tmp = TempDir::new().unwrap();
        mark_repo_root(tmp.path());
        write_settings(tmp.path(), "{not valid json");
        assert!(discover_repo_clud_config(tmp.path()).is_none());
    }

    // -----------------------------------------------------------------
    // `bad_commands` (zackees/clud#519).
    // -----------------------------------------------------------------

    #[test]
    fn bad_commands_array_parses_from_repo_settings() {
        let cfg = parse_repo_clud_config(
            r#"{"bad_commands":[{"id":"no-raw-playwright","match":"playwright","match_mode":"glob","replacement":"npm run test:integration","reason":"use the blessed pipeline","passthrough_prefixes":["soldr"],"allow_override":true}]}"#,
        )
        .expect("parses");
        assert_eq!(cfg.bad_commands.len(), 1);
        let rule = &cfg.bad_commands[0];
        assert_eq!(rule.id.as_deref(), Some("no-raw-playwright"));
        assert_eq!(rule.pattern, "playwright");
        assert_eq!(rule.match_mode, MatchMode::Glob);
        assert_eq!(rule.replacement, "npm run test:integration");
        assert_eq!(rule.reason, "use the blessed pipeline");
        assert_eq!(rule.passthrough_prefixes, vec!["soldr".to_string()]);
        assert!(rule.allow_override);
    }

    #[test]
    fn bad_commands_array_parses_with_only_required_fields() {
        let cfg = parse_repo_clud_config(
            r#"{"bad_commands":[{"match":"playwright","replacement":"npm run test:integration"}]}"#,
        )
        .expect("parses");
        let rule = &cfg.bad_commands[0];
        assert_eq!(rule.id, None);
        assert_eq!(rule.match_mode, MatchMode::Glob);
        assert!(rule.passthrough_prefixes.is_empty());
        assert!(!rule.allow_override);
        assert_eq!(rule.reason, "");
    }

    #[test]
    fn bad_command_argument_matchers_parse_with_per_pattern_modes() {
        let cfg = parse_repo_clud_config(
            r#"{"bad_commands":[{"match":"kubectl","replacement":"dry run first","arguments":{"ordered":["delete","namespace"],"any":["production",{"match":"^prod-[a-z]+$","match_mode":"regex"}],"none":["--dry-run=server"],"short_flags_all":["f","d"],"any_of":[{"contiguous":["-n","auto"]}]}}]}"#,
        )
        .expect("parses");
        let arguments = cfg.bad_commands[0]
            .arguments
            .as_ref()
            .expect("arguments parsed");
        assert_eq!(arguments.ordered.len(), 2);
        assert_eq!(arguments.any.len(), 2);
        assert_eq!(arguments.any[0].match_mode, MatchMode::Glob);
        assert_eq!(arguments.any[1].match_mode, MatchMode::Regex);
        assert_eq!(arguments.none.len(), 1);
        assert_eq!(arguments.short_flags_all, vec!['f', 'd']);
        assert_eq!(arguments.any_of.len(), 1);
    }

    #[test]
    fn malformed_nested_argument_pattern_skips_only_that_rule() {
        let cfg = parse_repo_clud_config(
            r#"{"bad_commands":[{"match":"git","replacement":"safe","arguments":{"any":[{"match":"(","match_mode":"regex"}]}},{"match":"rm","replacement":"safe"}]}"#,
        )
        .expect("top-level config parses");
        assert_eq!(cfg.bad_commands.len(), 1);
        assert_eq!(cfg.bad_commands[0].pattern, "rm");
    }

    #[test]
    fn bad_pipeline_rules_parse_and_default_stage_patterns_to_glob() {
        let cfg = parse_repo_clud_config(
            r#"{"bad_pipelines":[{"id":"no-download-to-shell","stages":[{"match":"curl"},{"match":"^(?:ba)?sh$","match_mode":"regex"}],"replacement":"download then inspect","reason":"hidden code"}]}"#,
        )
        .expect("parses");
        assert_eq!(cfg.bad_pipelines.len(), 1);
        let rule = &cfg.bad_pipelines[0];
        assert_eq!(rule.stages.len(), 2);
        assert_eq!(rule.stages[0].match_mode, MatchMode::Glob);
        assert_eq!(rule.stages[1].match_mode, MatchMode::Regex);
    }

    #[test]
    fn malformed_pipeline_rule_is_skipped_without_losing_valid_rules() {
        let cfg = parse_repo_clud_config(
            r#"{"bad_pipelines":[{"stages":[{"match":"curl"}],"replacement":"invalid"},{"stages":[{"match":"curl"},{"match":"sh"}],"replacement":"inspect"}]}"#,
        )
        .expect("top-level config parses");
        assert_eq!(cfg.bad_pipelines.len(), 1);
        assert_eq!(cfg.bad_pipelines[0].stages.len(), 2);
    }

    #[test]
    fn bad_pipelines_merge_and_dedupe_by_id_like_bad_commands() {
        let user = parse_raw_repo_clud_config(
            r#"{"bad_pipelines":[{"id":"shared","stages":[{"match":"wget"},{"match":"sh"}],"replacement":"user"},{"id":"user-only","stages":[{"match":"cat"},{"match":"sh"}],"replacement":"user"}]}"#,
        )
        .unwrap();
        let repo = parse_raw_repo_clud_config(
            r#"{"bad_pipelines":[{"id":"shared","stages":[{"match":"curl"},{"match":"sh"}],"replacement":"repo"}]}"#,
        )
        .unwrap();
        let merged = resolve(merge(repo, user));
        assert_eq!(merged.bad_pipelines.len(), 2);
        let shared = merged
            .bad_pipelines
            .iter()
            .find(|rule| rule.id.as_deref() == Some("shared"))
            .unwrap();
        assert_eq!(shared.replacement, "repo");
    }

    #[test]
    fn bad_pipelines_alone_count_as_an_activation_directive() {
        let raw = parse_raw_repo_clud_config(
            r#"{"bad_pipelines":[{"stages":[{"match":"curl"},{"match":"sh"}],"replacement":"inspect"}]}"#,
        )
        .unwrap();
        assert!(has_directive(&raw));
    }

    #[test]
    fn unsupported_wrapper_skips_only_the_malformed_rule() {
        let cfg = parse_repo_clud_config(
            r#"{"bad_commands":[{"match":"rm","through_wrappers":["mystery"],"replacement":"bad"},{"match":"git","replacement":"good"}]}"#,
        )
        .unwrap();
        assert_eq!(cfg.bad_commands.len(), 1);
        assert_eq!(cfg.bad_commands[0].pattern, "git");
    }

    #[test]
    fn typoed_or_empty_argument_matcher_skips_the_containing_rule() {
        for arguments in [
            r#"{"ayn":["--force"]}"#,
            r#"{}"#,
            r#"{"any_of":[{}]}"#,
            r#"{"any":[]}"#,
        ] {
            let json = format!(
                r#"{{"bad_commands":[{{"match":"git","arguments":{arguments},"replacement":"bad"}},{{"match":"rm","replacement":"good"}}]}}"#
            );
            let cfg = parse_repo_clud_config(&json).unwrap();
            assert_eq!(cfg.bad_commands.len(), 1, "{arguments}");
            assert_eq!(cfg.bad_commands[0].pattern, "rm", "{arguments}");
        }
    }

    #[test]
    fn bad_commands_empty_array_is_valid_noop() {
        let cfg = parse_repo_clud_config(r#"{"bad_commands":[]}"#).expect("parses");
        assert!(cfg.bad_commands.is_empty());
    }

    #[test]
    fn bad_commands_absent_key_is_valid_noop() {
        let cfg = parse_repo_clud_config(r#"{"rust":{"use_soldr":true}}"#).expect("parses");
        assert!(cfg.bad_commands.is_empty());
    }

    #[test]
    fn bad_commands_concatenates_user_and_repo_not_override() {
        let user = parse_raw_repo_clud_config(
            r#"{"bad_commands":[{"id":"user-rule","match":"foo","replacement":"bar"}]}"#,
        )
        .unwrap();
        let repo = parse_raw_repo_clud_config(
            r#"{"bad_commands":[{"id":"repo-rule","match":"baz","replacement":"qux"}]}"#,
        )
        .unwrap();
        let merged = resolve(merge(repo, user));
        let ids: Vec<_> = merged
            .bad_commands
            .iter()
            .filter_map(|r| r.id.as_deref())
            .collect();
        assert!(ids.contains(&"user-rule"));
        assert!(ids.contains(&"repo-rule"));
        assert_eq!(merged.bad_commands.len(), 2);
    }

    #[test]
    fn bad_commands_dedupes_by_id_repo_wins() {
        let user = parse_raw_repo_clud_config(
            r#"{"bad_commands":[{"id":"shared","match":"user-pattern","replacement":"user-fix"}]}"#,
        )
        .unwrap();
        let repo = parse_raw_repo_clud_config(
            r#"{"bad_commands":[{"id":"shared","match":"repo-pattern","replacement":"repo-fix"}]}"#,
        )
        .unwrap();
        let merged = resolve(merge(repo, user));
        assert_eq!(merged.bad_commands.len(), 1);
        assert_eq!(merged.bad_commands[0].pattern, "repo-pattern");
    }

    #[test]
    fn bad_commands_rules_without_id_never_dedupe() {
        let user =
            parse_raw_repo_clud_config(r#"{"bad_commands":[{"match":"same","replacement":"a"}]}"#)
                .unwrap();
        let repo =
            parse_raw_repo_clud_config(r#"{"bad_commands":[{"match":"same","replacement":"b"}]}"#)
                .unwrap();
        let merged = resolve(merge(repo, user));
        assert_eq!(merged.bad_commands.len(), 2);
    }

    #[test]
    fn has_directive_true_for_bad_commands_only() {
        let tmp = TempDir::new().unwrap();
        let home = tmp.path();
        write_settings(
            home,
            r#"{"bad_commands":[{"match":"playwright","replacement":"npm run test:integration"}]}"#,
        );
        let raw = read_and_parse_raw(&home.join(".clud").join("settings.json"), "user-level")
            .expect("parses");
        assert!(has_directive(&raw));
    }

    #[test]
    fn has_directive_true_for_rust_only_still_works() {
        let raw = parse_raw_repo_clud_config(r#"{"rust":{"use_soldr":true}}"#).unwrap();
        assert!(has_directive(&raw));
    }

    #[test]
    fn has_directive_false_for_empty_bad_commands_and_no_rust() {
        let raw = parse_raw_repo_clud_config(r#"{"bad_commands":[]}"#).unwrap();
        assert!(!has_directive(&raw));
    }

    #[test]
    fn malformed_rule_missing_required_field_warns_and_skips() {
        let cfg = parse_repo_clud_config(
            r#"{"bad_commands":[{"match":"playwright"},{"match":"cypress","replacement":"npm run test:e2e"}]}"#,
        )
        .expect("parses");
        assert_eq!(cfg.bad_commands.len(), 1);
        assert_eq!(cfg.bad_commands[0].pattern, "cypress");
    }

    #[test]
    fn malformed_rule_wrong_json_type_warns_and_skips() {
        let cfg = parse_repo_clud_config(
            r#"{"bad_commands":[{"match":123,"replacement":"npm run test:integration"}]}"#,
        )
        .expect("parses");
        assert!(cfg.bad_commands.is_empty());
    }

    #[test]
    fn malformed_glob_pattern_warns_and_skips() {
        let cfg = parse_repo_clud_config(
            r#"{"bad_commands":[{"match":"play[wright","replacement":"npm run test:integration"}]}"#,
        )
        .expect("parses");
        assert!(cfg.bad_commands.is_empty());
    }

    #[test]
    fn malformed_regex_pattern_warns_and_skips() {
        let cfg = parse_repo_clud_config(
            r#"{"bad_commands":[{"match":"play(wright","match_mode":"regex","replacement":"npm run test:integration"}]}"#,
        )
        .expect("parses");
        assert!(cfg.bad_commands.is_empty());
    }

    #[test]
    fn compile_match_pattern_glob_is_whole_token_exact() {
        let re = compile_match_pattern("play", MatchMode::Glob).unwrap();
        assert!(re.is_match("play"));
        assert!(!re.is_match("playwright"));
        assert!(!re.is_match("playlist-gen"));
    }

    #[test]
    fn compile_match_pattern_glob_wildcard_matches_family() {
        let re = compile_match_pattern("*-e2e-runner", MatchMode::Glob).unwrap();
        assert!(re.is_match("legacy-e2e-runner"));
        assert!(re.is_match("other-e2e-runner"));
        assert!(!re.is_match("e2e-runner-legacy"));
    }

    #[test]
    fn compile_match_pattern_regex_mode_full_token_anchored() {
        let re = compile_match_pattern("^(playwright|pw-cli)$", MatchMode::Regex).unwrap();
        assert!(re.is_match("playwright"));
        assert!(re.is_match("pw-cli"));
        assert!(!re.is_match("playwrightish"));
    }

    #[test]
    fn compile_match_pattern_is_case_insensitive() {
        let re = compile_match_pattern("playwright", MatchMode::Glob).unwrap();
        assert!(re.is_match("PLAYWRIGHT"));
        assert!(re.is_match("Playwright"));
    }
}
