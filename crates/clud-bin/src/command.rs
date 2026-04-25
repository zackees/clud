use crate::args::{Args, Command};
use crate::backend::{Backend, LaunchMode};
use crate::loop_spec::{
    self, blocked_path_from_done, cache_path, classify, done_marker_contract, ensure_loop_dir,
    fetch_via_gh, git_root_from, render_cache, resolve_current_repo, GhKind, MarkerPaths, TaskSpec,
};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

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
    #[serde(default)]
    pub repeat_schedule: Option<RepeatSchedule>,
    #[serde(default)]
    pub task_summary: Option<String>,
    /// When set, the outer loop should poll for DONE/BLOCKED marker files
    /// after each iteration and terminate accordingly.
    #[serde(default)]
    pub loop_markers: Option<LoopMarkers>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LoopMarkers {
    pub done_path: String,
    pub blocked_path: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RepeatSchedule {
    pub interval_secs: u64,
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
    let mut repeat_schedule: Option<RepeatSchedule> = None;
    let mut task_summary: Option<String> = None;

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
            no_done,
            done,
            repeat,
        }) => {
            iterations = *loop_count;
            let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
            let git_root = git_root_from(&cwd);
            let repeat_interval_secs = repeat
                .as_deref()
                .map(parse_repeat_interval)
                .transpose()
                .unwrap_or_else(|err| {
                    eprintln!("error: invalid --repeat value: {err}");
                    std::process::exit(1);
                });
            repeat_schedule =
                repeat_interval_secs.map(|interval_secs| RepeatSchedule { interval_secs });
            let use_done_markers = done.is_some() || (!*no_done && repeat_schedule.is_none());
            let marker_paths = if use_done_markers {
                Some(resolve_marker_paths(&cwd, &git_root, done.as_deref()))
            } else {
                None
            };
            if let Some(ref t) = task {
                let prompt_text = resolve_loop_task(t, &git_root, *refresh);
                task_summary = Some(summarize_task_name(&prompt_text, 50));
                let final_prompt =
                    if let Some((markers, display_done, display_blocked)) = marker_paths.as_ref() {
                        let _ = markers;
                        format!(
                            "{}{}",
                            prompt_text,
                            done_marker_contract(display_done, display_blocked)
                        )
                    } else {
                        prompt_text
                    };
                push_prompt(&mut cmd, backend, final_prompt);
            }
            if let Some((markers, _, _)) = marker_paths {
                loop_markers = Some(LoopMarkers {
                    done_path: markers.done.to_string_lossy().to_string(),
                    blocked_path: markers.blocked.to_string_lossy().to_string(),
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
        | Some(Command::Logs { .. })
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

    let is_loop = loop_markers.is_some() && repeat_schedule.is_none();
    let parent_has_tty = crate::session::terminals_are_interactive();
    let launch_mode = crate::backend::resolve_launch_mode(
        args.pty,
        args.subprocess,
        backend,
        codex_uses_exec,
        is_loop,
        parent_has_tty,
    );

    LaunchPlan {
        command: cmd,
        iterations,
        backend,
        launch_mode,
        cwd: std::env::current_dir()
            .ok()
            .map(|cwd| cwd.to_string_lossy().to_string()),
        repeat_schedule,
        task_summary,
        loop_markers,
    }
}

fn resolve_marker_paths(
    cwd: &Path,
    git_root: &Path,
    done_override: Option<&str>,
) -> (MarkerPaths, String, String) {
    match done_override {
        Some(raw) => {
            let display_done = raw.to_string();
            let display_blocked = blocked_path_from_done(Path::new(raw))
                .to_string_lossy()
                .to_string();
            let done = cwd.join(raw);
            let blocked = blocked_path_from_done(&done);
            (MarkerPaths { done, blocked }, display_done, display_blocked)
        }
        None => {
            let markers = loop_spec::default_marker_paths(git_root);
            (
                markers,
                ".clud/loop/DONE".to_string(),
                ".clud/loop/BLOCKED".to_string(),
            )
        }
    }
}

/// Parse a `--repeat` duration string into seconds.
///
/// Accepted forms (issue #61): `30s`, `5m`, `1h`, `24h`. The unit is the
/// only recognized suffix; anything more elaborate (compound durations,
/// fractional units, ISO-8601 etc.) is intentionally out of scope.
///
/// Errors when:
/// - input is empty or whitespace-only
/// - integer part is missing (e.g. `s`)
/// - unit part is missing (e.g. `30`)
/// - integer is `0` (a zero interval would busy-loop)
/// - fractional values (`1.5h`) — the `.` makes integer parsing fail
/// - negative values (`-1h`) — the leading `-` is treated as the unit
///   start, which fails the empty-integer check
/// - unsupported units (`30d`, `1y`)
/// - the multiplied result would overflow `u64` seconds
pub(crate) fn parse_repeat_interval(raw: &str) -> Result<u64, String> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Err("duration cannot be empty".to_string());
    }
    let split_at = trimmed
        .find(|c: char| !c.is_ascii_digit())
        .ok_or_else(|| "duration must include a unit like s, m, or h".to_string())?;
    if split_at == 0 {
        return Err("duration must start with a positive integer".to_string());
    }
    let (num_part, unit_part) = trimmed.split_at(split_at);
    let n: u64 = num_part
        .parse()
        .map_err(|_| format!("invalid duration value: {num_part}"))?;
    if n == 0 {
        return Err("duration must be greater than zero".to_string());
    }
    let unit = unit_part.trim().to_ascii_lowercase();
    let multiplier = match unit.as_str() {
        "s" => 1,
        "m" => 60,
        "h" => 60 * 60,
        _ => return Err(format!("unsupported duration unit: {unit_part}")),
    };
    n.checked_mul(multiplier)
        .ok_or_else(|| "duration is too large".to_string())
}

/// Decide whether `clud loop` flags imply that done-marker injection should
/// be disabled for this invocation. Issue #61.
///
/// Truth table (`repeat`, `no_done`, `done`):
/// - (Some, false, None)  → warn + disable (the `--repeat` implies `--no-done` case)
/// - (Some, true,  None)  → user already passed `--no-done`, no warning
/// - (Some, _,    Some)   → `--done <path>` overrides; no warning, contract on
/// - (None, _,    _)      → no `--repeat`, no warning emitted by this helper
///
/// Returns `Some(message)` to be printed to stderr when the warning should
/// fire, otherwise `None`.
pub fn repeat_implies_no_done_warning(
    repeat: Option<&str>,
    no_done: bool,
    done: Option<&str>,
) -> Option<&'static str> {
    if repeat.is_some() && !no_done && done.is_none() {
        Some(
            "[clud] warning: `--repeat` implies `--no-done`; \
             DONE marker injection/checking is disabled.",
        )
    } else {
        None
    }
}

