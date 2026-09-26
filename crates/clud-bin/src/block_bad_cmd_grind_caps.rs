//! Per-role shell caps for the `/grind` workflow's agents.
//!
//! Claude Code puts `agent_type` in a subagent's PreToolUse payload. When it
//! names one of the bundled `grind-*` roles, the role's shell allowlist (or,
//! for the integrator, denylist) decides the call. Every other caller —
//! the primary session, other subagents — is untouched.
//!
//! The agent definitions' `tools:` frontmatter is the hard limit on *which
//! tools* a role has (workers and reviewers get a shell only for read-only
//! `gh`); this module
//! is the second layer for the one tool frontmatter cannot narrow. See
//! [`docs/architecture/grind.md`](../../../docs/architecture/grind.md).
//!
//! Run-scoped facts come from `.clud/grind/run.json`, written by the `/grind`
//! router at the main checkout's root before the workflow starts, and found
//! from a linked worktree through its `.git` file: `mode`
//! (`parallel` permits the planner's `git worktree add`) and `ci` (permits the
//! integrator's `act`), plus the optional `scripts: {lint, test}` the router
//! detected. Workers and reviewers may never run the repo's lint/test
//! scripts; the integrator runs them before every push. During the read-only
//! planning pass the router also writes `"phase": "plan"`, which puts the
//! planner in plan-only mode (no worktrees, no file writes); the router
//! removes it before the real run.

use std::path::Path;

use serde_json::Value;

use super::block_bad_cmd_gate::statement_words;

pub(super) const PLANNER: &str = "grind-planner";
pub(super) const WORKER: &str = "grind-worker";
pub(super) const REVIEWER: &str = "grind-reviewer";
pub(super) const INTEGRATOR: &str = "grind-integrator";
pub(super) const LANDER: &str = "grind-lander";
pub(super) const PREWORK: &str = "grind-prework";

/// Read-only inspection commands every shell-holding role may run, subject
/// to [`inspect_ok`].
const INSPECT: &[&str] = &[
    "ls", "cat", "head", "tail", "wc", "find", "grep", "rg", "pwd", "echo", "test", "true",
];

const GIT_READ: &[&str] = &[
    "status",
    "log",
    "diff",
    "show",
    "rev-parse",
    "ls-files",
    "fetch",
    "merge-base",
    "describe",
    "cat-file",
    "rev-list",
    "blame",
];

/// `find -exec`/`-delete` run or remove things, and `tail -f` never exits.
/// A path-qualified program (`./test`, `bin/ls`) is never the builtin or the
/// system tool, so it is not inspection.
fn inspect_ok(program: &str, words: &[String]) -> bool {
    if !INSPECT.contains(&program) || words[0].contains(['/', '\\']) {
        return false;
    }
    let args = &words[1..];
    match program {
        "find" => !args.iter().any(|w| {
            matches!(
                w.as_str(),
                "-exec" | "-execdir" | "-ok" | "-okdir" | "-delete"
            ) || w.starts_with("-fprint")
                || w.starts_with("-fls")
        }),
        "tail" => !args.iter().any(|w| {
            w == "--follow"
                || w.starts_with("--follow=")
                || (w.starts_with('-')
                    && !w.starts_with("--")
                    && (w.contains('f') || w.contains('F')))
        }),
        _ => true,
    }
}

/// The run facts the router recorded.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(super) struct RunFacts {
    pub parallel: bool,
    pub ci: bool,
    /// `"phase": "plan"`: the router's read-only planning pass.
    pub plan_only: bool,
    /// The repo's lint/test scripts the router detected and the user agreed
    /// to run (`scripts: {lint, test}`); `None` when absent or not a string.
    pub scripts: RunScripts,
    /// The meta issue number (`meta`, a JSON number or string, `#` stripped):
    /// the one issue `grind-prework` may comment on.
    pub meta: Option<String>,
    /// Feature-branch mode (`feature: {branch, pr, worktree}`); `None` when
    /// absent, which keeps every cap exactly as in a plain run.
    pub feature: Option<FeatureFacts>,
    /// What happens to the feature PR once every goal PR has landed.
    pub feature_merge: FeatureMerge,
}

/// The feature branch a feature-branch-mode run lands its goal PRs into.
///
/// The main-session router keeps a single feature worktree (`worktree`);
/// that rule is enforced by the `/grind` skill, not here: the router is not
/// a capped role, and this hook only sees `grind-*` agent types.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(super) struct FeatureFacts {
    pub branch: String,
    /// The draft feature PR number (`#` or URL stripped). While `run.json`
    /// records the feature, its PR is treated as open.
    pub pr: Option<String>,
    pub worktree: Option<String>,
}

/// `feature_merge` in `run.json`: whether the lander may merge the feature PR.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(super) enum FeatureMerge {
    /// The lander readies and merges the feature PR (merge commit only).
    Auto,
    /// Leave the PR for the user to decide.
    #[default]
    DecideLater,
    /// Comment on the PR only; the user merges it.
    CommentOnly,
}

/// A PR number from a JSON number, `#n`, `n`, or a URL ending in the number.
fn pr_number(value: &Value) -> Option<String> {
    match value {
        Value::Number(n) => Some(n.to_string()),
        Value::String(s) => number_word(s),
        _ => None,
    }
}

fn number_word(word: &str) -> Option<String> {
    let bare = word
        .trim()
        .trim_matches(&['\'', '"'][..])
        .trim_end_matches('/');
    let tail = bare.rsplit('/').next().unwrap_or(bare);
    let num = tail.strip_prefix('#').unwrap_or(tail).trim();
    (!num.is_empty() && num.chars().all(|c| c.is_ascii_digit())).then(|| num.to_string())
}

/// The repo scripts recorded in `run.json`'s optional `scripts` object.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(super) struct RunScripts {
    pub lint: Option<String>,
    pub test: Option<String>,
}

impl RunFacts {
    pub(super) fn from_json(value: &Value) -> Self {
        let script = |name: &str| {
            value
                .get("scripts")
                .and_then(|s| s.get(name))
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(str::to_string)
        };
        Self {
            parallel: value.get("mode").and_then(Value::as_str) == Some("parallel"),
            ci: value.get("ci").and_then(Value::as_bool).unwrap_or(false),
            plan_only: value.get("phase").and_then(Value::as_str) == Some("plan"),
            scripts: RunScripts {
                lint: script("lint"),
                test: script("test"),
            },
            meta: value.get("meta").and_then(|m| match m {
                Value::Number(n) => Some(n.to_string()),
                Value::String(s) => {
                    let s = s.trim();
                    let s = s.strip_prefix('#').unwrap_or(s).trim();
                    (!s.is_empty()).then(|| s.to_string())
                }
                _ => None,
            }),
            feature: value.get("feature").and_then(|f| {
                let branch = f.get("branch")?.as_str()?.trim();
                (!branch.is_empty()).then(|| FeatureFacts {
                    branch: branch.to_string(),
                    pr: f.get("pr").and_then(pr_number),
                    worktree: f
                        .get("worktree")
                        .and_then(Value::as_str)
                        .map(str::trim)
                        .filter(|s| !s.is_empty())
                        .map(str::to_string),
                })
            }),
            feature_merge: match value.get("feature_merge").and_then(Value::as_str) {
                Some("auto") => FeatureMerge::Auto,
                Some("comment_only" | "comment") => FeatureMerge::CommentOnly,
                _ => FeatureMerge::DecideLater,
            },
        }
    }

