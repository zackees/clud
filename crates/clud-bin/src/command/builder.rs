use std::path::{Path, PathBuf};

use crate::args::{Args, Command};
use crate::backend::{
    Backend, HarnessSelection, LaunchMode, ModelProvider, PreferenceSource, ResolvedLaunchTarget,
};
use crate::codex_model::ModelSpec;
use crate::graphics::GraphicsConfig;
use crate::loop_spec::{done_marker_contract, git_root_from};
use crate::provider_catalog;

use super::loop_task::{resolve_loop_task, resolve_marker_paths};
use super::prompts::{
    build_do_prompt, build_fix_prompt, build_grind_prompt, build_up_prompt, push_prompt,
    push_prompt_interactive, REBASE_PROMPT,
};
use super::types::{LaunchPlan, LoopMarkers, RepeatSchedule};

const CODEX_PROJECT_DOC_FALLBACK_KEY: &str = "project_doc_fallback_filenames";
const CODEX_MD_PROJECT_DOC_FALLBACK_CONFIG: &str = r#"project_doc_fallback_filenames=["CODEX.md"]"#;
const CLAUDE_MD_PROJECT_DOC_FALLBACK_CONFIG: &str =
    r#"project_doc_fallback_filenames=["CLAUDE.md"]"#;

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
                | Some(Command::Do { .. })
        )
}

/// True when this launch routes a non-Claude model provider through the Claude
/// harness. Codex and DeepSeek both fit this pattern: the model driving the
/// harness is not Claude, and the harness offers it an `EnterPlanMode` tool
/// whose description instructs it to reach for plan mode *proactively* on any
/// non-trivial implementation ask. The result is unrequested planning sessions
/// in the middle of ordinary questions, so plan mode is stripped here
/// regardless of `--unattended` — the interactive case is precisely where it
/// was biting.
///
/// Deliberately narrow: a plain `clud` (Claude provider, Claude harness) keeps
/// plan mode, and `AskUserQuestion` is never touched by this rule.
fn is_non_claude_claude_harness_bridge(target: ResolvedLaunchTarget) -> bool {
    target.routing_mode == crate::backend::RoutingMode::Direct
        && matches!(
            target.model_provider,
            ModelProvider::Codex | ModelProvider::DeepSeek
        )
        && matches!(target.effective_harness, Backend::Claude)
}

/// True when this is the Codex-provider / Claude-harness bridge specifically.
/// Codex allows exactly one Claude process, so the `Task` tool (which creates
/// subagents) is stripped for Codex only — DeepSeek doesn't share that limit.
fn is_codex_claude_bridge(target: ResolvedLaunchTarget) -> bool {
    target.routing_mode == crate::backend::RoutingMode::Direct
        && matches!(target.model_provider, ModelProvider::Codex)
        && matches!(target.effective_harness, Backend::Claude)
}

pub fn bridge_suppresses_plan_mode(args: &Args, target: ResolvedLaunchTarget) -> bool {
    !args.allow_plan_mode && is_non_claude_claude_harness_bridge(target)
}

/// Green, stderr, TTY-only notice announcing the [`bridge_suppresses_plan_mode`]
/// suppression and how to undo it. Mirrors
/// [`crate::backend::saved_harness_override_notice`]: suppressed when stderr is
/// not a terminal or the caller wants structured output, so machine-readable
/// stderr stays clean.
pub fn plan_mode_suppression_notice(
    args: &Args,
    target: ResolvedLaunchTarget,
    stderr_is_terminal: bool,
    structured_output: bool,
) -> Option<String> {
    if structured_output || !stderr_is_terminal || !bridge_suppresses_plan_mode(args, target) {
        return None;
    }
    Some(
        "\x1b[32m[clud] Plan mode disabled on the non-Claude bridge \
         (the model can otherwise enter it unprompted). \
         Override with --allow-plan-mode\x1b[0m"
            .to_string(),
    )
}

pub fn build_launch_plan(args: &Args, backend: Backend, backend_path: &str) -> LaunchPlan {
    let target = ResolvedLaunchTarget {
        routing_mode: crate::backend::RoutingMode::Direct,
        model_provider: backend.as_model_provider(),
        requested_harness: HarnessSelection::Default,
        effective_harness: backend,
        provider_source: PreferenceSource::BuiltInDefault,
        harness_source: PreferenceSource::BuiltInDefault,
    };
    build_launch_plan_for_target(args, target, backend_path)
}

pub fn build_launch_plan_for_target(
    args: &Args,
    target: ResolvedLaunchTarget,
    backend_path: &str,
) -> LaunchPlan {
    let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    build_launch_plan_for_target_at(args, target, backend_path, &cwd)
}