/// Compute the wall-clock millis at which the next repeat run should fire,
/// given the millis at which the previous run *completed*. Issue #61.
///
/// This is the load-bearing "no-overlap" invariant: the next run is
/// scheduled **after the previous run completes**, not after the previous
/// run started. So a run that takes longer than the repeat interval simply
/// pushes the next run further into the future — runs serialize, never
/// overlap.
///
/// Saturates at `u64::MAX` rather than panicking, mirroring the daemon's
/// `saturating_mul` on the seconds→millis conversion.
pub fn next_run_at_millis(completed_at_millis: u64, interval_secs: u64) -> u64 {
    completed_at_millis.saturating_add(interval_secs.saturating_mul(1000))
}

pub fn summarize_task_name(input: &str, max_chars: usize) -> String {
    let normalized = input.split_whitespace().collect::<Vec<_>>().join(" ");
    if normalized.is_empty() || normalized.chars().count() <= max_chars {
        return normalized;
    }
    let keep = max_chars.saturating_sub(3);
    let prefix: String = normalized.chars().take(keep).collect();
    format!("{prefix}...")
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
    fn test_codex_interactive_defaults_to_pty_without_tty() {
        // Under `cargo test` there is no controlling terminal, so
        // `parent_has_tty` is false and the interactive TUI gets a PTY.
        // With a real TTY (normal user invocation), codex runs as a
        // subprocess that inherits the terminal directly — see
        // `backend::test_codex_interactive_with_tty_uses_subprocess`.
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
        assert_eq!(p.command[1], "exec");
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
        assert!(prompt.contains(".clud/loop/DONE"));
        assert!(prompt.contains(".clud/loop/BLOCKED"));
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
        assert_eq!(p.command[1], "exec");
    }

    #[test]
    fn test_codex_loop_forwards_passthrough_flags() {
        // `clud --codex loop "task" -- --verbose` must keep the passthrough
        // flag so the test harness can inject mock-agent flags the same way
        // it does for the claude path.
        let p = plan(&["clud", "--codex", "loop", "task", "--", "--verbose"]);
        assert!(p.command.contains(&"--verbose".to_string()));
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
        // contract, not the default `.clud/loop/DONE`.
        let p = plan(&["clud", "loop", "--done", "custom/DONE.txt", "task"]);
        let prompt = prompt_from_plan(&p);
        // The DONE side is the raw user-supplied string, untouched, so the
        // forward slash survives on every platform.
        assert!(
            prompt.contains("custom/DONE.txt"),
            "prompt missing custom DONE path: {prompt}"
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
    fn simulate_repeat(
        start_ms: u64,
        run_durations_ms: &[u64],
        interval_secs: u64,
    ) -> Vec<(u64, u64)> {
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
}
