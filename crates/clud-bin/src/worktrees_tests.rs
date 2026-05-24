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

/// Issue #110: the `locked` porcelain line may carry a reason payload
/// (e.g. `claude agent agent-abf (pid 12345)`). The `gc` liveness probe
/// needs the reason string preserved so it can extract the pid.
#[test]
fn porcelain_captures_locked_reason() {
    let raw = "\
worktree /tmp/wt-with-reason
HEAD aaa
branch refs/heads/feature/x
locked claude agent agent-abf9 (pid 12345)
";
    let v = parse_worktree_porcelain(raw);
    assert_eq!(v.len(), 1);
    assert!(v[0].locked);
    assert_eq!(
        v[0].locked_reason.as_deref(),
        Some("claude agent agent-abf9 (pid 12345)")
    );
}

#[test]
fn porcelain_bare_locked_has_no_reason() {
    let raw = "\
worktree /tmp/wt-bare-lock
HEAD aaa
branch refs/heads/feature/x
locked
";
    let v = parse_worktree_porcelain(raw);
    assert_eq!(v.len(), 1);
    assert!(v[0].locked);
    assert!(v[0].locked_reason.is_none());
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
