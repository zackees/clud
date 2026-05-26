use super::*;

fn parse(args: &[&str]) -> Args {
    let raw: Vec<String> = args.iter().map(|s| s.to_string()).collect();
    Args::parse_from_raw(raw)
}

#[test]
fn test_prompt_flag() {
    let args = parse(&["clud", "-p", "hello world"]);
    assert_eq!(args.prompt.as_deref(), Some("hello world"));
    assert!(!args.safe);
}

#[test]
fn test_message_flag() {
    let args = parse(&["clud", "-m", "fix the bug"]);
    assert_eq!(args.message.as_deref(), Some("fix the bug"));
}

#[test]
fn test_continue_flag() {
    let args = parse(&["clud", "-c"]);
    assert!(args.continue_session);
}

#[test]
fn test_claude_backend() {
    let args = parse(&["clud", "--claude"]);
    assert!(args.claude);
    assert!(!args.codex);
}

#[test]
fn test_codex_backend() {
    let args = parse(&["clud", "--codex"]);
    assert!(args.codex);
    assert!(!args.claude);
}

#[test]
fn test_model_flag() {
    let args = parse(&["clud", "--model", "opus"]);
    assert_eq!(args.model.as_deref(), Some("opus"));
}

#[test]
fn test_subprocess_flag() {
    let args = parse(&["clud", "--subprocess"]);
    assert!(args.subprocess);
    assert!(!args.pty);
}

#[test]
fn test_pty_flag() {
    let args = parse(&["clud", "--pty"]);
    assert!(args.pty);
    assert!(!args.subprocess);
}

#[test]
fn test_safe_flag() {
    let args = parse(&["clud", "--safe", "-p", "hello"]);
    assert!(args.safe);
    assert_eq!(args.prompt.as_deref(), Some("hello"));
}

#[test]
fn test_dry_run() {
    let args = parse(&["clud", "--dry-run", "-p", "hello"]);
    assert!(args.dry_run);
}

#[test]
fn test_detach_flag() {
    let args = parse(&["clud", "--detach", "-p", "hello"]);
    assert!(args.detach);
    assert!(!args.detachable);
}

#[test]
fn test_detachable_flag() {
    let args = parse(&["clud", "--detachable", "-p", "hello"]);
    assert!(args.detachable);
    assert!(!args.detach);
}

#[test]
fn test_loop_subcommand() {
    let args = parse(&["clud", "loop", "do the task"]);
    match args.command {
        Some(Command::Loop {
            ref task,
            loop_count,
            refresh,
            no_done,
            ref done,
            ref repeat,
        }) => {
            assert_eq!(task.as_deref(), Some("do the task"));
            assert_eq!(loop_count, 50);
            assert!(!refresh);
            assert!(!no_done);
            assert!(done.is_none());
            assert!(repeat.is_none());
        }
        _ => panic!("expected Loop subcommand"),
    }
}

#[test]
fn test_loop_with_count() {
    let args = parse(&["clud", "loop", "--loop-count", "5", "task"]);
    match args.command {
        Some(Command::Loop {
            ref task,
            loop_count,
            ..
        }) => {
            assert_eq!(task.as_deref(), Some("task"));
            assert_eq!(loop_count, 5);
        }
        _ => panic!("expected Loop subcommand"),
    }
}

#[test]
fn test_loop_refresh_flag() {
    let args = parse(&[
        "clud",
        "loop",
        "--refresh",
        "https://github.com/o/r/issues/42",
    ]);
    match args.command {
        Some(Command::Loop {
            ref task,
            refresh,
            no_done,
            ref done,
            ref repeat,
            ..
        }) => {
            assert_eq!(task.as_deref(), Some("https://github.com/o/r/issues/42"));
            assert!(refresh);
            assert!(!no_done);
            assert!(done.is_none());
            assert!(repeat.is_none());
        }
        _ => panic!("expected Loop subcommand"),
    }
}

#[test]
fn test_loop_no_done_flag() {
    let args = parse(&["clud", "loop", "--no-done", "task"]);
    match args.command {
        Some(Command::Loop { no_done, .. }) => {
            assert!(no_done);
        }
        _ => panic!("expected Loop subcommand"),
    }
}

#[test]
fn test_loop_no_done_marker_compat_alias() {
    let args = parse(&["clud", "loop", "--no-done-marker", "task"]);
    match args.command {
        Some(Command::Loop { no_done, .. }) => {
            assert!(no_done);
        }
        _ => panic!("expected Loop subcommand"),
    }
}

