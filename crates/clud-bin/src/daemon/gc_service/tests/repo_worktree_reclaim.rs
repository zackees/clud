//! Issue #1603: real-git integration test for repo-worktree reclaim, driven
//! through the tick's repo-worktree phase (`run_repo_worktree_phase`) and the
//! real purge pool. Every repository lives in a `tempfile` directory and the
//! registry is a temp redb file: no real checkout and no real `~/.clud` is
//! touched. The origin is a local path, so the PR lookup is unavailable and
//! only the patch-match evidence can prove a squash merge — no network.

use super::*;
use crate::daemon::gc_service::extern_repo::probe_cmd;
use crate::daemon::gc_service::repo_worktree::RepoWorktreeState;

fn run_git(cwd: &Path, args: &[&str]) -> String {
    match probe_cmd("git", cwd, args, Duration::from_secs(30)) {
        Some((0, out)) => out,
        other => panic!(
            "git {} in {} failed: {other:?}",
            args.join(" "),
            cwd.display()
        ),
    }
}

fn git_ok(cwd: &Path, args: &[&str]) -> bool {
    matches!(
        probe_cmd("git", cwd, args, Duration::from_secs(30)),
        Some((0, _))
    )
}

fn identity(root: &Path) {
    run_git(root, &["config", "user.email", "t@example.com"]);
    run_git(root, &["config", "user.name", "t"]);
    run_git(root, &["config", "commit.gpgsign", "false"]);
    run_git(root, &["config", "core.hooksPath", ""]);
}

fn commit_file(dir: &Path, name: &str, body: &str, msg: &str) {
    fs::write(dir.join(name), body).unwrap();
    run_git(dir, &["add", "-A"]);
    run_git(dir, &["commit", "-m", msg]);
}

fn has_branch(repo: &Path, branch: &str) -> bool {
    git_ok(
        repo,
        &[
            "rev-parse",
            "--verify",
            "--quiet",
            &format!("refs/heads/{branch}"),
        ],
    )
}

fn same_path(a: &str, b: &Path) -> bool {
    let a = fs::canonicalize(a).unwrap_or_else(|_| PathBuf::from(a));
    let b = fs::canonicalize(b).unwrap_or_else(|_| b.to_path_buf());
    a == b
}

/// Wait for the off-worker probe's snapshot and install it, as the worker
/// loop would.
fn apply_repo_snapshot(rx: &mpsc::Receiver<RegistryMsg>, spare_reasons: &mut SpareReasons) {
    let deadline = Instant::now() + Duration::from_secs(120);
    loop {
        let remaining = deadline.saturating_duration_since(Instant::now());
        assert!(!remaining.is_zero(), "repo-worktree probe never published");
        if let Ok(RegistryMsg::RepoWorktreeVerdicts(rows)) = rx.recv_timeout(remaining) {
            spare_reasons.replace_repo_worktrees(rows, now_unix());
            return;
        }
    }
}

/// Collect exactly `expected` reclaim outcomes, applying each as the worker
/// would. Other messages (the next probe's snapshot) are ignored.
fn expect_reclaims(
    rx: &mpsc::Receiver<RegistryMsg>,
    spare_reasons: &mut SpareReasons,
    expected: usize,
) -> Vec<(String, ReclaimOutcome)> {
    let deadline = Instant::now() + Duration::from_secs(120);
    let mut out = Vec::new();
    while out.len() < expected {
        let remaining = deadline.saturating_duration_since(Instant::now());
        assert!(
            !remaining.is_zero(),
            "got {} of {expected} reclaim outcomes: {out:?}",
            out.len()
        );
        if let Ok(RegistryMsg::RepoWorktreeReclaimed { path, outcome }) = rx.recv_timeout(remaining)
        {
            apply_repo_worktree_reclaimed(spare_reasons, &path, &outcome);
            out.push((path, outcome));
        }
    }
    out
}

fn outcome_for<'a>(outcomes: &'a [(String, ReclaimOutcome)], path: &Path) -> &'a ReclaimOutcome {
    &outcomes
        .iter()
        .find(|(p, _)| same_path(p, path))
        .unwrap_or_else(|| panic!("no outcome for {}: {outcomes:?}", path.display()))
        .1
}

