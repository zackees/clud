//! Unit tests for `hooks.rs`.
//!
//! Coverage:
//! - Payload deserialization with `#[serde(default)]` on every field.
//! - The `<context>` formatter (empty + populated).
//! - The `remember:` / `save this:` directive extractor.
//! - Per-runner exit code is always 0 even on malformed input.
//! - Env-var gating for `CLUD_MEMORY_AUTO_CONSOLIDATE_ON_STOP`.

use super::*;

#[test]
fn parse_json_handles_empty_payload() {
    let p: SessionStartPayload = parse_json("").expect("empty -> default");
    assert!(p.session_id.is_empty());
    assert!(p.cwd.is_empty());
    assert!(p.model.is_none());
}

#[test]
fn parse_json_handles_partial_payload() {
    let p: SessionStartPayload =
        parse_json(r#"{"session_id":"abc"}"#).expect("partial -> default fill");
    assert_eq!(p.session_id, "abc");
    assert!(p.cwd.is_empty());
}

#[test]
fn parse_json_session_start_full_payload() {
    let p: SessionStartPayload = parse_json(
        r#"{"session_id":"s1","cwd":"/tmp","model":"claude-opus-4-7","source":"claude"}"#,
    )
    .expect("full payload parses");
    assert_eq!(p.session_id, "s1");
    assert_eq!(p.cwd, "/tmp");
    assert_eq!(p.model.as_deref(), Some("claude-opus-4-7"));
}

#[test]
fn parse_json_codex_session_start_alias() {
    // Codex uses snake_case `working_directory`.
    let p: SessionStartPayload =
        parse_json(r#"{"session_id":"s1","working_directory":"/tmp"}"#).expect("codex variant");
    assert_eq!(p.cwd, "/tmp");
}

#[test]
fn parse_json_user_prompt_submit() {
    let p: UserPromptSubmitPayload =
        parse_json(r#"{"session_id":"s1","prompt":"remember: foo"}"#).expect("prompt parses");
    assert_eq!(p.session_id, "s1");
    assert_eq!(p.prompt, "remember: foo");
}

#[test]
fn parse_json_post_tool_use_codex_aliases() {
    // Codex emits `tool_call` and `tool_result`.
    let p: PostToolUsePayload = parse_json(
        r#"{"session_id":"s1","tool_name":"Read","tool_call":{"path":"a"},"tool_result":"ok"}"#,
    )
    .expect("codex aliases parse");
    assert_eq!(p.session_id, "s1");
    assert_eq!(p.tool_name, "Read");
    assert_eq!(p.tool_input["path"].as_str(), Some("a"));
    assert_eq!(p.tool_response.as_str(), Some("ok"));
}

#[test]
fn parse_json_stop_payload_full() {
    let p: StopPayload =
        parse_json(r#"{"session_id":"s1","reason":"task_done","stop_hook_active":true}"#)
            .expect("stop parses");
    assert_eq!(p.session_id, "s1");
    assert_eq!(p.reason.as_deref(), Some("task_done"));
    assert!(p.stop_hook_active);
}

#[test]
fn parse_json_rejects_malformed() {
    let r = parse_json::<SessionStartPayload>("{not valid json");
    assert!(r.is_err());
}

#[test]
fn empty_context_block_has_stable_format() {
    let block = empty_context_block();
    assert!(block.starts_with("<context source=\"clud-memory\">"));
    assert!(block.contains("## Recent memory"));
    assert!(block.contains("(none)"));
    assert!(block.ends_with("</context>\n"));
}

#[test]
fn format_context_block_emits_one_bullet_per_row() {
    let rows = vec![
        RecallRow {
            tier: "working".into(),
            content: "Build: bash build (Rust + maturin)".into(),
        },
        RecallRow {
            tier: "episodic".into(),
            content: "Lint mandatory after edits: bash lint".into(),
        },
    ];
    let block = format_context_block(&rows);
    assert!(block.contains("- [working] Build: bash build (Rust + maturin)"));
    assert!(block.contains("- [episodic] Lint mandatory after edits: bash lint"));
    assert!(block.starts_with("<context source=\"clud-memory\">"));
    assert!(block.ends_with("</context>\n"));
}

#[test]
fn format_context_block_truncates_long_content() {
    let long = "a".repeat(500);
    let rows = vec![RecallRow {
        tier: "working".into(),
        content: long,
    }];
    let block = format_context_block(&rows);
    // One bullet line; the content portion is truncated to <= 160 chars
    // (the excerpt cap), so the bullet should be far shorter than 500.
    let bullet_line = block
        .lines()
        .find(|l| l.starts_with("- ["))
        .expect("one bullet");
    assert!(bullet_line.chars().count() <= 200);
    assert!(bullet_line.contains('…'));
}

#[test]
fn format_context_block_empty_rows_returns_empty_block() {
    let block = format_context_block(&[]);
    assert_eq!(block, empty_context_block());
}

#[test]
fn extract_save_directive_remember_colon() {
    assert_eq!(
        extract_save_directive("remember: this is a note"),
        Some("this is a note".to_string())
    );
}

#[test]
fn extract_save_directive_remember_this() {
    assert_eq!(
        extract_save_directive("remember this: payload"),
        Some("payload".to_string())
    );
}

#[test]
fn extract_save_directive_save_this() {
    assert_eq!(
        extract_save_directive("save this: payload"),
        Some("payload".to_string())
    );
}

#[test]
fn extract_save_directive_is_case_insensitive() {
    assert_eq!(
        extract_save_directive("REMEMBER: hello"),
        Some("hello".to_string())
    );
    assert_eq!(
        extract_save_directive("Save This: hello"),
        Some("hello".to_string())
    );
}

#[test]
fn extract_save_directive_returns_none_without_directive() {
    assert!(extract_save_directive("can you help me fix this bug").is_none());
    assert!(extract_save_directive("remembering yesterday").is_none());
}

#[test]
fn extract_save_directive_trims_leading_whitespace() {
    assert_eq!(
        extract_save_directive("   remember:   spaced out"),
        Some("spaced out".to_string())
    );
}

#[test]
fn run_session_start_emits_block_on_malformed_stdin() {
    // No daemon, malformed stdin: must exit 0 and write an empty
    // context block so the upstream injection path sees a parseable
    // chunk.
    let mut out: Vec<u8> = Vec::new();
    let rc = run_session_start(b"this is not json".as_slice(), &mut out);
    assert_eq!(rc, 0);
    let body = String::from_utf8(out).unwrap();
    assert!(body.contains("<context source=\"clud-memory\">"));
}

#[test]
fn run_session_start_handles_empty_stdin() {
    let mut out: Vec<u8> = Vec::new();
    let rc = run_session_start(io::empty(), &mut out);
    assert_eq!(rc, 0);
    let body = String::from_utf8(out).unwrap();
    assert!(body.contains("<context source=\"clud-memory\">"));
}

#[test]
fn run_user_prompt_submit_noop_when_no_directive() {
    // No daemon — yet still must exit 0 because we never attempt the
    // save when no directive is present.
    let payload = br#"{"session_id":"s1","prompt":"hello there"}"#;
    let rc = run_user_prompt_submit(payload.as_slice());
    assert_eq!(rc, 0);
}

#[test]
fn run_user_prompt_submit_empty_stdin_is_noop() {
    let rc = run_user_prompt_submit(io::empty());
    assert_eq!(rc, 0);
}

#[test]
fn run_user_prompt_submit_handles_malformed_json() {
    let rc = run_user_prompt_submit(b"garbage".as_slice());
    assert_eq!(rc, 0);
}

#[test]
fn run_post_tool_use_is_noop_v0_1() {
    let payload = br#"{"session_id":"s1","tool_name":"Read"}"#;
    let rc = run_post_tool_use(payload.as_slice());
    assert_eq!(rc, 0);
}

#[test]
fn run_post_tool_use_handles_empty_stdin() {
    let rc = run_post_tool_use(io::empty());
    assert_eq!(rc, 0);
}

#[test]
fn run_stop_exits_zero_when_consolidate_disabled() {
    // SAFETY: single-threaded test; env mutation is scoped.
    let prev = std::env::var(ENV_AUTO_CONSOLIDATE_ON_STOP).ok();
    // SAFETY: tests run on one thread per #[cfg(test)] binary in cargo
    // by default for unit tests; if a future config changes that, the
    // worst case is a flaky read of the env var here.
    unsafe { std::env::remove_var(ENV_AUTO_CONSOLIDATE_ON_STOP) };
    let payload = br#"{"session_id":"s1","reason":"user_quit"}"#;
    let rc = run_stop(payload.as_slice());
    assert_eq!(rc, 0);
    if let Some(v) = prev {
        unsafe { std::env::set_var(ENV_AUTO_CONSOLIDATE_ON_STOP, v) };
    }
}

#[test]
fn run_stop_handles_malformed_payload() {
    let rc = run_stop(b"not json".as_slice());
    assert_eq!(rc, 0);
}

#[test]
fn auto_consolidate_on_stop_flag_parses_one_true_and_yes() {
    let prev = std::env::var(ENV_AUTO_CONSOLIDATE_ON_STOP).ok();
    let cases = [
        ("1", true),
        ("true", true),
        ("yes", true),
        ("0", false),
        ("no", false),
        ("", false),
    ];
    for (val, expected) in cases {
        unsafe { std::env::set_var(ENV_AUTO_CONSOLIDATE_ON_STOP, val) };
        assert_eq!(
            auto_consolidate_on_stop_enabled(),
            expected,
            "value {val:?} should map to {expected}",
        );
    }
    unsafe { std::env::remove_var(ENV_AUTO_CONSOLIDATE_ON_STOP) };
    assert!(!auto_consolidate_on_stop_enabled(), "unset -> false");
    if let Some(v) = prev {
        unsafe { std::env::set_var(ENV_AUTO_CONSOLIDATE_ON_STOP, v) };
    }
}

#[test]
fn read_stdin_capped_truncates_huge_payload() {
    // Build a payload larger than MAX_STDIN_BYTES and confirm we cap it.
    let n = MAX_STDIN_BYTES + 10_000;
    let big = vec![b'x'; n];
    let read = read_stdin_capped(big.as_slice());
    assert_eq!(read.len(), MAX_STDIN_BYTES);
}

#[test]
fn read_stdin_capped_passes_short_payload_through() {
    let read = read_stdin_capped(b"hello".as_slice());
    assert_eq!(read, "hello");
}

#[test]
fn peek_hook_subcommand_recognizes_each_verb() {
    let cases = [
        ("session-start", Some(HookSubcommand::SessionStart)),
        ("user-prompt-submit", Some(HookSubcommand::UserPromptSubmit)),
        ("post-tool-use", Some(HookSubcommand::PostToolUse)),
        ("stop", Some(HookSubcommand::Stop)),
    ];
    for (verb, expected) in cases {
        let argv = vec!["clud".to_string(), "hook".to_string(), verb.to_string()];
        let got = peek_hook_subcommand_from_argv(argv);
        assert_eq!(
            std::mem::discriminant(&got.unwrap()),
            std::mem::discriminant(&expected.unwrap()),
            "verb {verb}",
        );
    }
}

#[test]
fn peek_hook_subcommand_returns_none_without_hook_argv() {
    let argv = vec!["clud".to_string(), "--help".to_string()];
    assert!(peek_hook_subcommand_from_argv(argv).is_none());

    let argv = vec![
        "clud".to_string(),
        "memory".to_string(),
        "status".to_string(),
    ];
    assert!(peek_hook_subcommand_from_argv(argv).is_none());

    let argv = vec!["clud".to_string()];
    assert!(peek_hook_subcommand_from_argv(argv).is_none());
}

#[test]
fn peek_hook_subcommand_unknown_verb_returns_none() {
    // `clud hook --help` (the help flag is not a known verb) and
    // `clud hook nonsense` both fall through to clap so the user gets
    // a clean error.
    let argv = vec!["clud".to_string(), "hook".to_string(), "--help".to_string()];
    assert!(peek_hook_subcommand_from_argv(argv).is_none());

    let argv = vec![
        "clud".to_string(),
        "hook".to_string(),
        "nonsense".to_string(),
    ];
    assert!(peek_hook_subcommand_from_argv(argv).is_none());
}

#[test]
fn peek_hook_subcommand_stops_at_double_dash() {
    // Backend passthrough after `--` must never be treated as a hook
    // subcommand.
    let argv = vec![
        "clud".to_string(),
        "--".to_string(),
        "hook".to_string(),
        "session-start".to_string(),
    ];
    assert!(peek_hook_subcommand_from_argv(argv).is_none());
}

#[test]
fn peek_hook_subcommand_skips_leading_flags() {
    // The hook keyword may follow global flags (e.g. `--verbose hook`).
    let argv = vec![
        "clud".to_string(),
        "--verbose".to_string(),
        "hook".to_string(),
        "session-start".to_string(),
    ];
    assert!(matches!(
        peek_hook_subcommand_from_argv(argv),
        Some(HookSubcommand::SessionStart)
    ));
}

#[test]
fn dispatch_session_start_smoke() {
    // The public `dispatch` entry. We can't intercept the real
    // stdin/stdout from here, but `dispatch` must not panic and must
    // exit 0 for every variant when there's no daemon. We confirm the
    // formatter path stays well-formed via the runner-level tests
    // above; here we only exercise the match arm wiring for the three
    // no-output variants.
    let rc = dispatch(HookSubcommand::UserPromptSubmit);
    assert_eq!(rc, 0);
    let rc = dispatch(HookSubcommand::PostToolUse);
    assert_eq!(rc, 0);
    let rc = dispatch(HookSubcommand::Stop);
    assert_eq!(rc, 0);
}