    /// The feature PR number, when a feature run records one.
    fn feature_pr(&self) -> Option<&str> {
        self.feature.as_ref()?.pr.as_deref()
    }

    /// Search upward from `cwd` for `.clud/grind/run.json`. A linked worktree
    /// (a sibling directory in parallel mode) is resolved to its main checkout
    /// through the `gitdir:` line of its `.git` file.
    pub(super) fn discover(cwd: &Path) -> Self {
        for dir in cwd.ancestors() {
            let mut roots = vec![dir.to_path_buf()];
            if let Some(main) = main_checkout_of(dir) {
                roots.push(main);
            }
            for root in roots {
                let path = root.join(".clud").join("grind").join("run.json");
                if let Ok(text) = std::fs::read_to_string(&path) {
                    return serde_json::from_str(&text)
                        .map(|value| Self::from_json(&value))
                        .unwrap_or_default();
                }
            }
        }
        Self::default()
    }
}

/// The main checkout of a linked worktree rooted at `dir`, read lexically from
/// `dir/.git` (`gitdir: <main>/.git/worktrees/<name>`).
fn main_checkout_of(dir: &Path) -> Option<std::path::PathBuf> {
    let text = std::fs::read_to_string(dir.join(".git")).ok()?;
    main_checkout_from_gitdir(text.trim().strip_prefix("gitdir:")?.trim())
}

fn main_checkout_from_gitdir(gitdir: &str) -> Option<std::path::PathBuf> {
    let gitdir = Path::new(gitdir);
    let worktrees = gitdir.parent()?;
    if worktrees.file_name()? != "worktrees" {
        return None;
    }
    let dot_git = worktrees.parent()?;
    if dot_git.file_name()? != ".git" {
        return None;
    }
    dot_git.parent().map(Path::to_path_buf)
}

/// Set to `1` to let an `Agent` call start a `grind-*` agent type directly,
/// for testing one role by hand.
pub(super) const ALLOW_GRIND_AGENTS_ENV: &str = "CLUD_ALLOW_GRIND_AGENTS";

/// The denial for an `Agent` call that names a `grind-*` agent type, or
/// `None` to allow. The workflow never uses the `Agent` tool (its `agent()`
/// calls emit no tool event), so this only ever stops the model delegating
/// to a grind role on its own.
pub(super) fn agent_spawn_reason(
    tool_name: &str,
    tool_input: Option<&Value>,
    allow_env: Option<&str>,
) -> Option<String> {
    if !tool_name.eq_ignore_ascii_case("agent") || allow_env == Some("1") {
        return None;
    }
    let requested = tool_input?
        .get("subagent_type")
        .or_else(|| tool_input?.get("subagentType"))?
        .as_str()?;
    is_grind_role(requested).then(|| {
        format!(
            "Blocked: `{requested}` is an internal /grind role, started only by the \
             `grind-run` workflow. Use /grind to run the workflow, or do the work \
             yourself. To start this role by hand for testing, set \
             {ALLOW_GRIND_AGENTS_ENV}=1."
        )
    })
}

/// Whether `agent_type` is a capped `/grind` role.
pub(super) fn is_grind_role(agent_type: &str) -> bool {
    matches!(
        agent_type,
        PLANNER | WORKER | REVIEWER | INTEGRATOR | LANDER | PREWORK
    )
}

/// The denial reason for a shell `command` from `role`, or `None` to allow.
pub(super) fn shell_reason(role: &str, command: &str, run: &RunFacts) -> Option<String> {
    let statements = match statement_words(command) {
        Ok(statements) => statements,
        Err(construct) => return Some(format!("{role} may not use {construct}")),
    };
    if role == INTEGRATOR && is_script_retry_loop(&statements) {
        return Some(format!(
            "{role} may not rerun lint or test in a shell loop: read the failing tests first, and \
             rerun unchanged only an infrastructure error, at most twice (#1425)"
        ));
    }
    statements
        .iter()
        .find_map(|words| statement_reason(role, words, run))
}

/// Whether a command is a shell loop that runs the repo's lint or test script
/// (`for i in 1 2 3; do bash test; done`, `until ./test; do :; done`), which
/// reruns a deterministic failure instead of reading it (#1425). The flat
/// scanner splits the loop into statements, so a loop keyword heading one
/// statement plus a script in any statement is enough. A wait loop on a
/// marker file (`until [ -f test.exit ]; do sleep 5; done`) runs no script.
fn is_script_retry_loop(statements: &[Vec<String>]) -> bool {
    let looped = statements.iter().any(|words| {
        words
            .first()
            .is_some_and(|w| matches!(w.as_str(), "for" | "while" | "until"))
    });
    looped && statements.iter().any(|words| runs_repo_script(words.as_slice()))
}

/// The denial reason for a non-shell tool call from `role`, or `None` to
/// allow. No role may call `AskUserQuestion`: /grind asks every question up
/// front, in one round before prework. The planner never writes files, in
/// either phase, and neither does the prework agent.
pub(super) fn tool_reason(role: &str, tool_name: &str, run: &RunFacts) -> Option<String> {
    const WRITE_TOOLS: &[&str] = &["Write", "Edit", "MultiEdit", "NotebookEdit"];
    if tool_name.eq_ignore_ascii_case("AskUserQuestion") {
        return Some(format!(
            "{role} may not ask the user: /grind asks every question up front, in one round \
             before prework; follow the rule recorded in the plan instead"
        ));
    }
    if !matches!(role, PLANNER | PREWORK)
        || !WRITE_TOOLS
            .iter()
            .any(|t| t.eq_ignore_ascii_case(tool_name))
    {
        return None;
    }
    let mode = if run.plan_only && role == PLANNER {
        " (plan-only mode)"
    } else {
        ""
    };
    Some(format!("{role} may not write files{mode}"))
}

