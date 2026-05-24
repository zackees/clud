//! `--clean-worktrees` implementation — issue #83.
//!
//! Long-running `clud` use accumulates git worktrees (one per agent run,
//! plus manual debugging worktrees). This module enumerates them via
//! `git worktree list --porcelain`, classifies each as
//! clean / dirty / unpushed / no-upstream / branch-gone, and removes those
//! that are safe to remove. "Safe" means **clean** AND
//! (older than `--stale-after` OR upstream `[gone]`).
//!
//! The flow is intentionally side-effect free until we actually invoke
//! `git worktree remove`, so a `--dry-run` is a faithful preview of what a
//! real run would do.
//!
//! No new external crates — we shell out to `git` and parse text.

use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use running_process::{NativeProcess, ProcessConfig, ReadStatus, StderrMode, StdinMode};

use crate::subprocess;
use crate::win_creation_flags::invisible_helper_creationflags;

/// One row of the porcelain output from `git worktree list --porcelain`.
///
/// We only retain fields we actually use. `bare` worktrees and the main
/// worktree itself are flagged so we never try to remove them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorktreeEntry {
    pub path: PathBuf,
    pub head: Option<String>,
    pub branch: Option<String>,
    pub bare: bool,
    pub detached: bool,
    /// True when the porcelain block ends with a `locked` line.
    /// Per `git-worktree(1)` docs we must never remove these even with
    /// `--force`.
    pub locked: bool,
    /// Optional human-readable reason that accompanies a `locked` line.
    /// Claude Code emits `locked claude agent agent-<id> (pid <pid>)`
    /// when it holds a worktree for an in-flight agent; the GC
    /// liveness probe parses the pid out of this string.
    pub locked_reason: Option<String>,
    /// True when the porcelain block ends with a `prunable` line.
    /// Indicates git already considers the worktree stale; we use this
    /// as a hint but still apply our normal classification.
    pub prunable: bool,
}

/// Result of inspecting a worktree's working tree + upstream.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WorktreeStatus {
    /// Clean working tree, upstream exists, no unpushed commits.
    Clean,
    /// `git status --porcelain` produced output.
    Dirty,
    /// Working tree clean but `@{u}..HEAD` is non-empty.
    Unpushed,
    /// `HEAD` has no upstream configured.
    NoUpstream,
    /// `git branch -vv` reports the upstream as `[gone]` (deleted on remote).
    BranchGone,
}

impl WorktreeStatus {
    pub fn as_str(&self) -> &'static str {
        match self {
            WorktreeStatus::Clean => "clean",
            WorktreeStatus::Dirty => "dirty",
            WorktreeStatus::Unpushed => "unpushed",
            WorktreeStatus::NoUpstream => "no-upstream",
            WorktreeStatus::BranchGone => "branch-gone",
        }
    }
}

/// Inputs to the stale-detection predicate. Kept as a plain struct so the
/// logic is trivial to unit-test without touching disk or `git`.
#[derive(Debug, Clone, Copy)]
pub struct StalenessInputs {
    pub status: WorktreeStatus,
    /// Age of the worktree's working directory (mtime delta).
    pub age: Duration,
    /// Locked worktrees are never stale (skipped from removal entirely).
    pub locked: bool,
}

/// Options gathered from the CLI flags.
#[derive(Debug, Clone)]
pub struct CleanOptions {
    pub stale_after: Duration,
    pub dry_run: bool,
    pub yes: bool,
    pub force: bool,
}

