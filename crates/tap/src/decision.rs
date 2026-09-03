//! The pure argv -> decision function (#1067, Phase 4 of #1064).
//!
//! Everything before this phase reasons about command *text*, before the shell
//! expands it. `block_bad_cmd_rm_vars` is 1100 lines of abstract
//! interpretation trying to prove what `$SP` will become, and every parser bug
//! in it is a bypass. #1064 also showed the hook can simply fail to run.
//!
//! This runs *after* expansion. `$SP` has already become `/`; there is nothing
//! left to prove, only to observe. That vantage point is the whole reason the
//! wrapper is worth its ergonomic cost.
//!
//! Kept pure and filesystem-free on purpose. The issue is explicit: "`tap`'s
//! refusal logic is a pure argv -> decision function and must be tested as
//! one. Never validate a removal guard by performing a removal -- not on a
//! host, not in a container." A function that stats paths could not be tested
//! that way, so it does not stat them.

use std::path::{Component, Path, PathBuf};

/// What `tap` should do with an argv.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Decision {
    /// Run it. The argv is either not a removal, or every target is inside the
    /// session root.
    Exec,
    /// Refuse, with a message that says what to do instead.
    ///
    /// The wording matters more than usual: the reader is an agent that must
    /// restate the command, and a bare "denied" produces another guess.
    Refuse(String),
}

/// Programs whose operands are things to delete.
///
/// Deliberately short. v0 guards removals -- the incident class #1064 was
/// filed about -- and a longer list would imply coverage that has not been
/// thought through. `mv` overwrites and `dd` destroys, but their argument
/// grammars differ enough that guessing at them is how a guard acquires false
/// confidence.
const REMOVAL_PROGRAMS: &[&str] = &["rm", "rmdir", "unlink"];

/// Decide what to do with `argv` (the program and its already-expanded
/// arguments), for a session rooted at `session_root`.
///
/// `cwd` resolves relative operands; `home` is refused outright when a
/// removal names it. Both are passed in rather than read from the process so
/// the whole function stays testable without touching a filesystem.
#[must_use]
pub fn classify(argv: &[String], cwd: &Path, session_root: &Path, home: Option<&Path>) -> Decision {
    let Some(program) = argv.first() else {
        // Fails closed: an empty argv is not "nothing to do", it is a caller
        // this function does not understand.
        return Decision::Refuse(
            "tap: no command given. Usage: tap <program> [args...]".to_string(),
        );
    };

    if !is_removal(program) {
        return Decision::Exec;
    }

    let operands = removal_operands(&argv[1..]);
    if operands.is_empty() {
        // `rm -rf` with no target is either a typo or an unexpanded variable
        // that became nothing -- which is exactly the #1064 shape, one step
        // before it becomes `/`.
        return Decision::Refuse(format!(
            "tap: refusing `{program}` with no target.\n\
             An empty operand list usually means a variable expanded to \
             nothing. Name the path explicitly."
        ));
    }

    for operand in &operands {
        let resolved = resolve(operand, cwd);
        if let Some(reason) = forbidden_target(&resolved, session_root, home) {
            return Decision::Refuse(format!(
                "tap: refusing `{program} {operand}`.\n\
                 {reason}\n\
                 Resolved to: {}\n\
                 Session root: {}\n\
                 Restate the command with a path inside the session root.",
                resolved.display(),
                session_root.display()
            ));
        }
    }

    Decision::Exec
}

fn is_removal(program: &str) -> bool {
    // Compare on the file name so `/bin/rm` and `rm` are the same program.
    // `Path::file_name` is not enough on Unix for a Windows-style path, and
    // this argv may have been written on either.
    let name = program.rsplit(['/', '\\']).next().unwrap_or(program);
    let name = name.strip_suffix(".exe").unwrap_or(name);
    REMOVAL_PROGRAMS.contains(&name)
}

/// The operands of a removal: everything that is not a flag.
///
/// `--` ends flag parsing, per POSIX, and everything after it is an operand
/// even if it starts with `-`. Getting that wrong in the permissive direction
/// would skip a target; getting it wrong in the strict direction would only
/// refuse more, so the ambiguity is resolved toward refusing.
fn removal_operands(args: &[String]) -> Vec<String> {
    let mut operands = Vec::new();
    let mut flags_done = false;
    for arg in args {
        if !flags_done && arg == "--" {
            flags_done = true;
            continue;
        }
        if !flags_done && arg.starts_with('-') && arg.len() > 1 {
            continue;
        }
        operands.push(arg.clone());
    }
    operands
}

/// Make `operand` absolute against `cwd` and fold away `.` and `..`.
///
/// Lexical only -- no `canonicalize`, which would touch the filesystem and
/// resolve symlinks. That is a real limitation and it is the conservative
/// direction: a symlink pointing out of the session root is not caught here,
/// which is why the in-agent hook stays in place (the issue's "coverage is
/// per-session-root" residual risk).
fn resolve(operand: &str, cwd: &Path) -> PathBuf {
    let raw = Path::new(operand);
    let joined = if raw.is_absolute() {
        raw.to_path_buf()
    } else {
        cwd.join(raw)
    };

    let mut out = PathBuf::new();
    for component in joined.components() {
        match component {
            Component::ParentDir => {
                // Popping past the root leaves the root, which matches how the
                // kernel treats `/..`.
                out.pop();
            }
            Component::CurDir => {}
            other => out.push(other.as_os_str()),
        }
    }
    out
}

/// Why this target must not be removed, or `None` if it is allowed.
fn forbidden_target(resolved: &Path, session_root: &Path, home: Option<&Path>) -> Option<String> {
    if is_filesystem_root(resolved) {
        return Some(
            "That resolves to a filesystem root. This is the failure #1064 \
             was filed about: an unset variable expanding to nothing."
                .to_string(),
        );
    }
    if let Some(home) = home {
        if resolved == home {
            return Some("That resolves to the home directory.".to_string());
        }
    }
    if !is_within(resolved, session_root) {
        return Some(
            "That resolves outside the session root. tap only permits \
             removals inside the session it is gating."
                .to_string(),
        );
    }
    None
}

/// Whether `path` is a filesystem root: `/` on Unix, or a bare drive/UNC root
/// on Windows.
///
/// Checked structurally rather than by string compare, so `C:\`, `C:/`, and
/// `\\?\C:\` are all the same answer.
fn is_filesystem_root(path: &Path) -> bool {
    let mut components = path.components();
    match components.next() {
        Some(Component::RootDir) | Some(Component::Prefix(_)) => {}
        _ => return false,
    }
    // A prefix may be followed by exactly one RootDir and still be a root.
    for component in components {
        match component {
            Component::RootDir => {}
            _ => return false,
        }
    }
    true
}

/// Whether `path` is `root` itself or lives beneath it.
///
/// Component-wise, not string prefix: `/srv/project-old` must not count as
/// inside `/srv/project`.
fn is_within(path: &Path, root: &Path) -> bool {
    let mut path_components = path.components();
    for root_component in root.components() {
        match path_components.next() {
            Some(component) if component == root_component => {}
            _ => return false,
        }
    }
    true
}

#[cfg(test)]
#[path = "decision_tests.rs"]
mod tests;
