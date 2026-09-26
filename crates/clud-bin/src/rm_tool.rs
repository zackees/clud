//! `rm-file` / `rm-dir`: the one deletion path clud gives agents (#1340).
//!
//! `rm-file <path…>` removes files (and symlinks, as links); `rm-dir <path…>`
//! removes directories recursively. Both **trash by default**: every call
//! moves its paths into one entry under `~/.clud/trash/`, recoverable until
//! the daemon reaps it after [`TRASH_KEEP`]. `--purge` deletes for real,
//! `--tracked` allows git-tracked paths (normally `git rm`'s job), and
//! `--dry-run` only reports.
//!
//! Every path must resolve inside one of the caller's **roots**: the
//! `CLUD_RM_ROOTS` path list clud sets for a session, or, outside a session,
//! the current git checkout (the cwd outside a repo). A root that is a git
//! checkout also covers that repository's linked worktrees, so a `/grind`
//! integrator can clean its sibling worktree. Filesystem roots, `$HOME` and
//! its ancestors, a root itself, `.git`, and anything reached through a
//! symlinked parent that leaves the roots are always refused. Each call
//! writes one JSONL audit record under `~/.clud/state/logs/rm/`.
//!
//! The binaries are argv\[0\] aliases of `clud-shim` (see
//! `bin/clud_shim.rs`), installed next to the session's `rm` shim; `clud
//! rm-file` / `clud rm-dir` are the same code. The child `rm` shim
//! ([`crate::rm_guard`]) reuses [`resolve`] for its in-roots gate. See
//! [`docs/architecture/rm-tools.md`](../../../docs/architecture/rm-tools.md).

use std::collections::BTreeMap;
use std::ffi::OsStr;
use std::io::Write;
use std::path::{Component, Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

/// Path list (`std::env::split_paths` syntax) of the directories a session
/// may delete under. Set by clud for every session child.
pub const ROOTS_ENV: &str = "CLUD_RM_ROOTS";
/// Optional caller role recorded in the audit log.
pub const ROLE_ENV: &str = "CLUD_RM_ROLE";
/// Marks a trash entry as written by `rm-file` / `rm-dir`, and records what
/// it holds so it can be restored by hand.
pub const TRASH_MANIFEST: &str = ".clud-rm.json";
/// How long a trashed entry is kept before the daemon reaps it: the same 72
/// hours as `~/.clud/tmp` (see `gc::session_tmp::STALE_THRESHOLD`).
pub const TRASH_KEEP: Duration = crate::gc::session_tmp::STALE_THRESHOLD;

/// Which command is running.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    File,
    Dir,
}

impl Kind {
    pub fn command(self) -> &'static str {
        match self {
            Self::File => "rm-file",
            Self::Dir => "rm-dir",
        }
    }

    /// The command an argv\[0\] basename selects (`rm-file`, `rm-dir.exe`).
    pub fn from_program_name(name: &str) -> Option<Self> {
        let name = name.strip_suffix(".exe").unwrap_or(name);
        match name {
            "rm-file" => Some(Self::File),
            "rm-dir" => Some(Self::Dir),
            _ => None,
        }
    }
}

/// Parsed command line.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Options {
    pub purge: bool,
    pub tracked: bool,
    pub dry_run: bool,
    pub paths: Vec<String>,
}

fn usage(kind: Kind) -> String {
    let what = match kind {
        Kind::File => "files (a symlink is removed as a link)",
        Kind::Dir => "directories, recursively",
    };
    format!(
        "usage: {cmd} [--purge] [--tracked] [--dry-run] [--] <path>...\n\
         Remove {what}. Paths go to the clud trash (~/.clud/trash, kept {keep}h)\n\
         unless --purge is given, and must lie inside this session's roots\n\
         (${ROOTS_ENV}; outside a session, the current git checkout).\n\
         \n\
         \x20 --purge    delete for real instead of trashing\n\
         \x20 --tracked  allow git-tracked paths (otherwise use `git rm`)\n\
         \x20 --dry-run  report what would happen; change nothing",
        cmd = kind.command(),
        keep = TRASH_KEEP.as_secs() / 3_600,
    )
}