#[cfg(test)]
pub(crate) fn build_launch_plan_at(
    args: &Args,
    backend: Backend,
    backend_path: &str,
    cwd: &Path,
) -> LaunchPlan {
    let target = ResolvedLaunchTarget {
        routing_mode: crate::backend::RoutingMode::Direct,
        model_provider: backend.as_model_provider(),
        requested_harness: HarnessSelection::Default,
        effective_harness: backend,
        provider_source: PreferenceSource::BuiltInDefault,
        harness_source: PreferenceSource::BuiltInDefault,
    };
    build_launch_plan_for_target_at(args, target, backend_path, cwd)
}

fn build_launch_plan_for_target_at(
    args: &Args,
    target: ResolvedLaunchTarget,
    backend_path: &str,
    cwd: &Path,
) -> LaunchPlan {
    let backend = target.effective_harness;
    let mut cmd = vec![backend_path.to_string()];
    let mut iterations = 1u32;
    let mut repeat_schedule: Option<RepeatSchedule> = None;
    let mut task_summary: Option<String> = None;
    let model_selection = args.resolved_model_selection.clone().or_else(|| {
        provider_catalog::resolve(
            Some(target.model_provider),
            args.model.as_deref(),
            args.effort.as_deref(),
            args.context_window.as_deref(),
        )
        .ok()
        .flatten()
    });

    let codex_uses_exec = matches!(backend, Backend::Codex) && has_noninteractive_prompt(args);
    let codex_uses_resume = matches!(backend, Backend::Codex)
        && !codex_uses_exec
        && (args.continue_session || args.resume.is_some());

    if matches!(backend, Backend::Codex) {
        for override_value in &args.codex_config_overrides {
            cmd.push("-c".to_string());
            cmd.push(override_value.clone());
        }
        if !has_codex_project_doc_fallback_override(&args.codex_config_overrides) {
            if let Some(fallback_config) = codex_project_doc_fallback_config(cwd) {
                cmd.push("-c".to_string());
                cmd.push(fallback_config.to_string());
            }
        }
        if let Some(effort) = model_selection
            .as_ref()
            .and_then(|selection| selection.effort)
        {
            cmd.push("-c".to_string());
            cmd.push(format!("model_reasoning_effort=\"{}\"", effort.as_str()));
        }
    }

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

    // Two independent rules feed one `--disallowedTools` token:
    //
    // 1. `clud loop` is unattended by definition; `--unattended` extends the
    //    same policy to any other launch. Strip both tools that park a run on
    //    a human. `--dangerously-skip-permissions` does not cover these; the
    //    model reaches for them on its own, most often at the top of a `/loop`
    //    iteration.
    // 2. The Codex->Claude bridge allows exactly one Claude process. It strips
    //    the `Task` tool on every bridge launch, because Task creates Claude
    //    subagents whose requests all consume the same provider budget. Plan
    //    mode is also stripped unless explicitly restored; `AskUserQuestion`
    //    survives the bridge rules, so the token is composed rather than fixed.
    //
    // Emitted as one `=`-bound, comma-separated token on purpose. `claude`
    // declares `--disallowedTools <tools...>` as variadic, so the
    // space-separated spelling eats whatever argv token follows it — including
    // a later `-p <prompt>`, which makes claude exit 0 with no output and no
    // diagnostic. A single token cannot swallow anything, so this stays correct
    // wherever it lands relative to the prompt and to unknown-flag passthrough.
    let is_unattended = args.unattended || matches!(args.command, Some(Command::Loop { .. }));
    let mut disallowed: Vec<&str> = Vec::new();
    if matches!(backend, Backend::Claude) {
        // Order matters only for stability of the emitted token, which the
        // tests assert on literally.
        if is_unattended || bridge_suppresses_plan_mode(args, target) {
            disallowed.push("EnterPlanMode");
        }
        if is_codex_claude_bridge(target) {
            disallowed.push("Task");
        }
        if is_unattended {
            disallowed.push("AskUserQuestion");
        }
    }
    if !disallowed.is_empty() {
        cmd.push(format!("--disallowedTools={}", disallowed.join(",")));
    }

    // On the Codex-provider / Claude-harness bridge, `--model` selects a
    // *Codex* model even though the flag is handed to the Claude harness. The
    // short name is expanded to the wire id here so the harness forwards
    // something upstream understands, and so `--dry-run` shows what will
    // actually be billed rather than the shorthand that was typed.
    //
    // This works at all because a custom `ANTHROPIC_BASE_URL` makes the
    // gateway the owner of the model namespace: the harness passes the string
    // through without validating it. An id that does not parse is left alone
    // and rejected by the bridge, which owns the error message.
    let codex_model = codex_model_selection(args, target, model_selection.as_ref());
    let emitted_model = if target.routing_mode == crate::backend::RoutingMode::Unified {
        unified_gateway_model_selection(model_selection.as_ref()).or_else(|| args.model.clone())
    } else {
        codex_model.clone().or_else(|| {
            model_selection
                .as_ref()
                .and_then(|selection| selection.wire_model.clone())
                .or_else(|| args.model.clone())
        })
    };
    if let Some(emitted) = emitted_model {
        match backend {
            Backend::Claude => {
                cmd.push("--model".to_string());
                cmd.push(emitted);
            }
            Backend::Codex => {
                cmd.push("-m".to_string());
                cmd.push(emitted);
            }
        }
    }
    // Direct Codex-via-Claude already carries effort in the bridge-owned
    // `<wire-model>@<effort>` compatibility spelling. Passing Claude's
    // `--effort` as well would mutate harness session state and change the
    // established direct-route contract. Native Claude, DeepSeek's
    // Anthropic-compatible route, and future unified sessions use the harness
    // effort field directly.
    if matches!(backend, Backend::Claude)
        && !(target.routing_mode == crate::backend::RoutingMode::Direct
            && target.model_provider == ModelProvider::Codex)
    {
        if let Some(effort) = model_selection
            .as_ref()
            .and_then(|selection| selection.effort)
            .filter(|effort| *effort != crate::provider_catalog::EffortLevel::None)
        {
            cmd.push("--effort".to_string());
            cmd.push(effort.as_str().to_string());
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
            let git_root = git_root_from(cwd);
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
                Some(resolve_marker_paths(cwd, &git_root, done.as_deref()))
            } else {
                None
            };
            if let Some(ref t) = task {
                let prompt_text = resolve_loop_task(t, &git_root, *refresh);
                task_summary = Some(summarize_task_name(&prompt_text, 50));
                let final_prompt = if let Some(markers) = marker_paths.as_ref() {
                    // Issue #95: feed absolute paths into the contract so the
                    // model writes to the exact path clud is polling, not
                    // some invented alternative like `~/.loop/LOOP.md`.
                    format!(
                        "{}{}",
                        prompt_text,
                        done_marker_contract(&markers.done, &markers.blocked)
                    )
                } else {
                    prompt_text
                };
                push_prompt(&mut cmd, backend, final_prompt);
            }
            if let Some(markers) = marker_paths {
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
        Some(Command::Do { url }) => {
            // `do` runs the `/goal` contract, which drives a long interactive
            // Stop-hook loop. Headless `-p` mode buffers its single final
            // response and shows nothing until the whole goal completes, so the
            // terminal looks halted. Seed an interactive session instead.
            let prompt = build_do_prompt(url);
            push_prompt_interactive(&mut cmd, prompt);
        }
        Some(Command::Wasm { .. }) => {
            unreachable!("wasm execution is handled directly in main")
        }
        Some(Command::Grind { url }) => {
            // `grind` uses the `/loop` contract with DONE/BLOCKED markers so
            // each completed issue triggers re-invocation. When all issues are
            // done the agent writes BLOCKED and the loop terminates.
            let url = url.as_deref().unwrap_or("");
            let git_root = git_root_from(cwd);
            let marker_paths = resolve_marker_paths(cwd, &git_root, None);
            let prompt_text = build_grind_prompt(url);
            let final_prompt = format!(
                "{}{}",
                prompt_text,
                done_marker_contract(&marker_paths.done, &marker_paths.blocked)
            );
            task_summary = Some(format!("grind {url}"));
            loop_markers = Some(LoopMarkers {
                done_path: marker_paths.done.to_string_lossy().to_string(),
                blocked_path: marker_paths.blocked.to_string_lossy().to_string(),
            });
            iterations = 200; // high ceiling — loop terminates via BLOCKED
            push_prompt(&mut cmd, backend, final_prompt);
        }
        Some(Command::Auth { .. })
        | Some(Command::CodexAuth { .. })
        | Some(Command::DeepseekAuth { .. })
        | Some(Command::Attach { .. })
        | Some(Command::Kill { .. })
        | Some(Command::Slay)
        | Some(Command::List)
        | Some(Command::Top { .. })
        | Some(Command::Logs { .. })
        | Some(Command::Log { .. })
        | Some(Command::Gc { .. })
        | Some(Command::Config { .. })
        | Some(Command::Ui { .. })
        | Some(Command::Trash { .. })
        | Some(Command::Tool { .. })
        | Some(Command::Optimize { .. })
        | Some(Command::Symbols { .. })
        | Some(Command::Test { .. })
        | Some(Command::Settings { .. })
        | Some(Command::Daemon { .. })
        | Some(Command::InternalDaemon { .. })
        | Some(Command::InternalWorker { .. }) => {}
        Some(Command::Run) | None => {
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

    let is_loop_cmd = matches!(&args.command, Some(Command::Loop { .. }));
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

    // Issue: subprocess-mode loops on claude went silent until the iteration
    // finished, because `claude -p` buffers its single final response. Inject
    // `--output-format stream-json --verbose` so claude emits one JSON event
    // per turn step, and let the runtime render those into progress lines.
    // PTY-mode loops already stream the live TUI; codex doesn't expose this
    // flag at all.
    //
    // The flags MUST be inserted BEFORE the prompt (`-p <prompt>`) so that
    // `command[-1]` remains the prompt — downstream tooling, dry-run JSON
    // consumers, and integration tests rely on that contract.
    let stream_json_progress =
        matches!(backend, Backend::Claude) && is_loop_cmd && launch_mode == LaunchMode::Subprocess;
    if stream_json_progress {
        // For Claude, `push_prompt` emits `-p` then the prompt body. Find that
        // `-p` and slot the stream-json flags in just before it. This keeps
        // any earlier args (yolo, --model, etc.) and the prompt anchored at
        // the tail of the command.
        if let Some(p_idx) = cmd.iter().position(|a| a == "-p") {
            cmd.splice(
                p_idx..p_idx,
                [
                    "--output-format".to_string(),
                    "stream-json".to_string(),
                    "--verbose".to_string(),
                ],
            );
        }
    }

    LaunchPlan {
        command: cmd,
        iterations,
        backend,
        routing_mode: target.routing_mode,
        model_provider: Some(target.model_provider),
        requested_harness: Some(target.requested_harness),
        effective_harness: Some(target.effective_harness),
        provider_source: Some(target.provider_source),
        harness_source: Some(target.harness_source),
        launch_mode,
        cwd: Some(cwd.to_string_lossy().to_string()),
        graphics: GraphicsConfig {
            mode: args.graphics,
            image_path: args.graphics_image.clone(),
        },
        repeat_schedule,
        task_summary,
        loop_markers,
        stream_json_progress,
        codex_model,
        model_selection,
    }
}

/// Canonicalize `--model` for a Codex-provider / Claude-harness launch.
///
/// `None` when this is not that cross-route or neither model nor effort was
/// selected. Model-less effort pins the bridge's reviewed Terra default. A
/// parse failure remains bridge-owned and does not substitute another model.
fn codex_model_selection(
    args: &Args,
    target: ResolvedLaunchTarget,
    selection: Option<&provider_catalog::ResolvedModelSelection>,
) -> Option<String> {
    if target.routing_mode != crate::backend::RoutingMode::Direct
        || target.model_provider != ModelProvider::Codex
        || target.effective_harness != Backend::Claude
    {
        return None;
    }
    if let Some(selection) = selection {
        let mut value = selection.wire_model.clone().or_else(|| {
            selection.effort.map(|_| {
                ModelSpec::parse("terra")
                    .expect("the provider catalog must retain the Terra compatibility alias")
                    .model
            })
        })?;
        if let Some(effort) = selection.effort {
            value.push('@');
            value.push_str(effort.as_str());
        }
        return Some(value);
    }
    let requested = args.model.as_deref()?;
    ModelSpec::parse(requested).ok().map(|spec| spec.display())
}

/// Translate a normalized initial selection into the ID Claude Code must send
/// back to the unified discovery gateway. Provider wire IDs are not safe here:
/// an unrecognized `gpt-*` would look like an ordinary native Claude ID and
/// could be proxied to Anthropic.
fn unified_gateway_model_selection(
    selection: Option<&provider_catalog::ResolvedModelSelection>,
) -> Option<String> {
    let selection = selection?;
    if selection.provider == ModelProvider::Claude {
        return selection.wire_model.clone();
    }
    selection
        .model
        .as_deref()
        .and_then(provider_catalog::model_by_cli_id)
        .and_then(|model| model.discovery_id)
        .map(str::to_string)
}

fn has_codex_project_doc_fallback_override(overrides: &[String]) -> bool {
    overrides.iter().any(|value| {
        value
            .trim_start()
            .strip_prefix(CODEX_PROJECT_DOC_FALLBACK_KEY)
            .is_some_and(|rest| rest.trim_start().starts_with('='))
    })
}

fn codex_project_doc_fallback_config(cwd: &Path) -> Option<&'static str> {
    let repo_root = git_root_from(cwd);
    if repo_root.join("AGENTS.md").is_file() {
        return None;
    }
    if repo_root.join("CODEX.md").is_file() {
        return Some(CODEX_MD_PROJECT_DOC_FALLBACK_CONFIG);
    }
    if repo_root.join("CLAUDE.md").is_file() {
        return Some(CLAUDE_MD_PROJECT_DOC_FALLBACK_CONFIG);
    }
    None
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
