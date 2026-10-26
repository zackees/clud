//! Workspace identity, labels and resource names (#926 v2 §3).
//!
//! Every function is a pure string transform over facts the caller already
//! resolved (git remote, common dir, worktree root, branch, HEAD), so the
//! cross-form convergence rule is unit-testable on every host: `C:\dev\foo`,
//! `C:/dev/foo`, `/c/dev/foo`, `/mnt/c/dev/foo` and the verbatim forms of the
//! same directory must all produce one workspace id.

use sha2::{Digest, Sha256};

/// Label namespace for every resource dockhand creates.
pub const LABEL_PREFIX: &str = "io.clud.dockhand.";
/// Version of the label contract; bumped when a label's meaning changes.
pub const LABEL_SCHEMA: u32 = 1;

/// How the supervising daemon spells paths. A WSL client talks to the
/// Windows daemon (#926 v2 §1), so its `/mnt/c/...` input is read with
/// `Windows` semantics.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PathHost {
    Windows,
    Posix,
}

#[derive(Debug, PartialEq, Eq)]
pub enum IdentityError {
    /// Not absolute in any accepted form, so it cannot name a workspace.
    Relative(String),
    /// A `\\?\` form with no ordinary spelling, such as `\\?\Volume{...}`.
    UnsupportedVerbatim(String),
}

impl std::fmt::Display for IdentityError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Relative(raw) => write!(f, "workspace path is not absolute: {raw}"),
            Self::UnsupportedVerbatim(raw) => {
                write!(f, "workspace path has no ordinary spelling: {raw}")
            }
        }
    }
}

impl std::error::Error for IdentityError {}

/// The one canonical spelling of a workspace root: forward slashes, no
/// trailing separator, `.` and empty components removed, an upper-case drive
/// letter on Windows, and case-folded when the filesystem holding the path is
/// case-insensitive (decided per path by the caller, not per host).
pub fn canonical_root(
    raw: &str,
    host: PathHost,
    case_insensitive: bool,
) -> Result<String, IdentityError> {
    let (anchor, rest) = match host {
        PathHost::Windows => windows_anchor(raw)?,
        PathHost::Posix => posix_anchor(raw)?,
    };
    let mut parts: Vec<&str> = Vec::new();
    for part in rest.split('/') {
        match part {
            "" | "." => {}
            ".." => {
                parts.pop();
            }
            other => parts.push(other),
        }
    }
    let joined = format!("{anchor}{}", parts.join("/"));
    let root = if joined.len() > anchor.len() || !anchor.starts_with("//") {
        joined
    } else {
        anchor.trim_end_matches('/').to_string()
    };
    Ok(if case_insensitive {
        root.to_lowercase()
    } else {
        root
    })
}

/// Split a Windows-host path into its anchor (`C:/` or `//server/share/`)
/// and the slash-separated remainder.
fn windows_anchor(raw: &str) -> Result<(String, String), IdentityError> {
    let slashed = raw.replace('\\', "/");
    let unverbatim = if let Some(rest) = slashed.strip_prefix("//?/UNC/") {
        format!("//{rest}")
    } else if let Some(rest) = slashed.strip_prefix("//?/") {
        if !is_drive_prefixed(rest) {
            return Err(IdentityError::UnsupportedVerbatim(raw.to_string()));
        }
        rest.to_string()
    } else {
        slashed
    };
    if let Some(rest) = unverbatim.strip_prefix("/mnt/") {
        if let Some(drive) = msys_like(&format!("/{rest}")) {
            return Ok(split_drive(&drive));
        }
    }
    if let Some(unc) = unverbatim.strip_prefix("//") {
        let mut pieces = unc.splitn(3, '/');
        let server = pieces.next().unwrap_or_default();
        let share = pieces.next().unwrap_or_default();
        if server.is_empty() || share.is_empty() {
            return Err(IdentityError::Relative(raw.to_string()));
        }
        let rest = pieces.next().unwrap_or_default().to_string();
        return Ok((format!("//{server}/{share}/"), rest));
    }
    if is_drive_prefixed(&unverbatim) {
        let bytes = unverbatim.as_bytes();
        if bytes.len() > 2 && bytes[2] != b'/' {
            // `C:foo` is drive-relative, not a root.
            return Err(IdentityError::Relative(raw.to_string()));
        }
        return Ok(split_drive(&unverbatim));
    }
    if let Some(drive) = msys_like(&unverbatim) {
        return Ok(split_drive(&drive));
    }
    Err(IdentityError::Relative(raw.to_string()))
}