/// `Ok(None)` for `--help`.
pub fn parse_args(args: &[String]) -> Result<Option<Options>, String> {
    let mut options = Options::default();
    let mut flags = true;
    for arg in args {
        // Options come first: after the first path everything is a path, so
        // a file named `--purge` from a glob or `xargs` cannot switch modes.
        if flags && arg.starts_with('-') && arg != "-" {
            match arg.as_str() {
                "--" => flags = false,
                "--purge" => options.purge = true,
                "--tracked" => options.tracked = true,
                "--dry-run" | "-n" => options.dry_run = true,
                "--help" | "-h" => return Ok(None),
                other => {
                    return Err(format!(
                        "unknown option {other:?} (rm-file/rm-dir take only --purge, --tracked, \
                         --dry-run; put `--` before a path that starts with `-`)"
                    ))
                }
            }
            continue;
        }
        flags = false;
        options.paths.push(arg.clone());
    }
    if options.paths.is_empty() {
        return Err("no paths given".into());
    }
    Ok(Some(options))
}

/// Where a call may delete, resolved once per call.
#[derive(Debug, Clone)]
pub struct Roots {
    /// Canonical roots.
    pub roots: Vec<PathBuf>,
    /// Whether they came from [`ROOTS_ENV`] (a clud session).
    pub from_env: bool,
    /// Linked worktrees of the roots' repositories, found on first need.
    worktrees: Option<Vec<PathBuf>>,
}

impl Roots {
    /// Roots from `env_value` (the [`ROOTS_ENV`] value) or, when it is unset
    /// or empty, the nearest git checkout of `cwd` (the cwd outside a repo).
    /// Relative and unresolvable entries are dropped: a root must exist.
    pub fn resolve(env_value: Option<&OsStr>, cwd: &Path) -> Self {
        Self::resolve_with_home(env_value, cwd, home_dir().as_deref())
    }

    /// [`Roots::resolve`] with an explicit home: `$HOME` and its ancestors
    /// are never roots, whatever launched the session there.
    pub fn resolve_with_home(env_value: Option<&OsStr>, cwd: &Path, home: Option<&Path>) -> Self {
        let home = home.and_then(|h| std::fs::canonicalize(h).ok());
        let usable = |root: &PathBuf| home.as_ref().is_none_or(|home| !home.starts_with(root));
        if let Some(value) = env_value.filter(|v| !v.is_empty()) {
            let mut roots: Vec<PathBuf> = std::env::split_paths(value)
                .filter(|p| p.is_absolute())
                .filter_map(|p| std::fs::canonicalize(p).ok())
                .filter(usable)
                .collect();
            roots.dedup();
            return Self::fixed(roots, true);
        }
        let base = crate::block_bad_cmd::nearest_repo_root_public(cwd)
            .unwrap_or_else(|| cwd.to_path_buf());
        Self::fixed(
            std::fs::canonicalize(base)
                .into_iter()
                .filter(usable)
                .collect(),
            false,
        )
    }

    /// Exactly `roots` (assumed canonical), plus their repos' worktrees.
    pub fn fixed(roots: Vec<PathBuf>, from_env: bool) -> Self {
        Self {
            roots,
            from_env,
            worktrees: None,
        }
    }

    /// The deepest root strictly containing `path`, or why there is none.
    fn containing(&mut self, path: &Path) -> Result<PathBuf, String> {
        if let Some(root) = deepest_containing(&self.roots, path) {
            return Ok(root);
        }
        if let Some(root) = deepest_containing(self.worktrees(), path) {
            return Ok(root);
        }
        if self.roots.iter().any(|r| r == path) || self.worktrees().iter().any(|r| r == path) {
            return Err("is an allowed root itself".into());
        }
        let shown: Vec<String> = self.roots.iter().map(|r| r.display().to_string()).collect();
        Err(format!(
            "is outside the allowed roots ({})",
            if shown.is_empty() {
                "none".to_string()
            } else {
                shown.join(", ")
            }
        ))
    }

