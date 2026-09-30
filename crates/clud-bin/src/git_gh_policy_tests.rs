//! Issue #1486: the refused / passed argv table for the `git` and `gh`
//! aliases, the example-path derivation, and the frozen refusal strings.

use super::*;

fn split(line: &str) -> Vec<&str> {
    line.split_whitespace().collect()
}

fn git(line: &str) -> Option<Refusal> {
    git_refusal(&split(line))
}

fn gh(line: &str) -> Option<Refusal> {
    gh_refusal(&split(line))
}

// ---- git: refused forms, including after every global option. ----

#[test]
fn git_clone_is_refused_after_any_global_options() {
    for line in [
        "clone https://github.com/zackees/mimalloc-pprof",
        "clone --depth 1 -b main https://github.com/zackees/mimalloc-pprof dest",
        "-C /r clone https://github.com/zackees/mimalloc-pprof",
        "-c core.autocrlf=false clone https://github.com/zackees/mimalloc-pprof",
        "--git-dir /r/.git clone https://github.com/zackees/mimalloc-pprof",
        "--git-dir=/r/.git clone https://github.com/zackees/mimalloc-pprof",
        "--work-tree /r clone https://github.com/zackees/mimalloc-pprof",
        "--work-tree=/r clone https://github.com/zackees/mimalloc-pprof",
        "--no-pager clone https://github.com/zackees/mimalloc-pprof",
        "-P clone https://github.com/zackees/mimalloc-pprof",
        "--bare clone https://github.com/zackees/mimalloc-pprof",
        "--namespace ns clone https://github.com/zackees/mimalloc-pprof",
        "--namespace=ns clone https://github.com/zackees/mimalloc-pprof",
        "--exec-path=/x clone https://github.com/zackees/mimalloc-pprof",
        "--exec-path clone https://github.com/zackees/mimalloc-pprof",
        "-C /a -C b -c x=y --no-pager clone https://github.com/zackees/mimalloc-pprof",
    ] {
        assert_eq!(
            git(line),
            Some(Refusal::GitClone {
                source: Some("https://github.com/zackees/mimalloc-pprof".into())
            }),
            "{line}"
        );
    }
    assert_eq!(git("clone"), Some(Refusal::GitClone { source: None }));
}

#[test]
fn git_worktree_add_is_refused_and_keeps_its_options() {
    assert_eq!(
        git("-C /r worktree add /r-wt-1 -b feat/1486-x origin/main"),
        Some(Refusal::GitWorktreeAdd {
            dirs: vec!["/r".into()],
            path: Some("/r-wt-1".into()),
            branch: Some("feat/1486-x".into()),
            rest: split("-b feat/1486-x origin/main")
                .into_iter()
                .map(String::from)
                .collect(),
        })
    );
    assert_eq!(
        git("worktree add -f --lock --reason why ./x"),
        Some(Refusal::GitWorktreeAdd {
            dirs: vec![],
            path: Some("./x".into()),
            branch: None,
            rest: split("-f --lock --reason why")
                .into_iter()
                .map(String::from)
                .collect(),
        })
    );
    for line in [
        "worktree add ./x -b b origin/main",
        "--no-pager -c a=b worktree add ./x",
        "--git-dir=/r/.git worktree add ./x",
        "worktree add -Bfeat ./x",
    ] {
        assert!(
            matches!(git(line), Some(Refusal::GitWorktreeAdd { .. })),
            "{line}"
        );
    }
}

// ---- git: passed forms. ----

#[test]
fn every_other_git_command_passes() {
    for line in [
        "",
        "status",
        "push --force-with-lease",
        "-C /r status",
        "log --oneline -- clone",
        "commit -m clone",
        "worktree list",
        "worktree list --porcelain",
        "worktree remove ./x",
        "worktree prune",
        "worktree lock ./x",
        "worktree move ./x ./y",
        "worktree repair",
        "-C /r worktree remove /r-wt-1",
        "submodule update --init",
        "submodule add https://github.com/x/y",
        "config alias.cl clone",
        "--version",
        "-C",
        "help clone",
    ] {
        assert_eq!(git(line), None, "{line:?}");
    }
}

// ---- gh: refused and passed forms. ----

