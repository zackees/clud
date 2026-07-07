use super::builder::{
    build_launch_plan, build_launch_plan_at, next_run_at_millis, parse_repeat_interval,
    repeat_implies_no_done_warning,
};
use super::prompts::{build_fix_prompt, build_up_prompt, is_github_url, FIX_PROMPT};
use super::types::LaunchPlan;
use crate::args::Args;
use crate::backend::{Backend, LaunchMode};
use crate::clud_settings::DEFAULT_CODEX_GITHUB_PLUGIN_CONFIG_OVERRIDE;

fn parse(raw: &[&str]) -> Args {
    let raw: Vec<String> = raw.iter().map(|s| s.to_string()).collect();
    Args::parse_from_raw(raw)
}

fn plan(raw: &[&str]) -> LaunchPlan {
    let mut args = parse(raw);
    let backend = crate::backend::resolve_backend(args.claude, args.codex);
    if matches!(backend, Backend::Codex) {
        args.codex_config_overrides = vec![DEFAULT_CODEX_GITHUB_PLUGIN_CONFIG_OVERRIDE.to_string()];
    }
    build_launch_plan(&args, backend, backend.executable_name())
}

fn plan_at(raw: &[&str], cwd: &std::path::Path) -> LaunchPlan {
    let mut args = parse(raw);
    let backend = crate::backend::resolve_backend(args.claude, args.codex);
    if matches!(backend, Backend::Codex) {
        args.codex_config_overrides = vec![DEFAULT_CODEX_GITHUB_PLUGIN_CONFIG_OVERRIDE.to_string()];
    }
    build_launch_plan_at(&args, backend, backend.executable_name(), cwd)
}

fn prompt_from_plan(p: &LaunchPlan) -> &str {
    let idx = p.command.iter().position(|a| a == "-p").unwrap();
    &p.command[idx + 1]
}

/// Find the last positional (non-flag, non-subcommand) argument of the plan.
/// For codex we emit the prompt positionally, so this picks it up.
fn last_arg(p: &LaunchPlan) -> &str {
    p.command.last().map(String::as_str).unwrap_or("")
}

fn codex_prefix() -> Vec<String> {
    vec![
        "codex".to_string(),
        "-c".to_string(),
        DEFAULT_CODEX_GITHUB_PLUGIN_CONFIG_OVERRIDE.to_string(),
    ]
}

fn codex_exec_index(p: &LaunchPlan) -> usize {
    p.command.iter().position(|arg| arg == "exec").unwrap()
}

fn codex_config_values(p: &LaunchPlan) -> Vec<&str> {
    p.command
        .windows(2)
        .filter_map(|pair| (pair[0] == "-c").then_some(pair[1].as_str()))
        .collect()
}

#[test]
fn test_prompt_with_yolo() {
    let p = plan(&["clud", "-p", "hello"]);
    assert_eq!(
        p.command,
        vec!["claude", "--dangerously-skip-permissions", "-p", "hello"]
    );
    assert_eq!(p.iterations, 1);
    assert_eq!(p.launch_mode, LaunchMode::Subprocess);
}

#[test]
fn test_safe_mode_no_yolo() {
    let p = plan(&["clud", "--safe", "-p", "hello"]);
    assert_eq!(p.command, vec!["claude", "-p", "hello"]);
}

#[test]
fn test_codex_prompt_goes_through_exec_subcommand() {
    // Codex's `-p` is `--profile`, not a prompt flag. Non-interactive
    // runs must use `codex exec <prompt>` with the prompt as positional.
    let p = plan(&["clud", "--codex", "-p", "hello"]);
    assert_eq!(
        p.command,
        [
            codex_prefix(),
            vec![
                "exec".to_string(),
                "--dangerously-bypass-approvals-and-sandbox".to_string(),
                "hello".to_string(),
            ],
        ]
        .concat()
    );
    // `codex exec` is non-interactive; subprocess mode is fine.
    assert_eq!(p.launch_mode, LaunchMode::Subprocess);
}

#[test]
fn test_codex_interactive_defaults_to_pty_without_tty() {
    // Under `cargo test` there is no controlling terminal, so
    // `parent_has_tty` is false and the interactive TUI gets a PTY.
    // With a real TTY (normal user invocation), codex runs as a
    // subprocess that inherits the terminal directly — see
    // `backend::test_codex_interactive_with_tty_uses_subprocess`.
    let p = plan(&["clud", "--codex"]);
    assert_eq!(
        p.command,
        [
            codex_prefix(),
            vec!["--dangerously-bypass-approvals-and-sandbox".to_string()],
        ]
        .concat()
    );
    assert_eq!(p.launch_mode, LaunchMode::Pty);
}

#[test]
fn test_codex_keeps_native_agents_when_agents_md_exists() {
    let repo = tempfile::tempdir().unwrap();
    std::fs::write(repo.path().join("AGENTS.md"), "native agents").unwrap();
    std::fs::write(repo.path().join("CODEX.md"), "codex fallback").unwrap();
    std::fs::write(repo.path().join("CLAUDE.md"), "claude fallback").unwrap();

    let p = plan_at(&["clud", "--codex"], repo.path());

    assert!(!codex_config_values(&p)
        .iter()
        .any(|value| value.starts_with("project_doc_fallback_filenames=")));
}

