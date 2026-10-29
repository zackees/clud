//! JSON-path payload rules for a gateway request body.
//!
//! Ports CLIProxyAPI's `requests.payload` (`config.example.yaml:266-334`,
//! applied by its request pipeline): an operator can rewrite the upstream
//! request body per model. Three rule kinds, applied in this order:
//!
//! * `default` — set a parameter only when the caller left it out;
//! * `override` — always set it, replacing whatever the caller sent;
//! * `filter` — remove the parameter.
//!
//! A rule selects on a model name (`*` wildcards) and optionally on the
//! protocol clud is serving that request on. Parameters are addressed with
//! dotted JSON paths (`thinking.budget_tokens`, `messages.0.content`).
//!
//! clud had no such seam: a gateway launch forwarded the harness bytes
//! unchanged, so A/B probing of one parameter meant editing the harness. Rules
//! are read from the JSON file named by [`RULES_ENV`], re-read when the file
//! changes, and applied to `POST /v1/messages` before a route is chosen.
//!
//! Deliberately NOT ported in this slice (see `FEATURES.json` sub-rows):
//! gjson query syntax (`tools.#(name=="x")`), the header/`match`/`not-match`/
//! `exist` conditions, the `*-raw` variants, the OpenAI-shaped ingress path,
//! and a `~/.clud/settings.json` owner for the rules.
//!
//! A rule that cannot be applied never fails a client request: the payload is
//! forwarded as far as the good rules got it and the problem is reported to
//! the bridge log by [`apply`]'s [`Outcome::problems`].

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::SystemTime;

use serde::Deserialize;
use serde_json::{Map, Value};

/// Points at the JSON rules file. Unset means "no rules".
pub const RULES_ENV: &str = "CLUD_PAYLOAD_RULES";

/// A rules file larger than this is refused rather than parsed: the file is a
/// hand-edited switch, not a data feed.
pub const MAX_RULES_BYTES: u64 = 256 * 1024;

/// One `default`/`override` rule: which models it selects and what it sets.
#[derive(Debug, Clone, Default, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct SetRule {
    /// Empty selects every model.
    #[serde(default)]
    pub models: Vec<ModelMatch>,
    /// Dotted JSON path -> the value to set, verbatim.
    #[serde(default)]
    pub params: Map<String, Value>,
}

/// One `filter` rule: which models it selects and which paths it removes.
#[derive(Debug, Clone, Default, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct FilterRule {
    /// Empty selects every model.
    #[serde(default)]
    pub models: Vec<ModelMatch>,
    /// Dotted JSON paths to remove.
    #[serde(default)]
    pub params: Vec<String>,
}

/// A model selector: a name with `*` wildcards, optionally narrowed to one
/// protocol.
#[derive(Debug, Clone, Default, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct ModelMatch {
    pub name: String,
    /// Matched against the protocol clud serves the request on (`claude` for
    /// `POST /v1/messages`). Absent matches every protocol.
    #[serde(default)]
    pub protocol: Option<String>,
}

/// The whole rules document. Every kind is optional, so `{}` is a valid file
/// that changes nothing.
#[derive(Debug, Clone, Default, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct PayloadRules {
    #[serde(default)]
    pub default: Vec<SetRule>,
    #[serde(default, rename = "override")]
    pub override_rules: Vec<SetRule>,
    #[serde(default)]
    pub filter: Vec<FilterRule>,
}

impl PayloadRules {
    /// True when no rule could ever fire, so a caller can skip the work.
    pub fn is_empty(&self) -> bool {
        self.default.is_empty() && self.override_rules.is_empty() && self.filter.is_empty()
    }
}

/// What [`apply`] did, and what it could not do.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Outcome {
    /// Parameters set because the caller left them out.
    pub defaulted: usize,
    /// Parameters overwritten.
    pub overridden: usize,
    /// Paths removed.
    pub filtered: usize,
    /// Rule entries that could not be applied. The payload keeps every change
    /// that landed; these are for the operator, not for the client.
    pub problems: Vec<String>,
}

impl Outcome {
    /// True when the payload differs from what the caller sent.
    pub fn changed(&self) -> bool {
        self.defaulted + self.overridden + self.filtered > 0
    }
}