    fn worktrees(&mut self) -> &[PathBuf] {
        if self.worktrees.is_none() {
            let mut found = Vec::new();
            for root in &self.roots {
                // Only a root that is itself a checkout brings its worktrees;
                // a directory inside some repo (say, a dotfiles `$HOME`) must
                // not widen to that repo.
                let is_checkout = crate::block_bad_cmd::nearest_repo_root_public(root)
                    .and_then(|r| std::fs::canonicalize(r).ok())
                    .is_some_and(|r| &r == root);
                if !is_checkout {
                    continue;
                }
                let Ok(text) =
                    crate::worktrees::run_git(root, &["worktree", "list", "--porcelain"])
                else {
                    continue;
                };
                for line in text.lines() {
                    if let Some(path) = line.strip_prefix("worktree ") {
                        if let Ok(path) = std::fs::canonicalize(path.trim()) {
                            let home = home_dir().and_then(|h| std::fs::canonicalize(h).ok());
                            if home.as_ref().is_some_and(|home| home.starts_with(&path)) {
                                continue;
                            }
                            if !found.contains(&path) {
                                found.push(path);
                            }
                        }
                    }
                }
            }
            self.worktrees = Some(found);
        }
        self.worktrees.as_deref().unwrap_or_default()
    }
}

fn deepest_containing(roots: &[PathBuf], path: &Path) -> Option<PathBuf> {
    roots
        .iter()
        .filter(|root| path.starts_with(root) && path != root.as_path())
        .max_by_key(|root| root.components().count())
        .cloned()
}

/// A path that exists and may be deleted.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Target {
    /// Canonical parent joined with the final name (not followed).
    pub path: PathBuf,
    /// The root that contains it.
    pub root: PathBuf,
    pub is_dir: bool,
    pub is_symlink: bool,
}

/// What [`resolve`] found for one operand.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Resolved {
    Present(Target),
    /// Nothing there; carries the absolute (lexical) path.
    Missing(PathBuf),
}

/// Resolve one operand against `cwd` and check it may be deleted.
///
/// The parent is canonicalized (so a symlinked directory on the way is
/// followed and must itself land inside the roots); the final name is never
/// followed, so deleting a symlink removes the link, not its target.
pub fn resolve(
    raw: &str,
    cwd: &Path,
    home: Option<&Path>,
    roots: &mut Roots,
) -> Result<Resolved, String> {
    if raw.is_empty() {
        return Err("empty path".into());
    }
    // `Path::components` drops a trailing `.`, so check the operand as typed.
    let last = raw.trim_end_matches(['/', '\\']).rsplit(['/', '\\']).next();
    if matches!(last, Some("." | "..")) {
        return Err("does not name a file or directory (`.` or `..`); name it directly".into());
    }
    let absolute = if Path::new(raw).is_absolute() {
        PathBuf::from(raw)
    } else {
        cwd.join(raw)
    };
    let Some(Component::Normal(name)) = absolute.components().next_back() else {
        return Err(
            "does not name a file or directory (a root, `.` or `..`); name it directly".into(),
        );
    };
    let name = name.to_os_string();
    if absolute
        .components()
        .any(|c| c.as_os_str().to_string_lossy().eq_ignore_ascii_case(".git"))
    {
        return Err("is git metadata".into());
    }
    let parent = absolute.parent().ok_or("has no parent directory")?;
    let parent = match std::fs::canonicalize(parent) {
        Ok(parent) => parent,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            // Still hold a missing path to the roots: on Windows `/etc/x`
            // becomes `C:\etc\x`, whose parent is usually absent.
            let path = canonical_with_missing_tail(&absolute)?;
            roots.containing(&path)?;
            return Ok(Resolved::Missing(path));
        }
        Err(error) => return Err(format!("cannot resolve its directory: {error}")),
    };
    let path = parent.join(&name);
    if let Some(home) = home.and_then(|h| std::fs::canonicalize(h).ok()) {
        if home.starts_with(&path) {
            return Err("is your home directory or one of its ancestors".into());
        }
    }
    let root = roots.containing(&path)?;
    let meta = match std::fs::symlink_metadata(&path) {
        Ok(meta) => meta,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok(Resolved::Missing(path));
        }
        Err(error) => return Err(format!("cannot stat: {error}")),
    };
    let is_symlink = meta.file_type().is_symlink();
    let is_dir = !is_symlink && meta.is_dir();
    if is_dir {
        reject_mounts(&path)?;
    }
    Ok(Resolved::Present(Target {
        path,
        root,
        is_dir,
        is_symlink,
    }))
}

