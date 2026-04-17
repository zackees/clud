use crate::args::{Args, Command};
use crate::backend::{Backend, LaunchMode};
use crate::loop_spec::{
    self, cache_path, classify, ensure_loop_dir, fetch_via_gh, git_root_from, render_cache,
    resolve_current_repo, GhKind, TaskSpec, DONE_MARKER_CONTRACT,
};
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
    /// When set, the outer loop should poll for DONE/BLOCKED marker files
    /// under `<git_root>/.clud/loop/` after each iteration and terminate
    /// accordingly.
    #[serde(default)]
    pub loop_markers: Option<LoopMarkers>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LoopMarkers {
    pub git_root: String,
}

/// Returns true if `args` carries a prompt that should run non-interactively
/// (via `codex exec <prompt>` on the codex backend).
pub fn has_noninteractive_prompt(args: &Args) -> bool {
    args.prompt.is_some()
        || matches!(
            args.command,
            Some(Command::Loop { .. })
                | Some(Command::Up { .. })
                | Some(Command::Rebase)
                | Some(Command::Fix { .. })
        )
}

pub fn build_launch_plan(args: &Args, backend: Backend, backend_path: &str) -> LaunchPlan {
    let mut cmd = vec![backend_path.to_string()];
    let mut iterations = 1u32;

    let codex_uses_exec = matches!(backend, Backend::Codex) && has_noninteractive_prompt(args);
    let codex_uses_resume = matches!(backend, Backend::Codex)
        && !codex_uses_exec
        && (args.continue_session || args.resume.is_some());

    if codex_uses_exec {
        cmd.push("exec".to_string());
    } else if codex_uses_resume {
        cmd.push("resume".to_string());
    }

    if !args.safe {
        match backend {
            Backend::Claude => cmd.push("--dangerously-skip-permissions".to_string()),
            Backend::Codex => cmd.push("--dangerously-bypass-approvals-and-sandbox".to_string()),
        }
    }

    if let Some(ref model) = args.model {
        match backend {
            Backend::Claude => {
                cmd.push("--model".to_string());
                cmd.push(model.clone());
            }
            Backend::Codex => {
                cmd.push("-m".to_string());
                cmd.push(model.clone());
            }
        }
    }

    // Codex `resume` subcommand: emit `--last` when the user passed `-c` (continue).
    if codex_uses_resume && args.continue_session {
        cmd.push("--last".to_string());
    }

    let mut loop_markers: Option<LoopMarkers> = None;
    match &args.command {
        Some(Command::Loop {
            task,
            loop_count,
            refresh,
            no_done_marker,
        }) => {
            iterations = *loop_count;
            let cwd = std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from("."));
            let git_root = git_root_from(&cwd);
            if let Some(ref t) = task {
                let prompt_text = resolve_loop_task(t, &git_root, *refresh);
                let final_prompt = if *no_done_marker {
                    prompt_text
                } else {
                    format!("{}{}", prompt_text, DONE_MARKER_CONTRACT)
                };
                push_prompt(&mut cmd, backend, final_prompt);
            }
            if !*no_done_marker {
                loop_markers = Some(LoopMarkers {
                    git_root: git_root.to_string_lossy().to_string(),
                });
            }
        }
        Some(Command::Up { message, publish }) => {
            let prompt = build_up_prompt(message.as_deref(), *publish);
            push_prompt(&mut cmd, backend, prompt);
        }
        Some(Command::Rebase) => {
            push_prompt(&mut cmd, backend, REBASE_PROMPT.to_string());
        }
        Some(Command::Fix { url }) => {
            let prompt = build_fix_prompt(url.as_deref());
            push_prompt(&mut cmd, backend, prompt);
        }
        Some(Command::Wasm { .. }) => {
            unreachable!("wasm execution is handled directly in main")
        }
        Some(Command::Attach { .. })
        | Some(Command::Kill { .. })
        | Some(Command::List)
        | Some(Command::InternalDaemon { .. })
        | Some(Command::InternalWorker { .. }) => {}
        None => {
            if let Some(ref prompt) = args.prompt {
                push_prompt(&mut cmd, backend, prompt.clone());
            }
            if let Some(ref message) = args.message {
                // -m has no codex equivalent (codex's -m is --model, handled above).
                // Pass through to claude; drop for codex to avoid clobbering --model.
                if matches!(backend, Backend::Claude) {
                    cmd.push("-m".to_string());
                    cmd.push(message.clone());
                }
            }
            if args.continue_session && matches!(backend, Backend::Claude) {
                cmd.push("--continue".to_string());
            }
            if let Some(ref resume) = args.resume {
                match backend {
                    Backend::Claude => {
                        cmd.push("--resume".to_string());
                        if let Some(ref term) = resume {
                            cmd.push(term.clone());
                        }
                    }
                    Backend::Codex => {
                        // `resume` subcommand was emitted above; the session id
                        // (if any) goes as a positional argument.
                        if let Some(ref term) = resume {
                            cmd.push(term.clone());
                        }
                    }
                }
            }
        }
    }

    cmd.extend(args.passthrough.iter().cloned());

    let launch_mode =
        crate::backend::resolve_launch_mode(args.pty, args.subprocess, backend, codex_uses_exec);

    LaunchPlan {
        command: cmd,
        iterations,
        backend,
        launch_mode,
        cwd: std::env::current_dir()
            .ok()
            .map(|cwd| cwd.to_string_lossy().to_string()),
        loop_markers,
    }
}