#[test]
fn test_loop_done_path() {
    let args = parse(&["clud", "loop", "--done", "DONE.md", "task"]);
    match args.command {
        Some(Command::Loop {
            ref done, no_done, ..
        }) => {
            assert_eq!(done.as_deref(), Some("DONE.md"));
            assert!(!no_done);
        }
        _ => panic!("expected Loop subcommand"),
    }
}

#[test]
fn test_loop_repeat() {
    let args = parse(&["clud", "loop", "--repeat", "1h", "task"]);
    match args.command {
        Some(Command::Loop { ref repeat, .. }) => {
            assert_eq!(repeat.as_deref(), Some("1h"));
        }
        _ => panic!("expected Loop subcommand"),
    }
}

/// Issue #61: --repeat + --done <path> must parse cleanly. The two flags
/// compose; --done overrides --repeat's implicit --no-done at the
/// command-builder layer, but at the args layer they're orthogonal.
#[test]
fn test_loop_repeat_with_done() {
    let args = parse(&[
        "clud",
        "loop",
        "--repeat",
        "30m",
        "--done",
        "STATUS.md",
        "task",
    ]);
    match args.command {
        Some(Command::Loop {
            ref repeat,
            ref done,
            no_done,
            ..
        }) => {
            assert_eq!(repeat.as_deref(), Some("30m"));
            assert_eq!(done.as_deref(), Some("STATUS.md"));
            assert!(!no_done);
        }
        _ => panic!("expected Loop subcommand"),
    }
}

/// Issue #61: --repeat + --no-done must parse cleanly even though the
/// command-builder treats them as overlapping (both suppress the
/// contract). Clap should not reject the combination.
#[test]
fn test_loop_repeat_with_no_done() {
    let args = parse(&["clud", "loop", "--repeat", "5m", "--no-done", "task"]);
    match args.command {
        Some(Command::Loop {
            ref repeat,
            no_done,
            ref done,
            ..
        }) => {
            assert_eq!(repeat.as_deref(), Some("5m"));
            assert!(no_done);
            assert!(done.is_none());
        }
        _ => panic!("expected Loop subcommand"),
    }
}

/// Issue #61: --done and --no-done are mutually exclusive (clap
/// `conflicts_with`). Supplying both must fail — we don't pin the exact
/// error message because clap formatting drifts between versions, but
/// `try_parse_from` must return `Err`.
#[test]
fn test_loop_done_and_no_done_conflict() {
    let argv: Vec<String> = ["clud", "loop", "--done", "DONE.md", "--no-done", "task"]
        .iter()
        .map(|s| s.to_string())
        .collect();
    let result = Args::try_parse_from(argv);
    assert!(
        result.is_err(),
        "clap should reject simultaneous --done and --no-done"
    );
}

#[test]
fn test_up_subcommand() {
    let args = parse(&["clud", "up"]);
    assert!(matches!(args.command, Some(Command::Up { .. })));
}

#[test]
fn test_up_with_message() {
    let args = parse(&["clud", "up", "-m", "bump version"]);
    match args.command {
        Some(Command::Up {
            ref message,
            publish,
        }) => {
            assert_eq!(message.as_deref(), Some("bump version"));
            assert!(!publish);
        }
        _ => panic!("expected Up subcommand"),
    }
}

#[test]
fn test_up_with_publish() {
    let args = parse(&["clud", "up", "--publish"]);
    match args.command {
        Some(Command::Up {
            ref message,
            publish,
        }) => {
            assert!(message.is_none());
            assert!(publish);
        }
        _ => panic!("expected Up subcommand"),
    }
}

#[test]
fn test_up_with_message_and_publish() {
    let args = parse(&["clud", "up", "-m", "release", "--publish"]);
    match args.command {
        Some(Command::Up {
            ref message,
            publish,
        }) => {
            assert_eq!(message.as_deref(), Some("release"));
            assert!(publish);
        }
        _ => panic!("expected Up subcommand"),
    }
}

#[test]
fn test_rebase_subcommand() {
    let args = parse(&["clud", "rebase"]);
    assert!(matches!(args.command, Some(Command::Rebase)));
}

#[test]
fn test_fix_subcommand() {
    let args = parse(&["clud", "fix"]);
    assert!(matches!(args.command, Some(Command::Fix { .. })));
}