#[test]
fn gh_repo_clone_fork_clone_and_create_clone_are_refused() {
    assert_eq!(
        gh("repo clone zackees/clud"),
        Some(Refusal::GhRepoClone {
            repo: Some("zackees/clud".into())
        })
    );
    assert_eq!(
        gh("repo clone zackees/clud dest -- --depth 1"),
        Some(Refusal::GhRepoClone {
            repo: Some("zackees/clud".into())
        })
    );
    for line in [
        "repo fork zackees/clud --clone",
        "repo fork --clone zackees/clud",
        "repo fork zackees/clud --clone=true",
        "repo fork zackees/clud --clone=1",
        "repo fork --org acme --clone zackees/clud",
    ] {
        assert_eq!(
            gh(line),
            Some(Refusal::GhRepoForkClone {
                repo: Some("zackees/clud".into())
            }),
            "{line}"
        );
    }
    assert_eq!(
        gh("repo fork --clone"),
        Some(Refusal::GhRepoForkClone { repo: None })
    );
    for line in [
        "repo create mything --private --clone",
        "repo create mything --public -c",
        "repo create -d desc mything --clone=true",
    ] {
        assert_eq!(
            gh(line),
            Some(Refusal::GhRepoCreateClone {
                name: Some("mything".into())
            }),
            "{line}"
        );
    }
}

#[test]
fn every_other_gh_command_passes() {
    for line in [
        "",
        "issue list",
        "pr view --json state",
        "pr checks 12 --watch",
        "repo view zackees/clud",
        "repo fork zackees/clud",
        "repo fork zackees/clud --clone=false",
        "repo fork zackees/clud --remote",
        "repo fork zackees/clud -- --clone",
        "repo create mything --private",
        "repo list",
        "auth git-credential get",
        "api repos/zackees/clud",
        "release create v1 -c",
    ] {
        assert_eq!(gh(line), None, "{line:?}");
    }
}

// ---- example derivation. ----

#[test]
fn slugs_come_from_every_repo_spec_shape() {
    for (spec, slug) in [
        (
            "https://github.com/zackees/mimalloc-pprof",
            "mimalloc-pprof",
        ),
        ("https://github.com/zackees/clud.git", "clud"),
        ("https://github.com/zackees/clud/", "clud"),
        ("git@github.com:zackees/clud.git", "clud"),
        ("zackees/clud", "clud"),
        ("clud", "clud"),
        (r"C:\src\clud", "clud"),
        ("../weird name!", "weird-name"),
        ("..", "repo"),
        ("", "repo"),
    ] {
        assert_eq!(slug_from_spec(spec), slug, "{spec:?}");
    }
}

#[test]
fn worktree_suffix_prefers_an_issue_number() {
    assert_eq!(worktree_suffix(Some("feat/1486-x"), Some("./x")), "1486");
    assert_eq!(worktree_suffix(Some("b"), Some("../clud-wt-432")), "432");
    assert_eq!(worktree_suffix(Some("b"), Some("./scratch")), "scratch");
    assert_eq!(worktree_suffix(None, None), "new");
}

#[test]
fn repo_slug_resolves_the_main_checkout_from_a_linked_worktree() {
    let tmp = tempfile::tempdir().unwrap();
    let main = tmp.path().join("clud");
    std::fs::create_dir_all(main.join(".git").join("worktrees").join("wt1")).unwrap();
    std::fs::create_dir_all(main.join("src")).unwrap();
    assert_eq!(repo_slug_for_dir(&main.join("src")), "clud");
    let linked = tmp.path().join("elsewhere-wt-9");
    std::fs::create_dir_all(&linked).unwrap();
    let gitdir = main.join(".git").join("worktrees").join("wt1");
    std::fs::write(
        linked.join(".git"),
        format!("gitdir: {}\n", gitdir.display()),
    )
    .unwrap();
    assert_eq!(repo_slug_for_dir(&linked), "clud");
    let plain = tmp.path().join("clud2-wt-7");
    std::fs::create_dir_all(&plain).unwrap();
    assert_eq!(repo_slug_for_dir(&plain), "clud2", "the -wt- infix is cut");
}

#[test]
fn reservation_uses_the_effective_dir_after_every_dash_c() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path().join("clud");
    std::fs::create_dir_all(repo.join(".git")).unwrap();
    let refusal = git("-C clud worktree add ../x -b feat/432-y origin/main").unwrap();
    assert_eq!(
        reservation_for(&refusal, tmp.path()),
        ("clud".to_string(), "432".to_string())
    );
    let clone = git("clone https://github.com/zackees/mimalloc-pprof").unwrap();
    assert_eq!(
        reservation_for(&clone, tmp.path()),
        ("mimalloc-pprof".to_string(), CLONE_SUFFIX.to_string())
    );
}

