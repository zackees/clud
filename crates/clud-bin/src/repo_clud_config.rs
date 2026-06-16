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

use serde::Deserialize;
use std::path::{Path, PathBuf};

// ---------------------------------------------------------------------
// Raw parse types — every field is Option so the merge step can tell
// "user set this to false" from "user didn't set this".
// ---------------------------------------------------------------------

#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
#[serde(default)]
pub struct RawRepoCludConfig {
    pub rust: RawRustConfig,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
#[serde(default)]
pub struct RawRustConfig {
    pub use_soldr: Option<bool>,
    pub install: Option<bool>,
    pub version: Option<String>,
}

// ---------------------------------------------------------------------
// Resolved types — what the rest of the binary actually consumes.
// ---------------------------------------------------------------------

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RepoCludConfig {
    pub rust: RustConfig,
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

    if user.is_none() && repo.is_none() {
        return None;
    }

    let user = user.unwrap_or_default();
    let repo = repo.unwrap_or_default();
    Some(resolve(merge(repo, user)))
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
    discover_user_clud_config_raw().map(|raw| resolve(merge(raw, RawRepoCludConfig::default())))
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
/// `upper` wins per-field where set.
pub fn merge(upper: RawRepoCludConfig, lower: RawRepoCludConfig) -> RawRepoCludConfig {
    RawRepoCludConfig {
        rust: RawRustConfig {
            use_soldr: upper.rust.use_soldr.or(lower.rust.use_soldr),
            install: upper.rust.install.or(lower.rust.install),
            version: upper.rust.version.or(lower.rust.version),
        },
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
}