fn statement_reason(role: &str, words: &[String], run: &RunFacts) -> Option<String> {
    let program = program_name(&words[0]);
    let deny = |what: &str| Some(format!("{role} may not run `{what}`"));
    if let Some(reason) = feature_reason(role, words, run) {
        return Some(reason);
    }
    if creates_issue(words) {
        return Some(format!(
            "{role} may not run `gh issue create`: the /grind router files problems; report \
             them in your structured output instead"
        ));
    }
    if closes_issue(words) {
        return Some(
            "grind roles never close issues; the feature PR or `clud grind reconcile` closes \
             them (#1393)"
                .to_string(),
        );
    }
    match role {
        PLANNER => {
            if inspect_ok(&program, words) || is_gh_read(words) {
                return None;
            }
            if program == "git" {
                let Some(sub) = git_subcommand(words) else {
                    return Some(format!(
                        "{role} may not pass git global options other than -C"
                    ));
                };
                return match sub.as_slice() {
                    _ if is_git_read(&sub) => None,
                    [a, b, ..] if a == "worktree" && b == "list" => None,
                    [a, b, ..] if a == "worktree" && b == "add" && run.plan_only => {
                        Some(format!("{role} is in plan-only mode: no worktrees"))
                    }
                    [a, b, ..] if a == "worktree" && b == "add" && run.parallel => None,
                    [a, b, ..] if a == "worktree" && b == "add" => {
                        Some(format!("{role} may create worktrees only in parallel mode"))
                    }
                    _ => deny(&format!("git {}", sub.first().map_or("", String::as_str))),
                };
            }
            deny(&program)
        }
        PREWORK => {
            if inspect_ok(&program, words) || is_gh_read(words) {
                return None;
            }
            if program == "gh"
                && words.get(1).is_some_and(|w| w == "issue")
                && words.get(2).is_some_and(|w| w == "comment")
            {
                return match comment_target(&words[3..]) {
                    Err(flag) => Some(format!("{role} may not pass `{flag}` to gh issue comment")),
                    Ok(target) if target.is_some() && target == run.meta => None,
                    Ok(_) => Some(format!(
                        "{role} may comment only on the meta issue{}",
                        run.meta
                            .as_deref()
                            .map_or(String::new(), |m| format!(" (#{m})"))
                    )),
                };
            }
            if program == "git" {
                let Some(sub) = git_subcommand(words) else {
                    return Some(format!(
                        "{role} may not pass git global options other than -C"
                    ));
                };
                return match sub.as_slice() {
                    _ if is_git_read(&sub) => None,
                    [a, b, ..] if a == "worktree" && b == "list" => None,
                    _ => deny(&format!("git {}", sub.first().map_or("", String::as_str))),
                };
            }
            deny(&program)
        }
        LANDER => {
            if inspect_ok(&program, words) || is_gh_read(words) {
                return None;
            }
            if program == "gh"
                && words.get(1).is_some_and(|w| w == "pr")
                && words.get(2).is_some_and(|w| {
                    matches!(w.as_str(), "view" | "checks" | "diff" | "list" | "merge")
                })
            {
                return None;
            }
            if program == "gh"
                && words.get(1).is_some_and(|w| w == "pr")
                && words.get(2).is_some_and(|w| w == "ready")
            {
                let target = pr_target(&words[3..]);
                return match run.feature_pr() {
                    Some(pr) if target.as_deref() == Some(pr) => {
                        (run.feature_merge != FeatureMerge::Auto).then(|| {
                            format!(
                                "{role} may ready the feature PR only under feature_merge=auto; \
                                 {} leaves it for the user",
                                policy_name(run.feature_merge)
                            )
                        })
                    }
                    _ => Some(format!(
                        "{role} may run `gh pr ready` only on the feature PR"
                    )),
                };
            }
            if is_clud(&words[0])
                && words.get(1).is_some_and(|w| w == "tool")
                && words.get(2).is_some_and(|w| w == "run")
                && words
                    .get(3)
                    .is_some_and(|w| w.starts_with("github/pr_merge_watch"))
            {
                return None;
            }
            if program == "git" {
                let Some(sub) = git_subcommand(words) else {
                    return Some(format!(
                        "{role} may not pass git global options other than -C"
                    ));
                };
                if is_plain_push(&sub) || is_git_read(&sub) {
                    return None;
                }
                return deny(&format!("git {}", sub.first().map_or("", String::as_str)));
            }
            deny(&program)
        }
        INTEGRATOR => {
            // A denylist is only as good as what it sees, so look through
            // wrappers (`bash -c "docker run …"`, `env act`, `xargs bosn`):
            // every whitespace-separated token of every word counts.
            let tokens: Vec<String> = words
                .iter()
                .flat_map(|w| w.split_whitespace().map(program_name).collect::<Vec<_>>())
                .collect();
            let has = |name: &str| tokens.iter().any(|t| t == name);
            if has("bosn") {
                return deny("bosn");
            }
            if has("docker") || has("podman") {
                return Some(format!(
                    "{role} may not run containers directly; local CI goes through act"
                ));
            }
            if has("act") && !run.ci {
                return Some("local CI (act) is disabled for this grind run".to_string());
            }
            if tokens
                .windows(2)
                .any(|w| w[0] == "worktree" && w[1] == "add")
            {
                return Some(format!("{role} may not create worktrees"));
            }
            None
        }
        // Workers and reviewers hold a shell only to investigate on GitHub:
        // read-only `gh`.
        WORKER | REVIEWER if is_gh_read(words) => None,
        WORKER | REVIEWER if runs_repo_script(words) => Some(format!(
            "{role} may not run the repo's lint or test scripts; the grind-integrator runs \
             lint/test before every push"
        )),
        WORKER | REVIEWER => Some(format!(
            "{role} may only run read-only `gh` (issue/pr/run view|list, search); it cannot build, \
             lint or test"
        )),
        _ => deny(&program),
    }
}

fn policy_name(policy: FeatureMerge) -> &'static str {
    match policy {
        FeatureMerge::Auto => "auto",
        FeatureMerge::DecideLater => "decide later",
        FeatureMerge::CommentOnly => "comment only",
    }
}

/// The PR a `gh pr merge|ready` argument list targets: the first non-flag
/// argument, as a number. Value-taking flags skip their value.
fn pr_target(args: &[String]) -> Option<String> {
    const VALUE_FLAGS: &[&str] = &[
        "-R",
        "--repo",
        "-b",
        "--body",
        "-F",
        "--body-file",
        "-t",
        "--subject",
        "-A",
        "--author-email",
        "--match-head-commit",
    ];
    let mut i = 0;
    while let Some(word) = args.get(i) {
        i += 1;
        if word.starts_with('-') {
            if VALUE_FLAGS.contains(&word.as_str()) {
                i += 1;
            }
            continue;
        }
        return number_word(word);
    }
    None
}

/// Feature-branch-mode rules for every grind role: no `grind/*` branch is
/// deleted while `run.json` records a feature (its PR counts as open), and
/// the feature PR merges only as a merge commit, never with `--admin`, and
/// only under `feature_merge=auto`.
fn feature_reason(role: &str, words: &[String], run: &RunFacts) -> Option<String> {
    run.feature.as_ref()?;
    let program = program_name(&words[0]);
    if program == "git" {
        let sub = git_subcommand(words)?;
        let is_grind = |w: &String| w.trim_start_matches(['+', ':']).starts_with("grind/");
        let deletes = match sub.first().map(String::as_str) {
            Some("branch") => {
                sub[1..].iter().any(|w| {
                    w == "--delete"
                        || (w.starts_with('-') && !w.starts_with("--") && w.contains(['d', 'D']))
                }) && sub[1..].iter().any(is_grind)
            }
            Some("push") => {
                (sub[1..].iter().any(|w| w == "--delete" || w == "-d")
                    && sub[1..].iter().any(is_grind))
                    || sub[1..].iter().any(|w| w.starts_with(":grind/"))
            }
            _ => false,
        };
        return deletes.then(|| {
            format!("{role} may not delete grind/* branches while the feature PR is open")
        });
    }
    if program != "gh"
        || words.get(1).is_none_or(|w| w != "pr")
        || words.get(2).is_none_or(|w| w != "merge")
    {
        return None;
    }
    let feature_pr = run.feature_pr()?;
    let args = &words[3..];
    let Some(target) = pr_target(args) else {
        return Some(format!(
            "{role} must name the PR to `gh pr merge` in feature-branch mode"
        ));
    };
    let has = |names: &[&str]| args.iter().any(|w| names.contains(&w.as_str()));
    // Goal PRs head `grind/*` branches too: none is deleted while the
    // feature PR is open.
    if target != feature_pr {
        return has(&["--delete-branch", "-d"]).then(|| {
            format!("{role} may not delete grind/* branches while the feature PR is open")
        });
    }
    if has(&["--admin"]) {
        return Some(
            "the feature PR may never be merged with --admin; required reviews apply".to_string(),
        );
    }
    if has(&["--delete-branch", "-d"]) {
        return Some(format!(
            "{role} may not delete the feature branch when merging the feature PR"
        ));
    }
    if has(&["--squash", "-s", "--rebase", "-r"]) {
        return Some(
            "the feature PR lands as a merge commit (--merge), not squash or rebase".to_string(),
        );
    }
    (run.feature_merge != FeatureMerge::Auto).then(|| {
        format!(
            "{role} may not merge the feature PR: feature_merge is {}, which leaves the PR \
             for the user",
            policy_name(run.feature_merge)
        )
    })
}

