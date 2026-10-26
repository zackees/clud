use super::*;

fn win(raw: &str) -> String {
    canonical_root(raw, PathHost::Windows, true).unwrap()
}

fn repo() -> RepoIdentity {
    repo_identity(Some("git@github.com:talos-hq/clud.git"), "C:/dev/clud/.git")
}

fn ws(root: &str, branch: &str) -> Workspace {
    workspace(
        repo(),
        root.to_string(),
        Some(branch.to_string()),
        Some("abc123".to_string()),
        Classification::LinkedWorktree,
    )
}

#[test]
fn every_windows_spelling_of_one_directory_is_one_root() {
    let expected = "c:/dev/foo";
    for raw in [
        r"C:\dev\foo",
        r"c:\dev\foo\",
        "C:/dev/foo",
        "C:/dev//foo/./",
        "/c/dev/foo",
        "/C/dev/foo",
        "/mnt/c/dev/foo",
        r"\\?\C:\dev\foo",
        r"C:\dev\bar\..\foo",
    ] {
        assert_eq!(win(raw), expected, "{raw}");
    }
}

#[test]
fn unc_and_its_verbatim_form_agree() {
    assert_eq!(win(r"\\srv\share\dev\foo"), "//srv/share/dev/foo");
    assert_eq!(win(r"\\?\UNC\srv\share\dev\foo"), "//srv/share/dev/foo");
    assert_eq!(win("//srv/share/dev/foo/"), "//srv/share/dev/foo");
}

#[test]
fn drive_roots_keep_their_separator() {
    assert_eq!(win(r"C:\"), "c:/");
    assert_eq!(win("/c"), "c:/");
    assert_eq!(canonical_root("/", PathHost::Posix, false).unwrap(), "/");
}

#[test]
fn case_folding_follows_the_filesystem_not_the_host() {
    assert_eq!(
        canonical_root(r"C:\Dev\Foo", PathHost::Windows, false).unwrap(),
        "C:/Dev/Foo"
    );
    assert_eq!(
        canonical_root("/Users/Me/Repo", PathHost::Posix, true).unwrap(),
        "/users/me/repo"
    );
    assert_eq!(
        canonical_root("/home/Me/Repo/", PathHost::Posix, false).unwrap(),
        "/home/Me/Repo"
    );
}

#[test]
fn relative_and_unrepresentable_paths_are_refused() {
    for raw in ["dev/foo", "C:foo", "/home/me", "/mnt/data/x", r"\\srv"] {
        assert!(
            matches!(
                canonical_root(raw, PathHost::Windows, true),
                Err(IdentityError::Relative(_))
            ),
            "{raw}"
        );
    }
    assert!(matches!(
        canonical_root(r"\\?\Volume{1234}\dev", PathHost::Windows, true),
        Err(IdentityError::UnsupportedVerbatim(_))
    ));
    assert!(canonical_root("dev/foo", PathHost::Posix, false).is_err());
}

#[test]
fn remote_spellings_of_one_repository_agree() {
    let expected = Some("github.com/talos-hq/clud".to_string());
    for url in [
        "git@github.com:talos-hq/clud.git",
        "https://github.com/talos-hq/clud",
        "https://github.com/Talos-HQ/clud.git/",
        "ssh://git@github.com/talos-hq/clud.git",
        "ssh://git@github.com:22/talos-hq/clud.git",
    ] {
        assert_eq!(normalize_remote(url), expected, "{url}");
    }
    assert_eq!(
        normalize_remote("git@github.com:1password/cli.git"),
        Some("github.com/1password/cli".to_string()),
        "a leading digit in an scp owner is not a port"
    );
    assert_eq!(normalize_remote("not a url"), None);
    assert_eq!(normalize_remote("https://github.com/"), None);
}

#[test]
fn repo_identity_prefers_the_remote_and_falls_back_to_the_common_dir() {
    let a = repo_identity(Some("https://github.com/talos-hq/clud"), "C:/one/.git");
    let b = repo_identity(Some("git@github.com:talos-hq/clud.git"), "D:/two/.git");
    assert_eq!(a, b, "one remote, two clones: one repository");
    assert_eq!(a.slug, "clud");

    let local = repo_identity(None, "/home/me/scratch/.git");
    assert_eq!(local.slug, "scratch");
    assert_ne!(
        local.id,
        repo_identity(None, "/home/me/other/scratch/.git").id
    );
}

#[test]
fn worktrees_with_colliding_leaf_names_get_distinct_ids() {
    let a = ws("c:/dev/a/feature", "feature");
    let b = ws("c:/dev/b/feature", "feature");
    assert_ne!(a.id, b.id);
    assert_ne!(
        resource_name(&a, "rust", "target", "d1"),
        resource_name(&b, "rust", "target", "d1")
    );
    assert_eq!(
        a.id,
        ws("c:/dev/a/feature", "other-branch").id,
        "branch is not identity"
    );
}

#[test]
fn names_are_docker_safe_bounded_and_keyed_on_every_part() {
    let w = ws("c:/dev/clud", "feat/Ünïcode branch!");
    let name = resource_name(&w, "Rust Stack/v2", "cargo_home", "d1");
    assert!(name.len() <= 128, "{name}");
    assert!(
        name.chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-'),
        "{name}"
    );
    assert!(
        name.starts_with("dh-clud-rust-stack-v2-cargo-home-"),
        "{name}"
    );
    let base = resource_name(&w, "rust", "target", "d1");
    assert_ne!(base, resource_name(&w, "rust", "target", "d2"));
    assert_ne!(base, resource_name(&w, "rust", "cargo-home", "d1"));
    assert_ne!(base, resource_name(&w, "node", "target", "d1"));
}

#[test]
fn slugs_never_come_back_empty_or_overlong() {
    assert_eq!(slug("!!!", 10), "x");
    assert_eq!(slug("feat/Foo--Bar", 40), "feat-foo-bar");
    assert_eq!(slug("abcdefghij-klm", 11), "abcdefghij");
}

#[test]
fn labels_carry_the_complete_managed_contract() {
    let w = ws("c:/dev/clud", "feat/x");
    let facts = ResourceFacts {
        registry_id: "reg-1",
        stack: "rust",
        role: "target",
        scope_digest: "d1",
        created_at: 1_700_000_000,
    };
    let got = labels(&w, &facts);
    let keys: Vec<&str> = got
        .iter()
        .map(|(key, _)| key.strip_prefix(LABEL_PREFIX).unwrap())
        .collect();
    assert_eq!(
        keys,
        [
            "branch",
            "branch-slug",
            "created-at",
            "head",
            "managed",
            "registry",
            "repo-id",
            "repo-slug",
            "role",
            "schema",
            "spec-digest",
            "stack",
            "workspace-id",
            "workspace-kind",
            "workspace-root",
        ]
    );
    let value = |key: &str| {
        got.iter()
            .find(|(k, _)| k == &format!("{LABEL_PREFIX}{key}"))
            .map(|(_, v)| v.as_str())
            .unwrap()
    };
    assert_eq!(value("managed"), "true");
    assert_eq!(value("registry"), "reg-1");
    assert_eq!(value("workspace-id"), w.id);
    assert_eq!(value("branch-slug"), "feat-x");
    assert_eq!(value("workspace-kind"), "linked");
}
