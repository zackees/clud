use std::path::Path;
use std::time::{Duration, Instant, SystemTime};

use running_process::{
    NativeProcess, ProcessConfig, ReadStatus, StderrMode, StdinMode, StreamKind,
};

use crate::gc::TrackedEntry;
use crate::subprocess;
use crate::win_creation_flags::invisible_helper_creationflags;

use super::{DEFAULT_EXTERN_REPO_STALE_AFTER_SECS, ENV_GC_EXTERN_REPO_MAX_AGE_SECS};

/// Hard ceiling on a single git plumbing query in the purge probe.
///
/// This probe runs on the **registry worker thread** (`process_op` →
/// `dispatch_purge_entries` → `partition_purgeable`), the one thread that
/// owns redb. `worktrees::run_git` is deliberately patient — its read loop
/// leaves only on EOF or on a poll timeout *after* the child already
/// exited — so a git that stays alive with stdout open never returns. On
/// the worker that would wedge every client op at `WORKER_REPLY_TIMEOUT`,
/// including launch-path `record_repo_visit`. A checkout on a stale
/// network mount is enough to trigger it, so the probe gets its own
/// bounded runner that kills the child instead (same shape as the
/// `ls_files` startup guard, #556).
const GIT_PROBE_TIMEOUT: Duration = Duration::from_secs(5);

pub(super) fn extern_repo_stale_after() -> Duration {
    let secs = std::env::var(ENV_GC_EXTERN_REPO_MAX_AGE_SECS)
        .ok()
        .and_then(|raw| raw.trim().parse::<u64>().ok())
        .unwrap_or(DEFAULT_EXTERN_REPO_STALE_AFTER_SECS);
    Duration::from_secs(secs)
}

/// Whether a checkout holds work that an auto-purge would destroy.
///
/// This is the safety signal that replaced sole reliance on filesystem
/// mtime: mtime only answers "has anything been touched recently", which a
/// stray build artifact can reset forever, and says nothing about whether
/// there is *uncommitted or unpushed work* a delete would lose.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum GitWorkState {
    /// Working tree is clean AND every local commit exists on a remote —
    /// the checkout is trivially re-clonable, so deleting it loses nothing.
    Clean,
    /// Uncommitted changes, untracked files, unpushed commits, or a stash
    /// are present. Deleting would destroy local work, so pin the entry.
    HasLocalWork,
    /// Not a git work tree of its own. We cannot make a safety judgement,
    /// so callers fall back to the mtime idle gate alone (the pre-guard
    /// behaviour) rather than pinning forever.
    Unknown,
    /// A probe query exceeded `GIT_PROBE_TIMEOUT` or git could not be run.
    /// Distinct from `Unknown`: there *is* a repository here, we simply
    /// failed to read it, so falling back to mtime-only would delete a
    /// checkout whose contents we never managed to inspect. Spare instead.
    ProbeFailed,
}

/// Environment variables that redirect git's repository discovery. If the
/// daemon inherited any of them — it is long-lived and may have been started
/// from a git hook or `git rebase -x` — then every probe would answer about
/// *that* repository no matter which `cwd` we pass, which is the parent-repo
/// bug all over again but invisible to the `.git` gate. `ProcessConfig.env`
/// can only set overrides, never unset, so we detect and spare instead.
const GIT_DISCOVERY_ENV: &[&str] = &[
    "GIT_DIR",
    "GIT_WORK_TREE",
    "GIT_INDEX_FILE",
    "GIT_OBJECT_DIRECTORY",
    "GIT_COMMON_DIR",
];

fn git_discovery_env_is_poisoned() -> bool {
    GIT_DISCOVERY_ENV
        .iter()
        .any(|key| std::env::var_os(key).is_some())
}

