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
pub(crate) trait LedgerWriter {
    fn record(&self, entry: &CreatedEntry) -> Result<(), String>;
}

/// The daemon's GC registry (`GcOp::InsertCreated`).
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

    /// RED stub (#1667): the creator is not implemented yet.
    pub(super) fn create(
        _raw: &str,
        _request: &Request,
        _writer: &dyn LedgerWriter,
    ) -> Result<PathBuf, Failure> {
        Err(fail(1, "not implemented"))
    }
}

#[cfg(test)]
#[path = "safe_mktemp_tests.rs"]
mod tests;
