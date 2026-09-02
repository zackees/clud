//! The trust allowlist for foreign checkouts' hooks (zackees/clud#967 Phase 4,
//! #966 D9).
//!
//! Running a just-cloned repo's hook scripts is arbitrary code execution, and
//! cloning at the user's request is provenance, not consent. So an `extern`
//! root's own hooks stay off until the user names that checkout with
//! `clud extern trust <name>`.
//!
//! The allow entry is recorded in the parent's `.clud/settings.local.json`
//! — gitignored, so trust never travels through version control — keyed by
//! the checkout's directory name **and** its origin remote URL, so a re-clone
//! from a different remote does not inherit the trust.
//!
//! ```json
//! {
//!   "hook_trust": {
//!     "extern": [
//!       { "name": "running-process", "origin": "https://github.com/zackees/running-process.git" }
//!     ]
//!   }
//! }
//! ```
//!
//! The key lives in `settings.local.json` only. The shared
//! `.clud/settings.json` is deliberately never consulted for it: a trust
//! allowlist committed to version control would follow the checkout to every
//! machine, which is exactly what "per-machine trust" is supposed to prevent.
//! (`repo_clud_config` ignores unknown top-level keys, so the section also
//! does not disturb the settings parser.)

use serde_json::Value;
use std::path::{Path, PathBuf};

/// The JSON key under which the trust allowlist lives.
pub const HOOK_TRUST_KEY: &str = "hook_trust";

/// The sub-key holding the extern-checkout entries.
pub const EXTERN_ENTRIES_KEY: &str = "extern";

/// One allow entry: a checkout name and the origin URL it was trusted with.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TrustEntry {
    pub name: String,
    pub origin: String,
}

/// The parsed allowlist.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TrustStore {
    pub extern_entries: Vec<TrustEntry>,
}

impl TrustStore {
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.extern_entries.is_empty()
    }
}

/// The file the allowlist lives in: `<repo_root>/.clud/settings.local.json`.
#[must_use]
pub fn trust_file(repo_root: &Path) -> PathBuf {
    repo_root
        .join(".clud")
        .join(crate::repo_clud_config::LOCAL_SETTINGS_FILE)
}

/// Read the allowlist for `repo_root`. Lenient like every other clud config
/// read: a file that is missing, unreadable, or unparsable yields an empty
/// store with a stderr note, never a failure — a broken trust file must not
/// wedge a tool call.
#[must_use]
pub fn load(repo_root: &Path) -> TrustStore {
    let path = trust_file(repo_root);
    let Ok(text) = std::fs::read_to_string(&path) else {
        return TrustStore::default();
    };
    match parse(&text) {
        Ok(store) => store,
        Err(reason) => {
            eprintln!(
                "clud: failed to parse trust entries in {}: {reason}; ignoring them",
                path.display()
            );
            TrustStore::default()
        }
    }
}

/// Parse the trust section out of a `settings.local.json` body.
///
/// `Err` only when the document is not usable JSON. A `hook_trust` section
/// with a non-array `extern` value is skipped with a warning, mirroring the
/// lenient posture of `repo_clud_config`.
pub fn parse(text: &str) -> Result<TrustStore, String> {
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return Ok(TrustStore::default());
    }
    let root: Value = serde_json::from_str(trimmed).map_err(|error| error.to_string())?;
    let Some(object) = root.as_object() else {
        return Err("settings.local.json must contain a JSON object".to_string());
    };
    let Some(trust) = object.get(HOOK_TRUST_KEY) else {
        return Ok(TrustStore::default());
    };
    let Some(trust_object) = trust.as_object() else {
        eprintln!("clud: ignoring hook_trust: must be an object");
        return Ok(TrustStore::default());
    };
    let Some(entries) = trust_object.get(EXTERN_ENTRIES_KEY) else {
        return Ok(TrustStore::default());
    };
    let Some(entries) = entries.as_array() else {
        eprintln!("clud: ignoring hook_trust.extern: must be an array");
        return Ok(TrustStore::default());
    };
    let mut store = TrustStore::default();
    for entry in entries {
        let Some(entry) = entry.as_object() else {
            continue;
        };
        let Some(name) = entry
            .get("name")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|name| !name.is_empty())
        else {
            continue;
        };
        let Some(origin) = entry
            .get("origin")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|origin| !origin.is_empty())
        else {
            continue;
        };
        if !store
            .extern_entries
            .iter()
            .any(|seen| seen.name == name && seen.origin == origin)
        {
            store.extern_entries.push(TrustEntry {
                name: name.to_string(),
                origin: origin.to_string(),
            });
        }
    }
    store.extern_entries.sort_by(|left, right| {
        left.name
            .cmp(&right.name)
            .then_with(|| left.origin.cmp(&right.origin))
    });
    Ok(store)
}