#[test]
fn test_codex_uses_codex_md_as_project_doc_fallback_before_claude_md() {
    let repo = tempfile::tempdir().unwrap();
    std::fs::write(repo.path().join("CODEX.md"), "codex fallback").unwrap();
    std::fs::write(repo.path().join("CLAUDE.md"), "claude fallback").unwrap();

    let p = plan_at(&["clud", "--codex"], repo.path());

    assert!(codex_config_values(&p).contains(&r#"project_doc_fallback_filenames=["CODEX.md"]"#));
    assert!(!codex_config_values(&p)
        .iter()
        .any(|value| value.contains("CLAUDE.md")));
}

#[test]
fn test_codex_uses_claude_md_when_agents_and_codex_are_absent() {
    let repo = tempfile::tempdir().unwrap();
    std::fs::write(repo.path().join("CLAUDE.md"), "claude fallback").unwrap();

    let p = plan_at(&["clud", "--codex"], repo.path());

    assert!(codex_config_values(&p).contains(&r#"project_doc_fallback_filenames=["CLAUDE.md"]"#));
}

#[test]
fn test_codex_project_doc_fallback_noops_when_no_instruction_file_exists() {
    let repo = tempfile::tempdir().unwrap();

    let p = plan_at(&["clud", "--codex"], repo.path());

    assert!(!codex_config_values(&p)
        .iter()
        .any(|value| value.starts_with("project_doc_fallback_filenames=")));
}

#[test]
fn test_codex_continue_uses_resume_last() {
    // `-c` on codex maps to `codex resume --last`, not `--continue`.
    let p = plan(&["clud", "--codex", "-c"]);
    assert_eq!(
        p.command,
        [
            codex_prefix(),
            vec![
                "resume".to_string(),
                "--dangerously-bypass-approvals-and-sandbox".to_string(),
                "--last".to_string(),
            ],
        ]
        .concat()
    );
    assert_eq!(p.launch_mode, LaunchMode::Pty);
}

#[test]
fn test_codex_resume_with_session_id() {
    let p = plan(&["clud", "--codex", "-r", "sess-123"]);
    assert_eq!(
        p.command,
        [
            codex_prefix(),
            vec![
                "resume".to_string(),
                "--dangerously-bypass-approvals-and-sandbox".to_string(),
                "sess-123".to_string(),
            ],
        ]
        .concat()
    );
}

#[test]
fn test_codex_model_uses_short_m() {
    // Codex's model flag is `-m/--model`; Claude's is `--model`.
    let p = plan(&["clud", "--codex", "--model", "gpt-5"]);
    assert_eq!(
        p.command,
        [
            codex_prefix(),
            vec![
                "--dangerously-bypass-approvals-and-sandbox".to_string(),
                "-m".to_string(),
                "gpt-5".to_string(),
            ],
        ]
        .concat()
    );
}

#[test]
fn test_codex_up_routes_through_exec() {
    let p = plan(&["clud", "--codex", "up"]);
    assert_eq!(p.command[0], "codex");
    assert!(codex_exec_index(&p) > 0);
    // Prompt is positional (last arg), not behind `-p`.
    assert!(p.command.iter().all(|a| a != "-p"));
    assert!(last_arg(&p).contains("codeup"));
    assert_eq!(p.launch_mode, LaunchMode::Subprocess);
}

#[test]
fn test_model_flag() {
    let p = plan(&["clud", "--model", "opus", "-p", "hello"]);
    assert_eq!(
        p.command,
        vec![
            "claude",
            "--dangerously-skip-permissions",
            "--model",
            "opus",
            "-p",
            "hello"
        ]
    );
}

#[test]
fn test_continue_session() {
    let p = plan(&["clud", "-c"]);
    assert_eq!(
        p.command,
        vec!["claude", "--dangerously-skip-permissions", "--continue"]
    );
}

#[test]
fn test_message_flag() {
    let p = plan(&["clud", "-m", "fix bug"]);
    assert_eq!(
        p.command,
        vec!["claude", "--dangerously-skip-permissions", "-m", "fix bug"]
    );
}

#[test]
fn test_up_default() {
    let p = plan(&["clud", "up"]);
    let prompt = prompt_from_plan(&p);
    assert!(prompt.contains("lint"));
    assert!(prompt.contains("codeup"));
    assert!(prompt.contains("<your one-line summary>"));
    assert!(!prompt.contains("-p\n"));
}

#[test]
fn test_up_with_message() {
    let p = plan(&["clud", "up", "-m", "bump version"]);
    let prompt = prompt_from_plan(&p);
    assert!(prompt.contains("codeup -m \"bump version\""));
    assert!(!prompt.contains("<your one-line summary>"));
}

#[test]
fn test_up_with_publish() {
    let p = plan(&["clud", "up", "--publish"]);
    let prompt = prompt_from_plan(&p);
    assert!(prompt.contains("codeup -m \"<your one-line summary>\" -p"));
}

#[test]
fn test_up_with_message_and_publish() {
    let p = plan(&["clud", "up", "-m", "release v2", "--publish"]);
    let prompt = prompt_from_plan(&p);
    assert!(prompt.contains("codeup -m \"release v2\" -p"));
}

#[test]
fn test_rebase_command() {
    let p = plan(&["clud", "rebase"]);
    let prompt = prompt_from_plan(&p);
    assert!(prompt.contains("git fetch"));
    assert!(prompt.contains("rebase"));
}

#[test]
fn test_fix_default() {
    let p = plan(&["clud", "fix"]);
    let prompt = prompt_from_plan(&p);
    assert!(prompt.contains("linting"));
    assert!(prompt.contains("unit tests"));
}

#[test]
fn test_fix_with_github_url() {
    let p = plan(&[
        "clud",
        "fix",
        "https://github.com/user/repo/actions/runs/123",
    ]);
    let prompt = prompt_from_plan(&p);
    assert!(prompt.contains("https://github.com/user/repo/actions/runs/123"));
    assert!(prompt.contains("gh run view"));
    assert!(prompt.contains("lint-test"));
}

#[test]
fn test_fix_with_non_github_url() {
    let p = plan(&["clud", "fix", "https://example.com/logs"]);
    let prompt = prompt_from_plan(&p);
    assert!(prompt.contains("linting"));
    assert!(!prompt.contains("example.com"));
}

#[test]
fn test_loop_command() {
    let p = plan(&["clud", "loop", "--loop-count", "5", "do stuff"]);
    assert_eq!(p.iterations, 5);
    assert!(p.command.contains(&"-p".to_string()));
    let prompt = prompt_from_plan(&p);
    assert!(prompt.starts_with("do stuff"));
    // Issue #95: contract now embeds absolute paths; the relative
    // suffix is still present, but the separator is platform-native.
    assert!(
        prompt.contains(".clud/loop/DONE") || prompt.contains(".clud\\loop\\DONE"),
        "prompt missing DONE marker path: {prompt}"
    );
    assert!(
        prompt.contains(".clud/loop/BLOCKED") || prompt.contains(".clud\\loop\\BLOCKED"),
        "prompt missing BLOCKED marker path: {prompt}"
    );
    assert!(p.loop_markers.is_some());
}

#[test]
fn test_loop_default_count() {
    let p = plan(&["clud", "loop", "task"]);
    assert_eq!(p.iterations, 50);
}

#[test]
fn test_loop_no_done_omits_contract() {
    let p = plan(&["clud", "loop", "--no-done", "task"]);
    let prompt = prompt_from_plan(&p);
    assert_eq!(prompt, "task");
    assert!(p.loop_markers.is_none());
}

#[test]
fn test_loop_repeat_implies_no_done_contract() {
    let p = plan(&["clud", "loop", "--repeat", "1h", "task"]);
    let prompt = prompt_from_plan(&p);
    assert_eq!(prompt, "task");
    assert!(p.loop_markers.is_none());
    assert_eq!(
        p.repeat_schedule.as_ref().map(|s| s.interval_secs),
        Some(3600)
    );
}

#[test]
fn test_loop_repeat_with_done_override_restores_contract() {
    let p = plan(&[
        "clud", "loop", "--repeat", "1h", "--done", "DONE.md", "task",
    ]);
    let prompt = prompt_from_plan(&p);
    assert!(prompt.contains("DONE.md"));
    assert!(prompt.contains("BLOCKED.md"));
    assert!(p.loop_markers.is_some());
    let markers = p.loop_markers.unwrap();
    assert!(markers.done_path.ends_with("DONE.md"));
    assert!(markers.blocked_path.ends_with("BLOCKED.md"));
}

// ---- Issue #48: `clud --codex loop "..."` must drive codex the same ----
// way `clud loop` drives claude: exec subcommand, positional prompt,
// DONE/BLOCKED contract appended, loop_markers populated, and the
// non-interactive launch mode (subprocess) selected.

#[test]
fn test_codex_loop_routes_through_exec() {
    let p = plan(&["clud", "--codex", "loop", "--loop-count", "5", "do stuff"]);
    assert_eq!(p.command[0], "codex");
    assert!(codex_exec_index(&p) > 0);
    assert!(p
        .command
        .contains(&"--dangerously-bypass-approvals-and-sandbox".to_string()));
    assert_eq!(p.iterations, 5);
    assert_eq!(p.backend, Backend::Codex);
}

#[test]
fn test_codex_loop_prompt_is_positional_not_dash_p() {
    // Codex's `-p` is `--profile`; the prompt must be the final positional.
    let p = plan(&["clud", "--codex", "loop", "do stuff"]);
    assert!(
        p.command.iter().all(|a| a != "-p"),
        "codex must not emit -p for the prompt; cmd={:?}",
        p.command
    );
    let last = last_arg(&p);
    assert!(
        last.starts_with("do stuff"),
        "codex prompt must be the last positional arg; got: {last:?}"
    );
}

#[test]
fn test_codex_loop_appends_done_marker_contract() {
    let p = plan(&["clud", "--codex", "loop", "do stuff"]);
    let prompt = last_arg(&p);
    // Issue #95: absolute paths in contract; assert on the relative
    // suffix using platform-native separators.
    assert!(
        prompt.contains(".clud/loop/DONE") || prompt.contains(".clud\\loop\\DONE"),
        "prompt missing DONE marker path: {prompt}"
    );
    assert!(
        prompt.contains(".clud/loop/BLOCKED") || prompt.contains(".clud\\loop\\BLOCKED"),
        "prompt missing BLOCKED marker path: {prompt}"
    );
    assert!(p.loop_markers.is_some());
}

#[test]
fn test_codex_loop_default_count() {
    let p = plan(&["clud", "--codex", "loop", "task"]);
    assert_eq!(p.iterations, 50);
}

#[test]
fn test_codex_loop_no_done_omits_contract() {
    let p = plan(&["clud", "--codex", "loop", "--no-done", "task"]);
    let prompt = last_arg(&p);
    assert_eq!(prompt, "task");
    assert!(p.loop_markers.is_none());
}

#[test]
fn test_codex_loop_uses_subprocess_launch_mode() {
    // `codex exec` is non-interactive → subprocess (pipe-friendly),
    // just like `clud --codex -p "..."`.
    let p = plan(&["clud", "--codex", "loop", "task"]);
    assert_eq!(p.launch_mode, LaunchMode::Subprocess);
}

#[test]
fn test_codex_loop_safe_mode_omits_bypass_flag() {
    let p = plan(&["clud", "--codex", "--safe", "loop", "task"]);
    assert!(!p
        .command
        .contains(&"--dangerously-bypass-approvals-and-sandbox".to_string()));
    assert_eq!(p.command[0], "codex");
    assert!(codex_exec_index(&p) > 0);
}

#[test]
fn test_codex_loop_forwards_passthrough_flags() {
    // `clud --codex loop "task" -- --verbose` must keep the passthrough
    // flag so the test harness can inject mock-agent flags the same way
    // it does for the claude path.
    let p = plan(&["clud", "--codex", "loop", "task", "--", "--verbose"]);
    assert!(p.command.contains(&"--verbose".to_string()));
}

// ---- Stream-JSON progress injection ----
//
// `clud loop` against claude in *subprocess* launch mode (Windows default,
// or anywhere `--subprocess` is forced) used to go silent for the whole
// iteration because `claude -p` buffers its final response. The fix is to
// append `--output-format stream-json --verbose` so claude streams its
// turn events live, and let the runtime render each event as a one-line
// progress update. PTY-mode loops already show the live TUI, so no
// injection is needed there.

/// Helper: locate the index of `needle` in `cmd`, panicking with a
/// readable message if missing.
fn expect_arg(cmd: &[String], needle: &str) -> usize {
    cmd.iter().position(|a| a == needle).unwrap_or_else(|| {
        panic!("expected `{needle}` in command; got {cmd:?}");
    })
}

#[test]
fn test_claude_loop_subprocess_injects_stream_json() {
    let p = plan(&["clud", "--subprocess", "loop", "task"]);
    assert_eq!(p.launch_mode, LaunchMode::Subprocess);
    let idx = expect_arg(&p.command, "stream-json");
    assert_eq!(
        p.command[idx - 1],
        "--output-format",
        "stream-json must follow --output-format; cmd={:?}",
        p.command
    );
    assert!(
        p.command.iter().any(|a| a == "--verbose"),
        "stream-json requires --verbose per claude's CLI contract; cmd={:?}",
        p.command
    );
    assert!(
        p.stream_json_progress,
        "LaunchPlan must signal the runtime to parse stream-json"
    );
}

#[test]
fn test_claude_loop_stream_json_flags_emitted_before_prompt() {
    // Regression guard for PR #91 / commit 8c0818a: the stream-json flags
    // must be inserted BEFORE `-p <prompt>` so that `command[-1]` is the
    // prompt body. Dry-run consumers, the Python integration tests in
    // tests/test_hello.py, and downstream tooling all rely on the
    // "prompt is the last arg" contract.
    let p = plan(&["clud", "--subprocess", "loop", "do stuff"]);
    assert!(p.stream_json_progress);

    // Prompt body must still be the last positional.
    let last = p.command.last().expect("cmd is non-empty");
    assert!(
        last.starts_with("do stuff"),
        "command[-1] must be the prompt body, got: {last:?} (full cmd: {:?})",
        p.command
    );

    // Each stream-json flag must appear strictly before `-p`.
    let p_idx = expect_arg(&p.command, "-p");
    for flag in ["--output-format", "stream-json", "--verbose"] {
        let flag_idx = expect_arg(&p.command, flag);
        assert!(
            flag_idx < p_idx,
            "{flag} (idx {flag_idx}) must come before -p (idx {p_idx}); cmd={:?}",
            p.command
        );
    }
}

#[test]
fn test_claude_loop_pty_does_not_inject_stream_json() {
    // PTY mode runs the live claude TUI; switching it into the
    // non-interactive stream-json wire format would *remove* the
    // streaming UX we already have.
    let p = plan(&["clud", "--pty", "loop", "task"]);
    assert_eq!(p.launch_mode, LaunchMode::Pty);
    assert!(
        !p.command.iter().any(|a| a == "stream-json"),
        "pty-mode loop must NOT inject stream-json; cmd={:?}",
        p.command
    );
    assert!(
        !p.stream_json_progress,
        "pty mode does not need the stream-json renderer"
    );
}

#[test]
fn test_codex_loop_does_not_inject_stream_json() {
    // codex does not accept `--output-format stream-json` — the flag is
    // claude-only. Forcing subprocess to be explicit so the test is
    // platform-independent.
    let p = plan(&["clud", "--codex", "--subprocess", "loop", "task"]);
    assert!(
        !p.command.iter().any(|a| a == "stream-json"),
        "codex must NOT receive --output-format stream-json; cmd={:?}",
        p.command
    );
    assert!(!p.stream_json_progress);
}

#[test]
fn test_claude_plain_prompt_does_not_inject_stream_json() {
    // Single-shot `clud -p` is short-lived and not a loop, so we keep
    // the existing UX untouched. Stream-json injection is loop-only.
    let p = plan(&["clud", "--subprocess", "-p", "hello"]);
    assert!(
        !p.command.iter().any(|a| a == "stream-json"),
        "plain -p must NOT receive stream-json injection; cmd={:?}",
        p.command
    );
    assert!(!p.stream_json_progress);
}

#[test]
fn test_claude_loop_safe_mode_still_injects_stream_json() {
    // `--safe` only drops the YOLO permissions flag; it must not also
    // suppress progress streaming, which is orthogonal.
    let p = plan(&["clud", "--subprocess", "--safe", "loop", "task"]);
    assert!(p.command.iter().any(|a| a == "stream-json"));
    assert!(p.stream_json_progress);
    // Sanity: --safe removed the permissions bypass.
    assert!(!p
        .command
        .iter()
        .any(|a| a == "--dangerously-skip-permissions"));
}

#[test]
fn test_pty_override() {
    let p = plan(&["clud", "--pty", "-p", "hello"]);
    assert_eq!(p.launch_mode, LaunchMode::Pty);
}

#[test]
fn test_graphics_config_threads_into_launch_plan() {
    let p = plan(&[
        "clud",
        "--graphics=sixel",
        "--graphics-image",
        "banner.png",
        "--pty",
        "-p",
        "hello",
    ]);
    assert_eq!(p.graphics.mode, crate::graphics::GraphicsMode::Sixel);
    assert_eq!(
        p.graphics.image_path.as_ref().map(|path| path.as_os_str()),
        Some(std::ffi::OsStr::new("banner.png"))
    );
    assert!(!p.command.iter().any(|arg| arg.starts_with("--graphics")));
}

#[test]
fn test_passthrough_flags() {
    let p = plan(&["clud", "--some-flag", "-p", "hello"]);
    assert!(p.command.contains(&"--some-flag".to_string()));
}

#[test]
fn test_passthrough_after_separator() {
    let p = plan(&["clud", "-p", "hello", "--", "--verbose"]);
    assert!(p.command.contains(&"--verbose".to_string()));
}

#[test]
fn test_is_github_url() {
    assert!(is_github_url("https://github.com/user/repo"));
    assert!(is_github_url("http://github.com/user/repo"));
    assert!(!is_github_url("https://gitlab.com/user/repo"));
    assert!(!is_github_url("not a url"));
}

#[test]
fn test_build_fix_prompt_no_url() {
    let prompt = build_fix_prompt(None);
    assert_eq!(prompt, FIX_PROMPT);
}

#[test]
fn test_build_fix_prompt_github_url() {
    let prompt = build_fix_prompt(Some("https://github.com/user/repo/actions/runs/999"));
    assert!(prompt.contains("runs/999"));
    assert!(prompt.contains("gh run view"));
}

#[test]
fn test_build_up_prompt_default() {
    let prompt = build_up_prompt(None, false);
    assert!(prompt.contains("<your one-line summary>"));
    assert!(!prompt.contains(" -p"));
}

#[test]
fn test_build_up_prompt_custom_message() {
    let prompt = build_up_prompt(Some("my msg"), false);
    assert!(prompt.contains("codeup -m \"my msg\""));
    assert!(!prompt.contains("<your one-line summary>"));
}

#[test]
fn test_build_up_prompt_publish() {
    let prompt = build_up_prompt(None, true);
    assert!(prompt.contains("-p"));
}

// -----------------------------------------------------------------
// Issue #61: --repeat scheduling tests
// -----------------------------------------------------------------
//
// These cover three areas:
//   1. parse_repeat_interval: every accepted form + the rejection
//      cases the issue calls out (negative, fractional, unknown
//      unit, overflow, empty, zero, missing unit, missing value).
//   2. repeat_implies_no_done_warning: the precedence ladder
//      (--repeat alone fires; explicit --no-done suppresses;
//      --done <path> suppresses + restores contract).
//   3. next_run_at_millis: the no-overlap invariant. The next run
//      time is always derived from *completion*, so a long-running
//      iteration pushes the schedule out instead of overlapping.

#[test]
fn test_parse_repeat_interval_accepted_forms() {
    // The four forms the issue explicitly calls out.
    assert_eq!(parse_repeat_interval("30s").unwrap(), 30);
    assert_eq!(parse_repeat_interval("5m").unwrap(), 5 * 60);
    assert_eq!(parse_repeat_interval("1h").unwrap(), 60 * 60);
    assert_eq!(parse_repeat_interval("24h").unwrap(), 24 * 60 * 60);
}

#[test]
fn test_parse_repeat_interval_accepts_uppercase_unit() {
    // Case-insensitivity is a small kindness; matches argparse-style ergonomics
    // and the spec doesn't forbid it.
    assert_eq!(parse_repeat_interval("30S").unwrap(), 30);
    assert_eq!(parse_repeat_interval("2H").unwrap(), 7200);
}

#[test]
fn test_parse_repeat_interval_trims_surrounding_whitespace() {
    assert_eq!(parse_repeat_interval("  1h  ").unwrap(), 3600);
}

#[test]
fn test_parse_repeat_interval_rejects_empty() {
    let err = parse_repeat_interval("").unwrap_err();
    assert!(err.contains("empty"), "expected empty-error, got: {err}");
    let err = parse_repeat_interval("   ").unwrap_err();
    assert!(err.contains("empty"), "expected empty-error, got: {err}");
}

#[test]
fn test_parse_repeat_interval_rejects_zero() {
    // A zero interval would busy-loop the scheduler.
    let err = parse_repeat_interval("0s").unwrap_err();
    assert!(
        err.contains("greater than zero"),
        "expected zero-error, got: {err}"
    );
    let err = parse_repeat_interval("0h").unwrap_err();
    assert!(err.contains("greater than zero"), "got: {err}");
}

#[test]
fn test_parse_repeat_interval_rejects_missing_unit() {
    let err = parse_repeat_interval("30").unwrap_err();
    assert!(
        err.to_lowercase().contains("unit"),
        "expected unit-error, got: {err}"
    );
}

#[test]
fn test_parse_repeat_interval_rejects_missing_value() {
    // "s" alone — split point at index 0 → leading-non-digit error.
    let err = parse_repeat_interval("s").unwrap_err();
    assert!(
        err.contains("positive integer"),
        "expected leading-digit error, got: {err}"
    );
}

#[test]
fn test_parse_repeat_interval_rejects_negative() {
    // The leading `-` is non-digit; trips the "must start with positive integer"
    // branch. We don't accept negatives at all.
    let err = parse_repeat_interval("-1h").unwrap_err();
    assert!(
        err.contains("positive integer"),
        "expected leading-digit error, got: {err}"
    );
}

#[test]
fn test_parse_repeat_interval_rejects_fractional() {
    // "1.5h" — the `.` is non-digit, so we split into "1" + ".5h" and the
    // unit ".5h" doesn't match s/m/h.
    let err = parse_repeat_interval("1.5h").unwrap_err();
    assert!(
        err.to_lowercase().contains("unit"),
        "expected unit-error for fractional input, got: {err}"
    );
}

#[test]
fn test_parse_repeat_interval_rejects_unknown_unit() {
    for bad in &["30d", "1y", "1w", "30sec", "1hr", "10ms"] {
        let err = parse_repeat_interval(bad).unwrap_err();
        assert!(
            err.to_lowercase().contains("unit") || err.to_lowercase().contains("unsupported"),
            "expected unit-error for {bad:?}, got: {err}"
        );
    }
}

#[test]
fn test_parse_repeat_interval_rejects_overflow() {
    // u64::MAX seconds * 3600 obviously overflows; we should bubble that up
    // as a clean "too large" error, not panic.
    let err = parse_repeat_interval("18446744073709551615h").unwrap_err();
    assert!(
        err.contains("too large") || err.contains("invalid"),
        "expected overflow-error, got: {err}"
    );
}

#[test]
fn test_parse_repeat_interval_rejects_garbage() {
    // Note: the parser intentionally trims the unit part, so `"1 h"` and
    // similar inner-whitespace inputs are accepted (they normalize to `1h`).
    // We only assert genuinely malformed inputs here.
    for bad in &["abc", "1h2m", "h1", "1x", "-1h", "1.5h", " ", "0"] {
        assert!(
            parse_repeat_interval(bad).is_err(),
            "expected error for {bad:?}"
        );
    }
}

// ---- Flag-precedence: repeat_implies_no_done_warning ----

#[test]
fn test_warning_fires_when_repeat_alone() {
    // --repeat 1h with neither --no-done nor --done: warning + contract OFF.
    let msg = repeat_implies_no_done_warning(Some("1h"), false, None);
    assert!(msg.is_some(), "expected warning when --repeat is alone");
    let text = msg.unwrap();
    assert!(text.contains("--repeat"));
    assert!(text.contains("--no-done"));
    assert!(text.contains("DONE marker"));
}

#[test]
fn test_warning_suppressed_when_no_done_explicit() {
    // User already opted out — no need to warn them.
    let msg = repeat_implies_no_done_warning(Some("1h"), true, None);
    assert!(
        msg.is_none(),
        "explicit --no-done must suppress the warning, got: {msg:?}"
    );
}

#[test]
fn test_warning_suppressed_when_done_path_provided() {
    // --done <path> overrides --repeat's implicit --no-done; no warning.
    let msg = repeat_implies_no_done_warning(Some("1h"), false, Some("DONE.md"));
    assert!(
        msg.is_none(),
        "--done <path> must suppress the warning, got: {msg:?}"
    );
}

#[test]
fn test_warning_silent_without_repeat() {
    // Plain `clud loop "task"` without --repeat: helper never warns.
    let msg = repeat_implies_no_done_warning(None, false, None);
    assert!(msg.is_none());
    let msg = repeat_implies_no_done_warning(None, true, None);
    assert!(msg.is_none());
    let msg = repeat_implies_no_done_warning(None, false, Some("DONE.md"));
    assert!(msg.is_none());
}

// ---- Flag-precedence: contract / loop_markers behavior in plan ----

#[test]
fn test_loop_explicit_no_done_honored_without_repeat() {
    // Without --repeat, --no-done still suppresses the contract — this is
    // the original #2 behavior preserved. Already covered by
    // test_loop_no_done_omits_contract above; we add an explicit assert
    // that loop_markers is None to make the contract crystal clear.
    let p = plan(&["clud", "loop", "--no-done", "task"]);
    assert!(p.loop_markers.is_none());
    assert!(p.repeat_schedule.is_none());
    let prompt = prompt_from_plan(&p);
    assert!(!prompt.contains("DONE"));
    assert!(!prompt.contains("BLOCKED"));
}

#[test]
fn test_loop_repeat_with_explicit_no_done_still_omits_contract() {
    // Belt-and-suspenders: passing both --repeat and --no-done is
    // idempotent — no contract injection, no markers.
    let p = plan(&["clud", "loop", "--repeat", "30m", "--no-done", "task"]);
    assert!(p.loop_markers.is_none());
    assert_eq!(
        p.repeat_schedule.as_ref().map(|s| s.interval_secs),
        Some(30 * 60)
    );
    let prompt = prompt_from_plan(&p);
    assert_eq!(prompt, "task");
}

#[test]
fn test_loop_done_path_uses_supplied_path_in_prompt() {
    // --done <path> must thread the *supplied* path into the prompt
    // contract, not the default `.clud/loop/DONE`. Issue #95: the
    // contract now uses absolute paths, but the user-supplied filename
    // is still visible in the absolute form.
    let p = plan(&["clud", "loop", "--done", "custom/DONE.txt", "task"]);
    let prompt = prompt_from_plan(&p);
    // The DONE filename must appear; the directory segment may use
    // either separator depending on platform.
    assert!(
        prompt.contains("DONE.txt"),
        "prompt missing custom DONE filename: {prompt}"
    );
    // BLOCKED is derived from the DONE *filename's extension* via
    // `blocked_path_from_done`, which uses platform-native path joining.
    // On unix that's `custom/BLOCKED.txt`; on Windows `custom\BLOCKED.txt`.
    // The load-bearing invariant is that the BLOCKED filename mirrors the
    // DONE extension — assert on the filename to stay platform-agnostic.
    assert!(
        prompt.contains("BLOCKED.txt"),
        "prompt missing derived BLOCKED filename: {prompt}"
    );
    assert!(p.loop_markers.is_some());
    let markers = p.loop_markers.unwrap();
    assert!(markers.done_path.ends_with("DONE.txt"));
    assert!(markers.blocked_path.ends_with("BLOCKED.txt"));
}

#[test]
fn test_loop_repeat_30s_parses() {
    let p = plan(&["clud", "loop", "--repeat", "30s", "task"]);
    assert_eq!(
        p.repeat_schedule.as_ref().map(|s| s.interval_secs),
        Some(30)
    );
}

#[test]
fn test_loop_repeat_5m_parses() {
    let p = plan(&["clud", "loop", "--repeat", "5m", "task"]);
    assert_eq!(
        p.repeat_schedule.as_ref().map(|s| s.interval_secs),
        Some(5 * 60)
    );
}

#[test]
fn test_loop_repeat_24h_parses() {
    let p = plan(&["clud", "loop", "--repeat", "24h", "task"]);
    assert_eq!(
        p.repeat_schedule.as_ref().map(|s| s.interval_secs),
        Some(24 * 60 * 60)
    );
}

// ---- Scheduler: next-run computation + no-overlap invariant ----

#[test]
fn test_next_run_at_millis_basic() {
    // Run completed at t=10000 ms with a 30s interval → next run at t=40000 ms.
    assert_eq!(next_run_at_millis(10_000, 30), 40_000);
    assert_eq!(next_run_at_millis(0, 1), 1_000);
    assert_eq!(next_run_at_millis(0, 3600), 3_600_000);
}

#[test]
fn test_next_run_at_millis_long_run_pushes_schedule_out() {
    // The no-overlap invariant in numerical form: if a run that started at
    // t=0 takes 10 minutes (600_000 ms) and the interval is 1 minute
    // (60 s), the next run is scheduled at completion + interval = 660_000 ms,
    // *not* at 60_000 ms. Runs serialize; they never overlap.
    let started_at = 0u64;
    let duration_ms = 10 * 60 * 1000; // 10-minute run
    let interval_secs = 60; // 1-minute repeat
    let completed_at = started_at + duration_ms;
    let next = next_run_at_millis(completed_at, interval_secs);
    assert_eq!(
        next,
        completed_at + 60_000,
        "next run must be `interval` after completion, never overlapping the previous run"
    );
    assert!(
        next > started_at + (interval_secs * 1000),
        "long-running iteration must push the schedule past the original interval"
    );
}

#[test]
fn test_next_run_at_millis_short_run_respects_full_interval() {
    // A 1-second run with a 60-second repeat still waits the full minute
    // after completion before re-running.
    let completed_at = 1_000u64;
    let next = next_run_at_millis(completed_at, 60);
    assert_eq!(next, 61_000);
}

#[test]
fn test_next_run_at_millis_saturates_on_overflow() {
    // Pathological inputs must not panic — the daemon uses saturating
    // arithmetic so we mirror it here.
    assert_eq!(next_run_at_millis(u64::MAX, 1), u64::MAX);
    assert_eq!(next_run_at_millis(u64::MAX - 1, 3600), u64::MAX);
    assert_eq!(next_run_at_millis(0, u64::MAX), u64::MAX);
}

/// Higher-level scheduler simulation. Models the inner loop of
/// `run_repeat_worker` with synthetic clocks: the scheduler issues a
/// single run at a time, only sleeping between completions. This is a
/// pure-Rust simulation — we never spawn a real process — but it
/// exercises the same arithmetic the daemon uses.
fn simulate_repeat(start_ms: u64, run_durations_ms: &[u64], interval_secs: u64) -> Vec<(u64, u64)> {
    // Returns Vec<(start_ms, end_ms)> for each iteration.
    let mut now = start_ms;
    let mut runs = Vec::new();
    for &dur in run_durations_ms {
        let started = now;
        let ended = started + dur;
        runs.push((started, ended));
        now = next_run_at_millis(ended, interval_secs);
    }
    runs
}

#[test]
fn test_scheduler_first_run_is_immediate() {
    let runs = simulate_repeat(1_000, &[5_000], 60);
    // First run starts at start_ms exactly — no pre-sleep.
    assert_eq!(runs[0].0, 1_000);
    assert_eq!(runs[0].1, 6_000);
}

#[test]
fn test_scheduler_second_run_starts_interval_after_first_completes() {
    // Two short runs, 60s interval — second must start at first.end + 60s.
    let runs = simulate_repeat(0, &[1_000, 1_000], 60);
    assert_eq!(runs.len(), 2);
    let (first_start, first_end) = runs[0];
    let (second_start, _) = runs[1];
    assert_eq!(first_start, 0);
    assert_eq!(first_end, 1_000);
    assert_eq!(second_start, first_end + 60_000);
}

#[test]
fn test_scheduler_long_run_delays_second_run_no_overlap() {
    // First run takes 5 minutes; interval is 1 minute; second run must
    // NOT have overlapped the first.
    let interval = 60;
    let runs = simulate_repeat(0, &[5 * 60 * 1000, 1_000], interval);
    let (_first_start, first_end) = runs[0];
    let (second_start, _) = runs[1];
    assert_eq!(first_end, 5 * 60 * 1000);
    assert_eq!(
        second_start,
        first_end + interval * 1000,
        "second run start must be after first completion + interval"
    );
    assert!(
        second_start >= first_end,
        "no-overlap invariant violated: second run started before first finished"
    );
}

#[test]
fn test_scheduler_only_one_active_run_per_job() {
    // The simulation is inherently single-active by construction (each
    // iteration is processed sequentially). Assert that the runs are
    // strictly non-overlapping and strictly monotonic in time.
    let runs = simulate_repeat(0, &[100, 200, 50, 1_000], 30);
    for window in runs.windows(2) {
        let (_a_start, a_end) = window[0];
        let (b_start, _b_end) = window[1];
        assert!(
            b_start >= a_end,
            "runs overlapped: {:?} into {:?}",
            window[0],
            window[1]
        );
    }
    // And each run's own end is after its start.
    for (start, end) in runs {
        assert!(end >= start);
    }
}

#[test]
fn test_scheduler_3600s_interval_matches_1h_input() {
    // Cross-check between parse + scheduler: the seconds returned by
    // parse_repeat_interval drop directly into next_run_at_millis.
    let secs = parse_repeat_interval("1h").unwrap();
    assert_eq!(secs, 3600);
    let next = next_run_at_millis(0, secs);
    assert_eq!(next, 3_600_000);
}