/// `path` with its deepest existing ancestor canonicalized and the missing
/// components re-appended, so it compares against the canonical roots.
fn canonical_with_missing_tail(path: &Path) -> Result<PathBuf, String> {
    let mut tail = Vec::new();
    let mut ancestor = path;
    loop {
        match std::fs::canonicalize(ancestor) {
            Ok(base) => return Ok(tail.iter().rev().fold(base, |acc, part| acc.join(part))),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                tail.push(ancestor.file_name().ok_or("has no existing ancestor")?);
                ancestor = ancestor.parent().ok_or("has no existing ancestor")?;
            }
            Err(error) => return Err(format!("cannot resolve its directory: {error}")),
        }
    }
}

#[cfg(target_os = "linux")]
fn reject_mounts(target: &Path) -> Result<(), String> {
    let Ok(mounts) = std::fs::read_to_string("/proc/self/mountinfo") else {
        return Ok(());
    };
    for line in mounts.lines() {
        let Some(field) = line.split_whitespace().nth(4) else {
            continue;
        };
        let mount = field
            .replace("\\040", " ")
            .replace("\\011", "\t")
            .replace("\\012", "\n")
            .replace("\\134", "\\");
        if Path::new(&mount).starts_with(target) {
            return Err(format!("contains the mount point {mount}"));
        }
    }
    Ok(())
}

#[cfg(not(target_os = "linux"))]
fn reject_mounts(_: &Path) -> Result<(), String> {
    Ok(())
}

/// Whether `target` is, or contains, a git-tracked path.
pub fn is_tracked(target: &Target) -> bool {
    let (Some(parent), Some(name)) = (target.path.parent(), target.path.file_name()) else {
        return false;
    };
    if crate::block_bad_cmd::nearest_repo_root_public(parent).is_none() {
        return false;
    }
    let name = name.to_string_lossy();
    crate::worktrees::run_git(parent, &["ls-files", "--error-unmatch", "--", &name]).is_ok()
}

/// Everything a call needs from its environment, injectable for tests.
#[derive(Debug, Clone)]
pub struct Context {
    pub cwd: PathBuf,
    pub home: Option<PathBuf>,
    pub trash_root: PathBuf,
    /// `~/.clud/state/logs/rm`; `None` skips the audit record.
    pub audit_dir: Option<PathBuf>,
    pub session_id: Option<String>,
    pub role: String,
    /// Register the trash entry with the daemon's GC registry.
    pub register: bool,
    pub now: SystemTime,
}

impl Context {
    pub fn from_env() -> Result<(Self, Roots), String> {
        let cwd = std::env::current_dir().map_err(|e| format!("cannot resolve cwd: {e}"))?;
        let home = home_dir();
        let trash_root = crate::daemon::default_trash_dir()
            .map_err(|e| format!("cannot resolve the clud trash: {e}"))?;
        let audit_dir = crate::daemon::default_state_dir()
            .ok()
            .map(|state| state.join("logs").join("rm"));
        let roots_env = std::env::var_os(ROOTS_ENV);
        let roots = Roots::resolve(roots_env.as_deref(), &cwd);
        let role = std::env::var(ROLE_ENV)
            .ok()
            .filter(|r| !r.is_empty())
            .unwrap_or_else(|| if roots.from_env { "agent" } else { "user" }.to_string());
        let session_id = ["CLUD_SESSION_ID", crate::grind_facts::SESSION_ENV]
            .iter()
            .find_map(|key| std::env::var(key).ok().filter(|v| !v.is_empty()));
        Ok((
            Self {
                cwd,
                home,
                trash_root,
                audit_dir,
                session_id,
                role,
                register: true,
                now: SystemTime::now(),
            },
            roots,
        ))
    }
}

fn home_dir() -> Option<PathBuf> {
    let key = if cfg!(windows) { "USERPROFILE" } else { "HOME" };
    std::env::var_os(key)
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
}

/// One path's outcome, for output and the audit record.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Outcome {
    pub path: String,
    pub action: &'static str,
    pub reason: Option<String>,
    pub trash_path: Option<PathBuf>,
}