#[test]
fn test_fix_with_url() {
    let args = parse(&[
        "clud",
        "fix",
        "https://github.com/user/repo/actions/runs/123",
    ]);
    match args.command {
        Some(Command::Fix { ref url }) => {
            assert_eq!(
                url.as_deref(),
                Some("https://github.com/user/repo/actions/runs/123")
            );
        }
        _ => panic!("expected Fix subcommand"),
    }
}

#[test]
fn test_wasm_subcommand() {
    let args = parse(&["clud", "wasm", "guest.wasm"]);
    match args.command {
        Some(Command::Wasm {
            ref module,
            ref invoke,
        }) => {
            assert_eq!(module, "guest.wasm");
            assert_eq!(invoke, "run");
        }
        _ => panic!("expected Wasm subcommand"),
    }
}

#[test]
fn test_wasm_subcommand_custom_entrypoint() {
    let args = parse(&["clud", "wasm", "guest.wasm", "--invoke", "_start"]);
    match args.command {
        Some(Command::Wasm {
            ref module,
            ref invoke,
        }) => {
            assert_eq!(module, "guest.wasm");
            assert_eq!(invoke, "_start");
        }
        _ => panic!("expected Wasm subcommand"),
    }
}

#[test]
fn test_attach_without_session_id() {
    let args = parse(&["clud", "attach"]);
    match args.command {
        Some(Command::Attach { session_id, last }) => {
            assert!(session_id.is_none());
            assert!(!last);
        }
        _ => panic!("expected Attach subcommand"),
    }
}

#[test]
fn test_attach_with_session_id() {
    let args = parse(&["clud", "attach", "sess-123"]);
    match args.command {
        Some(Command::Attach { session_id, last }) => {
            assert_eq!(session_id.as_deref(), Some("sess-123"));
            assert!(!last);
        }
        _ => panic!("expected Attach subcommand"),
    }
}

#[test]
fn test_attach_with_last() {
    let args = parse(&["clud", "attach", "--last"]);
    match args.command {
        Some(Command::Attach { session_id, last }) => {
            assert!(session_id.is_none());
            assert!(last);
        }
        _ => panic!("expected Attach subcommand"),
    }
}

#[test]
fn test_kill_subcommand() {
    let args = parse(&["clud", "kill", "sess-123"]);
    match args.command {
        Some(Command::Kill { session_id, all }) => {
            assert_eq!(session_id.as_deref(), Some("sess-123"));
            assert!(!all);
        }
        _ => panic!("expected Kill subcommand"),
    }
}

#[test]
fn test_kill_all() {
    let args = parse(&["clud", "kill", "--all"]);
    match args.command {
        Some(Command::Kill { session_id, all }) => {
            assert!(session_id.is_none());
            assert!(all);
        }
        _ => panic!("expected Kill subcommand"),
    }
}

#[test]
fn test_name_flag() {
    let args = parse(&["clud", "--name", "my-session", "--detach", "-p", "hello"]);
    assert_eq!(args.session_name.as_deref(), Some("my-session"));
    assert!(args.detach);
}

#[test]
fn test_transcript_flag() {
    let args = parse(&["clud", "--transcript", "session.log", "-p", "hello"]);
    assert_eq!(
        args.transcript.as_ref().map(|p| p.as_os_str()),
        Some(std::ffi::OsStr::new("session.log"))
    );
}

#[test]
fn test_list_subcommand() {
    let args = parse(&["clud", "list"]);
    assert!(matches!(args.command, Some(Command::List)));
}

#[test]
fn test_logs_with_session_id() {
    let args = parse(&["clud", "logs", "sess-abc"]);
    match args.command {
        Some(Command::Logs {
            session_id,
            follow,
            lines,
            last,
        }) => {
            assert_eq!(session_id.as_deref(), Some("sess-abc"));
            assert!(!follow);
            assert!(lines.is_none());
            assert!(!last);
        }
        _ => panic!("expected Logs subcommand"),
    }
}

#[test]
fn test_logs_follow_flag() {
    let args = parse(&["clud", "logs", "-f", "sess-abc"]);
    match args.command {
        Some(Command::Logs {
            session_id,
            follow,
            last,
            ..
        }) => {
            assert_eq!(session_id.as_deref(), Some("sess-abc"));
            assert!(follow);
            assert!(!last);
        }
        _ => panic!("expected Logs subcommand"),
    }
}

