//! Cross-platform path normalization helpers.
//!
//! This is a small port of fbuild/zccache's `NormalizedPath` pattern: keep a
//! path together with a precomputed, slash-normalized comparison key so hashing
//! and ordering do not depend on the host platform's separator spelling.

use std::cmp::Ordering;
use std::ffi::OsStr;
use std::hash::{Hash, Hasher};
use std::ops::Deref;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use serde::{Deserialize, Deserializer, Serialize, Serializer};

#[derive(Debug, Clone)]
pub struct NormalizedPath {
    path: Arc<Path>,
    key: Arc<str>,
}

impl NormalizedPath {
    pub fn new(path: impl AsRef<Path>) -> Self {
        let path = normalize(path.as_ref());
        let key: Arc<str> = Arc::from(normalize_for_key(&path));
        let path: Arc<Path> = Arc::from(path);
        Self { path, key }
    }

    #[must_use]
    pub fn as_path(&self) -> &Path {
        &self.path
    }

    #[must_use]
    pub fn key(&self) -> &str {
        &self.key
    }

    #[must_use]
    pub fn into_path_buf(self) -> PathBuf {
        self.path.to_path_buf()
    }

    #[must_use]
    pub fn join(&self, path: impl AsRef<Path>) -> Self {
        Self::new(self.path.join(path))
    }

    /// Render the path with forward slashes for interchange strings.
    ///
    /// Use this for JSON, command-line arguments, log records, or tests that
    /// compare byte-for-byte across platforms. It is intentionally centralized
    /// so callers do not hand-roll `.replace('\\', "/")` and forget about
    /// `\\?\` prefixes or platform case rules.
    #[must_use]
    pub fn display_slash(&self) -> String {
        display_slash(self.as_path())
    }
}

impl PartialEq for NormalizedPath {
    fn eq(&self, other: &Self) -> bool {
        self.key == other.key
    }
}

impl Eq for NormalizedPath {}

impl Hash for NormalizedPath {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.key.hash(state);
    }
}

impl PartialOrd for NormalizedPath {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for NormalizedPath {
    fn cmp(&self, other: &Self) -> Ordering {
        self.key.cmp(&other.key)
    }
}

impl std::fmt::Display for NormalizedPath {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.path.display().fmt(f)
    }
}

impl AsRef<Path> for NormalizedPath {
    fn as_ref(&self) -> &Path {
        self.as_path()
    }
}

impl AsRef<OsStr> for NormalizedPath {
    fn as_ref(&self) -> &OsStr {
        self.as_path().as_os_str()
    }
}

impl Deref for NormalizedPath {
    type Target = Path;

    fn deref(&self) -> &Self::Target {
        self.as_path()
    }
}

impl From<PathBuf> for NormalizedPath {
    fn from(path: PathBuf) -> Self {
        Self::new(path)
    }
}

impl From<&Path> for NormalizedPath {
    fn from(path: &Path) -> Self {
        Self::new(path)
    }
}

impl From<String> for NormalizedPath {
    fn from(path: String) -> Self {
        Self::new(path)
    }
}

impl From<&str> for NormalizedPath {
    fn from(path: &str) -> Self {
        Self::new(path)
    }
}

impl From<&String> for NormalizedPath {
    fn from(path: &String) -> Self {
        Self::new(path)
    }
}

impl Serialize for NormalizedPath {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        self.display_slash().serialize(serializer)
    }
}

impl<'de> Deserialize<'de> for NormalizedPath {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        PathBuf::deserialize(deserializer).map(Self::new)
    }
}

#[must_use]
pub fn normalize(path: &Path) -> PathBuf {
    use std::path::Component;

    let mut components = Vec::new();
    for component in path.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                if let Some(Component::Normal(_)) = components.last() {
                    components.pop();
                } else {
                    components.push(component);
                }
            }
            _ => components.push(component),
        }
    }
    components.iter().collect()
}

#[must_use]
pub fn normalize_for_key(path: &Path) -> String {
    let mut s = display_slash(&normalize(path));
    if cfg!(any(windows, target_os = "macos")) {
        s.make_ascii_lowercase();
    }
    s
}

#[must_use]
pub fn display_slash(path: &Path) -> String {
    let mut s = path.to_string_lossy().into_owned();
    if let Some(stripped) = s.strip_prefix(r"\\?\UNC\") {
        s = format!(r"\\{stripped}");
    } else if let Some(stripped) = s.strip_prefix(r"\\?\") {
        s = stripped.to_string();
    }
    slash_separators(&s)
}

/// Normalize path separators in a path-shaped string.
///
/// This works on strings received from another platform, unlike
/// `std::path::Path` parsing, which treats `\` as a normal filename byte on
/// Unix. Use it for command names, serialized paths, and log strings, not for
/// arbitrary user text.
#[must_use]
pub fn slash_separators(raw: &str) -> String {
    raw.chars()
        .map(|ch| if ch == '\\' { '/' } else { ch })
        .collect()
}

/// Return a file stem using either `/` or `\` as a path separator.
///
/// This is for executable/path strings that may have been produced on a
/// different platform. `Path::new(r"C:\Tools\python.exe").file_stem()` returns
/// the whole string on Unix; this helper consistently returns `python`.
#[must_use]
pub fn file_stem_any_separator(raw: &str) -> Option<String> {
    let file_name = raw
        .rsplit(|ch| ['/', '\\'].contains(&ch))
        .find(|part| !part.is_empty())?;
    Path::new(file_name)
        .file_stem()
        .map(|stem| stem.to_string_lossy().into_owned())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    #[test]
    fn normalize_strips_dots_and_resolves_parents() {
        assert_eq!(normalize(Path::new("/a/./b/../c")), PathBuf::from("/a/c"));
    }

    #[test]
    fn key_never_contains_backslash() {
        let path = NormalizedPath::new(r"C:\Tools\python.exe");
        assert!(!path.key().contains('\\'), "key={}", path.key());
    }

    #[test]
    fn display_slash_strips_extended_length_prefix() {
        assert_eq!(
            display_slash(Path::new(r"\\?\C:\Users\test")),
            "C:/Users/test"
        );
        assert_eq!(
            display_slash(Path::new(r"\\?\UNC\server\share\dir")),
            "//server/share/dir"
        );
    }

    #[test]
    fn hash_agrees_with_eq() {
        let a = NormalizedPath::new("/usr/bin/python");
        let b = NormalizedPath::new("/usr/bin/python");
        let mut set = HashSet::new();
        set.insert(a);
        assert!(set.contains(&b));
    }

    #[test]
    fn serialize_emits_slash_form() {
        let p = NormalizedPath::new(r"C:\Tools\python.exe");
        let json = serde_json::to_string(&p).unwrap();
        assert_eq!(json, "\"C:/Tools/python.exe\"");
    }

    #[test]
    fn file_stem_handles_native_and_foreign_separators() {
        assert_eq!(
            file_stem_any_separator("/usr/bin/python3").as_deref(),
            Some("python3")
        );
        assert_eq!(
            file_stem_any_separator(r"C:\Tools\python.exe").as_deref(),
            Some("python")
        );
        assert_eq!(file_stem_any_separator("").as_deref(), None);
    }
}