/// Run one git plumbing query under a hard deadline, killing the child if
/// it overruns. Returns `None` on timeout, spawn failure, or non-zero exit
/// — the caller maps all three to `ProbeFailed`.
///
/// Deliberately not `worktrees::run_git`: that helper is unbounded by
/// design and other callers rely on its patience. See `GIT_PROBE_TIMEOUT`.
fn probe_git(cwd: &Path, args: &[&str]) -> Option<String> {
    let mut argv = vec!["git".to_string()];
    argv.extend(args.iter().map(|s| s.to_string()));
    let process = NativeProcess::new(ProcessConfig {
        command: subprocess::command_spec_for_subprocess(argv),
        cwd: Some(cwd.to_path_buf()),
        env: None,
        capture: true,
        // Keep stderr on its own pipe. The caller's signal is whether
        // stdout is *empty*, so a warning (a missing `core.fsmonitor`
        // hook, a `safe.directory` gripe) must never be mistaken for
        // porcelain output — that would report a spotless checkout as
        // holding local work and pin it forever.
        stderr_mode: StderrMode::Pipe,
        // git is a piped helper; suppress the conhost popup on Windows.
        creationflags: invisible_helper_creationflags(),
        create_process_group: false,
        stdin_mode: StdinMode::Null,
        nice: None,
    });
    process.start().ok()?;

    // `read_combined` yields both streams regardless of `stderr_mode`, so
    // the stream tag is what actually keeps stderr out of the signal.
    let push_stdout = |text: &mut String, event: running_process::StreamEvent| {
        if event.stream == StreamKind::Stdout {
            text.push_str(&String::from_utf8_lossy(&event.line));
            text.push('\n');
        }
    };

    let deadline = Instant::now() + GIT_PROBE_TIMEOUT;
    let mut text = String::new();
    loop {
        match process.read_combined(Some(Duration::from_millis(25))) {
            ReadStatus::Line(event) => push_stdout(&mut text, event),
            ReadStatus::Eof => break,
            ReadStatus::Timeout => {}
        }
        match process.poll() {
            Ok(Some(_)) => {
                // Drain whatever is still buffered before leaving.
                while let ReadStatus::Line(event) =
                    process.read_combined(Some(Duration::from_millis(5)))
                {
                    push_stdout(&mut text, event);
                }
                break;
            }
            Ok(None) => {}
            Err(_) => return None,
        }
        if Instant::now() >= deadline {
            // A wedged git must never hold the registry worker.
            let _ = process.kill();
            return None;
        }
    }

    match process.wait(Some(Duration::from_secs(1))) {
        Ok(0) => Some(text),
        _ => None,
    }
}