#[test]
fn test_logs_lines_flag() {
    let args = parse(&["clud", "logs", "-n", "100", "sess-abc"]);
    match args.command {
        Some(Command::Logs {
            session_id,
            lines,
            last,
            ..
        }) => {
            assert_eq!(session_id.as_deref(), Some("sess-abc"));
            assert_eq!(lines, Some(100));
            assert!(!last);
        }
        _ => panic!("expected Logs subcommand"),
    }
}

#[test]
fn test_logs_last_flag() {
    let args = parse(&["clud", "logs", "--last"]);
    match args.command {
        Some(Command::Logs {
            session_id,
            follow,
            last,
            ..
        }) => {
            assert!(session_id.is_none());
            assert!(!follow);
            assert!(last);
        }
        _ => panic!("expected Logs subcommand"),
    }
}

#[test]
fn test_logs_last_short_flag() {
    let args = parse(&["clud", "logs", "-l"]);
    match args.command {
        Some(Command::Logs { last, .. }) => {
            assert!(last);
        }
        _ => panic!("expected Logs subcommand"),
    }
}

/// `--last` conflicts with positional session id (mirrors `clud attach`).
#[test]
fn test_logs_last_with_session_id_conflicts() {
    let argv: Vec<String> = ["clud", "logs", "--last", "sess-abc"]
        .iter()
        .map(|s| s.to_string())
        .collect();
    let result = Args::try_parse_from(argv);
    assert!(
        result.is_err(),
        "clap should reject --last combined with a session id"
    );
}

#[test]
fn test_unknown_flags_passthrough() {
    let args = parse(&["clud", "--some-unknown-flag", "-p", "hello"]);
    assert_eq!(args.prompt.as_deref(), Some("hello"));
    assert_eq!(args.passthrough, vec!["--some-unknown-flag"]);
}

#[test]
fn test_passthrough_after_separator() {
    let args = parse(&["clud", "-p", "hello", "--", "--verbose", "--debug"]);
    assert_eq!(args.prompt.as_deref(), Some("hello"));
    assert_eq!(args.passthrough, vec!["--verbose", "--debug"]);
}

#[test]
fn test_verbose_flag() {
    let args = parse(&["clud", "-v"]);
    assert!(args.verbose);
}

#[test]
fn test_default_no_flags() {
    let args = parse(&["clud"]);
    assert!(args.prompt.is_none());
    assert!(args.message.is_none());
    assert!(!args.continue_session);
    assert!(!args.claude);
    assert!(!args.codex);
    assert!(!args.subprocess);
    assert!(!args.pty);
    assert!(!args.safe);
    assert!(!args.dry_run);
    assert!(!args.detach);
    assert!(!args.detachable);
    assert!(args.transcript.is_none());
    assert!(!args.no_dnd);
    assert!(!args.clean_worktrees);
    assert!(!args.fix_hooks);
    assert!(!args.yes);
    assert!(!args.force);
    assert_eq!(args.stale_after, "1d");
    assert!(args.command.is_none());
    assert!(args.passthrough.is_empty());
}

#[test]
fn test_no_dnd_flag() {
    let args = parse(&["clud", "--no-dnd"]);
    assert!(args.no_dnd);
}

#[test]
fn test_no_drag_drop_alias() {
    let args = parse(&["clud", "--no-drag-drop"]);
    assert!(args.no_dnd);
}

#[test]
fn test_no_dnd_default_false() {
    let args = parse(&["clud", "-p", "hello"]);
    assert!(!args.no_dnd);
}

/// Issue #83: top-level `--clean-worktrees` toggles the worktree-cleanup
/// path and accepts the surrounding flags (`--stale-after`, `--yes`,
/// `--force`, the existing `--dry-run`).
#[test]
fn test_clean_worktrees_flag() {
    let args = parse(&["clud", "--clean-worktrees"]);
    assert!(args.clean_worktrees);
    assert_eq!(args.stale_after, "1d");
    assert!(!args.yes);
    assert!(!args.force);
}

#[test]
fn test_clean_worktrees_with_stale_after() {
    let args = parse(&["clud", "--clean-worktrees", "--stale-after", "7d"]);
    assert!(args.clean_worktrees);
    assert_eq!(args.stale_after, "7d");
}

#[test]
fn test_clean_worktrees_with_yes_and_force() {
    let args = parse(&["clud", "--clean-worktrees", "--yes", "--force"]);
    assert!(args.clean_worktrees);
    assert!(args.yes);
    assert!(args.force);
}