/// `rm-file` / `rm-dir` from the process environment. Returns the exit code.
pub fn run(kind: Kind, args: &[String]) -> i32 {
    let options = match parse_args(args) {
        Ok(Some(options)) => options,
        Ok(None) => {
            println!("{}", usage(kind));
            return 0;
        }
        Err(error) => {
            eprintln!("{}: {error}\n{}", kind.command(), usage(kind));
            return 2;
        }
    };
    let (ctx, mut roots) = match Context::from_env() {
        Ok(found) => found,
        Err(error) => {
            eprintln!("{}: {error}", kind.command());
            return 1;
        }
    };
    let mut out = std::io::stdout().lock();
    let mut err = std::io::stderr().lock();
    run_with(kind, &options, &ctx, &mut roots, &mut out, &mut err)
}

/// The testable core of [`run`].
pub fn run_with(
    kind: Kind,
    options: &Options,
    ctx: &Context,
    roots: &mut Roots,
    out: &mut dyn Write,
    err: &mut dyn Write,
) -> i32 {
    let cmd = kind.command();
    let mut outcomes: Vec<Outcome> = Vec::new();
    let mut accepted: Vec<Target> = Vec::new();
    for raw in &options.paths {
        let refuse = |reason: String| Outcome {
            path: raw.clone(),
            action: "refused",
            reason: Some(reason),
            trash_path: None,
        };
        let target = match resolve(raw, &ctx.cwd, ctx.home.as_deref(), roots) {
            Ok(Resolved::Present(target)) => target,
            Ok(Resolved::Missing(path)) => {
                // `find … -exec rm-dir {} +` can list a path whose ancestor
                // this same call already took: that is not an error.
                if accepted.iter().any(|t| path.starts_with(&t.path)) {
                    outcomes.push(Outcome {
                        path: raw.clone(),
                        action: "skipped",
                        reason: Some("already inside a removed directory".into()),
                        trash_path: None,
                    });
                } else {
                    outcomes.push(refuse("no such file or directory".into()));
                }
                continue;
            }
            Err(reason) => {
                outcomes.push(refuse(reason));
                continue;
            }
        };
        if accepted.iter().any(|t| target.path.starts_with(&t.path)) {
            outcomes.push(Outcome {
                path: raw.clone(),
                action: "skipped",
                reason: Some("already inside a removed directory".into()),
                trash_path: None,
            });
            continue;
        }
        match kind {
            Kind::File if target.is_dir => {
                outcomes.push(refuse("is a directory; use rm-dir".into()));
                continue;
            }
            Kind::Dir if !target.is_dir => {
                outcomes.push(refuse(if target.is_symlink {
                    "is a symlink; use rm-file to remove the link".into()
                } else {
                    "is not a directory; use rm-file".into()
                }));
                continue;
            }
            _ => {}
        }
        if !options.tracked && is_tracked(&target) {
            outcomes.push(refuse(if target.is_dir {
                "contains git-tracked files; use `git rm -r`, or pass --tracked".into()
            } else {
                "is tracked by git; use `git rm`, or pass --tracked".into()
            }));
            continue;
        }
        // `find -depth` lists children first: the ancestor takes them over.
        accepted.retain(|t| {
            let inside = t.path.starts_with(&target.path);
            if inside {
                outcomes.push(Outcome {
                    path: t.path.display().to_string(),
                    action: "skipped",
                    reason: Some("inside a directory removed by the same call".into()),
                    trash_path: None,
                });
            }
            !inside
        });
        accepted.push(target);
    }

    let mut entry: Option<PathBuf> = None;
    let mut root_dirs: BTreeMap<PathBuf, PathBuf> = BTreeMap::new();
    let mut moved: Vec<(String, PathBuf)> = Vec::new();
    for target in &accepted {
        let shown = target.path.display().to_string();
        let outcome = if options.dry_run {
            Outcome {
                path: shown.clone(),
                action: if options.purge {
                    "would-purge"
                } else {
                    "would-trash"
                },
                reason: None,
                trash_path: None,
            }
        } else if options.purge {
            crate::gc::delete_audit::record("rm-tool.purge", &target.path, cmd);
            match purge(target) {
                Ok(()) => Outcome {
                    path: shown.clone(),
                    action: "purged",
                    reason: None,
                    trash_path: None,
                },
                Err(error) => Outcome {
                    path: shown.clone(),
                    action: "failed",
                    reason: Some(error),
                    trash_path: None,
                },
            }
        } else {
            let result = entry_dir(&mut entry, &ctx.trash_root, target, ctx.now).and_then(|dir| {
                let dest = dir.join(root_dir_name(&mut root_dirs, &target.root)).join(
                    target
                        .path
                        .strip_prefix(&target.root)
                        .unwrap_or(&target.path),
                );
                move_into_trash(&target.path, &dest).map(|()| dest)
            });
            match result {
                Ok(dest) => {
                    moved.push((shown.clone(), dest.clone()));
                    Outcome {
                        path: shown.clone(),
                        action: "trashed",
                        reason: None,
                        trash_path: Some(dest),
                    }
                }
                Err(error) => Outcome {
                    path: shown.clone(),
                    action: "failed",
                    reason: Some(error),
                    trash_path: None,
                },
            }
        };
        outcomes.push(outcome);
    }

    if let Some(dir) = &entry {
        if moved.is_empty() {
            let _ = std::fs::remove_dir_all(dir);
        } else if write_manifest(dir, kind, ctx, &moved) {
            if ctx.register {
                register_entry(dir, &moved[0].0, ctx.now);
            }
        } else {
            // Without its manifest the daemon would reap the entry at once,
            // so leave it unregistered and say where the files went.
            let _ = writeln!(
                err,
                "{cmd}: could not write {} in {}; the files are there but not kept for {}h",
                TRASH_MANIFEST,
                dir.display(),
                TRASH_KEEP.as_secs() / 3_600
            );
        }
    }

    let mut failed = false;
    for outcome in &outcomes {
        match outcome.action {
            "refused" | "failed" => {
                failed = true;
                let _ = writeln!(
                    err,
                    "{cmd}: {} {}: {}",
                    outcome.action,
                    outcome.path,
                    outcome.reason.as_deref().unwrap_or("")
                );
            }
            "trashed" => {
                let _ = writeln!(
                    out,
                    "trashed {} -> {}",
                    outcome.path,
                    outcome
                        .trash_path
                        .as_deref()
                        .map(|p| p.display().to_string())
                        .unwrap_or_default()
                );
            }
            "skipped" => {}
            action => {
                let _ = writeln!(out, "{action} {}", outcome.path);
            }
        }
    }
    let code = i32::from(failed);
    if let Some(dir) = &ctx.audit_dir {
        audit(dir, kind, options, ctx, roots, &outcomes, code);
    }
    code
}