/// `--clean-worktrees` entry point. Returns the process exit code.
pub fn run(opts: &CleanOptions) -> i32 {
    let main_repo = match locate_main_repo_root() {
        Ok(p) => p,
        Err(e) => {
            eprintln!("error: {e}");
            return 1;
        }
    };

    let raw = match run_git(&main_repo, &["worktree", "list", "--porcelain"]) {
        Ok(out) => out,
        Err(e) => {
            eprintln!("error: failed to list worktrees: {e}");
            return 1;
        }
    };
    let entries = parse_worktree_porcelain(&raw);

    // Gather classified rows. The first entry from `git worktree list` is the
    // main worktree — never a candidate for removal regardless of staleness.
    let mut rows: Vec<Classified> = Vec::new();
    for (idx, entry) in entries.iter().enumerate() {
        if entry.bare {
            continue;
        }
        let is_main = idx == 0;
        let status = if is_main {
            // Don't probe the main worktree; it's never a removal candidate
            // and inspecting it is wasted work.
            WorktreeStatus::Clean
        } else {
            classify_status(&entry.path)
        };
        let age = path_age(&entry.path).unwrap_or(Duration::ZERO);
        rows.push(Classified {
            entry: entry.clone(),
            status,
            age,
            is_main,
        });
    }

    // Print a status table for visibility.
    print_table(&rows, opts);

    // Decide actions.
    let plan = build_plan(&rows, opts);
    if plan.candidates.is_empty() {
        println!("\nNo worktrees match the removal criteria.");
        return 0;
    }

    println!("\nRemoval plan ({} candidate(s)):", plan.candidates.len());
    for c in &plan.candidates {
        println!("  remove  {}  ({})", c.entry.path.display(), c.reason);
    }
    if !plan.skipped.is_empty() {
        println!("\nSkipped ({}):", plan.skipped.len());
        for s in &plan.skipped {
            println!("  skip    {}  ({})", s.entry.path.display(), s.reason);
        }
    }

    if opts.dry_run {
        println!("\n--dry-run: no changes made.");
        return 0;
    }

    if !opts.yes && !confirm_interactive(plan.candidates.len()) {
        println!("Aborted.");
        return 0;
    }

    let mut removed = 0usize;
    let mut failed = 0usize;
    for c in &plan.candidates {
        let mut args: Vec<&str> = vec!["worktree", "remove"];
        if opts.force {
            args.push("--force");
        }
        let path_str = c.entry.path.to_string_lossy().to_string();
        args.push(&path_str);
        match run_git(&main_repo, &args) {
            Ok(_) => {
                println!("removed {}", c.entry.path.display());
                removed += 1;
            }
            Err(e) => {
                eprintln!("error removing {}: {}", c.entry.path.display(), e);
                failed += 1;
            }
        }
    }

    println!(
        "\nSummary: {removed} removed, {skipped} skipped, {failed} failed.",
        removed = removed,
        skipped = plan.skipped.len(),
        failed = failed,
    );

    if failed > 0 {
        1
    } else {
        0
    }
}

#[derive(Debug, Clone)]
struct Classified {
    entry: WorktreeEntry,
    status: WorktreeStatus,
    age: Duration,
    is_main: bool,
}

#[derive(Debug, Clone)]
struct Candidate {
    entry: WorktreeEntry,
    reason: String,
}

#[derive(Debug, Default)]
struct Plan {
    candidates: Vec<Candidate>,
    skipped: Vec<Candidate>,
}

