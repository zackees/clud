//! Issue #1485 acceptance 1, 2 and the size backstop. Every test works under
//! a `tempdir` home; none touches the real `~/.clud` or `~/dev`.

use super::*;
use crate::gc::session_tmp::{self, session_tmp_dir_for};
use std::time::{Duration, SystemTime};
use tempfile::tempdir;

fn age_path(path: &Path, age: Duration) {
    let when = SystemTime::now() - age;
    filetime::set_file_mtime(path, filetime::FileTime::from_system_time(when)).unwrap();
}

/// Acceptance 1: the root is `<home>/.clud/tmp-wt` and is not under the
/// `session_tmp` root.
#[test]
fn worktree_root_is_clud_home_tmp_wt_and_not_under_session_tmp() {
    let home = tempdir().unwrap();
    let root = worktree_root_for(home.path());
    assert_eq!(root, home.path().join(".clud").join("tmp-wt"));
    let tmp = session_tmp_dir_for(home.path());
    assert!(
        !root.starts_with(&tmp),
        "{} must not sit under {}",
        root.display(),
        tmp.display()
    );
    assert_eq!(root.parent(), tmp.parent(), "tmp-wt is a sibling of tmp");
}

/// Acceptance 1: the root exists after the start-up call, and the call is
/// idempotent (a second start with worktrees inside leaves them alone).
#[test]
fn ensure_worktree_root_creates_it_idempotently() {
    let home = tempdir().unwrap();
    let root = ensure_worktree_root_at(home.path()).unwrap();
    assert!(root.is_dir());
    let child = root.join("repo-wt-1");
    fs::create_dir_all(&child).unwrap();
    assert_eq!(ensure_worktree_root_at(home.path()).unwrap(), root);
    assert!(child.is_dir(), "a second start must not disturb worktrees");
}

/// Acceptance 2: the `session_tmp` sweep, given a `tmp-wt/<repo>-wt-1`
/// entry far past the stale threshold, never touches it, because the root is
/// outside its scope.
#[test]
fn session_tmp_sweep_never_touches_tmp_wt() {
    let home = tempdir().unwrap();
    let tmp = session_tmp_dir_for(home.path());
    fs::create_dir_all(&tmp).unwrap();
    let wt = ensure_worktree_root_at(home.path()).unwrap().join("repo-wt-1");
    fs::create_dir_all(&wt).unwrap();
    fs::write(wt.join("f.txt"), "work").unwrap();
    let ancient = Duration::from_secs(365 * 24 * 3600);
    age_path(&wt.join("f.txt"), ancient);
    age_path(&wt, ancient);
    // A stale sibling inside tmp proves the sweep really ran.
    let stale = tmp.join("stale");
    fs::create_dir_all(&stale).unwrap();
    age_path(&stale, ancient);

    session_tmp::sweep_stale_at(&tmp, SystemTime::now(), false).unwrap();

    assert!(!stale.exists(), "the sweep must have run over tmp");
    assert!(wt.join("f.txt").exists(), "tmp-wt is outside the sweep");
}

#[test]
fn under_root_matches_children_only() {
    let home = tempdir().unwrap();
    let root = ensure_worktree_root_at(home.path()).unwrap();
    let child = root.join("repo-wt-2");
    fs::create_dir_all(&child).unwrap();
    assert!(is_under_worktree_root(&child, &root));
    assert!(!is_under_worktree_root(&root, &root), "never the root itself");
    assert!(!is_under_worktree_root(home.path(), &root));
}

#[test]
fn size_check_is_under_over_or_unknown() {
    let dir = tempdir().unwrap();
    fs::create_dir_all(dir.path().join("a/b")).unwrap();
    fs::write(dir.path().join("a/b/x"), vec![0u8; 100]).unwrap();
    fs::write(dir.path().join("a/y"), vec![0u8; 50]).unwrap();
    assert_eq!(check_tree_size(dir.path(), 1_000, 100), SizeCheck::Under(150));
    assert!(matches!(
        check_tree_size(dir.path(), 120, 100),
        SizeCheck::Over(n) if n > 120
    ));
    assert_eq!(check_tree_size(dir.path(), 1_000, 1), SizeCheck::Unknown);
    assert_eq!(
        check_tree_size(&dir.path().join("missing"), 10, 10),
        SizeCheck::Under(0)
    );
}

#[test]
fn size_warning_only_when_over_or_unknown_and_enabled() {
    let root = Path::new("/r");
    assert_eq!(size_warning(root, 10, SizeCheck::Under(5)), None);
    assert!(size_warning(root, 10, SizeCheck::Over(11))
        .unwrap()
        .contains("never deleted for size"));
    assert!(size_warning(root, 10, SizeCheck::Unknown).is_some());
    assert_eq!(size_warning(root, 0, SizeCheck::Over(11)), None);
}

/// Names allocated under the root keep the `<repo>-wt-<suffix>` shape, so
/// reconcile's repo-scoped name matching is unchanged.
#[test]
fn allocated_names_keep_the_reconcile_shape() {
    let home = tempdir().unwrap();
    let wt = worktree_root_for(home.path()).join("clud2-wt-1485");
    let name = wt.file_name().unwrap().to_str().unwrap();
    assert!(crate::gc::reconcile::is_sibling_clone_dir_name("clud2", name));
    assert!(!crate::gc::reconcile::is_sibling_clone_dir_name(
        "clud2",
        WORKTREE_ROOT_DIR_NAME
    ));
}