/// Probe a checkout's git state. Runs at most three cheap plumbing queries,
/// and only ever on a directory the caller has already found idle, so the
/// common case (active repos, non-idle repos) never pays for it.
pub(super) fn git_work_state(cwd: &Path) -> GitWorkState {
    // Anchor the probe to *this* directory. `.extern-repos/` lives inside
    // the parent repo by convention, and the scanner tracks every immediate
    // child of it whether or not that child is a clone. Without this gate,
    // git walks up and answers about the **parent** repo: a plain directory
    // beside a parent with one untracked file reads as `HasLocalWork` and
    // is pinned forever, while a clean parent vouches for a checkout that
    // was never inspected. `.git` is a directory in a clone and a file in a
    // linked worktree, so `exists()` covers both. The gate alone is not
    // enough — a *malformed* `.git` (an interrupted clone leaves an empty
    // one) passes `exists()` and git then resumes walking up, verified:
    // from such a directory inside a dirty parent, `status --porcelain`
    // exits 0 and prints the parent's entries. Every query below is
    // therefore additionally pinned with `--git-dir`/`--work-tree`, which
    // disables discovery outright, so a malformed `.git` errors into
    // `ProbeFailed` (spare) instead of borrowing the parent's answer.
    if !cwd.join(".git").exists() {
        // A bare clone/mirror has no `.git`; its repository *is* the
        // directory. We do not probe it (a bare repo has no work tree, so
        // `status` is meaningless), but it plainly holds refs and objects,
        // so it must not fall through to the mtime-only verdict and be
        // deleted uninspected.
        if cwd.join("HEAD").is_file() && cwd.join("objects").is_dir() {
            return GitWorkState::ProbeFailed;
        }
        return GitWorkState::Unknown;
    }

    // An inherited GIT_DIR (etc.) outranks `cwd` in git's discovery, so the
    // probe would silently answer about someone else's repository.
    if git_discovery_env_is_poisoned() {
        return GitWorkState::ProbeFailed;
    }

    // `--no-optional-locks` keeps the probe read-only: a plain `status`
    // refreshes the stat cache, rewriting `.git/index`. That both bumps the
    // mtime this module uses as its idle signal (re-arming the stale window
    // for another full cycle) and contends with a user's concurrent git
    // command for `index.lock`.
    let Some(status) = probe_git(
        cwd,
        &[
            "--no-optional-locks",
            "--git-dir=.git",
            "--work-tree=.",
            "status",
            "--porcelain",
            "--untracked-files=normal",
        ],
    ) else {
        return GitWorkState::ProbeFailed;
    };
    if !status.trim().is_empty() {
        return GitWorkState::HasLocalWork;
    }

    // Any commit not reachable from *some* remote ref. `--all` rather than
    // `--branches` so commits made on a detached HEAD — a checkout pinned
    // at a tag or sha, then committed on — are covered too; `--branches`
    // only expands `refs/heads` and would report such a repo as clean and
    // let it be deleted. With no remotes configured every commit qualifies
    // → HasLocalWork, which is the conservative (spare) answer we want.
    let Some(unpushed) = probe_git(
        cwd,
        &[
            "--no-optional-locks",
            "--git-dir=.git",
            "--work-tree=.",
            "log",
            "--all",
            "--not",
            "--remotes",
            "--format=%H",
            "--max-count=1",
        ],
    ) else {
        return GitWorkState::ProbeFailed;
    };
    if !unpushed.trim().is_empty() {
        return GitWorkState::HasLocalWork;
    }

    let Some(stash) = probe_git(
        cwd,
        &[
            "--no-optional-locks",
            "--git-dir=.git",
            "--work-tree=.",
            "stash",
            "list",
        ],
    ) else {
        return GitWorkState::ProbeFailed;
    };
    if !stash.trim().is_empty() {
        return GitWorkState::HasLocalWork;
    }

    GitWorkState::Clean
}

/// Why an extern-repo row was or was not reclaimed. The repo's reap/spare
/// convention is to assert **spare + reason**, not just the outcome, so the
/// reason travels with the verdict instead of being recomputed at the log
/// site. Issue #896 surfaces these in `clud gc list`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct PurgeDecision {
    pub(crate) purge: bool,
    pub(crate) reason: &'static str,
    /// How `clud gc list` should classify the row (issue #896).
    ///
    /// Carried explicitly rather than inferred from `purge`, because "not
    /// purged" is three different things to a reader: a checkout GC will
    /// never auto-reclaim, one it simply has not aged into yet, and a row
    /// whose directory is gone. Collapsing them paints every
    /// recently-touched checkout yellow, which is the legibility loss #896
    /// exists to fix.
    pub(crate) class: PurgeClass,
}

/// The display classes a purge verdict maps onto.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PurgeClass {
    /// GC will take it on a future tick.
    Reclaimable,
    /// Not yet eligible, but nothing is holding it — it will age in.
    Spared,
    /// Deliberately retained; GC will keep refusing while this holds.
    Pinned,
    /// The registry row points at a path that is gone.
    Dangling,
}