fn selected(models: &[ModelMatch], model: &str, protocol: &str) -> bool {
    if models.is_empty() {
        return true;
    }
    models.iter().any(|candidate| {
        let protocol_fits = candidate
            .protocol
            .as_deref()
            .is_none_or(|wanted| wanted.eq_ignore_ascii_case(protocol));
        protocol_fits && wildcard_matches(&candidate.name, model)
    })
}

/// Case-insensitive `*` glob, which is the whole wildcard language upstream
/// supports for model names.
pub fn wildcard_matches(pattern: &str, text: &str) -> bool {
    let pattern = pattern.to_ascii_lowercase();
    let text = text.to_ascii_lowercase();
    let (pattern, text) = (pattern.as_bytes(), text.as_bytes());
    let (mut p, mut t) = (0, 0);
    let (mut star, mut resume) = (None, 0);
    while t < text.len() {
        if p < pattern.len() && pattern[p] == b'*' {
            // Remember the most recent star: on a later mismatch, resume from
            // the byte after the previous attempt at it.
            star = Some(p);
            resume = t;
            p += 1;
        } else if p < pattern.len() && pattern[p] == text[t] {
            p += 1;
            t += 1;
        } else if let Some(at) = star {
            p = at + 1;
            resume += 1;
            t = resume;
        } else {
            return false;
        }
    }
    while p < pattern.len() && pattern[p] == b'*' {
        p += 1;
    }
    p == pattern.len()
}

fn segments(path: &str) -> Result<Vec<&str>, String> {
    if path.is_empty() {
        return Err("empty JSON path".to_string());
    }
    let parts: Vec<&str> = path.split('.').collect();
    if parts.iter().any(|part| part.is_empty()) {
        return Err(format!("`{path}` has an empty path segment"));
    }
    Ok(parts)
}

/// True when `path` resolves to a value that is present and not null. A null
/// counts as missing, which is what makes a `default` rule able to fill it.
pub fn path_present(payload: &Value, path: &str) -> bool {
    let Ok(parts) = segments(path) else {
        return false;
    };
    let mut current = payload;
    for part in parts {
        current = match current {
            Value::Object(map) => match map.get(part) {
                Some(next) => next,
                None => return false,
            },
            Value::Array(items) => match part.parse::<usize>().ok().and_then(|i| items.get(i)) {
                Some(next) => next,
                None => return false,
            },
            _ => return false,
        };
    }
    !current.is_null()
}

fn set_in(current: &mut Value, parts: &[&str], value: Value) -> Result<(), String> {
    let Some((head, rest)) = parts.split_first() else {
        return Ok(());
    };
    if rest.is_empty() {
        match current {
            Value::Object(map) => {
                map.insert((*head).to_string(), value);
                return Ok(());
            }
            Value::Array(items) => {
                let index = array_index(head, items.len())?;
                items[index] = value;
                return Ok(());
            }
            _ => return Err(format!("`{head}` has no containing object")),
        }
    }
    match current {
        Value::Object(map) => {
            let entry = map
                .entry((*head).to_string())
                .or_insert_with(|| Value::Object(Map::new()));
            if entry.is_null() {
                *entry = Value::Object(Map::new());
            }
            set_in(entry, rest, value)
        }
        Value::Array(items) => {
            let len = items.len();
            let index = array_index(head, len)?;
            set_in(&mut items[index], rest, value)
        }
        _ => Err(format!("`{head}` has no containing object")),
    }
}

fn array_index(head: &str, len: usize) -> Result<usize, String> {
    let index = head
        .parse::<usize>()
        .map_err(|_| format!("`{head}` is not an array index"))?;
    if index >= len {
        // Growing an array would invent elements nobody asked for.
        return Err(format!("array index {index} is past the end ({len} items)"));
    }
    Ok(index)
}

fn remove_in(current: &mut Value, parts: &[&str]) -> bool {
    let Some((head, rest)) = parts.split_first() else {
        return false;
    };
    match current {
        Value::Object(map) if rest.is_empty() => map.remove(*head).is_some(),
        Value::Object(map) => map
            .get_mut(*head)
            .is_some_and(|child| remove_in(child, rest)),
        Value::Array(items) => match head.parse::<usize>() {
            Ok(index) if index < items.len() => {
                if rest.is_empty() {
                    items.remove(index);
                    true
                } else {
                    remove_in(&mut items[index], rest)
                }
            }
            _ => false,
        },
        _ => false,
    }
}