fn purge(target: &Target) -> Result<(), String> {
    let result = if target.is_dir {
        std::fs::remove_dir_all(&target.path)
    } else {
        remove_link_or_file(&target.path)
    };
    result.map_err(|e| e.to_string())
}

/// Remove a file or a symlink (on Windows a directory symlink or junction
/// needs `remove_dir`, which removes the link, never its target).
pub fn remove_link_or_file(path: &Path) -> std::io::Result<()> {
    match std::fs::remove_file(path) {
        Err(error) if cfg!(windows) => std::fs::remove_dir(path).map_err(|_| error),
        other => other,
    }
}

/// The call's trash entry, created on first use:
/// `<trash>/<utc>-<hex>-<first name>`.
fn entry_dir(
    entry: &mut Option<PathBuf>,
    trash_root: &Path,
    first: &Target,
    now: SystemTime,
) -> Result<PathBuf, String> {
    if let Some(dir) = entry {
        return Ok(dir.clone());
    }
    std::fs::create_dir_all(trash_root)
        .map_err(|e| format!("create {}: {e}", trash_root.display()))?;
    let since = now.duration_since(UNIX_EPOCH).unwrap_or_default();
    let name: String = first
        .path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default()
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || "._-".contains(c) {
                c
            } else {
                '_'
            }
        })
        .take(48)
        .collect();
    for _ in 0..32 {
        let dir = trash_root.join(format!(
            "{}-{}-{name}",
            crate::trash::timestamp_utcish(since.as_secs() as i64),
            crate::trash::randomish_hex(since)
        ));
        match std::fs::create_dir(&dir) {
            Ok(()) => {
                *entry = Some(dir.clone());
                return Ok(dir);
            }
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(error) => return Err(format!("create {}: {error}", dir.display())),
        }
    }
    Err("could not allocate a unique trash entry".into())
}