/// The issue number a `gh issue comment` argument list targets (`#` stripped,
/// or the trailing number of an issue URL), or `Err(flag)` for any flag other
/// than `-R/--repo`, `-b/--body` and `-F/--body-file` (so `--edit-last`,
/// `--delete-last`, `--editor` and `--web` are refused).
fn comment_target(args: &[String]) -> Result<Option<String>, String> {
    let mut target = None;
    let mut i = 0;
    while let Some(word) = args.get(i) {
        i += 1;
        if word.starts_with('-') {
            let name = word.split('=').next().unwrap_or(word);
            if !matches!(
                name,
                "-R" | "--repo" | "-b" | "--body" | "-F" | "--body-file"
            ) {
                return Err(name.to_string());
            }
            if !word.contains('=') {
                i += 1;
            }
            continue;
        }
        if target.is_some() {
            return Ok(None);
        }
        let bare = word.trim_matches(&['\'', '"'][..]).trim_end_matches('/');
        let tail = bare.rsplit('/').next().unwrap_or(bare);
        let num = tail.strip_prefix('#').unwrap_or(tail);
        if num.is_empty() || !num.chars().all(|c| c.is_ascii_digit()) {
            return Ok(None);
        }
        target = Some(num.to_string());
    }
    Ok(target)
}

fn program_name(word: &str) -> String {
    let word = word.trim_matches(&['\'', '"'][..]);
    let bare = word.rsplit(['/', '\\']).next().unwrap_or(word);
    bare.strip_suffix(".exe").unwrap_or(bare).to_string()
}

/// Whether a statement runs a repo `lint`/`test` script in any form: `./lint`,
/// `scripts/test.sh`, `bash lint`, `sh test.sh`, `cmd /c lint.bat`,
/// `powershell -File test.ps1`. The bare `test` builtin (`test -f x`) is not
/// a script: only a path-qualified or extension-bearing name, or a name handed
/// to a shell, counts.
fn runs_repo_script(words: &[String]) -> bool {
    const SHELLS: &[&str] = &[
        "bash",
        "sh",
        "zsh",
        "dash",
        "cmd",
        "powershell",
        "pwsh",
        "call",
        "source",
        ".",
    ];
    let tokens: Vec<&str> = words
        .iter()
        .flat_map(|w| w.split_whitespace())
        .map(|t| t.trim_matches(&['\'', '"'][..]))
        .collect();
    // `Some(qualified)` when `t` names a lint/test script; `qualified` is true
    // for a path or a script extension, which the `test` builtin never has.
    let script_name = |t: &str| -> Option<bool> {
        let bare = t.rsplit(['/', '\\']).next().unwrap_or(t);
        let pathed = bare.len() != t.len();
        let (stem, has_ext) = match bare.rsplit_once('.') {
            Some((stem, ext)) if !stem.is_empty() => {
                let ext = ext.to_ascii_lowercase();
                if !matches!(ext.as_str(), "sh" | "bash" | "bat" | "cmd" | "ps1" | "py") {
                    return None;
                }
                (stem, true)
            }
            _ => (bare, false),
        };
        matches!(stem, "lint" | "test").then_some(pathed || has_ext)
    };
    let mut after_shell = false;
    for t in &tokens {
        if let Some(qualified) = script_name(t) {
            if qualified || after_shell {
                return true;
            }
        }
        let program = program_name(t).to_ascii_lowercase();
        if SHELLS.contains(&program.as_str()) {
            after_shell = true;
        } else if after_shell && !t.starts_with(['-', '/']) {
            // A shell's first operand decides; `/c`, `-File`, `-c` do not.
            after_shell = false;
        }
    }
    false
}

/// Whether a statement runs `gh issue create`, looking through wrappers
/// (`bash -c "gh issue create …"`) the way the integrator's denylist does.
fn creates_issue(words: &[String]) -> bool {
    let tokens: Vec<String> = words
        .iter()
        .flat_map(|w| w.split_whitespace().map(program_name).collect::<Vec<_>>())
        .collect();
    tokens
        .windows(3)
        .any(|w| w[0] == "gh" && w[1] == "issue" && w[2] == "create")
}

/// Whether a statement closes an issue: `gh issue close …`, or `gh api` on an
/// `…/issues/<n>` path with a `state=closed` field (`-f`, `-F`, `--field`,
/// `--raw-field`, any method). Looks through wrappers like [`creates_issue`].
/// `clud grind reconcile` is a clud invocation, not `gh`, so it is untouched.
fn closes_issue(words: &[String]) -> bool {
    let tokens: Vec<&str> = words
        .iter()
        .flat_map(|w| w.split_whitespace())
        .map(|t| t.trim_matches(&['\'', '"'][..]))
        .collect();
    let is_gh = |t: &str| program_name(t) == "gh";
    if tokens
        .windows(3)
        .any(|w| is_gh(w[0]) && w[1] == "issue" && w[2] == "close")
    {
        return true;
    }
    let is_issue_path = |t: &str| {
        let t = t.trim_end_matches('/');
        t.split('/').collect::<Vec<_>>().windows(2).any(|p| {
            p[0] == "issues" && !p[1].is_empty() && p[1].chars().all(|c| c.is_ascii_digit())
        })
    };
    let field_closes = |flag: &str, value: Option<&&str>| {
        const FIELD_FLAGS: &[&str] = &["-f", "-F", "--field", "--raw-field"];
        let is_closed = |v: &str| v.trim_matches(&['\'', '"'][..]) == "state=closed";
        if FIELD_FLAGS.contains(&flag) {
            return value.is_some_and(|v| is_closed(v));
        }
        ["--field=", "--raw-field=", "-f", "-F"]
            .iter()
            .any(|p| flag.strip_prefix(p).is_some_and(is_closed))
    };
    tokens.iter().enumerate().any(|(i, t)| {
        if !is_gh(t) || tokens.get(i + 1) != Some(&"api") {
            return false;
        }
        let rest = &tokens[i + 2..];
        rest.iter().any(|t| is_issue_path(t))
            && rest
                .iter()
                .enumerate()
                .any(|(j, t)| field_closes(t, rest.get(j + 1)))
    })
}