/// Resolve the `clud loop` positional to an actual prompt body.
///
/// - GH issue/PR URL → fetch via `gh`, cache, return rendered body.
/// - Short-form `#42` → resolve owner/repo via `gh repo view`, then fetch.
/// - Local file path → read contents.
/// - Literal string → return as-is.
fn resolve_loop_task(task: &str, git_root: &std::path::Path, refresh: bool) -> String {
    match classify(task) {
        TaskSpec::GhIssue {
            owner,
            repo,
            kind,
            number,
        } => fetch_and_cache_or_die(git_root, &owner, &repo, kind, number, refresh),
        TaskSpec::ShortForm(number) => {
            let (owner, repo) = resolve_current_repo().unwrap_or_else(|e| {
                eprintln!("error: `{task}` requires a GH remote; could not resolve via `gh`: {e}");
                std::process::exit(1);
            });
            fetch_and_cache_or_die(git_root, &owner, &repo, GhKind::Issue, number, refresh)
        }
        TaskSpec::File(path) => match std::fs::read_to_string(&path) {
            Ok(contents) => contents,
            Err(e) => {
                eprintln!(
                    "error: failed to read task file '{}': {}",
                    path.display(),
                    e
                );
                std::process::exit(1);
            }
        },
        TaskSpec::Literal(s) => s,
    }
}

fn fetch_and_cache_or_die(
    git_root: &std::path::Path,
    owner: &str,
    repo: &str,
    kind: GhKind,
    number: u32,
    refresh: bool,
) -> String {
    let cache = cache_path(git_root, owner, repo, kind, number);
    if !refresh {
        if let Ok(existing) = std::fs::read_to_string(&cache) {
            eprintln!("[clud loop] using cached {}", cache.display());
            return strip_frontmatter(&existing);
        }
    }
    match fetch_via_gh(owner, repo, kind, number) {
        Ok(doc) => {
            let fetched_at = chrono_like_now();
            let rendered = render_cache(&doc, &fetched_at);
            if let Err(e) = ensure_loop_dir(git_root) {
                eprintln!(
                    "[clud loop] warning: could not create {}: {}",
                    loop_spec::LOOP_DIR,
                    e
                );
            }
            if let Err(e) = std::fs::write(&cache, &rendered) {
                eprintln!(
                    "[clud loop] warning: could not write cache {}: {}",
                    cache.display(),
                    e
                );
            } else {
                eprintln!("[clud loop] cached {}", cache.display());
            }
            strip_frontmatter(&rendered)
        }
        Err(e) => {
            eprintln!(
                "error: failed to fetch GH {} {}/{} #{}: {}",
                match kind {
                    GhKind::Issue => "issue",
                    GhKind::Pr => "pull request",
                },
                owner,
                repo,
                number,
                e
            );
            std::process::exit(1);
        }
    }
}

/// Strip a leading `---\n...\n---\n\n` frontmatter block.
fn strip_frontmatter(s: &str) -> String {
    if let Some(rest) = s.strip_prefix("---\n") {
        if let Some(end) = rest.find("\n---\n") {
            let after = &rest[end + "\n---\n".len()..];
            return after.trim_start_matches('\n').to_string();
        }
    }
    s.to_string()
}