/// A distinct directory name per root inside one entry (`repo`, `repo-2`).
fn root_dir_name(names: &mut BTreeMap<PathBuf, PathBuf>, root: &Path) -> PathBuf {
    if let Some(name) = names.get(root) {
        return name.clone();
    }
    let base = root
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| "root".into());
    let mut candidate = PathBuf::from(&base);
    let mut n = 2;
    while names.values().any(|v| v == &candidate) {
        candidate = PathBuf::from(format!("{base}-{n}"));
        n += 1;
    }
    names.insert(root.to_path_buf(), candidate.clone());
    candidate
}

/// Rename `source` to `dest`; across filesystems, copy (never following
/// symlinks) and then remove the source.
fn move_into_trash(source: &Path, dest: &Path) -> Result<(), String> {
    if let Some(parent) = dest.parent() {
        std::fs::create_dir_all(parent).map_err(|e| format!("create {}: {e}", parent.display()))?;
    }
    match std::fs::rename(source, dest) {
        Ok(()) => Ok(()),
        Err(error) if crate::trash::is_cross_volume_error(&error) => {
            copy_nofollow(source, dest).map_err(|e| format!("copy to the trash: {e}"))?;
            let meta = std::fs::symlink_metadata(source).map_err(|e| e.to_string())?;
            if meta.is_dir() {
                std::fs::remove_dir_all(source)
            } else {
                remove_link_or_file(source)
            }
            .map_err(|e| format!("remove after copying to the trash: {e}"))
        }
        Err(error) => Err(format!("move to the trash: {error}")),
    }
}

fn copy_nofollow(source: &Path, dest: &Path) -> std::io::Result<()> {
    let meta = std::fs::symlink_metadata(source)?;
    if meta.file_type().is_symlink() {
        let link = std::fs::read_link(source)?;
        #[cfg(unix)]
        return std::os::unix::fs::symlink(link, dest);
        #[cfg(windows)]
        return if std::fs::metadata(source).is_ok_and(|m| m.is_dir()) {
            std::os::windows::fs::symlink_dir(link, dest)
        } else {
            std::os::windows::fs::symlink_file(link, dest)
        };
    }
    if meta.is_dir() {
        std::fs::create_dir_all(dest)?;
        for entry in std::fs::read_dir(source)? {
            let entry = entry?;
            copy_nofollow(&entry.path(), &dest.join(entry.file_name()))?;
        }
        return Ok(());
    }
    std::fs::copy(source, dest).map(|_| ())
}

fn write_manifest(dir: &Path, kind: Kind, ctx: &Context, moved: &[(String, PathBuf)]) -> bool {
    let items: Vec<_> = moved
        .iter()
        .map(|(origin, trashed)| serde_json::json!({"origin": origin, "trashed": trashed}))
        .collect();
    let manifest = serde_json::json!({
        "version": 1,
        "command": kind.command(),
        "ts_unix": unix_secs(ctx.now),
        "cwd": ctx.cwd,
        "session_id": ctx.session_id,
        "role": ctx.role,
        "items": items,
    });
    serde_json::to_vec_pretty(&manifest)
        .is_ok_and(|bytes| std::fs::write(dir.join(TRASH_MANIFEST), bytes).is_ok())
}

/// Best effort: never starts a daemon; the daemon's sweep also finds an
/// unregistered entry by its manifest.
fn register_entry(dir: &Path, origin: &str, now: SystemTime) {
    let Ok(state_dir) = crate::daemon::default_state_dir() else {
        return;
    };
    let input = crate::gc::InsertInput {
        kind: "trash".to_string(),
        path: dir.to_string_lossy().into_owned(),
        repo_root: None,
        branch: None,
        agent_id: Some(origin.to_string()),
        now_unix: unix_secs(now),
    };
    let _ = crate::daemon::gc_client_insert(&state_dir, &input);
}

