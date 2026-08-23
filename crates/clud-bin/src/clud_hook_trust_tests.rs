//! Trust for a foreign repo's own hooks (#967 Phase 4).

use super::*;
use crate::clud_hook_roots::RootKind;
use std::fs;
use tempfile::TempDir;

fn root(path: &Path, kind: RootKind, trust: RootTrust) -> HookRoot {
    HookRoot {
        path: path.to_path_buf(),
        kind,
        trust,
    }
}

fn write_origin(repo: &Path, url: &str) {
    let git = repo.join(".git");
    fs::create_dir_all(&git).unwrap();
    fs::write(
        git.join("config"),
        format!("[core]\n\tbare = false\n[remote \"origin\"]\n\turl = {url}\n\tfetch = +refs/heads/*\n"),
    )
    .unwrap();
}

fn write_trust(parent: &Path, body: &str) {
    let dir = parent.join(".clud");
    fs::create_dir_all(&dir).unwrap();
    fs::write(dir.join("settings.local.json"), body).unwrap();
}

#[test]
fn an_implicitly_trusted_root_needs_no_grant() {
    // The parent, a declared child, a directory named with --add-dir: the
    // consent is the declaration itself.
    let tmp = TempDir::new().unwrap();

    for kind in [RootKind::Parent, RootKind::Child, RootKind::Extern] {
        assert_eq!(
            decide(tmp.path(), &root(tmp.path(), kind, RootTrust::Implicit)),
            TrustDecision::Allowed,
            "{kind:?}"
        );
    }
}

#[test]
fn an_agent_made_checkout_waits_for_an_explicit_allow() {
    // clud having cloned it is provenance, not consent to run its scripts.
    let tmp = TempDir::new().unwrap();
    let dep = tmp.path().join(".extern-repos").join("dep");
    write_origin(&dep, "https://github.com/someone/dep.git");

    assert_eq!(
        decide(tmp.path(), &root(&dep, RootKind::Extern, RootTrust::RequiresGrant)),
        TrustDecision::NeedsGrant
    );
}

#[test]
fn a_recorded_grant_matching_the_origin_allows_the_hooks() {
    let tmp = TempDir::new().unwrap();
    let dep = tmp.path().join(".extern-repos").join("dep");
    write_origin(&dep, "https://github.com/someone/dep.git");
    write_trust(
        tmp.path(),
        r#"{"extern_trust":{"dep":{"origin":"https://github.com/someone/dep.git","trusted":true}}}"#,
    );

    assert_eq!(
        decide(tmp.path(), &root(&dep, RootKind::Extern, RootTrust::RequiresGrant)),
        TrustDecision::Allowed
    );
}

#[test]
fn a_grant_does_not_transfer_to_a_different_repo_at_the_same_name() {
    // The whole reason the key carries an origin: deleting a checkout and
    // cloning something else under the same directory name must not inherit
    // the answer.
    let tmp = TempDir::new().unwrap();
    let dep = tmp.path().join(".extern-repos").join("dep");
    write_origin(&dep, "https://github.com/someone-else/evil.git");
    write_trust(
        tmp.path(),
        r#"{"extern_trust":{"dep":{"origin":"https://github.com/someone/dep.git","trusted":true}}}"#,
    );

    assert_eq!(
        decide(tmp.path(), &root(&dep, RootKind::Extern, RootTrust::RequiresGrant)),
        TrustDecision::NeedsGrant
    );
}

#[test]
fn cosmetic_origin_differences_do_not_revoke_a_grant() {
    let tmp = TempDir::new().unwrap();
    let dep = tmp.path().join(".extern-repos").join("dep");
    write_origin(&dep, "https://GitHub.com/Someone/Dep");
    write_trust(
        tmp.path(),
        r#"{"extern_trust":{"dep":{"origin":"https://github.com/someone/dep.git/","trusted":true}}}"#,
    );

    assert_eq!(
        decide(tmp.path(), &root(&dep, RootKind::Extern, RootTrust::RequiresGrant)),
        TrustDecision::Allowed
    );
}

#[test]
fn trusted_false_is_an_answer_not_an_absence() {
    let tmp = TempDir::new().unwrap();
    let dep = tmp.path().join(".extern-repos").join("dep");
    write_origin(&dep, "https://github.com/someone/dep.git");
    write_trust(
        tmp.path(),
        r#"{"extern_trust":{"dep":{"origin":"https://github.com/someone/dep.git","trusted":false}}}"#,
    );

    assert_eq!(
        decide(tmp.path(), &root(&dep, RootKind::Extern, RootTrust::RequiresGrant)),
        TrustDecision::NeedsGrant
    );
}