/// Pure purge decision, factored out so the matrix is unit-testable without
/// touching the filesystem or spawning git.
///
/// | is_dir | idle  | git state    | purgeable |
/// |--------|-------|--------------|-----------|
/// | false  | *     | *            | no (gone / unreadable)   |
/// | true   | false | *            | no (still active)        |
/// | true   | true  | HasLocalWork | no (pinned: local work)  |
/// | true   | true  | ProbeFailed  | no (pinned: unreadable)  |
/// | true   | true  | Clean        | yes                      |
/// | true   | true  | Unknown      | yes (mtime fallback)     |
pub(super) fn extern_repo_purge_decision(
    is_dir: bool,
    idle: bool,
    git: GitWorkState,
) -> PurgeDecision {
    if !is_dir {
        return PurgeDecision {
            purge: false,
            reason: "dangling: path missing",
            class: PurgeClass::Dangling,
        };
    }
    if !idle {
        return PurgeDecision {
            purge: false,
            reason: "spared: recently active",
            class: PurgeClass::Spared,
        };
    }
    match git {
        GitWorkState::HasLocalWork => PurgeDecision {
            purge: false,
            reason: "pinned: uncommitted or unpushed work",
            class: PurgeClass::Pinned,
        },
        GitWorkState::ProbeFailed => PurgeDecision {
            purge: false,
            reason: "pinned: git state unreadable",
            class: PurgeClass::Pinned,
        },
        GitWorkState::Clean => PurgeDecision {
            purge: true,
            reason: "reclaimable: clean and pushed",
            class: PurgeClass::Reclaimable,
        },
        GitWorkState::Unknown => PurgeDecision {
            purge: true,
            reason: "reclaimable: not a git checkout",
            class: PurgeClass::Reclaimable,
        },
    }
}

/// An extern-repo row is purgeable once the on-disk directory has been
/// inactive (no descendant `mtime` change) for `stale_after` **and** holds
/// no uncommitted/unpushed work. Anything the scanner tracks under
/// `<repo>/.extern-repos/` is, by convention, a clud-managed checkout, so
/// beyond the higher-level live-session check applied by `entry_is_live`
/// these two gates are the safety net: mtime throttles how eagerly we act,
/// the git-work check guards against deleting a checkout with local work.
/// Returns the full verdict. `gc list` (issue #896) displays the reason,
/// so it is recorded by the caller rather than discarded here.
pub(super) fn extern_repo_purge_verdict(
    entry: &TrackedEntry,
    stale_after: Duration,
) -> PurgeDecision {
    let path = Path::new(&entry.path);
    if !path.is_dir() {
        return extern_repo_purge_decision(false, false, GitWorkState::Unknown);
    }
    let idle = most_recent_mtime(path)
        .and_then(|mtime| SystemTime::now().duration_since(mtime).ok())
        .map(|age| age >= stale_after)
        .unwrap_or(false);
    if !idle {
        // Not idle yet — skip the git probe entirely.
        return extern_repo_purge_decision(true, false, GitWorkState::Unknown);
    }
    let decision = extern_repo_purge_decision(true, true, git_work_state(path));
    if !decision.purge {
        // The reason is the point: without it a pinned row is
        // indistinguishable from garbage that GC keeps failing to
        // collect. Issue #896 surfaces the same string in `gc list`.
        eprintln!("[clud] gc extern-repo {} — {}", entry.path, decision.reason);
    }
    decision
}

