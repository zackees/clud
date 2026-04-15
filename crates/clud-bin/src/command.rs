use crate::args::{Args, Command};
use crate::backend::{Backend, LaunchMode};
use serde::{Deserialize, Serialize};

const FIX_PROMPT: &str = "\
Look for linting like ./lint, or npm or python, choose the most likely one, \
then look for unit tests like ./test or pytest or npm test, run the most likely one. \
For each stage fix until it works, rerunning it until it does.";

const GITHUB_FIX_VALIDATION: &str = "\
run `lint-test` upto 5 times, fixing on each time or until it passes. \
If you run into a locked file then try two times, same with misc system error. Else halt.";

const GITHUB_FIX_TEMPLATE: &str = "\
First, download the logs from the GitHub URL: {url}

IMPORTANT: Use the global `gh` tool to download the logs. For example:
- For workflow runs: `gh run view <run_id> --log`
- For pull requests: `gh pr checks <pr_number> --watch` or `gh pr view <pr_number>`

If the `gh` tool is not found, warn the user that the GitHub CLI tool is not available and fall back to \
using other methods such as curl or web requests to fetch the relevant information from the GitHub API or \
page content.

After downloading and analyzing the logs:
1. Generate a recommended fix based on the errors/issues found
2. List all the steps required to implement the fix
3. Execute the fix by implementing each step

Then proceed with the validation process:
{validation}";

const REBASE_PROMPT: &str = "\
First, unconditionally run `git fetch` to update all remote branches. \
Then rebase to the current origin head. Use the git tool to figure out \
what the origin is. If there is no rebase then do a pull and attempt \
to do a rebase, if it's not successful then finish the rebase line \
by line, don't revert any files. After that print out a summary of \
what you did to make it work, or just say \"No rebase necessary\".";

const UP_PROMPT: &str = "\
You are preparing this repo for a commit to master. Follow these steps:

1. Run `bash lint` (or equivalent linting command for this repo). \
If it fails, fix all errors and rerun until it passes.

2. Run `bash test` (or equivalent test command for this repo). \
If it fails, fix all errors and rerun until it passes.

3. Remove all slop and temporary files: leftover debug prints, \
TODO/FIXME comments you introduced, .bak files, __pycache__ dirs, \
temp files, and any other artifacts that shouldn't be committed.

4. After lint and tests pass, review the git diff and come up with \
a concise one-line summary describing what changed in this repo.

5. Every 30 seconds while working, output a brief status summary of \
what you're doing and current pass/fail state.

6. Once everything passes and is clean, run:
   codeup -m \"<your one-line summary>\"
   (codeup is a global command installed on the system)

7. If codeup fails, read the output, investigate and fix the breakage, \
then rerun lint and test again to make sure fixes didn't break anything, \
and retry codeup. Repeat up to 5 times before giving up.

8. If codeup succeeds (exit code 0), halt.";

const UP_CODEUP_STEP_MARKER: &str =
    "6. Once everything passes and is clean, run:\n   codeup -m \"<your one-line summary>\"";

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LaunchPlan {
    pub command: Vec<String>,
    pub iterations: u32,
    pub backend: Backend,
    pub launch_mode: LaunchMode,
    pub cwd: Option<String>,
}

pub fn build_launch_plan(
    args: &Args,
    backend: Backend,
    launch_mode: LaunchMode,
    backend_path: &str,
) -> LaunchPlan {
    let mut cmd = vec![backend_path.to_string()];
    let mut iterations = 1u32;

    if !args.safe {
        match backend {
            Backend::Claude => cmd.push("--dangerously-skip-permissions".to_string()),
            Backend::Codex => cmd.push("--dangerously-bypass-approvals-and-sandbox".to_string()),
        }
    }

    if let Some(ref model) = args.model {
        cmd.push("--model".to_string());
        cmd.push(model.clone());
    }

    match &args.command {
        Some(Command::Loop { prompt, loop_count }) => {
            iterations = *loop_count;
            if let Some(ref p) = prompt {
                let prompt_text = read_prompt_or_literal(p);
                cmd.push("-p".to_string());
                cmd.push(prompt_text);
            }
        }
        Some(Command::Up { message, publish }) => {
            let prompt = build_up_prompt(message.as_deref(), *publish);
            cmd.push("-p".to_string());
            cmd.push(prompt);
        }
        Some(Command::Rebase) => {
            cmd.push("-p".to_string());
            cmd.push(REBASE_PROMPT.to_string());
        }
        Some(Command::Fix { url }) => {
            let prompt = build_fix_prompt(url.as_deref());
            cmd.push("-p".to_string());
            cmd.push(prompt);
        }
        Some(Command::Wasm { .. }) => {
            unreachable!("wasm execution is handled directly in main")
        }
        Some(Command::Attach { .. })
        | Some(Command::InternalDaemon { .. })
        | Some(Command::InternalWorker { .. }) => {}
        None => {
            if let Some(ref prompt) = args.prompt {
                cmd.push("-p".to_string());
                cmd.push(prompt.clone());
            }
            if let Some(ref message) = args.message {
                cmd.push("-m".to_string());
                cmd.push(message.clone());
            }
            if args.continue_session {
                cmd.push("--continue".to_string());
            }
            if let Some(ref resume) = args.resume {
                cmd.push("--resume".to_string());
                if let Some(ref term) = resume {
                    cmd.push(term.clone());
                }
            }
        }
    }

    cmd.extend(args.passthrough.iter().cloned());

    LaunchPlan {
        command: cmd,
        iterations,
        backend,
        launch_mode,
        cwd: std::env::current_dir()
            .ok()
            .map(|cwd| cwd.to_string_lossy().to_string()),
    }
}