fn build_plan(rows: &[Classified], opts: &CleanOptions) -> Plan {
    let mut plan = Plan::default();
    for row in rows {
        if row.is_main {
            continue;
        }
        if row.entry.locked {
            plan.skipped.push(Candidate {
                entry: row.entry.clone(),
                reason: "locked".to_string(),
            });
            continue;
        }
        let inputs = StalenessInputs {
            status: row.status,
            age: row.age,
            locked: row.entry.locked,
        };
        match decide_action(inputs, opts) {
            Action::Remove(reason) => plan.candidates.push(Candidate {
                entry: row.entry.clone(),
                reason,
            }),
            Action::Skip(reason) => plan.skipped.push(Candidate {
                entry: row.entry.clone(),
                reason,
            }),
            Action::Ignore => {}
        }
    }
    plan
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum Action {
    Remove(String),
    Skip(String),
    /// Worktree didn't trip any criterion; don't print it in the skipped list.
    Ignore,
}

fn decide_action(inputs: StalenessInputs, opts: &CleanOptions) -> Action {
    if inputs.locked {
        return Action::Skip("locked".to_string());
    }
    let is_old = inputs.age >= opts.stale_after;
    match inputs.status {
        WorktreeStatus::Clean => {
            if is_old {
                Action::Remove(format!("clean + stale ({})", fmt_age(inputs.age)))
            } else {
                Action::Ignore
            }
        }
        WorktreeStatus::BranchGone => {
            // Upstream is gone → always a removal candidate when clean-ish.
            // We treat branch-gone as already-implied-clean: the branch went
            // away on the remote, so even if we have local commits they're
            // unreachable from the remote.
            Action::Remove("upstream branch gone".to_string())
        }
        WorktreeStatus::NoUpstream => {
            if is_old {
                if opts.force {
                    Action::Remove(format!(
                        "no upstream + stale ({}) [--force]",
                        fmt_age(inputs.age)
                    ))
                } else {
                    Action::Skip("no upstream (use --force to remove)".to_string())
                }
            } else {
                Action::Ignore
            }
        }
        WorktreeStatus::Dirty => {
            if opts.force {
                Action::Remove("dirty [--force]".to_string())
            } else {
                Action::Skip("dirty (use --force to remove)".to_string())
            }
        }
        WorktreeStatus::Unpushed => {
            if opts.force {
                Action::Remove("unpushed commits [--force]".to_string())
            } else {
                Action::Skip("unpushed commits (use --force to remove)".to_string())
            }
        }
    }
}

pub(crate) fn fmt_age(d: Duration) -> String {
    let secs = d.as_secs();
    if secs >= 86_400 {
        format!("{}d", secs / 86_400)
    } else if secs >= 3_600 {
        format!("{}h", secs / 3_600)
    } else if secs >= 60 {
        format!("{}m", secs / 60)
    } else {
        format!("{}s", secs)
    }
}

fn print_table(rows: &[Classified], _opts: &CleanOptions) {
    println!("Worktrees:");
    if rows.is_empty() {
        println!("  (none)");
        return;
    }
    let path_w = rows
        .iter()
        .map(|r| r.entry.path.to_string_lossy().len())
        .max()
        .unwrap_or(0)
        .max(4);
    println!(
        "  {:<path_w$}  {:<12}  {:<10}  NOTES",
        "PATH",
        "STATUS",
        "AGE",
        path_w = path_w
    );
    for r in rows {
        let notes = if r.is_main {
            "main".to_string()
        } else if r.entry.locked {
            "locked".to_string()
        } else if r.entry.prunable {
            "prunable".to_string()
        } else {
            String::new()
        };
        println!(
            "  {:<path_w$}  {:<12}  {:<10}  {}",
            r.entry.path.display(),
            r.status.as_str(),
            fmt_age(r.age),
            notes,
            path_w = path_w
        );
    }
}

fn confirm_interactive(n: usize) -> bool {
    print!("Remove {n} worktree(s)? [y/N] ");
    let _ = io::stdout().flush();
    let mut line = String::new();
    if io::stdin().read_line(&mut line).is_err() {
        return false;
    }
    matches!(line.trim().to_ascii_lowercase().as_str(), "y" | "yes")
}

// ---------- git-side helpers ----------

/// Returns the absolute path of the **main** worktree (the directory that owns
/// `.git/worktrees/`). When called from inside any worktree of the same repo,
/// `git worktree list --porcelain` from that worktree still reports the main
/// worktree first, so we can use the current directory as a starting point.
pub fn locate_main_repo_root() -> Result<PathBuf, String> {
    let out = run_git(
        Path::new("."),
        &["rev-parse", "--path-format=absolute", "--git-common-dir"],
    )?;
    let common = PathBuf::from(out.trim());
    // `--git-common-dir` points at `<main>/.git` (or a bare repo dir). The
    // main worktree is its parent. If the path's filename is `.git`, take
    // the parent; otherwise the repo itself is bare and we use the dir.
    let main_root = if common.file_name().and_then(|n| n.to_str()) == Some(".git") {
        common
            .parent()
            .ok_or_else(|| format!("could not derive repo root from {}", common.display()))?
            .to_path_buf()
    } else {
        common
    };
    Ok(main_root)
}

/// Run `git <args>` in `cwd` and return its stdout on success.
///
/// All subprocess spawning in `clud` flows through `running-process-core`
/// (enforced by `ci/banned_imports.py`). We capture combined stdout/stderr
/// so the error path can surface git's diagnostic on failure.
pub(crate) fn run_git(cwd: &Path, args: &[&str]) -> Result<String, String> {
    let mut argv = vec!["git".to_string()];
    argv.extend(args.iter().map(|s| s.to_string()));
    let config = ProcessConfig {
        command: subprocess::command_spec_for_subprocess(argv.clone()),
        cwd: Some(cwd.to_path_buf()),
        env: None,
        capture: true,
        stderr_mode: StderrMode::Stdout,
        // git is a piped helper; suppress the conhost popup on Windows.
        creationflags: invisible_helper_creationflags(),
        create_process_group: false,
        stdin_mode: StdinMode::Null,
        nice: None,
    };
    let process = NativeProcess::new(config);
    process
        .start()
        .map_err(|e| format!("failed to start git: {e}"))?;

    let mut buf = Vec::<u8>::new();
    loop {
        match process.read_combined(Some(Duration::from_millis(100))) {
            ReadStatus::Line(event) => {
                buf.extend_from_slice(&event.line);
                buf.push(b'\n');
            }
            ReadStatus::Timeout => {
                if process.returncode().is_some() {
                    break;
                }
            }
            ReadStatus::Eof => break,
        }
    }
    let exit_code = process
        .wait(Some(Duration::from_secs(30)))
        .map_err(|e| format!("waiting for git: {e}"))?;
    let output = String::from_utf8_lossy(&buf).into_owned();
    if exit_code != 0 {
        return Err(format!(
            "git {} failed (exit {}): {}",
            args.join(" "),
            exit_code,
            output.trim()
        ));
    }
    Ok(output)
}

/// Classify a single worktree's working tree + upstream state.
///
/// Returns `Dirty` if `git status --porcelain` is non-empty (regardless of
/// upstream state), otherwise checks upstream:
/// - `[gone]` upstream → `BranchGone`
/// - no upstream configured → `NoUpstream`
/// - `@{u}..HEAD` has commits → `Unpushed`
/// - everything in order → `Clean`
///
/// Any unexpected git error is treated as `Dirty` (safer default — we'd
/// rather skip than mistakenly remove).
pub fn classify_status(path: &Path) -> WorktreeStatus {
    // Dirty check first — cheap and definitive.
    match run_git(path, &["status", "--porcelain"]) {
        Ok(out) if !out.trim().is_empty() => return WorktreeStatus::Dirty,
        Ok(_) => {}
        Err(_) => return WorktreeStatus::Dirty,
    }

    // Branch-gone check via `git for-each-ref` over the current branch.
    // `git rev-parse --abbrev-ref HEAD` yields the branch name; "HEAD"
    // means detached.
    let branch = match run_git(path, &["rev-parse", "--abbrev-ref", "HEAD"]) {
        Ok(s) => s.trim().to_string(),
        Err(_) => return WorktreeStatus::Dirty,
    };
    if branch == "HEAD" || branch.is_empty() {
        return WorktreeStatus::NoUpstream;
    }

    // Use `git for-each-ref --format='%(upstream:track)'` to detect [gone].
    // The format produces literally "[gone]" if the upstream has been
    // deleted, "[ahead N]" / "[behind N]" / empty otherwise.
    let track = run_git(
        path,
        &[
            "for-each-ref",
            "--format=%(upstream:track)",
            &format!("refs/heads/{}", branch),
        ],
    )
    .unwrap_or_default();
    if track.contains("gone") {
        return WorktreeStatus::BranchGone;
    }

    // Does the branch have an upstream at all? `rev-parse @{u}` errors when
    // there's no upstream — that's the "no-upstream" signal.
    if run_git(path, &["rev-parse", "--abbrev-ref", "@{u}"]).is_err() {
        return WorktreeStatus::NoUpstream;
    }

    // Unpushed commits?
    match run_git(path, &["rev-list", "--count", "@{u}..HEAD"]) {
        Ok(out) => {
            let n: u64 = out.trim().parse().unwrap_or(0);
            if n > 0 {
                WorktreeStatus::Unpushed
            } else {
                WorktreeStatus::Clean
            }
        }
        Err(_) => WorktreeStatus::Clean,
    }
}

fn path_age(path: &Path) -> Option<Duration> {
    let meta = std::fs::metadata(path).ok()?;
    let mtime = meta.modified().ok()?;
    SystemTime::now().duration_since(mtime).ok()
}

// ---------- porcelain parser ----------

/// Parse the output of `git worktree list --porcelain`.
///
/// Format (one block per worktree, separated by blank lines):
///
/// ```text
/// worktree <path>
/// HEAD <sha>
/// branch refs/heads/<name>
/// [bare]
/// [detached]
/// [locked [reason...]]
/// [prunable [reason...]]
/// ```
pub fn parse_worktree_porcelain(raw: &str) -> Vec<WorktreeEntry> {
    let mut out = Vec::new();
    let mut current: Option<WorktreeEntry> = None;
    for line in raw.lines() {
        let line = line.trim_end_matches('\r');
        if line.is_empty() {
            if let Some(e) = current.take() {
                out.push(e);
            }
            continue;
        }
        let mut parts = line.splitn(2, ' ');
        let key = parts.next().unwrap_or("");
        let value = parts.next().unwrap_or("");
        match key {
            "worktree" => {
                if let Some(e) = current.take() {
                    out.push(e);
                }
                current = Some(WorktreeEntry {
                    path: PathBuf::from(value),
                    head: None,
                    branch: None,
                    bare: false,
                    detached: false,
                    locked: false,
                    locked_reason: None,
                    prunable: false,
                });
            }
            "HEAD" => {
                if let Some(c) = current.as_mut() {
                    c.head = Some(value.to_string());
                }
            }
            "branch" => {
                if let Some(c) = current.as_mut() {
                    let name = value
                        .strip_prefix("refs/heads/")
                        .unwrap_or(value)
                        .to_string();
                    c.branch = Some(name);
                }
            }
            "bare" => {
                if let Some(c) = current.as_mut() {
                    c.bare = true;
                }
            }
            "detached" => {
                if let Some(c) = current.as_mut() {
                    c.detached = true;
                }
            }
            "locked" => {
                if let Some(c) = current.as_mut() {
                    c.locked = true;
                    let reason = value.trim();
                    if !reason.is_empty() {
                        c.locked_reason = Some(reason.to_string());
                    }
                }
            }
            "prunable" => {
                if let Some(c) = current.as_mut() {
                    c.prunable = true;
                }
            }
            _ => {}
        }
    }
    if let Some(e) = current.take() {
        out.push(e);
    }
    out
}

// ---------- duration parser ----------

/// Parse a `--stale-after` value like `1d`, `2h`, `30m`, `45s`.
///
/// Accepted units: `s` (seconds), `m` (minutes), `h` (hours), `d` (days).
/// The value must be a positive integer. Whitespace around the input is
/// trimmed. Unit characters are case-insensitive (`30M` == `30m`).
pub fn parse_duration(raw: &str) -> Result<Duration, String> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Err("duration cannot be empty".to_string());
    }
    let split_at = trimmed
        .find(|c: char| !c.is_ascii_digit())
        .ok_or_else(|| "duration must include a unit like s, m, h, or d".to_string())?;
    if split_at == 0 {
        return Err("duration must start with a positive integer".to_string());
    }
    let (num_part, unit_part) = trimmed.split_at(split_at);
    let n: u64 = num_part
        .parse()
        .map_err(|_| format!("invalid duration value: {num_part}"))?;
    if n == 0 {
        return Err("duration must be greater than zero".to_string());
    }
    let unit = unit_part.trim().to_ascii_lowercase();
    let secs_multiplier: u64 = match unit.as_str() {
        "s" => 1,
        "m" => 60,
        "h" => 60 * 60,
        "d" => 60 * 60 * 24,
        _ => return Err(format!("unsupported duration unit: {unit_part}")),
    };
    let total_secs = n
        .checked_mul(secs_multiplier)
        .ok_or_else(|| "duration is too large".to_string())?;
    Ok(Duration::from_secs(total_secs))
}

#[cfg(test)]
#[path = "worktrees_tests.rs"]
mod tests;
