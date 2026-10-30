//! Unit tests for the builtin-command prompt builders: `up`, `rebase`,
//! `fix`, and `do`, plus the `/grind` seed prompt.
//!
//! Split out of `tests.rs` by the LOC guard; the tests are unchanged apart
//! from `use super::*` for the shared helpers.

use super::*;

#[test]
fn test_model_flag() {
    let p = plan(&["clud", "--model", "opus", "-p", "hello"]);
    assert_eq!(
        command_without_deletion_policy(&p),
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
        command_without_deletion_policy(&p),
        vec!["claude", "--dangerously-skip-permissions", "--continue"]
    );
}

#[test]
fn test_message_flag() {
    let p = plan(&["clud", "-m", "fix bug"]);
    assert_eq!(
        command_without_deletion_policy(&p),
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
fn test_do_command_resolves_goal_prompt() {
    let p = plan(&["clud", "do", "https://github.com/zackees/clud/issues/866"]);
    // `do` seeds an interactive session, so the prompt is the trailing
    // positional (no `-p`). Claude caps the /goal condition (#2372), so the
    // seed is short and the live contract rides in the appended system prompt.
    let seed = last_arg(&p);
    assert!(seed.starts_with("/goal "));
    assert!(seed.contains("https://github.com/zackees/clud/issues/866"));
    assert!(seed.chars().count() < crate::command::prompts::GOAL_SEED_CAP);
    assert!(!seed.contains("# /do: rendered for this checkout by"));
    let appended = appended_system_prompt(&p);
    assert!(appended.contains("# /do: rendered for this checkout by"));
    assert!(appended.contains("Verdict: `default`"));
    assert!(appended.contains("Read the issue and implement it"));
}

fn appended_system_prompt(p: &LaunchPlan) -> &str {
    let idx = p
        .command
        .iter()
        .position(|a| a == "--append-system-prompt")
        .expect("claude plan carries --append-system-prompt");
    &p.command[idx + 1]
}

#[test]
fn test_do_goal_seed_stays_under_claude_cap_for_long_target_and_contract() {
    let target = format!("refactor {}", "x".repeat(6000));
    let args = parse(&["clud", "do", &target]);
    let mut args = args;
    args.do_rendered_prompt = Some("procedure line\n".repeat(1000));
    let backend = crate::backend::resolve_backend(args.claude, args.codex);
    let p = build_launch_plan(&args, backend, backend.executable_name());
    let seed = last_arg(&p);
    assert!(seed.starts_with("/goal "));
    assert!(
        seed.chars().count() < crate::command::prompts::GOAL_SEED_CAP,
        "got {}",
        seed.chars().count()
    );
    assert!(seed.contains("target truncated"));
    assert!(appended_system_prompt(&p).contains(&"procedure line\n".repeat(1000)));
}

#[test]
fn test_do_codex_delivers_contract_via_developer_instructions() {
    let p = plan(&[
        "clud",
        "--codex",
        "do",
        "https://github.com/zackees/clud/issues/866",
    ]);
    let seed = last_arg(&p);
    assert!(seed.starts_with("/goal "));
    assert!(seed.chars().count() < crate::command::prompts::GOAL_SEED_CAP);
    let dev = p
        .command
        .iter()
        .find(|a| a.starts_with("developer_instructions="))
        .expect("developer_instructions override");
    assert!(dev.contains("rendered for this checkout by"));
}

#[test]
fn test_do_command_on_a_meta_issue_seeds_grind() {
    let mut args = parse(&["clud", "do", "https://github.com/zackees/clud/issues/900"]);
    args.do_meta = true;
    args.do_rendered_prompt = Some(crate::command::do_prompt::render(
        "https://github.com/zackees/clud/issues/900",
        &Ok(crate::command::do_kind::DoKind::Meta),
        &Ok(crate::command::do_prompt::BranchFacts {
            branch: Some("main".into()),
            default_branch: "main".into(),
            ..Default::default()
        }),
    ));
    let backend = crate::backend::resolve_backend(args.claude, args.codex);
    let p = build_launch_plan(&args, backend, backend.executable_name());
    let seed = last_arg(&p);
    assert!(seed.starts_with("/goal "));
    assert!(seed.chars().count() < crate::command::prompts::GOAL_SEED_CAP);
    let appended = appended_system_prompt(&p);
    assert!(appended.contains("This is a meta issue"));
    assert!(appended.contains("Workflow tool"));
    assert!(appended.contains("use Read to read the complete grind router"));
    assert!(!appended.contains("Read the issue and implement it"));
}

#[test]
fn test_build_do_prompt_is_short_and_names_the_target() {
    let seed = build_do_prompt(Some("Verdict: x\nFinish"), "fix \"quotes\" $(touch s)");
    assert!(seed.starts_with("/goal "));
    assert!(seed.contains("fix \"quotes\" $(touch s)"));
    assert!(!seed.contains("Verdict: x"));
}

#[test]
fn test_do_plan_without_a_prepared_contract_fails_closed() {
    for rendered in [None, Some(""), Some("  ")] {
        let seed = build_do_prompt(rendered, "t");
        assert!(seed.starts_with("/goal Stop:"));
        assert!(seed.contains("do not start work"));
    }
}

#[test]
fn test_build_grind_prompt_substitutes_url() {
    let prompt = build_grind_prompt("https://example.com/repo/issues");
    assert_eq!(prompt, "/grind https://example.com/repo/issues");
    assert!(prompt.contains("https://example.com/repo/issues"));
    assert!(!prompt.contains("{url}"));
}