/// Apply every rule that selects `model` on `protocol`, in `default` ->
/// `override` -> `filter` order, to `payload`.
///
/// `payload` must be the JSON object clud is about to forward. A rule entry
/// that cannot be applied is recorded in [`Outcome::problems`] and skipped;
/// nothing here can fail the request.
pub fn apply(rules: &PayloadRules, model: &str, protocol: &str, payload: &mut Value) -> Outcome {
    let mut outcome = Outcome::default();
    for rule in &rules.default {
        if !selected(&rule.models, model, protocol) {
            continue;
        }
        for (path, value) in &rule.params {
            if path_present(payload, path) {
                continue;
            }
            match segments(path).and_then(|parts| set_in(payload, &parts, value.clone())) {
                Ok(()) => outcome.defaulted += 1,
                Err(reason) => outcome.problems.push(format!("default {path}: {reason}")),
            }
        }
    }
    for rule in &rules.override_rules {
        if !selected(&rule.models, model, protocol) {
            continue;
        }
        for (path, value) in &rule.params {
            match segments(path).and_then(|parts| set_in(payload, &parts, value.clone())) {
                Ok(()) => outcome.overridden += 1,
                Err(reason) => outcome.problems.push(format!("override {path}: {reason}")),
            }
        }
    }
    for rule in &rules.filter {
        if !selected(&rule.models, model, protocol) {
            continue;
        }
        for path in &rule.params {
            match segments(path) {
                Ok(parts) if remove_in(payload, &parts) => outcome.filtered += 1,
                Ok(_) => {}
                Err(reason) => outcome.problems.push(format!("filter {path}: {reason}")),
            }
        }
    }
    outcome
}

/// Parse a rules document. Refuses a non-object and any unknown key, so a typo
/// (`overide`) is reported instead of silently doing nothing.
pub fn parse(bytes: &[u8]) -> Result<PayloadRules, String> {
    let value: Value =
        serde_json::from_slice(bytes).map_err(|error| format!("rules are not JSON: {error}"))?;
    // serde would happily read a struct out of a positional JSON array; a rules
    // document is an object or nothing.
    if !value.is_object() {
        return Err("a rules document is a JSON object".to_string());
    }
    let rules: PayloadRules =
        serde_json::from_value(value).map_err(|error| format!("rules do not parse: {error}"))?;
    validate(&rules)?;
    Ok(rules)
}

fn validate(rules: &PayloadRules) -> Result<(), String> {
    let set_rules = rules.default.iter().chain(&rules.override_rules);
    let filter_models = rules.filter.iter().flat_map(|rule| rule.models.iter());
    for model in set_rules
        .flat_map(|rule| rule.models.iter())
        .chain(filter_models)
    {
        if model.name.trim().is_empty() {
            return Err("a model selector has an empty name".to_string());
        }
        if model
            .protocol
            .as_ref()
            .is_some_and(|value| value.trim().is_empty())
        {
            return Err("a model selector has an empty protocol".to_string());
        }
    }
    // Only `default`/`override` paths must be well formed here: a filter path
    // that is not well formed removes nothing, and is reported per request.
    for rule in rules.default.iter().chain(&rules.override_rules) {
        for path in rule.params.keys() {
            segments(path).map_err(|reason| format!("rules: {reason}"))?;
        }
    }
    Ok(())
}

/// Read and parse one rules file.
pub fn from_path(path: &Path) -> Result<PayloadRules, String> {
    let metadata = std::fs::metadata(path)
        .map_err(|error| format!("{} is unreadable: {error}", path.display()))?;
    if metadata.len() > MAX_RULES_BYTES {
        return Err(format!(
            "{} is {} bytes, over the {MAX_RULES_BYTES}-byte limit",
            path.display(),
            metadata.len()
        ));
    }
    let bytes = std::fs::read(path)
        .map_err(|error| format!("{} is unreadable: {error}", path.display()))?;
    parse(&bytes)
}

/// The rules [`active`] last parsed, keyed by what it observed on disk.
struct Cached {
    path: PathBuf,
    modified: Option<SystemTime>,
    len: u64,
    rules: Arc<PayloadRules>,
}

static CACHE: OnceLock<Mutex<Option<Cached>>> = OnceLock::new();

fn fingerprint(path: &Path) -> (Option<SystemTime>, u64) {
    match std::fs::metadata(path) {
        Ok(metadata) => (metadata.modified().ok(), metadata.len()),
        Err(_) => (None, 0),
    }
}