fn is_clud(word: &str) -> bool {
    matches!(word, "$CLUD_EXE" | "${CLUD_EXE}" | "\"$CLUD_EXE\"") || program_name(word) == "clud"
}

fn is_gh_read(words: &[String]) -> bool {
    program_name(&words[0]) == "gh"
        && matches!(
            (
                words.get(1).map(String::as_str),
                words.get(2).map(String::as_str)
            ),
            (
                Some("issue" | "pr" | "run"),
                Some("view" | "list" | "diff" | "checks")
            ) | (Some("search"), Some(_))
        )
}

/// A read-only git invocation. `branch` and `remote` only in their listing
/// forms: `git branch -D` and `git remote add` mutate the repository.
fn is_git_read(sub: &[String]) -> bool {
    let Some(first) = sub.first() else {
        return false;
    };
    match first.as_str() {
        "branch" => sub[1..].iter().all(|w| {
            matches!(
                w.as_str(),
                "-a" | "-r" | "-v" | "-vv" | "--list" | "--show-current" | "--all" | "--remotes"
            ) || w.starts_with("--contains")
                || w.starts_with("--merged")
                || w.starts_with("--format")
        }),
        "remote" => sub[1..]
            .first()
            .is_none_or(|w| matches!(w.as_str(), "-v" | "show" | "get-url")),
        other => GIT_READ.contains(&other),
    }
}

/// The words after `git` and its `-C <dir>` options, or `None` when any other
/// global option precedes the subcommand. `-c core.sshCommand=…`,
/// `-c alias.x=!cmd`, `--exec-path` and friends run arbitrary programs under
/// an otherwise read-only subcommand.
fn git_subcommand(words: &[String]) -> Option<Vec<String>> {
    let mut i = 1;
    while let Some(word) = words.get(i) {
        if word == "-C" {
            i += 2;
        } else if word.starts_with('-') {
            return None;
        } else {
            break;
        }
    }
    Some(words.get(i..).unwrap_or_default().to_vec())
}

