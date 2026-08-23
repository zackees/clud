//! The typed hook-root registry (zackees/clud#966 §5-6, #967 Phase 3).
//!
//! A session can touch files in more than one repo. Which repo's hooks should
//! fire for a given file is not a question of path geometry — it depends on
//! what the containing repo *is* to the session:
//!
//! | kind | how it is registered | relationship |
//! | --- | --- | --- |
//! | `parent` | the session root | the repo the session belongs to |
//! | `extern` | immediate children of `.extern-repos/` | a temporary, foreign visit |
//! | `child` | **declared** in `.clud/settings.json` | an organizational part of the parent |
//!
//! The firing rule follows from the relationship, per #966 §6:
//!
//! - **parent paths** — the parent's hooks fire, as always.
//! - **`extern` paths** — the parent's hooks **never** fire. Its guards are
//!   meaningless against a repo it does not own and will not keep; at best
//!   they error, which is exactly the #841 ENOENT wedge. That repo's own hooks
//!   are what should run there (Phase 4, trust-gated).
//! - **`child` paths** — **both** fire. A declared child is part of the
//!   parent's world, so the parent's guards apply to it, and the child's own
//!   hooks run rooted at the child (Phase 4).
//! - **unregistered paths** — clud policy only.
//!
//! Note what is *not* here: nested git repos are not auto-detected as
//! children. Declaration is the consent that makes the child tier's
//! no-prompt trust sound (#966 D8), and it collapses if nothing was declared.
//!
//! ## Containment comes from the touched path, never from cwd alone
//!
//! A subagent editing `.extern-repos/<sub>/src/lib.rs` typically still has the
//! session cwd at the parent root, so the payload's `cwd` says nothing useful
//! about which repo is being touched. Containment is resolved from the tool's
//! *inputs* — `file_path` for the editing tools, cwd plus `cd` targets for
//! Bash — which is what makes the rule hold for subagents at all (#966 D5).

use std::path::{Path, PathBuf};

use serde_json::Value;

/// What a registered root is to the session.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RootKind {
    Parent,
    /// A temporary, foreign checkout under `.extern-repos/`.
    Extern,
    /// A declared organizational child of the parent.
    Child,
}