/// The rules named by [`RULES_ENV`], re-read whenever the file changes.
///
/// `None` means "no rules": the variable is unset, or the file could not be
/// read or parsed. A parse failure is reported on stderr once per change rather
/// than per request, and the previous rules stay out of the way — forwarding
/// the caller's own bytes is the safe behavior.
pub fn active() -> Option<Arc<PayloadRules>> {
    let path = PathBuf::from(std::env::var_os(RULES_ENV)?);
    let (modified, len) = fingerprint(&path);
    let cache = CACHE.get_or_init(|| Mutex::new(None));
    let mut guard = cache.lock().ok()?;
    if let Some(cached) = guard.as_ref() {
        if cached.path == path && cached.modified == modified && cached.len == len {
            return Some(Arc::clone(&cached.rules));
        }
    }
    if modified.is_none() {
        eprintln!(
            "payload rules: {} is unreadable; forwarding request bodies unchanged",
            path.display()
        );
        *guard = None;
        return None;
    }
    match from_path(&path) {
        Ok(rules) => {
            let rules = Arc::new(rules);
            *guard = Some(Cached {
                path,
                modified,
                len,
                rules: Arc::clone(&rules),
            });
            Some(rules)
        }
        Err(reason) => {
            eprintln!("payload rules: {reason}; forwarding request bodies unchanged");
            *guard = None;
            None
        }
    }
}

/// Apply the active rules to one request body, returning the body to forward.
///
/// The bytes are returned untouched when no rule fired, so a launch without
/// [`RULES_ENV`] is byte-for-byte the pre-existing behavior.
pub fn rewrite_body(
    model: &str,
    protocol: &str,
    body: Vec<u8>,
    payload: &mut Value,
) -> (Vec<u8>, Outcome) {
    let Some(rules) = active() else {
        return (body, Outcome::default());
    };
    if rules.is_empty() {
        return (body, Outcome::default());
    }
    let outcome = apply(&rules, model, protocol, payload);
    if !outcome.changed() {
        return (body, outcome);
    }
    match serde_json::to_vec(payload) {
        Ok(rewritten) => (rewritten, outcome),
        Err(reason) => {
            let mut outcome = outcome;
            outcome
                .problems
                .push(format!("re-serialize failed: {reason}"));
            (body, outcome)
        }
    }
}

/// `clud __apply-payload-rules [--rules <path>] [--model M] [--protocol P]` —
/// the process seam behind the Python subprocess test. Reads
/// `{"rules": {...}, "payload": {...}}` on stdin (or the rules from `--rules`
/// and just the payload on stdin) and writes `{"payload": ..., "outcome": ...}`.
pub fn run_apply_stdin(rules_path: Option<&Path>, model: &str, protocol: &str) -> i32 {
    use std::io::{Read, Write};

    let mut body = Vec::new();
    if std::io::stdin().read_to_end(&mut body).is_err() {
        eprintln!("could not read stdin");
        return 2;
    }
    let document: Value = match serde_json::from_slice(&body) {
        Ok(document) => document,
        Err(error) => {
            eprintln!("stdin is not JSON: {error}");
            return 2;
        }
    };
    let rules = match rules_path {
        Some(path) => match from_path(path) {
            Ok(rules) => rules,
            Err(reason) => {
                eprintln!("{reason}");
                return 2;
            }
        },
        None => match document.get("rules") {
            Some(rules) => match serde_json::from_value::<PayloadRules>(rules.clone()) {
                Ok(rules) => rules,
                Err(error) => {
                    eprintln!("rules do not parse: {error}");
                    return 2;
                }
            },
            None => {
                eprintln!("stdin carries no `rules` object");
                return 2;
            }
        },
    };
    let mut payload = document.get("payload").cloned().unwrap_or(Value::Null);
    let outcome = apply(&rules, model, protocol, &mut payload);
    let rendered = serde_json::json!({
        "payload": payload,
        "outcome": {
            "defaulted": outcome.defaulted,
            "overridden": outcome.overridden,
            "filtered": outcome.filtered,
            "problems": outcome.problems,
            "changed": outcome.changed(),
        },
    });
    let mut bytes = serde_json::to_vec(&rendered).unwrap_or_default();
    bytes.push(b'\n');
    match std::io::stdout().write_all(&bytes) {
        Ok(()) => 0,
        Err(error) => {
            eprintln!("could not write the result: {error}");
            2
        }
    }
}

#[cfg(test)]
#[path = "payload_rules_tests.rs"]
mod tests;
