//! Paths under `$HOME` outside the roots (#1956, #1957).
//!
//! A single regular file or symlink strictly under the user's home may be
//! trashed: an agent that wrote `~/.local/share/applications/x.desktop`, or a
//! memory file under `~/.claude/projects/<slug>/memory/`, must be able to
//! take it back. A directory (so any recursive delete) is allowed only
//! strictly inside the tool data dirs in [`DIR_ROOTS`]; anywhere else under
//! home it keeps the roots-or-ledger refusal. The personal data folders in
//! [`PERSONAL`] and the credential paths in [`DENYLIST`] are refused
//! outright, as is a directory that contains a denylisted path. Home itself
//! and `.git` components are refused earlier, by `resolve`.

use std::path::Path;

/// Paths under `$HOME` (file or directory prefix, `/`-separated) no delete
/// reaches. Matched component-wise, case-insensitively.
pub const DENYLIST: &[&str] = &[
    ".ssh",
    ".gnupg",
    ".aws",
    ".azure",
    ".kube",
    ".docker/config.json",
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
    ".claude/settings.json",
    ".claude.json",
    ".codex/auth.json",
    ".codex/config.toml",
    ".clud/settings.json",
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

/// The user's personal data folders: nothing in them is deleted outside the
/// allowed roots, not even a single file.
pub const PERSONAL: &[&str] = &[
    "Documents",
    "Desktop",
    "Pictures",
    "Music",
    "Videos",
    "Public",
    "Templates",
];

/// Tool data dirs a directory may be deleted inside (never the dir itself).
pub const DIR_ROOTS: &[&str] = &[".claude", ".codex", ".cache"];

/// The reason recorded in the audit log for a path this rule allowed.
pub const GRANT_REASON: &str = "under $HOME (#1956)";

fn lowered(rel: &Path) -> Vec<String> {
    rel.components()
        .map(|c| c.as_os_str().to_string_lossy().to_lowercase())
        .collect()
}

fn split(entry: &str) -> Vec<String> {
    entry.split('/').map(str::to_lowercase).collect()
}

/// The denylist entry `rel` (relative to home) is, lies inside, or
/// contains, if any.
pub fn denied(rel: &Path) -> Option<&'static str> {
    let parts = lowered(rel);
    DENYLIST.iter().copied().find(|entry| {
        let want = split(entry);
        let n = parts.len().min(want.len());
        parts[..n] == want[..n]
    })
}

/// The personal data folder `rel` is or lies inside, if any.
pub fn personal(rel: &Path) -> Option<&'static str> {
    let first = lowered(rel).into_iter().next()?;
    PERSONAL
        .iter()
        .copied()
        .find(|name| name.to_lowercase() == first)
}

/// Whether a directory at `rel` lies strictly inside one of [`DIR_ROOTS`].
pub fn dir_allowed(rel: &Path) -> bool {
    let parts = lowered(rel);
    parts.len() >= 2 && DIR_ROOTS.iter().any(|root| parts[0] == *root)
}

/// Whether `path` (canonical parent + final name, existing) may be deleted
/// under `home` (ownership is the caller's check). `Ok(false)` means the
/// rule does not apply (so the caller keeps its own refusal); `Err` is a
/// denylist or personal-folder refusal.
pub fn check(path: &Path, is_dir: bool, home: Option<&Path>) -> Result<bool, String> {
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
            "is or contains ~/{entry}, on the safe-rm home denylist (credentials, settings and \
             shell startup files are never deleted outside the allowed roots); ask the user to \
             remove it"
        ));
    }
    if let Some(folder) = personal(rel) {
        return Err(format!(
            "is in ~/{folder}, a personal data folder safe-rm never deletes from outside the \
             allowed roots; ask the user to remove it"
        ));
    }
    Ok(!is_dir || dir_allowed(rel))
}
