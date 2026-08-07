use super::*;
use std::path::PathBuf;

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
fn test_harness_flag_is_typed_and_not_forwarded() {
    for (raw, expected) in [
        (
            vec!["clud", "--harness", "default"],
            HarnessSelection::Default,
        ),
        (
            vec!["clud", "--harness=claude", "--codex"],
            HarnessSelection::Claude,
        ),
        (
            vec!["clud", "--codex", "--harness", "codex"],
            HarnessSelection::Codex,
        ),
    ] {
        let args = parse(&raw);
        assert_eq!(args.harness, Some(expected));
        assert!(args.passthrough.is_empty());
    }
}

#[test]
fn test_harness_rejects_invalid_or_missing_values() {
    assert!(Args::try_parse_from(["clud", "--harness", "other"]).is_err());
    assert!(Args::try_parse_from(["clud", "--harness"]).is_err());
}

/// #629: auth management is a first-class clud command, never backend
/// passthrough. This intentionally uses clap directly so the RED state proves
/// the command family is registered before the unknown-flag splitter learns it.
#[test]
fn codex_auth_status_is_a_registered_command() {
    let args = Args::try_parse_from(["clud", "codex-auth", "status"]).unwrap();
    assert!(matches!(
        args.command,
        Some(Command::CodexAuth {
            subcommand: CodexAuthSubcommand::Status { json: false }
        })
    ));
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
fn test_graphics_flag() {
    let args = parse(&[
        "clud",
        "--graphics",
        "sixel",
        "--graphics-image",
        "banner.png",
    ]);
    assert_eq!(args.graphics, crate::graphics::GraphicsMode::Sixel);
    assert_eq!(
        args.graphics_image.as_ref().map(|p| p.as_os_str()),
        Some(std::ffi::OsStr::new("banner.png"))
    );
    assert!(args.passthrough.is_empty());
}

#[test]
fn test_graphics_equals_form_stays_known() {
    let args = parse(&["clud", "--graphics=off", "--some-backend-flag"]);
    assert_eq!(args.graphics, crate::graphics::GraphicsMode::Off);
    assert_eq!(args.passthrough, vec!["--some-backend-flag"]);
}

#[test]
fn test_demo_gfx_sixel_flag() {
    let args = parse(&["clud", "--demo-gfx-sixel", "--some-backend-flag"]);
    assert!(args.demo_gfx_sixel);
    assert_eq!(args.passthrough, vec!["--some-backend-flag"]);
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
fn test_optimize_defaults_to_rust_global_soldr() {
    let args = parse(&["clud", "optimize"]);
    match args.command {
        Some(Command::Optimize {
            target,
            global,
            repo,
            install_soldr,
            use_soldr_shims,
            ref soldr_version,
        }) => {
            assert_eq!(target, OptimizeTarget::Rust);
            assert!(!global);
            assert!(!repo);
            assert!(install_soldr);
            assert!(use_soldr_shims);
            assert_eq!(soldr_version, "0.7.11");
        }
        other => panic!("expected Optimize subcommand, got {other:?}"),
    }
}

#[test]
fn test_optimize_rust_repo_flags() {
    let args = parse(&[
        "clud",
        "optimize",
        "rust",
        "--repo",
        "--install-soldr=false",
        "--use-soldr-shims=false",
        "--soldr-version",
        "1.2.3",
    ]);
    match args.command {
        Some(Command::Optimize {
            target,
            global,
            repo,
            install_soldr,
            use_soldr_shims,
            ref soldr_version,
        }) => {
            assert_eq!(target, OptimizeTarget::Rust);
            assert!(!global);
            assert!(repo);
            assert!(!install_soldr);
            assert!(!use_soldr_shims);
            assert_eq!(soldr_version, "1.2.3");
        }
        other => panic!("expected Optimize subcommand, got {other:?}"),
    }
}

#[test]
fn test_settings_defaults_to_interactive() {
    let args = parse(&["clud", "settings"]);
    match args.command {
        Some(Command::Settings { list }) => assert!(!list),
        other => panic!("expected Settings subcommand, got {other:?}"),
    }
}

#[test]
fn test_settings_list_flag() {
    let args = parse(&["clud", "settings", "--list"]);
    match args.command {
        Some(Command::Settings { list }) => assert!(list),
        other => panic!("expected Settings subcommand, got {other:?}"),
    }
}

#[test]
fn test_optimize_soldr_alias_selects_rust() {
    let args = parse(&["clud", "optimize", "soldr"]);
    match args.command {
        Some(Command::Optimize { target, .. }) => {
            assert_eq!(target, OptimizeTarget::Rust);
        }
        other => panic!("expected Optimize subcommand, got {other:?}"),
    }
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
fn test_do_subcommand() {
    let args = parse(&["clud", "do", "https://github.com/zackees/clud/issues/866"]);
    match args.command {
        Some(Command::Do { ref url }) => {
            assert_eq!(url, "https://github.com/zackees/clud/issues/866");
        }
        _ => panic!("expected Do subcommand"),
    }
}

#[test]
fn test_grind_subcommand_without_url() {
    let args = parse(&["clud", "grind"]);
    match args.command {
        Some(Command::Grind { ref url }) => assert!(url.is_none()),
        _ => panic!("expected Grind subcommand"),
    }
}

#[test]
fn test_grind_subcommand_with_url() {
    let args = parse(&["clud", "grind", "https://github.com/zackees/clud/issues"]);
    match args.command {
        Some(Command::Grind { ref url }) => {
            assert_eq!(
                url.as_deref(),
                Some("https://github.com/zackees/clud/issues")
            );
        }
        _ => panic!("expected Grind subcommand"),
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
fn test_slay_subcommand() {
    let args = parse(&["clud", "slay"]);
    assert!(matches!(args.command, Some(Command::Slay)));
    assert!(args.passthrough.is_empty());
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

/// Issue #508: the documented `clud tool run <tool> … run -- <cmd…>` shape.
///
/// The separator and everything behind it are *data* for the bundled tool,
/// which does its own `run -- <cmd…>` split. Before the fix the splitter
/// claimed them for `passthrough` — which nothing on the `tool` path reads —
/// and the tool reported `run: missing command`.
#[test]
fn tool_run_forwards_the_separator_and_everything_after_it() {
    let args = parse(&[
        "clud",
        "tool",
        "run",
        "docker/docker-build.py",
        "soldr",
        "C:/repo",
        "run",
        "--",
        "soldr",
        "cargo",
        "fmt",
        "-p",
        "soldr-cli",
        "--",
        "--check",
    ]);
    assert!(
        args.passthrough.is_empty(),
        "nothing may be diverted to passthrough: {:?}",
        args.passthrough
    );
    let Some(Command::Tool {
        subcommand: ToolSubcommand::Run { rel_path, args },
    }) = args.command
    else {
        panic!("expected `tool run`");
    };
    assert_eq!(rel_path, "docker/docker-build.py");
    // Both separators survive, and so does `-p`, which is a clud value flag
    // and would otherwise have been eaten along with its value.
    assert_eq!(
        args,
        vec![
            "soldr",
            "C:/repo",
            "run",
            "--",
            "soldr",
            "cargo",
            "fmt",
            "-p",
            "soldr-cli",
            "--",
            "--check",
        ]
    );
}

/// The acceptance criterion from the issue, in its smallest form.
#[test]
fn tool_run_round_trips_a_trivial_command_after_the_separator() {
    let args = parse(&[
        "clud",
        "tool",
        "run",
        "docker/docker-build.py",
        "soldr",
        "C:/repo",
        "run",
        "--",
        "echo",
        "hi",
    ]);
    let Some(Command::Tool {
        subcommand: ToolSubcommand::Run { args, .. },
    }) = args.command
    else {
        panic!("expected `tool run`");
    };
    assert_eq!(args, vec!["soldr", "C:/repo", "run", "--", "echo", "hi"]);
}

/// The behaviour the fix must *not* break, and which a broader fix does break:
/// every subcommand other than `tool` keeps `--` as clud's end-of-flags marker.
///
/// Found the hard way — routing `--` to clap for `loop` makes it reject
/// `--verbose` as an unknown argument and call `exit(2)`, which takes the whole
/// process with it.
#[test]
fn separator_after_a_non_tool_subcommand_still_feeds_passthrough() {
    let args = parse(&["clud", "loop", "task", "--", "--verbose", "--debug"]);
    assert_eq!(args.passthrough, vec!["--verbose", "--debug"]);
    assert!(matches!(args.command, Some(Command::Loop { .. })));
}

/// The behaviour the fix must *not* break: before any subcommand, `--` still
/// ends clud's own flags and hands the rest to the backend agent.
#[test]
fn separator_before_a_subcommand_still_feeds_passthrough() {
    let args = parse(&["clud", "-p", "hello", "--", "--verbose"]);
    assert_eq!(args.prompt.as_deref(), Some("hello"));
    assert_eq!(args.passthrough, vec!["--verbose"]);
    assert!(args.command.is_none());
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
    assert_eq!(args.graphics, crate::graphics::GraphicsMode::Auto);
    assert!(args.graphics_image.is_none());
    assert!(!args.safe);
    assert!(!args.dry_run);
    assert!(!args.detach);
    assert!(!args.detachable);
    assert!(args.transcript.is_none());
    assert!(!args.no_dnd);
    assert!(!args.clean_worktrees);
    assert!(!args.fix_hooks);
    assert!(!args.no_fix_hooks);
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

#[path = "args_tests/commands.rs"]
mod commands;