#[test]
fn a_grant_recorded_without_an_origin_still_counts() {
    // Weaker, but an explicit answer about this name -- and the only form
    // available when the checkout has no readable remote.
    let tmp = TempDir::new().unwrap();
    let dep = tmp.path().join(".extern-repos").join("dep");
    fs::create_dir_all(dep.join(".git")).unwrap();
    write_trust(tmp.path(), r#"{"extern_trust":{"dep":{"trusted":true}}}"#);

    assert_eq!(
        decide(tmp.path(), &root(&dep, RootKind::Extern, RootTrust::RequiresGrant)),
        TrustDecision::Allowed
    );
}

#[test]
fn a_grant_naming_an_origin_does_not_apply_to_a_checkout_without_one() {
    let tmp = TempDir::new().unwrap();
    let dep = tmp.path().join(".extern-repos").join("dep");
    fs::create_dir_all(dep.join(".git")).unwrap();
    write_trust(
        tmp.path(),
        r#"{"extern_trust":{"dep":{"origin":"https://github.com/someone/dep.git","trusted":true}}}"#,
    );

    assert_eq!(
        decide(tmp.path(), &root(&dep, RootKind::Extern, RootTrust::RequiresGrant)),
        TrustDecision::NeedsGrant
    );
}

#[test]
fn a_missing_or_unparsable_trust_file_grants_nothing() {
    let tmp = TempDir::new().unwrap();
    let dep = tmp.path().join(".extern-repos").join("dep");
    write_origin(&dep, "https://github.com/someone/dep.git");

    assert!(!is_granted(tmp.path(), "dep", Some("https://github.com/someone/dep.git")));

    write_trust(tmp.path(), "{not json");
    assert!(!is_granted(tmp.path(), "dep", Some("https://github.com/someone/dep.git")));
}

// -----------------------------------------------------------------
// Reading the origin.
// -----------------------------------------------------------------

#[test]
fn the_origin_comes_from_git_config_without_spawning_git() {
    let tmp = TempDir::new().unwrap();
    write_origin(tmp.path(), "git@github.com:someone/dep.git");

    assert_eq!(
        origin_url(tmp.path()).as_deref(),
        Some("git@github.com:someone/dep.git")
    );
}

#[test]
fn a_url_under_another_remote_is_not_the_origin() {
    let tmp = TempDir::new().unwrap();
    let git = tmp.path().join(".git");
    fs::create_dir_all(&git).unwrap();
    fs::write(
        git.join("config"),
        "[remote \"upstream\"]\n\turl = https://example.com/upstream.git\n",
    )
    .unwrap();

    assert_eq!(origin_url(tmp.path()), None);
}

#[test]
fn a_checkout_with_no_config_reports_no_origin() {
    let tmp = TempDir::new().unwrap();
    fs::create_dir_all(tmp.path().join(".git")).unwrap();

    assert_eq!(origin_url(tmp.path()), None);
}

// -----------------------------------------------------------------
// The notice.
// -----------------------------------------------------------------

#[test]
fn the_notice_names_the_repo_the_file_and_what_to_write() {
    let tmp = TempDir::new().unwrap();
    let dep = tmp.path().join(".extern-repos").join("dep");
    write_origin(&dep, "https://github.com/someone/dep.git");

    let notice = grant_notice(
        tmp.path(),
        &root(&dep, RootKind::Extern, RootTrust::RequiresGrant),
    );

    assert!(notice.contains("dep"), "{notice}");
    assert!(notice.contains("settings.local.json"), "{notice}");
    assert!(notice.contains("extern_trust"), "{notice}");
    assert!(notice.contains("https://github.com/someone/dep.git"), "{notice}");
    assert!(notice.contains("gitignored"), "{notice}");
}

#[test]
fn a_held_back_root_is_mentioned_once_not_every_tool_call() {
    let tmp = TempDir::new().unwrap();
    let dep = tmp.path().join(".extern-repos").join("dep");
    write_origin(&dep, "https://github.com/someone/dep.git");
    let held = root(&dep, RootKind::Extern, RootTrust::RequiresGrant);

    assert!(!notice_marker(tmp.path(), "dep").exists());
    notify_once(tmp.path(), &held);
    assert!(notice_marker(tmp.path(), "dep").exists());

    // Second call must not re-announce; the marker is the whole mechanism.
    notify_once(tmp.path(), &held);
    assert!(notice_marker(tmp.path(), "dep").exists());
}
