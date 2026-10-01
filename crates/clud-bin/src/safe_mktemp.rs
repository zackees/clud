//! `safe-mktemp <path>` (#1667, DD-135 slice 2, DD-136): create one scratch
//! directory and record it in the creation ledger, so `safe-rm -r` from the
//! same session (and its sub-agents, which share the session env) may remove
//! it even outside the allowed roots.
//!
//! This is the only writer of creation-ledger rows. It records only the
//! directory its own exclusive `mkdir` just made: an existing path (a
//! directory, a file, a symlink, dangling or not) fails with nothing created
//! or recorded, and the parent must already exist (parents are never created,
//! so none is ever recorded). The identity in the row comes from a handle
//! opened `O_NOFOLLOW` on the new directory, and the directory must still be
//! empty, owned by the caller and at the same device/inode as that handle
//! before anything is recorded. If the daemon insert fails, the directory is
//! removed again (only if it is still that empty directory) and the call
//! exits non-zero, so nothing is ever left that looks deletable but is not.
//!
//! Windows: there is no file identity without new unsafe code, so the ledger
//! refuses there (`rm_tool::ledger`); `safe-mktemp` fails before creating
//! anything rather than make a directory `safe-rm` would refuse.
//! Policy: `docs/architecture/rm-tools.md#creation-ledger`.

use std::ffi::OsString;
use std::path::PathBuf;

use crate::gc::CreatedEntry;

/// The multicall name (`crate::shim_registry::SHIMS`).
pub const COMMAND: &str = "safe-mktemp";

/// Where a created directory is recorded. The production writer is the
/// daemon; tests inject one. Crate-internal on purpose: no CLI form records
/// a path, so a row can only follow `safe-mktemp`'s own `mkdir`.
// Windows refuses before reading any of it (`create`).
#[cfg_attr(not(unix), allow(dead_code))]
pub(crate) trait LedgerWriter {
    fn record(&self, entry: &CreatedEntry) -> Result<(), String>;
}

/// The daemon's GC registry (`GcOp::InsertCreated`).
// Windows refuses before reading any of it (`create`).
#[cfg_attr(not(unix), allow(dead_code))]
struct DaemonWriter {
    state_dir: Option<PathBuf>,
}

impl LedgerWriter for DaemonWriter {
    fn record(&self, entry: &CreatedEntry) -> Result<(), String> {
        let state = self.state_dir.as_deref().ok_or("no clud state directory")?;
        crate::daemon::gc_client_insert_created(state, entry)
            .map(|_| ())
            .map_err(|error| format!("clud daemon: {error}"))
    }
}

/// Everything a call needs from its environment.
// Windows refuses before reading any of it (`create`).
#[cfg_attr(not(unix), allow(dead_code))]
#[derive(Debug, Clone)]
pub(crate) struct Request {
    pub cwd: PathBuf,
    pub session_id: Option<String>,
    pub role: String,
    pub now_unix: i64,
}

/// A refusal: exit code and message.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Failure {
    pub code: i32,
    pub message: String,
}

fn fail(code: i32, message: impl Into<String>) -> Failure {
    Failure {
        code,
        message: message.into(),
    }
}

fn usage() -> String {
    format!(
        "usage: {COMMAND} <path>\n\
         Create one new directory at <path> (its parent must exist) and record it in\n\
         the clud creation ledger, so `safe-rm -r <path>` from this session can remove\n\
         it even outside the allowed locations. Prints the created path."
    )
}

/// `safe-mktemp` from the process environment. Returns the exit code.
pub fn run(args: &[OsString]) -> i32 {
    let args: Option<Vec<String>> = args.iter().map(|a| a.clone().into_string().ok()).collect();
    let Some(args) = args else {
        eprintln!("{COMMAND}: non-UTF8 arguments are not supported");
        return 2;
    };
    let path = match args.as_slice() {
        [flag] if flag == "-h" || flag == "--help" => {
            println!("{}", usage());
            return 0;
        }
        [path] if !path.starts_with('-') => path.clone(),
        [dash, path] if dash == "--" => path.clone(),
        _ => {
            eprintln!("{COMMAND}: expected exactly one path\n{}", usage());
            return 2;
        }
    };
    let cwd = match std::env::current_dir() {
        Ok(cwd) => cwd,
        Err(error) => {
            eprintln!("{COMMAND}: cannot resolve cwd: {error}");
            return 2;
        }
    };
    let request = Request {
        cwd,
        session_id: crate::rm_tool::session_id_from_env(),
        role: std::env::var(crate::rm_tool::ROLE_ENV)
            .ok()
            .filter(|role| !role.is_empty())
            .unwrap_or_else(|| "agent".to_string()),
        now_unix: std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs() as i64)
            .unwrap_or(0),
    };
    let writer = DaemonWriter {
        state_dir: crate::daemon::default_state_dir().ok(),
    };
    match create(&path, &request, &writer) {
        Ok(created) => {
            println!("{}", created.display());
            0
        }
        Err(failure) => {
            eprintln!("{COMMAND}: {}", failure.message);
            failure.code
        }
    }
}

/// Create `raw` and record it. `Ok` is the canonical created path.
pub(crate) fn create(
    raw: &str,
    request: &Request,
    writer: &dyn LedgerWriter,
) -> Result<PathBuf, Failure> {
    #[cfg(not(unix))]
    {
        let _ = (raw, request, writer);
        Err(fail(
            2,
            "not supported on Windows: the creation ledger needs a device/inode identity, \
             which clud cannot read there, so safe-rm would refuse the directory; nothing \
             created. Use the session temp directory for scratch data instead.",
        ))
    }
    #[cfg(unix)]
    {
        unix::create(raw, request, writer)
    }
}

#[cfg(unix)]
mod unix {
    use super::*;
    use std::os::unix::fs::{DirBuilderExt, MetadataExt, OpenOptionsExt};
    use std::path::{Component, Path};

    /// The `(dev, ino)` of the directory this call made.
    type Identity = (u64, u64);

    pub(super) fn create(
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
                    "no clud session id (CLUD_SESSION_ID / CLAUDE_CODE_SESSION_ID); the \
                     creation ledger is per session, so nothing created",
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
        let now = std::fs::symlink_metadata(target)
            .map_err(|error| format!("cannot stat it: {error}"))?;
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
}

#[cfg(test)]
#[path = "safe_mktemp_tests.rs"]
mod tests;
