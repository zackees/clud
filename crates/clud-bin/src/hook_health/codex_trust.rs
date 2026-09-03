use std::collections::HashSet;
use std::fs;
use std::io;
use std::path::Path;

use toml_edit::{table, value, DocumentMut, Item};

use super::{CODEX_PRE_TOOL_USE_STATE, CURRENT_CODEX_HOOKS_FEATURE, LEGACY_CODEX_HOOKS_FEATURE};

pub(in crate::hook_health) fn add_codex_project_trust(
    config_path: &Path,
    project_key: &str,
) -> io::Result<()> {
    let text = fs::read_to_string(config_path).unwrap_or_default();
    let mut document = if text.trim().is_empty() {
        DocumentMut::new()
    } else {
        text.parse::<DocumentMut>()
            .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error.to_string()))?
    };
    if document.get("projects").is_none() {
        document["projects"] = table();
    }
    if document["projects"].get(project_key).is_none() {
        document["projects"][project_key] = table();
    }
    document["projects"][project_key]["trust_level"] = value("trusted");
    if let Some(parent) = config_path.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::write(config_path, document.to_string())
}

pub(in crate::hook_health) fn migrate_codex_hooks_feature_flag(
    config_path: &Path,
) -> io::Result<()> {
    let text = fs::read_to_string(config_path)?;
    let mut document = text
        .parse::<DocumentMut>()
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error.to_string()))?;

    let Some(legacy_value) = document
        .get("features")
        .and_then(|item| item.get(LEGACY_CODEX_HOOKS_FEATURE))
        .cloned()
    else {
        return Ok(());
    };

    if document["features"]
        .get(CURRENT_CODEX_HOOKS_FEATURE)
        .is_none()
    {
        document["features"][CURRENT_CODEX_HOOKS_FEATURE] = legacy_value;
    }
    if let Some(features) = document["features"].as_table_mut() {
        features.remove(LEGACY_CODEX_HOOKS_FEATURE);
    }

    fs::write(config_path, document.to_string())
}

pub(in crate::hook_health) fn codex_hook_state_key(
    source_path: &Path,
    group_index: usize,
    handler_index: usize,
) -> String {
    format!(
        "{}:{CODEX_PRE_TOOL_USE_STATE}:{group_index}:{handler_index}",
        source_path.to_string_lossy()
    )
}

pub(in crate::hook_health) fn has_trusted_hook_state(
    keys: &HashSet<String>,
    expected: &str,
) -> bool {
    keys.iter()
        .any(|key| key == expected || normalized_state_key(key) == normalized_state_key(expected))
}

pub(in crate::hook_health) fn normalized_state_key(key: &str) -> String {
    let Some((path, suffix)) = key.split_once(&format!(":{CODEX_PRE_TOOL_USE_STATE}:")) else {
        return key.to_string();
    };
    format!(
        "{}:{CODEX_PRE_TOOL_USE_STATE}:{suffix}",
        normalize_project_path_key(path)
    )
}

pub fn codex_project_key(path: &Path) -> String {
    normalize_project_path_key(&path.to_string_lossy())
}

pub(in crate::hook_health) fn normalize_project_path_key(raw: &str) -> String {
    let mut s = raw.replace('/', "\\");
    if let Some(stripped) = s.strip_prefix(r"\\?\") {
        s = stripped.to_string();
    }
    if looks_like_windows_path(&s) {
        s.to_ascii_lowercase()
    } else {
        raw.to_string()
    }
}

pub(in crate::hook_health) fn looks_like_windows_path(path: &str) -> bool {
    let bytes = path.as_bytes();
    bytes.len() >= 2 && bytes[1] == b':' || path.starts_with(r"\\?\")
}

pub(in crate::hook_health) fn is_extended_key_for(key: &str, canonical_key: &str) -> bool {
    let Some(stripped) = key.strip_prefix(r"\\?\") else {
        return false;
    };
    normalize_project_path_key(key) == canonical_key
        || crate::path_norm::slash_separators(stripped) == canonical_key
}

/// Whether the repo at `repo_root` is trusted in the codex project trust
/// table — `[projects."<key>"] trust_level = "trusted"` in
/// `~/.codex/config.toml`, where `<key>` is the normalized project path
/// (see [`codex_project_key`]). `false` when the config is missing or
/// unparsable.
///
/// This is the boundary clud honors before running a sub-repo's own hooks in
/// a codex session (zackees/clud#967 Phase 4): codex itself would not run a
/// repo's hooks until the project is trusted here, and clud does not go
/// around codex's model.
pub fn codex_project_trusted(repo_root: &Path, home: &Path) -> bool {
    let config_path = home.join(".codex").join("config.toml");
    let canonical_key = codex_project_key(repo_root);
    let Ok(text) = fs::read_to_string(&config_path) else {
        return false;
    };
    let Ok(document) = text.parse::<DocumentMut>() else {
        return false;
    };
    let Some(projects) = document.get("projects").and_then(Item::as_table) else {
        return false;
    };
    // Bound to a local: the iterator temporary borrows `document`, which must
    // outlive the block's tail expression.
    let trusted = projects.iter().any(|(key, item)| {
        let trusted = item
            .get("trust_level")
            .and_then(Item::as_str)
            .map(|level| level.eq_ignore_ascii_case("trusted"))
            .unwrap_or(false);
        trusted && (key == canonical_key || is_extended_key_for(key, &canonical_key))
    });
    trusted
}