/// Whether the allowlist trusts a checkout with this name, last seen at
/// `origin`.
///
/// An entry matches when the name agrees **and** the current origin agrees.
/// A checkout with no readable origin matches by name alone: it cannot be
/// re-cloned from a different remote, which is the only thing the origin key
/// exists to defend against.
#[must_use]
pub fn is_trusted(store: &TrustStore, name: &str, origin: Option<&str>) -> bool {
    store.extern_entries.iter().any(|entry| {
        entry.name == name
            && match origin {
                Some(origin) => entry.origin == origin,
                None => true,
            }
    })
}

/// Record an allow entry for `name` + `origin` in the parent's
/// `settings.local.json`, preserving every other key in the file.
pub fn record(repo_root: &Path, name: &str, origin: &str) -> Result<(), String> {
    let path = trust_file(repo_root);
    let mut root: Value = match std::fs::read_to_string(&path) {
        Ok(text) if !text.trim().is_empty() => {
            serde_json::from_str(&text).map_err(|error| error.to_string())?
        }
        _ => Value::Object(serde_json::Map::new()),
    };
    let trust = root
        .as_object_mut()
        .ok_or_else(|| "settings.local.json must contain a JSON object".to_string())?
        .entry(HOOK_TRUST_KEY.to_string())
        .or_insert_with(|| Value::Object(serde_json::Map::new()));
    let entries = trust
        .as_object_mut()
        .ok_or_else(|| "hook_trust must be an object".to_string())?
        .entry(EXTERN_ENTRIES_KEY.to_string())
        .or_insert_with(|| Value::Array(Vec::new()));
    let entries = entries
        .as_array_mut()
        .ok_or_else(|| "hook_trust.extern must be an array".to_string())?;
    entries.retain(|entry| {
        entry
            .get("name")
            .and_then(Value::as_str)
            .map(|seen| seen != name)
            .unwrap_or(true)
    });
    entries.push(serde_json::json!({
        "name": name,
        "origin": origin,
    }));
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|error| format!("failed to create {}: {error}", parent.display()))?;
    }
    std::fs::write(&path, format!("{root:#}\n"))
        .map_err(|error| format!("failed to write {}: {error}", path.display()))
}

/// Remove the allow entry for `name`, whatever origin it carries.
///
/// Returns whether anything was removed.
pub fn revoke(repo_root: &Path, name: &str) -> Result<bool, String> {
    let path = trust_file(repo_root);
    let Ok(text) = std::fs::read_to_string(&path) else {
        return Ok(false);
    };
    let mut root: Value = serde_json::from_str(&text).map_err(|error| error.to_string())?;
    let Some(entries) = root
        .get_mut(HOOK_TRUST_KEY)
        .and_then(Value::as_object_mut)
        .and_then(|trust| trust.get_mut(EXTERN_ENTRIES_KEY))
        .and_then(Value::as_array_mut)
    else {
        return Ok(false);
    };
    let before = entries.len();
    entries.retain(|entry| {
        entry
            .get("name")
            .and_then(Value::as_str)
            .map(|seen| seen != name)
            .unwrap_or(true)
    });
    let removed = entries.len() != before;
    if removed {
        std::fs::write(&path, format!("{root:#}\n"))
            .map_err(|error| format!("failed to write {}: {error}", path.display()))?;
    }
    Ok(removed)
}

