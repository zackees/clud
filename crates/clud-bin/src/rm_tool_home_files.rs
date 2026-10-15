//! Single files under `$HOME` outside the roots (#1956).
//!
//! A recursive or directory delete still needs an allowed root or a ledger
//! entry. One regular file or symlink strictly under the user's home may be
//! trashed anyway: an agent that wrote `~/.local/share/applications/x.desktop`
//! with its Write tool must be able to take it back. A short denylist keeps
//! credentials, shell startup files and similar high-value paths refused.

use std::path::Path;

/// Paths under `$HOME` (file or directory prefix, `/`-separated) a single
/// file delete never reaches. Matched component-wise, case-insensitively.
pub const DENYLIST: &[&str] = &[
    ".ssh",
    ".gnupg",
    ".aws",
    ".azure",
    ".kube",
    ".docker",
    ".password-store",
    ".local/share/keyrings",
    ".config/gh",
    ".config/gcloud",
    ".config/fish/config.fish",
    ".mozilla",
    ".thunderbird",
    ".config/google-chrome",
    ".config/chromium",
    ".cargo/credentials",
    ".cargo/credentials.toml",
    ".claude/.credentials.json",
    ".claude.json",
    ".codex/auth.json",
    ".bashrc",
    ".bash_profile",
    ".bash_login",
    ".bash_logout",
    ".profile",
    ".zshrc",
    ".zshenv",
    ".zprofile",
    ".zlogin",
    ".zlogout",
    ".gitconfig",
    ".git-credentials",
    ".netrc",
    ".pgpass",
    ".npmrc",
    ".pypirc",
    ".Xauthority",
];

/// The reason recorded in the audit log for a file this rule allowed.
pub const GRANT_REASON: &str = "single file under $HOME (#1956)";

/// The denylist entry covering `rel` (a path relative to home), if any.
pub fn denied(rel: &Path) -> Option<&'static str> {
    let parts: Vec<String> = rel
        .components()
        .map(|c| c.as_os_str().to_string_lossy().to_lowercase())
        .collect();
    DENYLIST.iter().copied().find(|entry| {
        let want: Vec<String> = entry.split('/').map(str::to_lowercase).collect();
        parts.len() >= want.len() && parts[..want.len()] == want[..]
    })
}

/// Whether `path` (canonical parent + final name, existing) may be deleted
/// as a single file under `home`. `Ok(false)` means the rule does not
/// apply (so the caller keeps its own refusal); `Err` is a denylist refusal.
pub fn check(
    path: &Path,
    meta: &std::fs::Metadata,
    home: Option<&Path>,
    owner: Option<u32>,
) -> Result<bool, String> {
    let is_symlink = meta.file_type().is_symlink();
    if !is_symlink && !meta.is_file() {
        return Ok(false);
    }
    let Some(home) = home.and_then(|h| crate::path_norm::canonicalize_plain(h).ok()) else {
        return Ok(false);
    };
    let Ok(rel) = path.strip_prefix(&home) else {
        return Ok(false);
    };
    if rel.as_os_str().is_empty() {
        return Ok(false);
    }
    if let Some(entry) = denied(rel) {
        return Err(format!(
            "is on the safe-rm home denylist (~/{entry}: credentials, shell startup files and \
             similar paths are never deleted outside the allowed roots); ask the user to remove \
             it"
        ));
    }
    if !owned_by(meta, owner) {
        return Ok(false);
    }
    Ok(true)
}

#[cfg(unix)]
fn owned_by(meta: &std::fs::Metadata, owner: Option<u32>) -> bool {
    use std::os::unix::fs::MetadataExt;
    owner.is_none_or(|uid| meta.uid() == uid)
}

#[cfg(not(unix))]
fn owned_by(_: &std::fs::Metadata, _: Option<u32>) -> bool {
    true
}