/// Whether a trash entry written by `rm-file` / `rm-dir` must still be kept
/// at `now`. Entries without the manifest (`clud trash` quarantine) are
/// never kept by this rule.
pub fn keep_trash_entry(dir: &Path, now: SystemTime) -> bool {
    let Ok(meta) = std::fs::metadata(dir.join(TRASH_MANIFEST)) else {
        return false;
    };
    meta.modified()
        .ok()
        .and_then(|mtime| now.duration_since(mtime).ok())
        .is_none_or(|age| age < TRASH_KEEP)
}

/// Trash entries written by `rm-file` / `rm-dir` that are due for removal:
/// manifest-bearing directories under `trash_root` older than
/// [`TRASH_KEEP`]. The daemon reaps these even when registration failed.
pub fn expired_trash_entries(trash_root: &Path, now: SystemTime) -> Vec<PathBuf> {
    let Ok(entries) = std::fs::read_dir(trash_root) else {
        return Vec::new();
    };
    entries
        .filter_map(Result::ok)
        .map(|e| e.path())
        .filter(|dir| dir.join(TRASH_MANIFEST).is_file() && !keep_trash_entry(dir, now))
        .collect()
}

fn unix_secs(t: SystemTime) -> i64 {
    t.duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

/// Append one JSONL record for this call to `<dir>/<YYYY-MM-DD>.jsonl`.
/// Best effort: auditing never changes the outcome.
fn audit(
    dir: &Path,
    kind: Kind,
    options: &Options,
    ctx: &Context,
    roots: &Roots,
    outcomes: &[Outcome],
    code: i32,
) {
    let paths: Vec<_> = outcomes
        .iter()
        .map(|o| {
            serde_json::json!({
                "path": o.path,
                "action": o.action,
                "reason": o.reason,
                "trash_path": o.trash_path,
            })
        })
        .collect();
    let record = serde_json::json!({
        "ts_unix": unix_secs(ctx.now),
        "command": kind.command(),
        "session_id": ctx.session_id,
        "role": ctx.role,
        "cwd": ctx.cwd,
        "roots": roots.roots,
        "purge": options.purge,
        "tracked": options.tracked,
        "dry_run": options.dry_run,
        "paths": paths,
        "exit": code,
    });
    append_audit(dir, ctx.now, &record);
}

/// Append `record` to the day's audit file under `dir`.
pub fn append_audit(dir: &Path, now: SystemTime, record: &serde_json::Value) {
    let fields = crate::civil_time::civil_from_unix_secs(unix_secs(now));
    let file = dir.join(format!(
        "{:04}-{:02}-{:02}.jsonl",
        fields.0, fields.1, fields.2
    ));
    let _ = std::fs::create_dir_all(dir);
    if let Ok(mut handle) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(file)
    {
        let _ = handle.write_all(format!("{record}\n").as_bytes());
    }
}

/// The [`ROOTS_ENV`] value for a session launched from `cwd`: its git
/// checkout (the cwd outside a repo) and clud's session temp directory,
/// which holds every agent scratchpad. `None` when neither resolves.
pub fn session_roots_value(cwd: &Path) -> Option<String> {
    let base =
        crate::block_bad_cmd::nearest_repo_root_public(cwd).unwrap_or_else(|| cwd.to_path_buf());
    let mut roots: Vec<PathBuf> = vec![base];
    if let Some(tmp) = crate::gc::session_tmp::session_tmp_dir() {
        roots.push(tmp);
    }
    let home = home_dir().and_then(|h| std::fs::canonicalize(h).ok());
    let roots: Vec<PathBuf> = roots
        .into_iter()
        .filter(|p| p.is_absolute() && p.is_dir())
        .filter(|p| {
            let canonical = std::fs::canonicalize(p).unwrap_or_else(|_| p.clone());
            home.as_ref()
                .is_none_or(|home| !home.starts_with(&canonical))
        })
        .collect();
    if roots.is_empty() {
        return None;
    }
    std::env::join_paths(roots)
        .ok()
        .map(|v| v.to_string_lossy().into_owned())
}

#[cfg(test)]
#[path = "rm_tool_tests.rs"]
mod tests;
