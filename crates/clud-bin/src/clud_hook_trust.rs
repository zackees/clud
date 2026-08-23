//! Trust for a foreign repo's own hooks (zackees/clud#966 §7, #967 Phase 4).
//!
//! Running the hooks a repo declares is executing that repo's code. For the
//! repo the session belongs to, that is unremarkable. For a checkout the agent
//! cloned into `.extern-repos/` at some point during a task, it is arbitrary
//! code from a source nobody vetted — and clud having created the clone is
//! *provenance*, not consent to run its scripts on every Edit.
//!
//! So a root whose [`RootTrust`] is `RequiresGrant` gets its hooks skipped
//! until someone allows it, once, and the allow is recorded:
//!
//! - in the **parent's** `.clud/settings.local.json`, which is gitignored, so
//!   a decision made on one machine never travels through version control to
//!   another;
//! - keyed by **name *and* origin URL**, so deleting a checkout and cloning a
//!   different repo under the same directory name does not inherit the
//!   answer.
//!
//! Roots whose trust is `Implicit` — the parent, a declared child, a directory
//! the user named with `--add-dir` — need no gate: their existence in a file
//! the user wrote, or on the command line they typed, is the consent.
//!
//! This mirrors the harness's own per-folder workspace trust, which cannot
//! reach nested repos (anthropics/claude-code#88871), rather than working
//! around it.

use std::path::{Path, PathBuf};

use serde_json::Value;

use crate::clud_hook_roots::{HookRoot, RootTrust};

/// Where the parent records its answers, relative to the parent root.
pub const TRUST_FILE_REL: &[&str] = &[".clud", "settings.local.json"];

/// The key holding them.
pub const TRUST_KEY: &str = "extern_trust";

/// What a root's hooks are allowed to do right now.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TrustDecision {
    /// Run them.
    Allowed,
    /// Skip them, and tell the user how to change that.
    NeedsGrant,
}

/// Decide whether `root`'s own hooks may run, given the parent's records.
#[must_use]
pub fn decide(parent_root: &Path, root: &HookRoot) -> TrustDecision {
    if root.trust == RootTrust::Implicit {
        return TrustDecision::Allowed;
    }
    let Some(name) = root_name(&root.path) else {
        return TrustDecision::NeedsGrant;
    };
    let origin = origin_url(&root.path);
    if is_granted(parent_root, &name, origin.as_deref()) {
        TrustDecision::Allowed
    } else {
        TrustDecision::NeedsGrant
    }
}

/// Whether the parent has recorded a grant for `name` at `origin`.
///
/// A recorded origin that no longer matches means the directory now holds a
/// different repo, so the old answer does not apply.
#[must_use]
pub fn is_granted(parent_root: &Path, name: &str, origin: Option<&str>) -> bool {
    let Some(entry) = read_entry(parent_root, name) else {
        return false;
    };
    if entry.get("trusted").and_then(Value::as_bool) != Some(true) {
        return false;
    }
    match (entry.get("origin").and_then(Value::as_str), origin) {
        // Recorded without an origin: the weakest form, but still an explicit
        // answer about this name.
        (None, _) => true,
        (Some(recorded), Some(actual)) => same_origin(recorded, actual),
        // Recorded against an origin the checkout no longer has.
        (Some(_), None) => false,
    }
}

fn read_entry(parent_root: &Path, name: &str) -> Option<Value> {
    let mut path = parent_root.to_path_buf();
    for segment in TRUST_FILE_REL {
        path.push(segment);
    }
    let text = std::fs::read_to_string(&path).ok()?;
    let document: Value = serde_json::from_str(&text).ok()?;
    document.get(TRUST_KEY)?.get(name).cloned()
}

/// Compare origins ignoring the cosmetic differences between spellings of the
/// same remote.
fn same_origin(left: &str, right: &str) -> bool {
    normalize_origin(left) == normalize_origin(right)
}

fn normalize_origin(raw: &str) -> String {
    let trimmed = raw.trim().trim_end_matches('/');
    let trimmed = trimmed.strip_suffix(".git").unwrap_or(trimmed);
    trimmed.to_ascii_lowercase()
}

/// The directory name a grant is keyed on.
#[must_use]
pub fn root_name(path: &Path) -> Option<String> {
    path.file_name()
        .map(|name| name.to_string_lossy().into_owned())
}

/// The checkout's `origin` remote, read straight from `.git/config`.
///
/// Parsed rather than shelled out to: this runs on the hook path, where a
/// `git` spawn per tool call is exactly the cost the guard is supposed to
/// avoid. A worktree's `.git` is a file pointing elsewhere, which this does
/// not follow — an unreadable origin simply means the grant has to be
/// recorded without one.
#[must_use]
pub fn origin_url(repo: &Path) -> Option<String> {
    let text = std::fs::read_to_string(repo.join(".git").join("config")).ok()?;
    let mut in_origin = false;
    for line in text.lines() {
        let line = line.trim();
        if line.starts_with('[') {
            in_origin = line.replace(char::is_whitespace, "") == "[remote\"origin\"]";
            continue;
        }
        if !in_origin {
            continue;
        }
        if let Some((key, value)) = line.split_once('=') {
            if key.trim() == "url" {
                let value = value.trim();
                if !value.is_empty() {
                    return Some(value.to_string());
                }
            }
        }
    }
    None
}

/// The one-time notice shown when a root's hooks are held back.
#[must_use]
pub fn grant_notice(parent_root: &Path, root: &HookRoot) -> String {
    let name = root_name(&root.path).unwrap_or_else(|| root.path.to_string_lossy().into_owned());
    let origin = origin_url(&root.path);
    let mut path = parent_root.to_path_buf();
    for segment in TRUST_FILE_REL {
        path.push(segment);
    }
    let origin_line = match &origin {
        Some(origin) => format!("\"origin\": {}, ", Value::String(origin.clone())),
        None => String::new(),
    };
    format!(
        "[clud] {name} declares its own hooks, which clud is not running: it is a checkout made \
         during this session, so its scripts are code nobody has vetted. To allow them, add this \
         to {} (gitignored, so it stays on this machine):\n  \
         {{\"{TRUST_KEY}\": {{\"{name}\": {{{origin_line}\"trusted\": true}}}}}}",
        crate::path_norm::display_slash(&path),
    )
}

/// Where the once-per-root notice is remembered, so a held-back root is
/// mentioned once rather than on every tool call.
#[must_use]
pub fn notice_marker(parent_root: &Path, name: &str) -> PathBuf {
    parent_root
        .join(".clud")
        .join("cache")
        .join("extern-trust-notices")
        .join(format!("{name}.notified"))
}

/// Show the notice unless this root has already been mentioned.
///
/// Best-effort: if the marker cannot be written the notice simply repeats,
/// which is noisy rather than harmful.
pub fn notify_once(parent_root: &Path, root: &HookRoot) {
    let Some(name) = root_name(&root.path) else {
        return;
    };
    let marker = notice_marker(parent_root, &name);
    if marker.exists() {
        return;
    }
    eprintln!("{}", grant_notice(parent_root, root));
    if let Some(parent) = marker.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let _ = std::fs::write(&marker, "1");
}

#[cfg(test)]
#[path = "clud_hook_trust_tests.rs"]
mod clud_hook_trust_tests;