fn state_for(spare_reasons: &SpareReasons, path: &Path) -> (RepoWorktreeState, String) {
    let (rows, _) = spare_reasons.repo_worktrees();
    let row = rows
        .iter()
        .find(|r| same_path(&r.path, path))
        .unwrap_or_else(|| panic!("no row for {}: {rows:?}", path.display()));
    (row.verdict.state, row.verdict.reason.clone())
}

/// After a squash merge, a real tick removes the landed worktree and its
/// local branch; a dirty sibling, an unmerged sibling and the main checkout
/// are kept; and a sibling that turns dirty between the probe and the delete
/// is spared by the pool's re-verification.
#[test]
fn a_real_tick_reclaims_only_the_squash_merged_clean_worktree() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    let origin = root.join("origin");
    fs::create_dir_all(&origin).unwrap();
    run_git(&origin, &["init", "--initial-branch=main"]);
    identity(&origin);
    commit_file(&origin, "base.txt", "base\n", "base");

    let repo = root.join("repo");
    run_git(
        root,
        &["clone", origin.to_str().unwrap(), repo.to_str().unwrap()],
    );
    identity(&repo);

    let merged = root.join("wt-merged");
    let race = root.join("wt-race");
    let dirty = root.join("wt-dirty");
    let unmerged = root.join("wt-unmerged");
    for (dir, branch) in [
        (&merged, "feat-merged"),
        (&race, "feat-race"),
        (&dirty, "feat-dirty"),
        (&unmerged, "feat-unmerged"),
    ] {
        run_git(
            &repo,
            &["worktree", "add", "-b", branch, dir.to_str().unwrap()],
        );
    }
    commit_file(&merged, "m1.txt", "m1\n", "merged part 1");
    commit_file(&merged, "m2.txt", "m2\n", "merged part 2");
    commit_file(&race, "r.txt", "r\n", "race");
    commit_file(&dirty, "d.txt", "d\n", "dirty");
    commit_file(&unmerged, "u.txt", "u\n", "never merged");

    // Unrelated work first, then one squash commit per landed branch.
    commit_file(&origin, "other.txt", "other\n", "unrelated");
    fs::write(origin.join("m1.txt"), "m1\n").unwrap();
    commit_file(&origin, "m2.txt", "m2\n", "merged (#1)");
    commit_file(&origin, "r.txt", "r\n", "race (#2)");
    commit_file(&origin, "d.txt", "d\n", "dirty (#3)");
    run_git(&repo, &["fetch", "origin"]);
    run_git(&repo, &["remote", "set-head", "origin", "main"]);
    // Work in progress after its merge.
    fs::write(dirty.join("d.txt"), "d, edited\n").unwrap();

    let registry = Registry::open_at(&root.join("gc.redb")).unwrap();
    registry
        .record_repo_visit(&repo.to_string_lossy(), &repo.to_string_lossy(), now_unix())
        .unwrap();
    let pool_tx = spawn_purge_pool(2);
    let (completion_tx, rx) = mpsc::channel::<RegistryMsg>();
    let mut spare_reasons = SpareReasons::new();
    let config = RepoWorktreeGcConfig {
        mode: ReclaimMode::Delete,
        delete_remote: false,
    };

    // Tick 1: nothing cached yet, so nothing is dispatched; the probe runs.
    let dispatched = run_repo_worktree_phase(
        &registry,
        &pool_tx,
        &completion_tx,
        None,
        &mut spare_reasons,
        Vec::new(),
        config,
    );
    assert_eq!(dispatched, 0, "no snapshot yet: nothing may be dispatched");
    apply_repo_snapshot(&rx, &mut spare_reasons);

    use RepoWorktreeState::{Pinned, Reclaimable};
    assert_eq!(
        state_for(&spare_reasons, &merged),
        (Reclaimable, "landed by patch match".to_string())
    );
    assert_eq!(state_for(&spare_reasons, &race).0, Reclaimable);
    assert_eq!(
        state_for(&spare_reasons, &dirty),
        (Pinned, "dirty".to_string())
    );
    assert_eq!(
        state_for(&spare_reasons, &unmerged),
        (Pinned, "unverifiable".to_string())
    );
    assert_eq!(
        state_for(&spare_reasons, &repo),
        (Pinned, "main checkout".to_string())
    );

    // The race: the developer comes back to wt-race after the probe.
    fs::write(race.join("r.txt"), "r, edited\n").unwrap();

    // Tick 2: both cached-reclaimable rows are dispatched; the pool
    // re-verifies each from scratch before touching it.
    let dispatched = run_repo_worktree_phase(
        &registry,
        &pool_tx,
        &completion_tx,
        None,
        &mut spare_reasons,
        Vec::new(),
        config,
    );
    assert_eq!(dispatched, 2, "exactly the two reclaimable rows");
    let outcomes = expect_reclaims(&rx, &mut spare_reasons, 2);

    assert!(
        matches!(
            outcome_for(&outcomes, &merged),
            ReclaimOutcome::Removed { .. }
        ),
        "{outcomes:?}"
    );
    assert!(!merged.exists(), "landed worktree dir must be gone");
    assert!(
        !has_branch(&repo, "feat-merged"),
        "local branch must be gone"
    );
    assert!(
        !run_git(&repo, &["worktree", "list", "--porcelain"]).contains("wt-merged"),
        "git must no longer list it"
    );
    assert!(
        spare_reasons
            .repo_worktrees()
            .0
            .iter()
            .all(|r| !same_path(&r.path, &merged)),
        "gc list must drop the removed row"
    );

    assert_eq!(
        outcome_for(&outcomes, &race),
        &ReclaimOutcome::Spared("now dirty".to_string()),
        "the pool must veto a worktree that turned dirty after the probe"
    );
    assert!(race.exists());
    assert!(has_branch(&repo, "feat-race"));
    assert_eq!(
        fs::read_to_string(race.join("r.txt")).unwrap(),
        "r, edited\n",
        "the racing edit must survive"
    );

    for (dir, branch) in [(&dirty, "feat-dirty"), (&unmerged, "feat-unmerged")] {
        assert!(dir.exists(), "{} must be kept", dir.display());
        assert!(has_branch(&repo, branch), "{branch} must be kept");
    }
    assert_eq!(
        fs::read_to_string(dirty.join("d.txt")).unwrap(),
        "d, edited\n"
    );
    assert!(repo.join(".git").exists(), "main checkout untouched");
    assert!(has_branch(&repo, "main"));
}