fn posix_anchor(raw: &str) -> Result<(String, String), IdentityError> {
    match raw.strip_prefix('/') {
        Some(rest) => Ok(("/".to_string(), rest.to_string())),
        None => Err(IdentityError::Relative(raw.to_string())),
    }
}

fn is_drive_prefixed(path: &str) -> bool {
    let bytes = path.as_bytes();
    bytes.len() >= 2 && bytes[0].is_ascii_alphabetic() && bytes[1] == b':'
}

/// `/c` or `/c/...` as `C:/...`; any other shape is not a drive path.
fn msys_like(path: &str) -> Option<String> {
    crate::rm_tool::msys_drive_path(path)
}

fn split_drive(path: &str) -> (String, String) {
    let drive = path[..1].to_ascii_uppercase();
    let rest = path[2..].trim_start_matches('/').to_string();
    (format!("{drive}:/"), rest)
}

/// Who a repository is, independent of where it is checked out.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RepoIdentity {
    /// Stable hash: of the normalized remote, else of the common git dir.
    pub id: String,
    /// Readable, Docker-safe name for humans.
    pub slug: String,
}

/// Normalize a git remote URL to `host/owner/name`, lower-cased, so the
/// scp, `ssh://` and `https://` spellings of one repository agree.
pub fn normalize_remote(url: &str) -> Option<String> {
    let trimmed = url.trim().trim_end_matches('/');
    // `scheme://[user@]host[:port]/path` splits at the first `/`, and its
    // `:` is a port; scp-style `user@host:path` splits at the `:`.
    let (host, path) = match trimmed.split_once("://") {
        Some((_, rest)) => {
            let (authority, path) = rest.split_once('/')?;
            let host = authority.rsplit('@').next().unwrap_or(authority);
            (host.split(':').next().unwrap_or(host), path)
        }
        None => {
            let rest = trimmed.split_once('@').map_or(trimmed, |(_, rest)| rest);
            rest.split_once(':')?
        }
    };
    let path = path.trim_end_matches(".git").trim_matches('/');
    if host.is_empty() || !path.contains('/') {
        return None;
    }
    Some(format!("{host}/{path}").to_lowercase())
}

/// The repository identity: the normalized remote when there is one, else
/// the canonical common git directory.
pub fn repo_identity(remote: Option<&str>, canonical_common_dir: &str) -> RepoIdentity {
    match remote.and_then(normalize_remote) {
        Some(normalized) => {
            let leaf = normalized.rsplit('/').next().unwrap_or(&normalized);
            RepoIdentity {
                id: short_hash(&format!("remote:{normalized}")),
                slug: slug(leaf, 24),
            }
        }
        None => {
            let leaf = canonical_common_dir
                .trim_end_matches('/')
                .trim_end_matches("/.git")
                .rsplit('/')
                .next()
                .unwrap_or("repo");
            RepoIdentity {
                id: short_hash(&format!("commondir:{canonical_common_dir}")),
                slug: slug(leaf, 24),
            }
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Classification {
    MainWorktree,
    LinkedWorktree,
    NonGit,
}

impl Classification {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::MainWorktree => "main",
            Self::LinkedWorktree => "linked",
            Self::NonGit => "non-git",
        }
    }
}

/// One checkout: a repository at a canonical root.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Workspace {
    pub repo: RepoIdentity,
    /// Stable hash of the repository id and the canonical root.
    pub id: String,
    /// From [`canonical_root`].
    pub root: String,
    pub branch: Option<String>,
    pub head: Option<String>,
    pub classification: Classification,
}

