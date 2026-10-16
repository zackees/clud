//! Unix half of `crate::safe_mktemp`: the exclusive `mkdir`, the
//! `O_NOFOLLOW` identity check and the ledger insert.

use crate::gc::CreatedEntry;
use crate::safe_mktemp::{fail, Failure, LedgerWriter, Request, COMMAND};
use std::os::unix::fs::{DirBuilderExt, MetadataExt, OpenOptionsExt};
use std::path::PathBuf;
use std::path::{Component, Path};

/// The `(dev, ino)` of the directory this call made.
type Identity = (u64, u64);

pub(crate) fn create(
    raw: &str,
    request: &Request,
    writer: &dyn LedgerWriter,
) -> Result<PathBuf, Failure> {
    let session = request
        .session_id
        .clone()
        .filter(|id| !id.is_empty())
        .ok_or_else(|| {
            fail(
                2,
                "no clud session id (CLUD_SESSION_ID / CLAUDE_CODE_SESSION_ID); \
                 launch through clud, or set CLUD_SESSION_ID to a unique value before \
                 starting this session; nothing created",
            )
        })?;
    let target = target_path(raw, &request.cwd)?;
    if let Err(error) = std::fs::DirBuilder::new().mode(0o700).create(&target) {
        return Err(if error.kind() == std::io::ErrorKind::AlreadyExists {
            fail(
                1,
                format!(
                    "{} already exists; nothing created or recorded",
                    target.display()
                ),
            )
        } else {
            fail(1, format!("cannot create {}: {error}", target.display()))
        });
    }
    let identity = verify_fresh(&target).map_err(|why| {
        fail(
            1,
            format!(
                "{} changed right after it was created ({why}); not recorded and left \
                 in place",
                target.display()
            ),
        )
    })?;
    let entry = CreatedEntry {
        session_id: session,
        path: target.display().to_string(),
        kind: crate::gc::CreatedKind::Dir,
        role: request.role.clone(),
        created_unix: request.now_unix,
        dev: Some(identity.0),
        ino: Some(identity.1),
        uid: Some(euid()),
    };
    if let Err(error) = writer.record(&entry) {
        let cleanup = remove_if_same(&target, identity);
        return Err(fail(
            1,
            format!("creation ledger insert failed ({error}); {cleanup}; nothing recorded",),
        ));
    }
    Ok(target)
}

fn euid() -> u32 {
    // SAFETY: geteuid has no preconditions and cannot fail.
    unsafe { libc::geteuid() }
}

/// The canonical parent joined with the final name. The final component
/// must be a plain name, and the parent must exist.
fn target_path(raw: &str, cwd: &Path) -> Result<PathBuf, Failure> {
    let path = Path::new(raw);
    let absolute = if path.is_absolute() {
        path.to_path_buf()
    } else {
        cwd.join(path)
    };
    let (Some(Component::Normal(name)), Some(parent)) =
        (absolute.components().next_back(), absolute.parent())
    else {
        return Err(fail(2, format!("{raw}: not a plain directory name")));
    };
    let parent = crate::path_norm::canonicalize_plain(parent).map_err(|error| {
        fail(
            1,
            format!(
                "parent {} is not usable ({error}); {COMMAND} creates one directory, \
                 never its parents",
                parent.display()
            ),
        )
    })?;
    Ok(parent.join(name))
}

/// Open the new directory without following a symlink, and require it to
/// be an empty directory the caller owns at the path's current identity.
fn verify_fresh(target: &Path) -> Result<Identity, String> {
    let handle = std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC)
        .open(target)
        .map_err(|error| format!("cannot open it as a directory: {error}"))?;
    let meta = handle
        .metadata()
        .map_err(|error| format!("cannot stat it: {error}"))?;
    if !meta.is_dir() {
        return Err("not a directory".into());
    }
    if meta.uid() != euid() {
        return Err("not owned by you".into());
    }
    let identity = (meta.dev(), meta.ino());
    let mut entries =
        std::fs::read_dir(target).map_err(|error| format!("cannot list it: {error}"))?;
    if entries.next().is_some() {
        return Err("not empty".into());
    }
    let now =
        std::fs::symlink_metadata(target).map_err(|error| format!("cannot stat it: {error}"))?;
    if now.file_type().is_symlink() || (now.dev(), now.ino()) != identity {
        return Err("replaced".into());
    }
    Ok(identity)
}

/// Undo this call's `mkdir` when it is still the same empty directory.
fn remove_if_same(target: &Path, identity: Identity) -> String {
    let same = std::fs::symlink_metadata(target)
        .map(|m| !m.file_type().is_symlink() && (m.dev(), m.ino()) == identity)
        .unwrap_or(false);
    if !same {
        return format!("{} was changed, so it was left in place", target.display());
    }
    match std::fs::remove_dir(target) {
        Ok(()) => format!("removed {} again", target.display()),
        Err(error) => format!(
            "could not remove {} ({error}); it is NOT deletable via the ledger",
            target.display()
        ),
    }
}

#[cfg(test)]
#[path = "safe_mktemp_tests.rs"]
mod tests;
