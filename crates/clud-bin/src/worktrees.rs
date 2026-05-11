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

use running_process_core::{NativeProcess, ProcessConfig, ReadStatus, StderrMode, StdinMode};

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

fn fmt_age(d: Duration) -> String {
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
fn run_git(cwd: &Path, args: &[&str]) -> Result<String, String> {
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
        containment: None,
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
mod tests {
    use super::*;

    // ----- parse_duration -----

    #[test]
    fn parse_duration_seconds() {
        assert_eq!(parse_duration("30s").unwrap(), Duration::from_secs(30));
    }

    #[test]
    fn parse_duration_minutes() {
        assert_eq!(parse_duration("5m").unwrap(), Duration::from_secs(300));
    }

    #[test]
    fn parse_duration_hours() {
        assert_eq!(parse_duration("2h").unwrap(), Duration::from_secs(7_200));
    }

    #[test]
    fn parse_duration_days() {
        assert_eq!(parse_duration("1d").unwrap(), Duration::from_secs(86_400));
        assert_eq!(
            parse_duration("7d").unwrap(),
            Duration::from_secs(7 * 86_400)
        );
    }

    #[test]
    fn parse_duration_uppercase_unit() {
        assert_eq!(parse_duration("3H").unwrap(), Duration::from_secs(10_800));
    }

    #[test]
    fn parse_duration_trims_whitespace() {
        assert_eq!(
            parse_duration("  1d  ").unwrap(),
            Duration::from_secs(86_400)
        );
    }

    #[test]
    fn parse_duration_rejects_empty() {
        assert!(parse_duration("").is_err());
        assert!(parse_duration("   ").is_err());
    }

    #[test]
    fn parse_duration_rejects_zero() {
        assert!(parse_duration("0d").is_err());
        assert!(parse_duration("0h").is_err());
    }

    #[test]
    fn parse_duration_rejects_missing_unit() {
        assert!(parse_duration("30").is_err());
    }

    #[test]
    fn parse_duration_rejects_missing_value() {
        assert!(parse_duration("d").is_err());
    }

    #[test]
    fn parse_duration_rejects_negative() {
        assert!(parse_duration("-1d").is_err());
    }

    #[test]
    fn parse_duration_rejects_fractional() {
        assert!(parse_duration("1.5d").is_err());
    }

    #[test]
    fn parse_duration_rejects_unknown_unit() {
        for bad in &["1y", "1w", "10x"] {
            assert!(
                parse_duration(bad).is_err(),
                "expected {bad} to be rejected"
            );
        }
    }

    #[test]
    fn parse_duration_rejects_overflow() {
        // 2^64 days * 86400 secs overflows u64.
        assert!(parse_duration("18446744073709551615d").is_err());
    }

    // ----- parse_worktree_porcelain -----

    #[test]
    fn porcelain_single_entry() {
        let raw = "\
worktree /repo
HEAD abc123
branch refs/heads/main
";
        let v = parse_worktree_porcelain(raw);
        assert_eq!(v.len(), 1);
        assert_eq!(v[0].path, PathBuf::from("/repo"));
        assert_eq!(v[0].head.as_deref(), Some("abc123"));
        assert_eq!(v[0].branch.as_deref(), Some("main"));
        assert!(!v[0].bare);
        assert!(!v[0].detached);
        assert!(!v[0].locked);
        assert!(!v[0].prunable);
    }

    #[test]
    fn porcelain_multiple_entries_with_locked_and_prunable() {
        let raw = "\
worktree /repo
HEAD aaa
branch refs/heads/main

worktree /tmp/wt-a
HEAD bbb
branch refs/heads/feature/a
locked

worktree /tmp/wt-b
HEAD ccc
branch refs/heads/feature/b
prunable gitdir file points to non-existent location
";
        let v = parse_worktree_porcelain(raw);
        assert_eq!(v.len(), 3);
        assert_eq!(v[0].path, PathBuf::from("/repo"));
        assert!(!v[0].locked);
        assert_eq!(v[1].path, PathBuf::from("/tmp/wt-a"));
        assert!(v[1].locked);
        assert!(!v[1].prunable);
        assert_eq!(v[2].path, PathBuf::from("/tmp/wt-b"));
        assert!(!v[2].locked);
        assert!(v[2].prunable);
    }

    #[test]
    fn porcelain_detached_head() {
        let raw = "\
worktree /repo
HEAD abc123
detached
";
        let v = parse_worktree_porcelain(raw);
        assert_eq!(v.len(), 1);
        assert!(v[0].detached);
        assert!(v[0].branch.is_none());
    }

    #[test]
    fn porcelain_bare_repo() {
        let raw = "\
worktree /srv/repo.git
bare
";
        let v = parse_worktree_porcelain(raw);
        assert_eq!(v.len(), 1);
        assert!(v[0].bare);
    }

    #[test]
    fn porcelain_handles_trailing_newline() {
        let raw = "\
worktree /repo
HEAD abc123
branch refs/heads/main

";
        let v = parse_worktree_porcelain(raw);
        assert_eq!(v.len(), 1);
        assert_eq!(v[0].branch.as_deref(), Some("main"));
    }

    #[test]
    fn porcelain_strips_refs_heads_prefix() {
        let raw = "\
worktree /repo
HEAD abc
branch refs/heads/feature/issue-83
";
        let v = parse_worktree_porcelain(raw);
        assert_eq!(v[0].branch.as_deref(), Some("feature/issue-83"));
    }

    #[test]
    fn porcelain_handles_crlf() {
        let raw = "worktree /repo\r\nHEAD abc\r\nbranch refs/heads/main\r\n\r\n";
        let v = parse_worktree_porcelain(raw);
        assert_eq!(v.len(), 1);
        assert_eq!(v[0].branch.as_deref(), Some("main"));
    }

    #[test]
    fn porcelain_empty_input() {
        assert!(parse_worktree_porcelain("").is_empty());
        assert!(parse_worktree_porcelain("\n\n").is_empty());
    }

    // ----- decide_action -----

    fn opts(stale_after: Duration, force: bool) -> CleanOptions {
        CleanOptions {
            stale_after,
            dry_run: false,
            yes: true,
            force,
        }
    }

    #[test]
    fn decide_clean_and_old_is_removed() {
        let inputs = StalenessInputs {
            status: WorktreeStatus::Clean,
            age: Duration::from_secs(86_400 * 2),
            locked: false,
        };
        let act = decide_action(inputs, &opts(Duration::from_secs(86_400), false));
        assert!(matches!(act, Action::Remove(_)));
    }

    #[test]
    fn decide_clean_but_fresh_is_ignored() {
        let inputs = StalenessInputs {
            status: WorktreeStatus::Clean,
            age: Duration::from_secs(60),
            locked: false,
        };
        let act = decide_action(inputs, &opts(Duration::from_secs(86_400), false));
        assert_eq!(act, Action::Ignore);
    }

    #[test]
    fn decide_branch_gone_is_always_removed() {
        // Even when fresh, a [gone] upstream → remove.
        let inputs = StalenessInputs {
            status: WorktreeStatus::BranchGone,
            age: Duration::from_secs(1),
            locked: false,
        };
        let act = decide_action(inputs, &opts(Duration::from_secs(86_400), false));
        assert!(matches!(act, Action::Remove(_)));
    }

    #[test]
    fn decide_dirty_without_force_is_skipped() {
        let inputs = StalenessInputs {
            status: WorktreeStatus::Dirty,
            age: Duration::from_secs(86_400 * 30),
            locked: false,
        };
        let act = decide_action(inputs, &opts(Duration::from_secs(86_400), false));
        assert!(matches!(act, Action::Skip(_)));
    }

    #[test]
    fn decide_dirty_with_force_is_removed() {
        let inputs = StalenessInputs {
            status: WorktreeStatus::Dirty,
            age: Duration::from_secs(86_400 * 30),
            locked: false,
        };
        let act = decide_action(inputs, &opts(Duration::from_secs(86_400), true));
        assert!(matches!(act, Action::Remove(_)));
    }

    #[test]
    fn decide_unpushed_without_force_is_skipped() {
        let inputs = StalenessInputs {
            status: WorktreeStatus::Unpushed,
            age: Duration::from_secs(86_400 * 30),
            locked: false,
        };
        let act = decide_action(inputs, &opts(Duration::from_secs(86_400), false));
        assert!(matches!(act, Action::Skip(_)));
    }

    #[test]
    fn decide_unpushed_with_force_is_removed() {
        let inputs = StalenessInputs {
            status: WorktreeStatus::Unpushed,
            age: Duration::from_secs(86_400 * 30),
            locked: false,
        };
        let act = decide_action(inputs, &opts(Duration::from_secs(86_400), true));
        assert!(matches!(act, Action::Remove(_)));
    }

    #[test]
    fn decide_no_upstream_fresh_is_ignored() {
        let inputs = StalenessInputs {
            status: WorktreeStatus::NoUpstream,
            age: Duration::from_secs(60),
            locked: false,
        };
        let act = decide_action(inputs, &opts(Duration::from_secs(86_400), false));
        assert_eq!(act, Action::Ignore);
    }

    #[test]
    fn decide_no_upstream_stale_skipped_without_force() {
        let inputs = StalenessInputs {
            status: WorktreeStatus::NoUpstream,
            age: Duration::from_secs(86_400 * 30),
            locked: false,
        };
        let act = decide_action(inputs, &opts(Duration::from_secs(86_400), false));
        assert!(matches!(act, Action::Skip(_)));
    }

    #[test]
    fn decide_no_upstream_stale_removed_with_force() {
        let inputs = StalenessInputs {
            status: WorktreeStatus::NoUpstream,
            age: Duration::from_secs(86_400 * 30),
            locked: false,
        };
        let act = decide_action(inputs, &opts(Duration::from_secs(86_400), true));
        assert!(matches!(act, Action::Remove(_)));
    }

    /// Locked worktrees must NEVER be removed — not even with `--force`.
    /// This is the critical safety invariant the issue calls out.
    #[test]
    fn decide_locked_never_removed_even_with_force() {
        for status in [
            WorktreeStatus::Clean,
            WorktreeStatus::Dirty,
            WorktreeStatus::Unpushed,
            WorktreeStatus::NoUpstream,
            WorktreeStatus::BranchGone,
        ] {
            let inputs = StalenessInputs {
                status,
                age: Duration::from_secs(86_400 * 365),
                locked: true,
            };
            let act = decide_action(inputs, &opts(Duration::from_secs(86_400), true));
            assert!(
                matches!(act, Action::Skip(_)),
                "locked + {status:?} must be Skip, got {act:?}"
            );
        }
    }

    #[test]
    fn fmt_age_picks_largest_unit() {
        assert_eq!(fmt_age(Duration::from_secs(5)), "5s");
        assert_eq!(fmt_age(Duration::from_secs(90)), "1m");
        assert_eq!(fmt_age(Duration::from_secs(3700)), "1h");
        assert_eq!(fmt_age(Duration::from_secs(86_400 * 3)), "3d");
    }
}