/// ISO-8601 UTC timestamp via system time; avoids pulling chrono.
fn chrono_like_now() -> String {
    use std::time::{SystemTime, UNIX_EPOCH};
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let (y, mo, d, h, mi, se) = unix_to_ymd_hms(secs);
    format!("{y:04}-{mo:02}-{d:02}T{h:02}:{mi:02}:{se:02}Z")
}

fn unix_to_ymd_hms(secs: u64) -> (u32, u32, u32, u32, u32, u32) {
    let se = (secs % 60) as u32;
    let mi = ((secs / 60) % 60) as u32;
    let h = ((secs / 3600) % 24) as u32;
    let days = secs / 86_400;
    // Civil-from-days: Howard Hinnant.
    let z = days as i64 + 719_468;
    let era = z.div_euclid(146_097);
    let doe = (z - era * 146_097) as u64;
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe as i64 + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let mo = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if mo <= 2 { y + 1 } else { y } as u32;
    (y, mo as u32, d as u32, h, mi, se)
}

/// Push a prompt into `cmd` using the right convention for the backend.
/// Claude uses `-p <prompt>`; codex takes the prompt as a positional argument
/// (either to `codex exec` or to the interactive TUI).
fn push_prompt(cmd: &mut Vec<String>, backend: Backend, prompt: String) {
    match backend {
        Backend::Claude => {
            cmd.push("-p".to_string());
            cmd.push(prompt);
        }
        Backend::Codex => {
            cmd.push(prompt);
        }
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
        build_launch_plan(&args, backend, backend.executable_name())
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
            vec![
                "codex",
                "exec",
                "--dangerously-bypass-approvals-and-sandbox",
                "hello",
            ]
        );
        // `codex exec` is non-interactive; subprocess mode is fine.
        assert_eq!(p.launch_mode, LaunchMode::Subprocess);
    }

    #[test]
    fn test_codex_interactive_defaults_to_pty() {
        // `clud --codex` with no prompt launches the TUI; it needs a
        // pseudo-console to receive keyboard input reliably.
        let p = plan(&["clud", "--codex"]);
        assert_eq!(
            p.command,
            vec!["codex", "--dangerously-bypass-approvals-and-sandbox"]
        );
        assert_eq!(p.launch_mode, LaunchMode::Pty);
    }

    #[test]
    fn test_codex_continue_uses_resume_last() {
        // `-c` on codex maps to `codex resume --last`, not `--continue`.
        let p = plan(&["clud", "--codex", "-c"]);
        assert_eq!(
            p.command,
            vec![
                "codex",
                "resume",
                "--dangerously-bypass-approvals-and-sandbox",
                "--last",
            ]
        );
        assert_eq!(p.launch_mode, LaunchMode::Pty);
    }

    #[test]
    fn test_codex_resume_with_session_id() {
        let p = plan(&["clud", "--codex", "-r", "sess-123"]);
        assert_eq!(
            p.command,
            vec![
                "codex",
                "resume",
                "--dangerously-bypass-approvals-and-sandbox",
                "sess-123",
            ]
        );
    }

    #[test]
    fn test_codex_model_uses_short_m() {
        // Codex's model flag is `-m/--model`; Claude's is `--model`.
        let p = plan(&["clud", "--codex", "--model", "gpt-5"]);
        assert_eq!(
            p.command,
            vec![
                "codex",
                "--dangerously-bypass-approvals-and-sandbox",
                "-m",
                "gpt-5"
            ]
        );
    }

    #[test]
    fn test_codex_up_routes_through_exec() {
        let p = plan(&["clud", "--codex", "up"]);
        assert_eq!(p.command[0], "codex");
        assert_eq!(p.command[1], "exec");
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
        assert!(prompt.contains(".clud/loop/DONE"));
        assert!(prompt.contains(".clud/loop/BLOCKED"));
        assert!(p.loop_markers.is_some());
    }

    #[test]
    fn test_loop_default_count() {
        let p = plan(&["clud", "loop", "task"]);
        assert_eq!(p.iterations, 50);
    }

    #[test]
    fn test_loop_no_done_marker_omits_contract() {
        let p = plan(&["clud", "loop", "--no-done-marker", "task"]);
        let prompt = prompt_from_plan(&p);
        assert_eq!(prompt, "task");
        assert!(p.loop_markers.is_none());
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
