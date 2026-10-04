use super::super::Roots;
use super::*;

fn setup() -> (tempfile::TempDir, PathBuf, PathBuf, PathBuf) {
    let tmp = tempfile::tempdir().unwrap();
    let launch = tmp.path().join("launch");
    let sibling = tmp.path().join("sibling");
    let other = tmp.path().join("other");
    for root in [&launch, &sibling, &other] {
        fs::create_dir_all(root.join(".git")).unwrap();
        fs::write(root.join("obsolete.rs"), "old").unwrap();
    }
    (tmp, launch, sibling, other)
}

#[test]
fn grant_is_scoped_audited_and_refuses_ungranted_sibling() {
    let (tmp, launch, sibling, other) = setup();
    let state = tmp.path().join("state");
    grant(
        &sibling,
        "approved migration cleanup",
        "session-a",
        std::slice::from_ref(&launch),
        &state,
    )
    .unwrap();
    assert!(load(&state, "session-b", std::slice::from_ref(&launch)).is_empty());
    let grants = load(&state, "session-a", std::slice::from_ref(&launch));
    assert_eq!(grants.len(), 1);
    let mut roots = Roots::fixed(vec![launch], true).with_session_grants(grants);
    let accepted = super::super::resolve(
        sibling.join("obsolete.rs").to_str().unwrap(),
        tmp.path(),
        None,
        &mut roots,
    )
    .unwrap();
    let super::super::Resolved::Present(target) = accepted else {
        panic!("expected existing file");
    };
    assert!(target
        .ledger
        .unwrap()
        .contains("approved migration cleanup"));
    assert!(super::super::resolve(
        other.join("obsolete.rs").to_str().unwrap(),
        tmp.path(),
        None,
        &mut roots,
    )
    .is_err());
    assert!(
        super::super::resolve(sibling.to_str().unwrap(), tmp.path(), None, &mut roots).is_err()
    );
}

#[cfg(unix)]
#[test]
fn symlink_out_of_granted_checkout_is_refused() {
    use std::os::unix::fs::symlink;
    let (tmp, launch, sibling, other) = setup();
    symlink(&other, sibling.join("escape")).unwrap();
    let state = tmp.path().join("state");
    grant(
        &sibling,
        "approved cleanup",
        "session-a",
        std::slice::from_ref(&launch),
        &state,
    )
    .unwrap();
    let mut roots =
        Roots::fixed(vec![launch], true).with_session_grants(load(&state, "session-a", &[]));
    // Loading against a different launch scope cannot transfer a grant.
    assert!(roots.session_grants.is_empty());
    roots = Roots::fixed(vec![tmp.path().join("launch")], true).with_session_grants(load(
        &state,
        "session-a",
        &[tmp.path().join("launch")],
    ));
    assert!(super::super::resolve(
        sibling.join("escape/obsolete.rs").to_str().unwrap(),
        tmp.path(),
        None,
        &mut roots,
    )
    .is_err());
}

#[cfg(unix)]
#[test]
fn replaced_checkout_does_not_keep_its_grant() {
    let (tmp, launch, sibling, _) = setup();
    let state = tmp.path().join("state");
    grant(
        &sibling,
        "approved cleanup",
        "session-a",
        std::slice::from_ref(&launch),
        &state,
    )
    .unwrap();
    fs::rename(&sibling, tmp.path().join("old-sibling")).unwrap();
    fs::create_dir_all(sibling.join(".git")).unwrap();
    assert!(load(&state, "session-a", &[launch]).is_empty());
}