pub fn workspace(
    repo: RepoIdentity,
    canonical_root: String,
    branch: Option<String>,
    head: Option<String>,
    classification: Classification,
) -> Workspace {
    let id = short_hash(&format!("{}\n{canonical_root}", repo.id));
    Workspace {
        repo,
        id,
        root: canonical_root,
        branch,
        head,
        classification,
    }
}

/// What a resource is for, beyond the workspace that owns it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ResourceFacts<'a> {
    /// The registry that created it (#926 v2 §1).
    pub registry_id: &'a str,
    pub stack: &'a str,
    /// `container`, `target`, `cargo-home`, `image`, ...
    pub role: &'a str,
    /// The spec digest (spec-scoped role) or the stack constant (stack-scoped).
    pub scope_digest: &'a str,
    /// Unix seconds; provenance only.
    pub created_at: u64,
}

/// The complete managed-label contract. Branch, HEAD and root are recorded
/// as they were at creation and are provenance only: no lifecycle decision
/// may read them (#926 v2 §3).
pub fn labels(workspace: &Workspace, facts: &ResourceFacts<'_>) -> Vec<(String, String)> {
    let branch = workspace.branch.clone().unwrap_or_default();
    let mut pairs = vec![
        ("managed", "true".to_string()),
        ("schema", LABEL_SCHEMA.to_string()),
        ("registry", facts.registry_id.to_string()),
        ("repo-id", workspace.repo.id.clone()),
        ("repo-slug", workspace.repo.slug.clone()),
        ("workspace-id", workspace.id.clone()),
        ("workspace-root", workspace.root.clone()),
        (
            "workspace-kind",
            workspace.classification.as_str().to_string(),
        ),
        ("branch-slug", slug(&branch, 40)),
        ("branch", branch),
        ("head", workspace.head.clone().unwrap_or_default()),
        ("stack", facts.stack.to_string()),
        ("role", facts.role.to_string()),
        ("spec-digest", facts.scope_digest.to_string()),
        ("created-at", facts.created_at.to_string()),
    ];
    pairs.sort_by(|a, b| a.0.cmp(b.0));
    pairs
        .drain(..)
        .map(|(key, value)| (format!("{LABEL_PREFIX}{key}"), value))
        .collect()
}

/// A Docker-safe, collision-resistant name for a workspace resource. The
/// readable parts are bounded slugs; uniqueness comes from the hash of the
/// full key `(workspace, stack, role, scope digest)`.
pub fn resource_name(workspace: &Workspace, stack: &str, role: &str, scope_digest: &str) -> String {
    let key = short_hash(&format!(
        "{}\n{stack}\n{role}\n{scope_digest}",
        workspace.id
    ));
    format!(
        "dh-{}-{}-{}-{key}",
        workspace.repo.slug,
        slug(stack, 16),
        slug(role, 16)
    )
}

/// Lower-case `[a-z0-9-]`, runs of anything else collapsed to one `-`,
/// trimmed, at most `max` bytes, never empty.
pub fn slug(raw: &str, max: usize) -> String {
    let mut out = String::new();
    for ch in raw.chars() {
        if ch.is_ascii_alphanumeric() {
            out.push(ch.to_ascii_lowercase());
        } else if !out.ends_with('-') && !out.is_empty() {
            out.push('-');
        }
    }
    out.truncate(max);
    let trimmed = out.trim_matches('-');
    if trimmed.is_empty() {
        "x".to_string()
    } else {
        trimmed.to_string()
    }
}

/// First 16 hex digits of SHA-256: 64 bits, ample for per-machine keys.
fn short_hash(input: &str) -> String {
    Sha256::digest(input.as_bytes())
        .iter()
        .take(8)
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

#[cfg(test)]
#[path = "identity_tests.rs"]
mod tests;
