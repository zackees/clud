use super::*;

fn git(cwd: &Path, args: &[&str]) {
    worktrees::run_git(cwd, args).unwrap_or_else(|e| panic!("git {args:?}: {e}"));
}

/// A repo with one live worktree, one whose directory was deleted, and one
/// deleted-but-locked worktree.
fn fixture() -> (tempfile::TempDir, PathBuf) {
    let dir = tempfile::tempdir().unwrap();
    let repo = dir.path().join("repo");
    fs::create_dir_all(&repo).unwrap();
    git(&repo, &["init", "-q"]);
    git(&repo, &["config", "user.email", "t@example.com"]);
    git(&repo, &["config", "user.name", "t"]);
    git(&repo, &["commit", "-q", "--allow-empty", "-m", "init"]);
    for name in ["live", "dead", "locked"] {
        let path = dir.path().join(name);
        git(
            &repo,
            &["worktree", "add", "-q", path.to_str().unwrap(), "-b", name],
        );
    }
    git(
        &repo,
        &[
            "worktree",
            "lock",
            dir.path().join("locked").to_str().unwrap(),
        ],
    );
    fs::remove_dir_all(dir.path().join("dead")).unwrap();
    fs::remove_dir_all(dir.path().join("locked")).unwrap();
    (dir, repo)
}

#[test]
fn counts_only_dead_unlocked_registrations() {
    let (_dir, repo) = fixture();
    let common = common_dir(&repo).unwrap();
    assert_eq!(dead_entries(&common).len(), 1);
}

#[test]
fn dry_run_reports_without_pruning() {
    let (_dir, repo) = fixture();
    let repos = vec![repo.to_string_lossy().into_owned()];
    let report = prune_repos(&repos, true);
    assert_eq!((report.total_dead(), report.total_pruned()), (1, 0));
    assert_eq!(count_dead(&repos), 1, "dry run must not prune");
    assert!(report.summary_line().contains("would prune 1"));
}

#[test]
fn prune_removes_dead_and_keeps_live_and_locked() {
    let (dir, repo) = fixture();
    let repos = vec![repo.to_string_lossy().into_owned()];
    let report = prune_repos(&repos, false);
    assert_eq!((report.total_dead(), report.total_pruned()), (1, 1));
    assert_eq!(count_dead(&repos), 0);
    let admin = common_dir(&repo).unwrap().join("worktrees");
    assert!(admin.join("live").is_dir());
    assert!(admin.join("locked").is_dir(), "locked entry must be spared");
    assert!(!admin.join("dead").exists());
    assert!(dir.path().join("live").is_dir());
}

#[test]
fn missing_repo_and_duplicate_common_dirs_are_handled() {
    let (dir, repo) = fixture();
    let live = dir.path().join("live");
    let repos = vec![
        repo.to_string_lossy().into_owned(),
        live.to_string_lossy().into_owned(),
        dir.path().join("gone").to_string_lossy().into_owned(),
    ];
    assert_eq!(count_dead(&repos), 1, "one repo, counted once");
}