fn most_recent_mtime(path: &Path) -> Option<SystemTime> {
    let metadata = std::fs::symlink_metadata(path).ok()?;
    let mut latest = metadata.modified().ok()?;
    if metadata.is_dir() {
        let entries = std::fs::read_dir(path).ok()?;
        for entry in entries.flatten() {
            if let Some(child_mtime) = most_recent_mtime(&entry.path()) {
                if child_mtime > latest {
                    latest = child_mtime;
                }
            }
        }
    }
    Some(latest)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Assert **spare + reason**, not just the outcome: the reason is what
    /// makes a pinned row distinguishable from uncollected garbage.
    #[test]
    fn decision_matrix_covers_every_gate() {
        let cases = [
            // (is_dir, idle, git state)   → (purge, reason, display class)
            (
                (false, true, GitWorkState::Clean),
                (false, "dangling: path missing", PurgeClass::Dangling),
            ),
            (
                (true, false, GitWorkState::Clean),
                // Not yet eligible, but nothing is holding it: it ages in,
                // so it must NOT be classed as pinned (issue #896).
                (false, "spared: recently active", PurgeClass::Spared),
            ),
            (
                (true, true, GitWorkState::HasLocalWork),
                (
                    false,
                    "pinned: uncommitted or unpushed work",
                    PurgeClass::Pinned,
                ),
            ),
            (
                (true, true, GitWorkState::ProbeFailed),
                (false, "pinned: git state unreadable", PurgeClass::Pinned),
            ),
            (
                (true, true, GitWorkState::Clean),
                (
                    true,
                    "reclaimable: clean and pushed",
                    PurgeClass::Reclaimable,
                ),
            ),
            (
                (true, true, GitWorkState::Unknown),
                (
                    true,
                    "reclaimable: not a git checkout",
                    PurgeClass::Reclaimable,
                ),
            ),
        ];
        for ((is_dir, idle, git), (purge, reason, class)) in cases {
            let got = extern_repo_purge_decision(is_dir, idle, git);
            assert_eq!(
                got,
                PurgeDecision {
                    purge,
                    reason,
                    class
                },
                "is_dir={is_dir} idle={idle} git={git:?}"
            );
        }
    }

    /// A probe we could not complete must never authorize a delete. This is
    /// the timeout path: `Unknown` purges on mtime alone, so mapping a hung
    /// git onto it would delete a checkout nobody ever inspected.
    #[test]
    fn unreadable_git_state_is_spared_not_purged() {
        assert!(!extern_repo_purge_decision(true, true, GitWorkState::ProbeFailed).purge);
        assert!(extern_repo_purge_decision(true, true, GitWorkState::Unknown).purge);
    }

    fn git(cwd: &Path, args: &[&str]) -> String {
        probe_git(cwd, args)
            .unwrap_or_else(|| panic!("git {} failed in {}", args.join(" "), cwd.display()))
    }

    fn init_repo(root: &Path) {
        git(root, &["init", "--initial-branch=main"]);
        git(root, &["config", "user.email", "t@example.com"]);
        git(root, &["config", "user.name", "t"]);
        // Insulate the fixture from the developer's global config: a global
        // `commit.gpgsign` with no key, or a `core.hooksPath` pointing at
        // real hooks, would otherwise fail `git commit` here.
        git(root, &["config", "commit.gpgsign", "false"]);
        git(root, &["config", "core.hooksPath", ""]);
    }

    #[test]
    fn standalone_non_git_dir_reads_as_unknown() {
        let tmp = tempfile::tempdir().unwrap();
        assert_eq!(git_work_state(tmp.path()), GitWorkState::Unknown);
    }

    /// Regression guard for the production layout. `.extern-repos/` lives
    /// *inside* the parent repo and the scanner tracks every immediate
    /// child, git or not. Before the `.git` anchor, git walked up and
    /// answered about the parent: a plain directory next to a parent with
    /// one untracked file reported `HasLocalWork` and was pinned forever,
    /// and `Unknown` was unreachable in production. Without this test a
    /// probe that always says `HasLocalWork` passes the whole suite.
    #[test]
    fn non_git_dir_nested_inside_a_repo_still_reads_as_unknown() {
        let tmp = tempfile::tempdir().unwrap();
        let parent = tmp.path();
        init_repo(parent);
        // Make the parent unmistakably dirty.
        std::fs::write(parent.join("dirty.txt"), "uncommitted").unwrap();

        let child = parent.join(".extern-repos").join("plain-dir");
        std::fs::create_dir_all(&child).unwrap();

        assert_eq!(
            git_work_state(&child),
            GitWorkState::Unknown,
            "probe must answer about the checkout, not the enclosing repo"
        );
    }

    /// A bare clone/mirror has no `.git` and no work tree, but it does hold
    /// refs and objects. It must be spared, not handed to the mtime-only
    /// verdict that would delete it without ever looking inside.
    #[test]
    fn bare_repo_is_spared_rather_than_purged_uninspected() {
        let tmp = tempfile::tempdir().unwrap();
        git(tmp.path(), &["init", "--bare", "mirror.git"]);
        let bare = tmp.path().join("mirror.git");

        assert!(!bare.join(".git").exists(), "fixture is not actually bare");
        assert_eq!(git_work_state(&bare), GitWorkState::ProbeFailed);
        assert!(!extern_repo_purge_decision(true, true, git_work_state(&bare)).purge);
    }

    /// Regression guard for the hole the `.git` *existence* gate left open.
    /// An interrupted clone leaves a `.git` that exists but is not a usable
    /// gitdir; `exists()` passes it through, and git then resumes its
    /// upward walk. Verified against real git: from such a directory inside
    /// a dirty parent repo, `status --porcelain` exits 0 and prints the
    /// **parent's** entries — so without the pinned `--git-dir`/`--work-tree`
    /// this reads as `HasLocalWork` (pinned forever on a dirty parent) or,
    /// worse, as `Clean` on a tidy parent, authorizing a delete of a
    /// checkout that was never inspected.
    #[test]
    fn malformed_git_dir_inside_a_repo_does_not_borrow_the_parents_answer() {
        let tmp = tempfile::tempdir().unwrap();
        let parent = tmp.path();
        init_repo(parent);
        std::fs::write(parent.join("dirty.txt"), "uncommitted").unwrap();

        // `.git` exists, so the gate passes — but it is empty, so it is not
        // a repository.
        let child = parent.join(".extern-repos").join("interrupted-clone");
        std::fs::create_dir_all(child.join(".git")).unwrap();

        assert_eq!(
            git_work_state(&child),
            GitWorkState::ProbeFailed,
            "a malformed .git must fail closed, not inherit the parent repo"
        );
        // Fail closed means spare, never purge.
        assert!(!extern_repo_purge_decision(true, true, GitWorkState::ProbeFailed).purge);
    }

    #[test]
    fn unpushed_or_untracked_repo_is_pinned() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        init_repo(root);
        std::fs::write(root.join("a.txt"), "hello").unwrap();
        git(root, &["add", "-A"]);
        git(root, &["commit", "-m", "init"]);

        // Committed but with no remote → the commit counts as unpushed work.
        assert_eq!(git_work_state(root), GitWorkState::HasLocalWork);

        // An untracked file is likewise local work.
        std::fs::write(root.join("scratch.txt"), "wip").unwrap();
        assert_eq!(git_work_state(root), GitWorkState::HasLocalWork);
    }

    /// The only test that produces `Clean` from a real repository — without
    /// it, a `git_work_state` hardwired to `HasLocalWork` passes everything.
    /// Uses a local bare upstream so no network (and no `git clone`, which
    /// the repo's own guard blocks) is involved.
    #[test]
    fn clean_fully_pushed_checkout_is_reclaimable() {
        let tmp = tempfile::tempdir().unwrap();
        let upstream = tmp.path().join("upstream.git");
        let work = tmp.path().join("work");
        std::fs::create_dir_all(&work).unwrap();
        git(tmp.path(), &["init", "--bare", "upstream.git"]);

        init_repo(&work);
        std::fs::write(work.join("a.txt"), "hello").unwrap();
        git(&work, &["add", "-A"]);
        git(&work, &["commit", "-m", "init"]);
        git(
            &work,
            &["remote", "add", "origin", &upstream.to_string_lossy()],
        );
        git(&work, &["push", "-u", "origin", "main"]);

        // Sanity-check the fixture itself: `--remotes` only sees
        // `refs/remotes/*`, so a push that left no tracking ref would make
        // the assertion below vacuous.
        assert!(
            !git(
                &work,
                &["for-each-ref", "--format=%(refname)", "refs/remotes"]
            )
            .trim()
            .is_empty(),
            "fixture produced no remote-tracking ref"
        );

        assert_eq!(git_work_state(&work), GitWorkState::Clean);

        // A commit made on a detached HEAD is still local work: `--all`
        // (not `--branches`) is what catches it.
        git(&work, &["checkout", "--detach"]);
        std::fs::write(work.join("b.txt"), "detached").unwrap();
        git(&work, &["add", "-A"]);
        git(&work, &["commit", "-m", "detached work"]);
        assert_eq!(git_work_state(&work), GitWorkState::HasLocalWork);
    }
}