#[test]
fn test_clean_worktrees_with_dry_run() {
    let args = parse(&["clud", "--clean-worktrees", "--dry-run"]);
    assert!(args.clean_worktrees);
    assert!(args.dry_run);
}

#[test]
fn test_fix_hooks_flag() {
    let args = parse(&["clud", "--fix-hooks"]);
    assert!(args.fix_hooks);
    assert!(!args.dry_run);
}

#[test]
fn test_fix_hooks_dry_run_with_backend_selection() {
    let args = parse(&["clud", "--fix-hooks", "--dry-run", "--codex"]);
    assert!(args.fix_hooks);
    assert!(args.dry_run);
    assert!(args.codex);
    assert!(!args.claude);
}

#[test]
fn test_yes_short_flag() {
    let args = parse(&["clud", "--clean-worktrees", "-y"]);
    assert!(args.yes);
}

// ---------- issue #110: `clud gc` subcommand group ----------

/// `clud gc` with no subcommand must parse successfully and yield
/// `Some(Command::Gc { subcommand: None })` so the runtime can print
/// help and exit 0.
#[test]
fn test_gc_bare_subcommand_yields_none() {
    let args = parse(&["clud", "gc"]);
    match args.command {
        Some(Command::Gc { ref subcommand }) => assert!(subcommand.is_none()),
        _ => panic!("expected Gc subcommand"),
    }
}

#[test]
fn test_gc_list() {
    let args = parse(&["clud", "gc", "list"]);
    match args.command {
        Some(Command::Gc {
            subcommand: Some(GcSubcommand::List { json }),
        }) => assert!(!json),
        _ => panic!("expected Gc::List"),
    }
}

#[test]
fn test_gc_list_json() {
    // Issue #135: `clud gc list --json` emits JSON for downstream tooling.
    let args = parse(&["clud", "gc", "list", "--json"]);
    match args.command {
        Some(Command::Gc {
            subcommand: Some(GcSubcommand::List { json }),
        }) => assert!(json),
        _ => panic!("expected Gc::List --json"),
    }
}

#[test]
fn test_gc_purge_with_duration() {
    let args = parse(&["clud", "gc", "purge", "1d"]);
    match args.command {
        Some(Command::Gc {
            subcommand:
                Some(GcSubcommand::Purge {
                    ref duration,
                    dry_run,
                    yes,
                    ref kind,
                }),
        }) => {
            assert_eq!(duration.as_deref(), Some("1d"));
            assert!(!dry_run);
            assert!(!yes);
            assert!(kind.is_none());
        }
        _ => panic!("expected Gc::Purge"),
    }
}

#[test]
fn test_gc_purge_without_duration_means_purge_all() {
    // Issue #135 Phase 1: bare `clud gc purge` -> purge ALL non-live-locked.
    let args = parse(&["clud", "gc", "purge"]);
    match args.command {
        Some(Command::Gc {
            subcommand: Some(GcSubcommand::Purge { ref duration, .. }),
        }) => {
            assert!(duration.is_none(), "purge with no arg -> None duration");
        }
        _ => panic!("expected bare Gc::Purge"),
    }
}

#[test]
fn test_gc_purge_dry_run_yes_kind() {
    let args = parse(&[
        "clud",
        "gc",
        "purge",
        "7d",
        "--dry-run",
        "--yes",
        "--kind",
        "worktree",
    ]);
    match args.command {
        Some(Command::Gc {
            subcommand:
                Some(GcSubcommand::Purge {
                    ref duration,
                    dry_run,
                    yes,
                    ref kind,
                }),
        }) => {
            assert_eq!(duration.as_deref(), Some("7d"));
            assert!(dry_run);
            assert!(yes);
            assert_eq!(kind.as_deref(), Some("worktree"));
        }
        _ => panic!("expected Gc::Purge with flags"),
    }
}

#[test]
fn test_no_daemon_flag() {
    // Issue #135: `--no-daemon` disables auto-spawn.
    let args = parse(&["clud", "--no-daemon", "-p", "hi"]);
    assert!(args.no_daemon);
}

#[test]
fn test_gc_reconcile() {
    let args = parse(&["clud", "gc", "reconcile"]);
    match args.command {
        Some(Command::Gc {
            subcommand: Some(GcSubcommand::Reconcile),
        }) => {}
        _ => panic!("expected Gc::Reconcile"),
    }
}