// ---- the message contract. ----

#[test]
fn refusal_strings_are_frozen() {
    assert_eq!(
        REFUSED_GIT_CLONE,
        "git clone is redirected inside a clud session"
    );
    assert_eq!(
        REFUSED_GIT_WORKTREE_ADD,
        "git worktree add is redirected inside a clud session"
    );
    assert_eq!(
        REFUSED_GH_REPO_CLONE,
        "gh repo clone is redirected inside a clud session"
    );
    assert_eq!(
        REFUSED_GH_REPO_FORK_CLONE,
        "gh repo fork --clone is redirected inside a clud session"
    );
    assert_eq!(
        REFUSED_GH_REPO_CREATE_CLONE,
        "gh repo create --clone is redirected inside a clud session"
    );
    assert_eq!(SAFE_CLONE, "safe-gh-clone");
    assert_eq!(SAFE_WORKTREE, "safe-gh-worktree");
    assert_ne!(REFUSAL_EXIT_CODE, 0);
}

/// #1486 acceptance 2 and 6: the message names the helper, the invocation
/// form and a concrete path, and that path exists when the text is built.
#[test]
fn the_worktree_message_prints_a_path_that_exists() {
    let home = tempfile::tempdir().unwrap();
    let root = crate::gc::worktree_root::worktree_root_for(home.path());
    let refusal = git("worktree add ./x -b feat/432-y origin/main").unwrap();
    let reserved =
        crate::gc::worktree_root::alloc_wt_path_in(&root, "clud", "432").map_err(|e| e.to_string());
    let text = refusal_message(&refusal, "clud", "432", &reserved);
    let path = reserved.unwrap();
    assert!(path.is_dir(), "the printed path exists");
    let lines: Vec<&str> = text.lines().collect();
    assert_eq!(lines[0], REFUSED_GIT_WORKTREE_ADD);
    assert!(lines[1].contains("use: safe-gh-worktree <repo-slug> <issue-number>"));
    assert!(lines[2].starts_with("  e.g. safe-gh-worktree clud 432 --path "));
    assert!(lines[2].ends_with(" -b feat/432-y origin/main"), "{text}");
    assert!(text.contains(&path.display().to_string()), "{text}");
    assert!(
        text.contains("clud-wt-432-2"),
        "names the next ordinal: {text}"
    );
}

#[test]
fn clone_messages_name_safe_gh_clone() {
    let home = tempfile::tempdir().unwrap();
    let root = crate::gc::worktree_root::worktree_root_for(home.path());
    for (refusal, source) in [
        (
            git("clone https://github.com/zackees/mimalloc-pprof").unwrap(),
            "https://github.com/zackees/mimalloc-pprof",
        ),
        (gh("repo clone zackees/clud").unwrap(), "zackees/clud"),
        (
            gh("repo fork zackees/clud --clone").unwrap(),
            "<owner/repo>",
        ),
        (gh("repo create x --clone").unwrap(), "<owner/repo>"),
    ] {
        let reserved = crate::gc::worktree_root::alloc_wt_path_in(&root, "x", CLONE_SUFFIX)
            .map_err(|e| e.to_string());
        let text = refusal_message(&refusal, "x", CLONE_SUFFIX, &reserved);
        assert!(text.starts_with(refusal.headline()), "{text}");
        assert!(
            text.contains(&format!("e.g. safe-gh-clone {source} --path ")),
            "{text}"
        );
        assert!(reserved.unwrap().is_dir());
    }
}

#[test]
fn a_failed_reservation_still_refuses_with_the_helper_named() {
    let refusal = git("clone https://github.com/zackees/clud").unwrap();
    let text = refusal_message(&refusal, "clud", "clone", &Err("no home".into()));
    assert!(text.starts_with(REFUSED_GIT_CLONE));
    assert!(text.contains("safe-gh-clone"));
    assert!(text.contains("could not reserve it now: no home"));
}

#[test]
fn shell_quote_leaves_plain_words_and_quotes_the_rest() {
    assert_eq!(shell_quote("feat/1486-x"), "feat/1486-x");
    assert_eq!(shell_quote("a b"), "'a b'");
    assert_eq!(shell_quote("it's"), r"'it'\''s'");
    assert_eq!(shell_quote(""), "''");
}
