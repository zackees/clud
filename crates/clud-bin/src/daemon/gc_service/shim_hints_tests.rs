//! Tests for `shim_hints` (#2103). Repositories are laid out as plain files
//! in a `tempfile` directory (the `.git` dir, `gitdir` and `commondir`
//! files git itself writes), so no `git` runs.

use super::*;

fn write(path: &Path, text: &str) {
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, text).unwrap();
}

/// `main/` with a `.git` dir, and a linked worktree `wt/` elsewhere.
fn repo_with_linked_worktree(root: &Path) -> (PathBuf, PathBuf) {
    let main = root.join("main");
    std::fs::create_dir_all(main.join(".git").join("worktrees").join("wt")).unwrap();
    write(
        &main.join(".git").join("worktrees").join("wt").join("commondir"),
        "../..\n",
    );
    let wt = root.join("elsewhere").join("wt");
    write(
        &wt.join(".git"),
        &format!(
            "gitdir: {}\n",
            main.join(".git").join("worktrees").join("wt").display()
        ),
    );
    (
        std::fs::canonicalize(&main).unwrap(),
        std::fs::canonicalize(&wt).unwrap(),
    )
}

fn source(root: &Path) -> HintSource {
    HintSource::under(&root.join("state"))
}

fn line(cwd: &Path, argv: &[&str]) -> String {
    format!(
        "{}\n",
        serde_json::json!({"tool": "git", "cwd": cwd, "argv": argv})
    )
}

#[test]
fn a_subdirectory_resolves_to_its_checkout() {
    let dir = tempfile::tempdir().unwrap();
    let (main, _) = repo_with_linked_worktree(dir.path());
    std::fs::create_dir_all(main.join("src").join("deep")).unwrap();
    assert_eq!(repo_root_for(&main.join("src").join("deep")), Some(main));
}

#[test]
fn a_linked_worktree_resolves_to_the_main_checkout() {
    let dir = tempfile::tempdir().unwrap();
    let (main, wt) = repo_with_linked_worktree(dir.path());
    assert_eq!(repo_root_for(&wt), Some(main));
}

#[test]
fn a_submodule_is_its_own_root() {
    let dir = tempfile::tempdir().unwrap();
    let parent = dir.path().join("super");
    std::fs::create_dir_all(parent.join(".git").join("modules").join("sub")).unwrap();
    let sub = parent.join("sub");
    write(&sub.join(".git"), "gitdir: ../.git/modules/sub\n");
    let sub = std::fs::canonicalize(sub).unwrap();
    assert_eq!(repo_root_for(&sub), Some(sub));
}

#[test]
fn a_directory_outside_any_repository_has_no_root() {
    let dir = tempfile::tempdir().unwrap();
    let plain = dir.path().join("plain");
    std::fs::create_dir_all(&plain).unwrap();
    // A tempdir may sit inside a checkout on a developer box; only assert
    // when no ancestor is one.
    if dir.path().ancestors().all(|a| !a.join(".git").exists()) {
        assert_eq!(repo_root_for(&plain), None);
    }
}

#[test]
fn ingest_names_the_repo_of_the_cwd_and_of_a_directory_argument() {
    let dir = tempfile::tempdir().unwrap();
    let (main, wt) = repo_with_linked_worktree(dir.path());
    let other = dir.path().join("other");
    std::fs::create_dir_all(other.join(".git")).unwrap();
    let other = std::fs::canonicalize(other).unwrap();
    let source = source(dir.path());
    let outside = dir.path().join("outside");
    std::fs::create_dir_all(&outside).unwrap();
    write(
        &source.log,
        &format!(
            "{}{}",
            line(&wt, &["status"]),
            line(&outside, &["-C", other.to_str().unwrap(), "fetch"]),
        ),
    );
    let repos = ingest(&source);
    assert!(repos.contains_key(main.to_str().unwrap()), "{repos:?}");
    assert!(repos.contains_key(other.to_str().unwrap()), "{repos:?}");
    assert_eq!(repos[main.to_str().unwrap()], wt.to_str().unwrap());
}

#[test]
fn ingest_resumes_from_its_offset_and_leaves_a_partial_line() {
    let dir = tempfile::tempdir().unwrap();
    let (main, _) = repo_with_linked_worktree(dir.path());
    let source = source(dir.path());
    let first = line(&main, &["status"]);
    write(&source.log, &format!("{first}{{\"cwd\":"));
    assert_eq!(ingest(&source).len(), 1);
    let offset: u64 = std::fs::read_to_string(&source.cursor)
        .unwrap()
        .parse()
        .unwrap();
    assert_eq!(offset, first.len() as u64, "the partial line is not consumed");
    assert!(ingest(&source).is_empty(), "nothing new past the offset");
}

#[test]
fn ingest_restarts_after_the_log_rotates() {
    let dir = tempfile::tempdir().unwrap();
    let (main, _) = repo_with_linked_worktree(dir.path());
    let source = source(dir.path());
    write(&source.cursor, "999999");
    write(&source.log, &line(&main, &["status"]));
    assert_eq!(ingest(&source).len(), 1, "a shorter log has rotated");
}

#[test]
fn ingest_without_a_log_is_empty_and_writes_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let source = source(dir.path());
    assert!(ingest(&source).is_empty());
    assert!(!source.cursor.exists());
}