/// Observe mode reports and never dispatches, even for a reclaimable row.
#[test]
fn observe_mode_never_dispatches() {
    use crate::daemon::gc_service::repo_worktree::RepoWorktreeVerdict;
    let tmp = tempfile::tempdir().unwrap();
    let wt = tmp.path().join("wt");
    fs::create_dir_all(&wt).unwrap();
    let registry = Registry::open_at(&tmp.path().join("gc.redb")).unwrap();
    let mut spare_reasons = SpareReasons::new();
    spare_reasons.replace_repo_worktrees(
        vec![RepoWorktreeRow {
            path: wt.to_string_lossy().to_string(),
            repo_root: tmp.path().to_string_lossy().to_string(),
            branch: Some("feat".to_string()),
            tip: Some("abc".to_string()),
            mtime_unix: 0,
            verdict: RepoWorktreeVerdict {
                state: RepoWorktreeState::Reclaimable,
                reason: "merged via PR #1".to_string(),
            },
        }],
        now_unix(),
    );
    let pool_tx = spawn_purge_pool(1);
    let (completion_tx, _rx) = mpsc::channel::<RegistryMsg>();
    let dispatched = run_repo_worktree_phase(
        &registry,
        &pool_tx,
        &completion_tx,
        None,
        &mut spare_reasons,
        Vec::new(),
        RepoWorktreeGcConfig {
            mode: ReclaimMode::Observe,
            delete_remote: false,
        },
    );
    assert_eq!(dispatched, 0);
    assert!(wt.exists());
}