impl RootKind {
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Parent => "parent",
            Self::Extern => "extern",
            Self::Child => "child",
        }
    }

    /// Not `FromStr`: the label set is clud's own encoding for
    /// [`HOOK_ROOTS_ENV`], not a general parse of user text.
    #[must_use]
    pub fn from_label(raw: &str) -> Option<Self> {
        match raw {
            "parent" => Some(Self::Parent),
            "extern" => Some(Self::Extern),
            "child" => Some(Self::Child),
            _ => None,
        }
    }

    /// Whether the **parent's** hooks apply to a path inside a root of this
    /// kind.
    #[must_use]
    pub fn parent_hooks_apply(self) -> bool {
        match self {
            Self::Parent | Self::Child => true,
            Self::Extern => false,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HookRoot {
    pub path: PathBuf,
    pub kind: RootKind,
}

/// The directory whose immediate children are `extern` roots by convention.
pub const EXTERN_REPOS_DIR: &str = ".extern-repos";

/// Env var carrying roots clud resolved at launch that the hook cannot
/// rediscover on its own — `--add-dir` targets and
/// `permissions.additionalDirectories`, neither of which appears in a hook
/// payload. JSON, because a path-separated list is ambiguous on Windows where
/// paths contain `:`.
pub const HOOK_ROOTS_ENV: &str = "CLUD_HOOK_ROOTS";

/// Every root registered for a session, most specific first.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct HookRoots {
    roots: Vec<HookRoot>,
}

impl HookRoots {
    /// Resolve the registry for a session rooted at `parent`.
    ///
    /// `declared_children` comes from the parent's `.clud/settings.json`;
    /// `env_roots` from [`HOOK_ROOTS_ENV`].
    #[must_use]
    pub fn resolve(parent: &Path, declared_children: &[String], env_roots: Option<&str>) -> Self {
        let mut roots = vec![HookRoot {
            path: lexical_normalize(parent),
            kind: RootKind::Parent,
        }];

        for path in extern_children(parent) {
            roots.push(HookRoot {
                path,
                kind: RootKind::Extern,
            });
        }

        for declared in declared_children {
            let path = resolve_relative(parent, declared);
            roots.push(HookRoot {
                path,
                kind: RootKind::Child,
            });
        }

        if let Some(encoded) = env_roots {
            roots.extend(parse_env_roots(encoded));
        }

        // Most specific first, so a root nested inside another wins the
        // containment lookup regardless of registration order.
        roots.sort_by(|left, right| {
            depth_of(&right.path)
                .cmp(&depth_of(&left.path))
                .then_with(|| left.path.cmp(&right.path))
        });
        roots.dedup_by(|left, right| key_of(&left.path) == key_of(&right.path));
        Self { roots }
    }

    /// A registry with only the session root, for callers that have no
    /// settings to consult.
    #[must_use]
    pub fn parent_only(parent: &Path) -> Self {
        Self::resolve(parent, &[], None)
    }

    #[must_use]
    pub fn all(&self) -> &[HookRoot] {
        &self.roots
    }

    /// Every registered root's path, for the cwd-pinning set.
    #[must_use]
    pub fn paths(&self) -> Vec<PathBuf> {
        self.roots.iter().map(|root| root.path.clone()).collect()
    }

    /// The most specific registered root containing `path`.
    #[must_use]
    pub fn containing(&self, path: &Path) -> Option<&HookRoot> {
        let normalized = lexical_normalize(path);
        self.roots
            .iter()
            .find(|root| is_within(&normalized, &root.path))
    }

    /// Whether the parent's own hooks should fire for `path`.
    ///
    /// An unregistered path is not the parent's business either — it is
    /// outside every root the session knows about.
    #[must_use]
    pub fn parent_hooks_apply_to(&self, path: &Path) -> bool {
        self.containing(path)
            .is_some_and(|root| root.kind.parent_hooks_apply())
    }

    /// Encode for [`HOOK_ROOTS_ENV`].
    #[must_use]
    pub fn to_env_value(&self) -> String {
        let entries: Vec<Value> = self
            .roots
            .iter()
            .map(|root| {
                serde_json::json!({
                    "kind": root.kind.as_str(),
                    "path": root.path.to_string_lossy(),
                })
            })
            .collect();
        Value::Array(entries).to_string()
    }
}

/// The paths a tool call *names*, resolved against `cwd`.
///
/// Empty when the tool names none, which is the common case for `Bash`. The
/// caller decides the fallback, because only it knows whether the command
/// relocates itself with a `cd` first.
///
/// This is the input to containment, and it is deliberately *not* the payload
/// cwd: a subagent editing `.extern-repos/<sub>/src/lib.rs` typically still
/// has the session cwd at the parent root, so keying on cwd would answer
/// "parent" for a file that is plainly not the parent's (#966 D5).
#[must_use]
pub fn tool_input_paths(tool_input: Option<&Value>, cwd: &Path) -> Vec<PathBuf> {
    const PATH_FIELDS: &[&str] = &["file_path", "notebook_path", "path"];
    let mut found = Vec::new();
    let Some(object) = tool_input.and_then(Value::as_object) else {
        return found;
    };
    for field in PATH_FIELDS {
        if let Some(raw) = object.get(*field).and_then(Value::as_str) {
            let raw = raw.trim();
            if !raw.is_empty() {
                found.push(resolve_relative(cwd, raw));
            }
        }
    }
    found
}

/// Immediate children of `<parent>/.extern-repos/`, which are `extern` roots
/// by the GC-tracked convention.
fn extern_children(parent: &Path) -> Vec<PathBuf> {
    let dir = parent.join(EXTERN_REPOS_DIR);
    let Ok(entries) = std::fs::read_dir(&dir) else {
        return Vec::new();
    };
    let mut found: Vec<PathBuf> = entries
        .flatten()
        .filter(|entry| entry.path().is_dir())
        .map(|entry| lexical_normalize(&entry.path()))
        .collect();
    found.sort();
    found
}

fn parse_env_roots(encoded: &str) -> Vec<HookRoot> {
    let Ok(Value::Array(entries)) = serde_json::from_str::<Value>(encoded) else {
        eprintln!("clud: ignoring unparsable {HOOK_ROOTS_ENV}");
        return Vec::new();
    };
    entries
        .iter()
        .filter_map(|entry| {
            let path = entry.get("path").and_then(Value::as_str)?;
            let kind = entry
                .get("kind")
                .and_then(Value::as_str)
                .and_then(RootKind::from_label)?;
            Some(HookRoot {
                path: lexical_normalize(Path::new(path)),
                kind,
            })
        })
        .collect()
}

fn resolve_relative(base: &Path, candidate: &str) -> PathBuf {
    let path = Path::new(candidate);
    let joined = if path.is_absolute() || looks_absolute(candidate) {
        path.to_path_buf()
    } else {
        base.join(path)
    };
    lexical_normalize(&joined)
}

/// Absolute in *either* platform's spelling — settings files travel between
/// them, so `Path::is_absolute` alone would misread a foreign declaration.
fn looks_absolute(raw: &str) -> bool {
    let bytes = raw.as_bytes();
    if matches!(bytes.first(), Some(b'/') | Some(b'\\')) {
        return true;
    }
    bytes.len() >= 2 && bytes[0].is_ascii_alphabetic() && bytes[1] == b':'
}

fn lexical_normalize(path: &Path) -> PathBuf {
    use std::path::Component;
    let mut out = PathBuf::new();
    for component in path.components() {
        match component {
            Component::ParentDir => {
                out.pop();
            }
            Component::CurDir => {}
            other => out.push(other.as_os_str()),
        }
    }
    out
}

fn key_of(path: &Path) -> String {
    crate::path_norm::normalize_for_key(path)
}

fn depth_of(path: &Path) -> usize {
    path.components().count()
}

/// Whether `path` is `root` or lives under it, compared on normalized keys so
/// Windows drive-letter and separator casing do not decide it.
fn is_within(path: &Path, root: &Path) -> bool {
    let path_key = key_of(path);
    let root_key = key_of(root);
    if path_key == root_key {
        return true;
    }
    let prefix = if root_key.ends_with('/') {
        root_key
    } else {
        format!("{root_key}/")
    };
    path_key.starts_with(&prefix)
}

#[cfg(test)]
#[path = "clud_hook_roots_tests.rs"]
mod clud_hook_roots_tests;