/// `git push [-u] [origin] [<branch>]`: no force, delete, mirror, or refspec
/// that writes somewhere other than the branch of the same name.
fn is_plain_push(sub: &[String]) -> bool {
    sub.first().is_some_and(|s| s == "push")
        && sub[1..].iter().all(|w| {
            matches!(w.as_str(), "-u" | "--set-upstream")
                || (!w.starts_with('-') && !w.contains(':') && !w.starts_with('+'))
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(parallel: bool, ci: bool) -> RunFacts {
        RunFacts {
            parallel,
            ci,
            ..RunFacts::default()
        }
    }

    #[test]
    fn run_facts_parse_scripts() {
        let facts = RunFacts::from_json(&serde_json::json!({
            "mode": "sequential",
            "scripts": {"lint": "./lint", "test": "bash test"}
        }));
        assert_eq!(facts.scripts.lint.as_deref(), Some("./lint"));
        assert_eq!(facts.scripts.test.as_deref(), Some("bash test"));
        let none = RunFacts::from_json(&serde_json::json!({"mode": "sequential"}));
        assert_eq!(none.scripts, RunScripts::default());
        let invalid = RunFacts::from_json(&serde_json::json!({
            "scripts": {"lint": 5, "test": ""}
        }));
        assert_eq!(invalid.scripts, RunScripts::default());
        let not_object = RunFacts::from_json(&serde_json::json!({"scripts": "lint"}));
        assert_eq!(not_object.scripts, RunScripts::default());
    }

    #[test]
    fn workers_cannot_run_repo_lint_or_test_scripts() {
        let facts = run(true, true);
        for role in [WORKER, REVIEWER] {
            for command in [
                "./lint",
                "./test",
                "bash ./lint",
                "bash lint",
                "bash test",
                "sh test.sh",
                "cmd /c lint.bat",
                "powershell -File test.ps1",
                "pwsh -NoProfile -File ./lint.ps1",
                "bash -c 'bash lint'",
            ] {
                let reason = shell_reason(role, command, &facts)
                    .unwrap_or_else(|| panic!("{role} allowed `{command}`"));
                assert!(
                    reason.contains("integrator") && reason.contains("lint/test"),
                    "{role} `{command}`: {reason}"
                );
            }
        }
        // `./test` is not the `test` builtin, for the inspecting roles either.
        for role in [PLANNER, LANDER] {
            assert!(allowed(role, "test -f Cargo.toml", &facts));
            assert!(!allowed(role, "./test", &facts));
            assert!(!allowed(role, "./lint", &facts));
        }
    }

    #[test]
    fn integrator_runs_repo_lint_and_test_scripts() {
        let facts = run(false, false);
        for command in [
            "./lint",
            "./test",
            "bash ./lint && bash ./test",
            "sh test.sh",
            "cmd /c lint.bat",
            "powershell -File test.ps1",
        ] {
            assert!(allowed(INTEGRATOR, command, &facts), "{command}");
        }
    }

    fn allowed(role: &str, command: &str, facts: &RunFacts) -> bool {
        shell_reason(role, command, facts).is_none()
    }

    /// #1425: a retry loop around the repo's lint/test scripts is refused;
    /// single runs and marker-file wait loops are not.
    #[test]
    fn integrator_may_not_retry_lint_or_test_in_a_loop() {
        let facts = run(false, false);
        for command in [
            "for i in 1 2 3; do bash test; done",
            "for i in 1 2; do bash ./test --integration; done",
            "until bash ./test; do sleep 1; done",
            "while ! ./lint; do true; done",
        ] {
            let reason = shell_reason(INTEGRATOR, command, &facts)
                .unwrap_or_else(|| panic!("integrator allowed `{command}`"));
            assert!(reason.contains("#1425"), "`{command}`: {reason}");
        }
        for command in [
            "bash ./test",
            "bash ./lint && bash ./test --integration",
            "until [ -f test.exit ]; do sleep 5; done",
            "for f in a.rs b.rs; do cat $f; done",
        ] {
            assert!(allowed(INTEGRATOR, command, &facts), "{command}");
        }
    }

    #[test]
    fn only_bundled_roles_are_capped() {
        assert!(is_grind_role("grind-planner"));
        assert!(is_grind_role("grind-lander"));
        assert!(!is_grind_role("general-purpose"));
        assert!(!is_grind_role("grind-other"));
    }

    #[test]
    fn planner_reads_but_does_not_build() {
        let facts = run(false, false);
        assert!(allowed(PLANNER, "git -C /r log --oneline -5", &facts));
        assert!(allowed(PLANNER, "gh issue view 12 && ls src", &facts));
        assert!(!allowed(PLANNER, "cargo build", &facts));
        assert!(!allowed(PLANNER, "git commit -m x", &facts));
        assert!(allowed(PLANNER, "git branch --show-current", &facts));
        assert!(!allowed(PLANNER, "git branch -D main", &facts));
        assert!(allowed(PLANNER, "git remote -v", &facts));
        assert!(!allowed(
            PLANNER,
            "git remote add x https://e.com/x",
            &facts
        ));
        assert!(!allowed(PLANNER, "ls $(cargo metadata)", &facts));
    }

    #[test]
    fn planner_worktrees_only_in_parallel_mode() {
        let add = "git -C /r worktree add /r-wt-1 -b feat/x origin/main";
        assert!(!allowed(PLANNER, add, &run(false, false)));
        assert!(allowed(PLANNER, add, &run(true, false)));
    }

    #[test]
    fn run_facts_parse_plan_phase() {
        let plan = |v: serde_json::Value| RunFacts::from_json(&v).plan_only;
        assert!(plan(
            serde_json::json!({"mode": "parallel", "phase": "plan"})
        ));
        assert!(!plan(serde_json::json!({"mode": "parallel"})));
        assert!(!plan(
            serde_json::json!({"mode": "parallel", "phase": "run"})
        ));
    }

    #[test]
    fn plan_only_planner_cannot_add_worktrees_even_in_parallel() {
        let plan = RunFacts {
            plan_only: true,
            ..run(true, false)
        };
        let add = "git -C /r worktree add /r-wt-1 -b grind/1 origin/main";
        let reason = shell_reason(PLANNER, add, &plan).expect("plan-only add allowed");
        assert!(reason.contains("plan-only"), "{reason}");
        assert!(allowed(PLANNER, add, &run(true, false)));
        assert!(!allowed(PLANNER, "git push origin x", &plan));
        assert!(allowed(PLANNER, "gh issue view 1", &plan));
        assert!(allowed(PLANNER, "git log", &plan));
    }

    #[test]
    fn planner_cannot_write_files() {
        let plan = RunFacts {
            plan_only: true,
            ..run(true, false)
        };
        for tool in ["Write", "Edit", "MultiEdit", "NotebookEdit", "write"] {
            assert!(
                tool_reason(PLANNER, tool, &run(true, false)).is_some(),
                "{tool}"
            );
            let reason = tool_reason(PLANNER, tool, &plan).unwrap();
            assert!(reason.contains("plan-only"), "{reason}");
        }
        assert!(tool_reason(PLANNER, "Read", &plan).is_none());
        assert!(tool_reason(WORKER, "Write", &plan).is_none());
    }

    #[test]
    fn every_grind_role_is_denied_ask_user_question() {
        let plan = RunFacts {
            plan_only: true,
            ..run(true, false)
        };
        for facts in [run(false, false), plan.clone()] {
            for role in [PLANNER, WORKER, REVIEWER, INTEGRATOR, LANDER] {
                let reason = tool_reason(role, "AskUserQuestion", &facts)
                    .unwrap_or_else(|| panic!("{role} allowed AskUserQuestion"));
                assert!(reason.contains("up front"), "{role}: {reason}");
            }
            assert!(tool_reason(WORKER, "Read", &facts).is_none());
            assert!(tool_reason(PLANNER, "Write", &facts).is_some());
        }
    }

    #[test]
    fn workers_and_reviewers_only_investigate_with_gh() {
        for role in [WORKER, REVIEWER] {
            assert!(allowed(
                role,
                "gh issue view 12 --comments",
                &run(true, true)
            ));
            assert!(allowed(
                role,
                "gh search code foo --repo a/b",
                &run(true, true)
            ));
            assert!(!allowed(role, "gh pr merge 5", &run(true, true)));
            assert!(!allowed(role, "cargo test", &run(true, true)));
            assert!(!allowed(role, "ls", &run(true, true)));
        }
    }

    #[test]
    fn integrator_builds_but_skips_bosn_containers_and_worktrees() {
        let facts = run(true, false);
        assert!(allowed(INTEGRATOR, "bash lint && bash test", &facts));
        assert!(allowed(INTEGRATOR, "git push -u origin feat/x", &facts));
        assert!(!allowed(INTEGRATOR, "bosn init", &facts));
        assert!(!allowed(INTEGRATOR, "docker run --rm img", &facts));
        assert!(!allowed(INTEGRATOR, "git worktree add /x", &facts));
    }

    #[test]
    fn integrator_act_follows_the_ci_answer() {
        let act = "act -W .github/workflows/ci.yml -j test --pull=false";
        assert!(!allowed(INTEGRATOR, act, &run(true, false)));
        assert!(allowed(INTEGRATOR, act, &run(true, true)));
    }

    #[test]
    fn lander_watches_and_merges_only() {
        let facts = run(true, false);
        assert!(allowed(
            LANDER,
            "\"$CLUD_EXE\" tool run github/pr_merge_watch.py 5",
            &facts
        ));
        assert!(allowed(LANDER, "gh pr merge 5 --admin --squash", &facts));
        assert!(allowed(LANDER, "gh run view 99 --log-failed", &facts));
        assert!(!allowed(LANDER, "cargo test", &facts));
        assert!(!allowed(LANDER, "git commit -am fix", &facts));
    }

    /// #1409: during the bug stage `run.json` has no `feature` yet (the router
    /// adds it when the feature stage starts), so the lander merges each bug
    /// PR into `<main>`, whatever the feature merge policy will be.
    #[test]
    fn bug_stage_lander_may_merge_into_main() {
        for policy in ["auto", "later", "comment"] {
            let bugs = RunFacts::from_json(&serde_json::json!({
                "mode": "parallel", "meta": 100, "feature_merge": policy,
                "tracks": {"101": "bug", "102": "feature"}
            }));
            assert!(bugs.feature.is_none(), "{policy}");
            for command in [
                "gh pr merge 101 --merge",
                "gh pr merge 101 --squash --admin",
                "gh pr merge https://github.com/o/r/pull/101 --merge",
            ] {
                assert!(allowed(LANDER, command, &bugs), "{policy}: {command}");
            }
        }
        // Once the feature stage starts, a goal PR still merges (into the
        // feature branch), but the feature PR follows the policy.
        let feature = RunFacts::from_json(&serde_json::json!({
            "mode": "parallel", "meta": 100, "feature_merge": "later",
            "feature": {"branch": "grind/meta-100-1f3a", "pr": 7}
        }));
        assert!(allowed(LANDER, "gh pr merge 102 --merge", &feature));
        assert!(!allowed(LANDER, "gh pr merge 7 --merge", &feature));
    }

    #[test]
    fn no_grind_subagent_can_create_issues() {
        for facts in [run(false, false), run(true, false)] {
            for role in [PLANNER, WORKER, REVIEWER, INTEGRATOR, LANDER, PREWORK] {
                for command in [
                    "gh issue create --title x --body y",
                    "gh issue create --label grind:followup --title x --body y",
                ] {
                    let reason = shell_reason(role, command, &facts)
                        .unwrap_or_else(|| panic!("{role} allowed `{command}`"));
                    assert!(!allowed(role, command, &facts));
                    assert!(
                        reason.contains("the /grind router files problems"),
                        "{role} `{command}`: {reason}"
                    );
                }
            }
        }
        assert!(!allowed(
            INTEGRATOR,
            "bash -c 'gh issue create --title x --body y'",
            &run(true, false)
        ));
    }

    #[test]
    fn no_grind_role_closes_issues() {
        let facts = run(true, false);
        let closes = |role: &str, command: &str| {
            shell_reason(role, command, &facts)
                .is_some_and(|r| r.contains("never close issues") && r.contains("#1393"))
        };
        for role in [INTEGRATOR, LANDER, WORKER, PLANNER, REVIEWER, PREWORK] {
            for command in [
                "gh issue close 12",
                "gh issue close 12 --comment done",
                "gh api repos/o/r/issues/12 -X PATCH -f state=closed",
                "gh api --method PATCH repos/o/r/issues/12 -F state=closed",
                "gh api repos/o/r/issues/12 --field state=closed",
                "gh api repos/o/r/issues/12 --raw-field=state=closed",
                "bash -c 'gh issue close 12'",
            ] {
                assert!(closes(role, command), "{role} `{command}`");
            }
        }
        // Reopening and commenting are not closing.
        for command in [
            "gh issue reopen 12",
            "gh issue comment 12 -b x",
            "gh api repos/o/r/issues/12 -X PATCH -f state=open",
            "clud grind reconcile",
        ] {
            assert!(!closes(LANDER, command), "{command}");
        }
        assert!(allowed(INTEGRATOR, "gh issue reopen 12", &facts));
        assert!(allowed(INTEGRATOR, "gh issue comment 12 -b x", &facts));
        assert!(allowed(INTEGRATOR, "clud grind reconcile", &facts));
        // Only grind roles are capped at all.
        assert!(!is_grind_role("general-purpose"));
    }

    #[test]
    fn inspection_cannot_run_delete_or_hang() {
        let facts = run(true, true);
        for role in [PLANNER, LANDER] {
            assert!(allowed(role, "find src -name '*.rs'", &facts));
            assert!(!allowed(role, "find . -exec cargo build ;", &facts));
            assert!(!allowed(role, "find . -delete", &facts));
            assert!(allowed(role, "tail -n 60 log.txt", &facts));
            assert!(!allowed(role, "tail -f log.txt", &facts));
            assert!(!allowed(role, "tail -nF 5 log.txt", &facts));
        }
    }

    #[test]
    fn git_global_options_other_than_dir_are_refused() {
        let facts = run(true, false);
        assert!(!allowed(PLANNER, "git -c core.sshCommand=sh fetch", &facts));
        assert!(!allowed(PLANNER, "git -c alias.x=!cargo x", &facts));
        assert!(!allowed(PLANNER, "git --exec-path=/tmp log", &facts));
        assert!(!allowed(LANDER, "git -c core.pager=sh log", &facts));
        assert!(allowed(LANDER, "git -C /r status", &facts));
    }

    #[test]
    fn lander_pushes_and_edits_prs_narrowly() {
        let facts = run(true, false);
        assert!(allowed(LANDER, "git push -u origin grind/5-x", &facts));
        assert!(!allowed(
            LANDER,
            "git push --force origin grind/5-x",
            &facts
        ));
        assert!(!allowed(LANDER, "git push origin --delete main", &facts));
        assert!(!allowed(LANDER, "git push origin HEAD:main", &facts));
        assert!(!allowed(LANDER, "git push origin +grind/5-x", &facts));
        assert!(allowed(LANDER, "gh pr checks 5", &facts));
        assert!(!allowed(LANDER, "gh pr close 5", &facts));
        assert!(!allowed(LANDER, "gh pr review 5 --approve", &facts));
    }

    #[test]
    fn integrator_bans_see_through_wrappers() {
        let facts = run(true, false);
        assert!(!allowed(INTEGRATOR, "bash -c 'docker run img'", &facts));
        assert!(!allowed(INTEGRATOR, "env act -j test", &facts));
        assert!(!allowed(INTEGRATOR, "sh -c bosn", &facts));
        assert!(!allowed(
            INTEGRATOR,
            "bash -c 'git worktree add /x'",
            &facts
        ));
        assert!(allowed(INTEGRATOR, "bash -c 'bash lint'", &facts));
    }

    #[test]
    fn linked_worktrees_resolve_to_their_main_checkout() {
        assert_eq!(
            main_checkout_from_gitdir("/home/u/repo/.git/worktrees/wt-5"),
            Some(std::path::PathBuf::from("/home/u/repo"))
        );
        assert_eq!(main_checkout_from_gitdir("/home/u/repo/.git"), None);
    }

    #[test]
    fn run_facts_are_found_from_a_linked_worktree() {
        let dir = tempfile::tempdir().unwrap();
        let main = dir.path().join("repo");
        let wt = dir.path().join("repo-wt-5");
        std::fs::create_dir_all(main.join(".clud/grind")).unwrap();
        std::fs::create_dir_all(main.join(".git/worktrees/repo-wt-5")).unwrap();
        std::fs::create_dir_all(wt.join("src")).unwrap();
        std::fs::write(
            main.join(".clud/grind/run.json"),
            r#"{"mode":"parallel","ci":true}"#,
        )
        .unwrap();
        std::fs::write(
            wt.join(".git"),
            format!(
                "gitdir: {}\n",
                main.join(".git/worktrees/repo-wt-5").display()
            ),
        )
        .unwrap();
        assert_eq!(RunFacts::discover(&wt.join("src")), run(true, true));
    }

    #[test]
    fn agent_calls_for_grind_roles_are_refused_unless_opted_in() {
        let call = |ty: &str| serde_json::json!({"subagent_type": ty, "prompt": "x"});
        let worker = call("grind-worker");
        assert!(agent_spawn_reason("Agent", Some(&worker), None).is_some());
        assert!(agent_spawn_reason("Agent", Some(&worker), Some("0")).is_some());
        assert!(agent_spawn_reason("Agent", Some(&worker), Some("1")).is_none());
        let explore = call("Explore");
        assert!(agent_spawn_reason("Agent", Some(&explore), None).is_none());
        // Only the Agent tool: a Bash call that mentions a role is not a spawn.
        assert!(agent_spawn_reason("Bash", Some(&worker), None).is_none());
        let reason = agent_spawn_reason("Agent", Some(&call("grind-lander")), None).unwrap();
        assert!(reason.contains("grind-run") && reason.contains(ALLOW_GRIND_AGENTS_ENV));
    }

    #[test]
    fn prework_is_a_capped_role() {
        assert!(is_grind_role("grind-prework"));
        assert!(is_grind_role(PREWORK));
    }

    #[test]
    fn prework_comments_only_on_the_meta_issue() {
        let facts = RunFacts {
            meta: Some("100".to_string()),
            ..run(false, false)
        };
        for command in [
            "gh issue comment 100 --body x",
            "gh issue comment #100 -R o/r --body-file p.md",
            "gh issue comment https://github.com/o/r/issues/100 -b x",
            "gh issue view 7",
            "git log -1",
        ] {
            assert!(allowed(PREWORK, command, &facts), "{command}");
        }
        let reason = shell_reason(PREWORK, "gh issue comment 101 --body x", &facts).unwrap();
        assert!(reason.contains("only on the meta issue"), "{reason}");
        for command in [
            "gh issue comment 100 --edit-last --body x",
            "gh issue create --title t",
            "gh issue edit 100 --title t",
            "gh api -X POST repos/o/r/issues/100/comments",
            "git worktree add ../x",
            "git commit -m x",
            "git push origin x",
            "cargo build",
        ] {
            assert!(!allowed(PREWORK, command, &facts), "{command}");
        }
        let reason =
            shell_reason(PREWORK, "gh issue comment 100 --body x", &run(false, false)).unwrap();
        assert!(reason.contains("only on the meta issue"), "{reason}");
        let parallel = RunFacts {
            meta: Some("100".to_string()),
            ..run(true, false)
        };
        assert!(!allowed(PREWORK, "git worktree add ../x", &parallel));
    }

    #[test]
    fn prework_may_not_write_files() {
        let facts = run(false, false);
        assert!(tool_reason(PREWORK, "Write", &facts).is_some());
        assert!(tool_reason(PREWORK, "Edit", &facts).is_some());
        assert!(tool_reason(PREWORK, "Read", &facts).is_none());
        assert!(tool_reason(PREWORK, "AskUserQuestion", &facts).is_some());
    }

    #[test]
    fn run_facts_parse_meta() {
        let meta = |v: serde_json::Value| RunFacts::from_json(&v).meta;
        assert_eq!(
            meta(serde_json::json!({"meta": 100})).as_deref(),
            Some("100")
        );
        assert_eq!(
            meta(serde_json::json!({"meta": "#100"})).as_deref(),
            Some("100")
        );
        assert_eq!(meta(serde_json::json!({"meta": ""})), None);
        assert_eq!(meta(serde_json::json!({"mode": "sequential"})), None);
    }

    fn feature_run(policy: FeatureMerge) -> RunFacts {
        RunFacts {
            feature: Some(FeatureFacts {
                branch: "grind/meta-100-r1".to_string(),
                pr: Some("9".to_string()),
                worktree: Some(".clud/grind/worktrees/feature".to_string()),
            }),
            feature_merge: policy,
            ..run(true, false)
        }
    }

    const POLICIES: [FeatureMerge; 3] = [
        FeatureMerge::Auto,
        FeatureMerge::DecideLater,
        FeatureMerge::CommentOnly,
    ];

    #[test]
    fn run_facts_parse_feature_and_policy() {
        let pr = |p: serde_json::Value| {
            RunFacts::from_json(&serde_json::json!({
                "feature": {"branch": "grind/meta-1-x", "pr": p, "worktree": "wt"}
            }))
            .feature
            .unwrap()
            .pr
        };
        assert_eq!(pr(serde_json::json!(123)).as_deref(), Some("123"));
        assert_eq!(pr(serde_json::json!("#123")).as_deref(), Some("123"));
        assert_eq!(
            pr(serde_json::json!("https://github.com/o/r/pull/123")).as_deref(),
            Some("123")
        );
        let facts = RunFacts::from_json(&serde_json::json!({
            "feature": {"branch": "grind/meta-1-x", "pr": 5, "worktree": "wt"}
        }));
        let feature = facts.feature.unwrap();
        assert_eq!(feature.branch, "grind/meta-1-x");
        assert_eq!(feature.worktree.as_deref(), Some("wt"));
        assert_eq!(facts.feature_merge, FeatureMerge::DecideLater);
        let none = RunFacts::from_json(&serde_json::json!({"mode": "sequential"}));
        assert_eq!(none.feature, None);
        assert_eq!(none.feature_merge, FeatureMerge::DecideLater);
        let policy = |p: &str| RunFacts::from_json(&serde_json::json!({"feature_merge": p}));
        assert_eq!(policy("auto").feature_merge, FeatureMerge::Auto);
        assert_eq!(policy("later").feature_merge, FeatureMerge::DecideLater);
        assert_eq!(
            policy("decide_later").feature_merge,
            FeatureMerge::DecideLater
        );
        assert_eq!(policy("comment").feature_merge, FeatureMerge::CommentOnly);
        assert_eq!(
            policy("comment_only").feature_merge,
            FeatureMerge::CommentOnly
        );
    }

    #[test]
    fn lander_merges_feature_pr_only_under_auto_with_merge_commit() {
        let auto = feature_run(FeatureMerge::Auto);
        assert!(allowed(LANDER, "gh pr merge 9 --merge", &auto));
        assert!(allowed(
            LANDER,
            "gh pr merge https://github.com/o/r/pull/9 --merge",
            &auto
        ));
        assert!(!allowed(LANDER, "gh pr merge 9 --merge --admin", &auto));
        assert!(!allowed(LANDER, "gh pr merge 9 --squash", &auto));
        assert!(!allowed(LANDER, "gh pr merge 9 --rebase", &auto));
        for policy in [FeatureMerge::DecideLater, FeatureMerge::CommentOnly] {
            let reason = shell_reason(LANDER, "gh pr merge 9 --merge", &feature_run(policy))
                .expect("non-auto feature merge allowed");
            assert!(reason.contains(policy_name(policy)), "{reason}");
        }
        for policy in POLICIES {
            assert!(allowed(
                LANDER,
                "gh pr merge 5 --merge",
                &feature_run(policy)
            ));
            assert!(allowed(
                LANDER,
                "gh pr merge 5 --admin --squash",
                &feature_run(policy)
            ));
        }
    }

    #[test]
    fn feature_pr_admin_is_always_denied() {
        for policy in POLICIES {
            let reason = shell_reason(
                LANDER,
                "gh pr merge #9 --admin --merge",
                &feature_run(policy),
            )
            .expect("admin feature merge allowed");
            assert!(reason.contains("--admin"), "{reason}");
        }
    }

    #[test]
    fn lander_readies_feature_pr_only_under_auto() {
        assert!(allowed(
            LANDER,
            "gh pr ready 9",
            &feature_run(FeatureMerge::Auto)
        ));
        assert!(!allowed(
            LANDER,
            "gh pr ready 5",
            &feature_run(FeatureMerge::Auto)
        ));
        for policy in [FeatureMerge::DecideLater, FeatureMerge::CommentOnly] {
            assert!(!allowed(LANDER, "gh pr ready 9", &feature_run(policy)));
        }
        assert!(!allowed(LANDER, "gh pr ready 9", &run(true, false)));
    }

    #[test]
    fn grind_branches_are_not_deleted_while_feature_pr_open() {
        let facts = feature_run(FeatureMerge::Auto);
        for role in [LANDER, INTEGRATOR] {
            for command in [
                "git branch -d grind/5-x",
                "git branch -D grind/meta-100-r1",
                "git push origin --delete grind/5-x",
                "git push origin :grind/5-x",
                "gh pr merge 9 --merge --delete-branch",
                "gh pr merge 9 --merge -d",
                "gh pr merge 5 --admin --merge --delete-branch",
            ] {
                assert!(!allowed(role, command, &facts), "{role} `{command}`");
            }
        }
        assert!(allowed(
            INTEGRATOR,
            "git branch -D grind/5-x",
            &run(true, false)
        ));
        assert!(allowed(INTEGRATOR, "git branch -D feat/x", &facts));
    }

    #[test]
    fn run_facts_parse_mode_and_ci() {
        let facts = RunFacts::from_json(&serde_json::json!({"mode": "parallel", "ci": true}));
        assert_eq!(facts, run(true, true));
        assert_eq!(
            RunFacts::from_json(&serde_json::json!({"mode": "sequential"})),
            run(false, false)
        );
    }
}