/// A checkout name is usable as a trust key and a directory lookup only when
/// it is a bare directory name — no path separators, no `.`/`..`.
#[must_use]
pub fn valid_name(name: &str) -> bool {
    !name.is_empty() && name != "." && name != ".." && !name.contains('/') && !name.contains('\\')
}

/// The `origin` remote URL of the git repository at `repo_root`, read from
/// its config file.
///
/// Read directly rather than through `git` so the trust check costs no
/// process spawn on the per-tool-call hot path. Handles a plain `.git/`
/// directory and the `.git` file a linked worktree uses (`gitdir: <path>`).
/// `None` when there is no repo, no `origin` remote, or the config cannot be
/// parsed — a checkout with no origin cannot be re-cloned from a different
/// remote, which is the case the origin key exists for.
#[must_use]
pub fn origin_of(repo_root: &Path) -> Option<String> {
    let git_dir = git_dir_of(repo_root)?;
    let config_path = git_dir.join("config");
    let text = std::fs::read_to_string(&config_path).ok()?;
    remote_origin_url(&text)
}

fn git_dir_of(repo_root: &Path) -> Option<PathBuf> {
    let dot_git = repo_root.join(".git");
    if dot_git.is_dir() {
        return Some(dot_git);
    }
    // A linked worktree: `.git` is a file whose first line is
    // `gitdir: <path>`, relative to the worktree root.
    if dot_git.is_file() {
        let text = std::fs::read_to_string(&dot_git).ok()?;
        let target = text.lines().next()?.strip_prefix("gitdir:")?.trim();
        let path = Path::new(target);
        return Some(if path.is_absolute() {
            path.to_path_buf()
        } else {
            repo_root.join(path)
        });
    }
    None
}

/// Extract `remote.origin.url` from a git config file body.
///
/// Handles both section spellings (`[remote "origin"]` and `[remote.origin]`)
/// case-insensitively, `url = <value>` and `url=<value>`, quoted values, and
/// a `url` line that continues a `[remote "origin"]` section. Not a full git
/// config parser — anything it cannot recognize is treated as absent, and the
/// consequence of that is at worst a name-only trust match.
fn remote_origin_url(text: &str) -> Option<String> {
    let mut in_origin_section = false;
    for raw_line in text.lines() {
        let line = raw_line.trim();
        if line.is_empty() || line.starts_with('#') || line.starts_with(';') {
            continue;
        }
        if line.starts_with('[') {
            in_origin_section = section_is_origin(line);
            continue;
        }
        if !in_origin_section {
            continue;
        }
        let (key, value) = line.split_once('=')?;
        if key.trim().eq_ignore_ascii_case("url") {
            let value = value.trim().trim_matches('"').to_string();
            if !value.is_empty() {
                return Some(value);
            }
        }
    }
    None
}

fn section_is_origin(header: &str) -> bool {
    let inner = header.trim_start_matches('[').trim_end_matches(']').trim();
    let Some((section, rest)) = inner.split_once(['.', ' ']) else {
        return inner.eq_ignore_ascii_case("remote");
    };
    if !section.eq_ignore_ascii_case("remote") {
        return false;
    }
    let sub = rest.trim().trim_matches('"');
    sub.eq_ignore_ascii_case("origin")
}

/// Where the checkout named `name` lives, if it exists under any of the
/// repo's known extern roots (sibling first, then the legacy in-tree one).
#[must_use]
pub fn extern_dir_for(parent: &Path, name: &str) -> Option<PathBuf> {
    if !valid_name(name) {
        return None;
    }
    crate::extern_root::known_roots(parent)
        .into_iter()
        .map(|root| root.join(name))
        .find(|path| path.is_dir())
}

#[cfg(test)]
#[path = "hook_trust_tests.rs"]
mod hook_trust_tests;