fn build_up_prompt(message: Option<&str>, publish: bool) -> String {
    let mut prompt = UP_PROMPT.to_string();

    match (message, publish) {
        (Some(msg), true) => {
            let replacement = format!(
                "6. Once everything passes and is clean, run:\n   codeup -m \"{}\" -p",
                msg
            );
            prompt = prompt.replace(UP_CODEUP_STEP_MARKER, &replacement);
        }
        (Some(msg), false) => {
            let replacement = format!(
                "6. Once everything passes and is clean, run:\n   codeup -m \"{}\"",
                msg
            );
            prompt = prompt.replace(UP_CODEUP_STEP_MARKER, &replacement);
        }
        (None, true) => {
            let replacement =
                "6. Once everything passes and is clean, run:\n   codeup -m \"<your one-line summary>\" -p";
            prompt = prompt.replace(UP_CODEUP_STEP_MARKER, replacement);
        }
        (None, false) => {}
    }

    prompt
}

fn build_fix_prompt(url: Option<&str>) -> String {
    match url {
        Some(u) if is_github_url(u) => GITHUB_FIX_TEMPLATE
            .replace("{url}", u)
            .replace("{validation}", GITHUB_FIX_VALIDATION),
        _ => FIX_PROMPT.to_string(),
    }
}

fn is_github_url(url: &str) -> bool {
    url.starts_with("https://github.com/") || url.starts_with("http://github.com/")
}

fn read_prompt_or_literal(input: &str) -> String {
    let path = std::path::Path::new(input);
    if path.is_file() {
        match std::fs::read_to_string(path) {
            Ok(contents) => contents,
            Err(e) => {
                eprintln!("error: failed to read prompt file '{}': {}", input, e);
                std::process::exit(1);
            }
        }
    } else {
        input.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::args::Args;

    fn parse(raw: &[&str]) -> Args {
        let raw: Vec<String> = raw.iter().map(|s| s.to_string()).collect();
        Args::parse_from_raw(raw)
    }

    fn plan(raw: &[&str]) -> LaunchPlan {
        let args = parse(raw);
        let backend = crate::backend::resolve_backend(args.claude, args.codex);
        let launch_mode = crate::backend::resolve_launch_mode(args.pty, args.subprocess, backend);
        build_launch_plan(&args, backend, launch_mode, backend.executable_name())
    }

    fn prompt_from_plan(p: &LaunchPlan) -> &str {
        let idx = p.command.iter().position(|a| a == "-p").unwrap();
        &p.command[idx + 1]
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
    fn test_codex_backend() {
        let p = plan(&["clud", "--codex", "-p", "hello"]);
        assert_eq!(
            p.command,
            vec![
                "codex",
                "--dangerously-bypass-approvals-and-sandbox",
                "-p",
                "hello"
            ]
        );
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
        assert!(p.command.contains(&"do stuff".to_string()));
    }

    #[test]
    fn test_loop_default_count() {
        let p = plan(&["clud", "loop", "task"]);
        assert_eq!(p.iterations, 50);
    }

    #[test]
    fn test_pty_override() {
        let p = plan(&["clud", "--pty", "-p", "hello"]);
        assert_eq!(p.launch_mode, LaunchMode::Pty);
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
}
