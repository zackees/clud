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
//! integrator's `act`).

use std::path::Path;

use serde_json::Value;

use super::block_bad_cmd_gate::statement_words;

pub(super) const PLANNER: &str = "grind-planner";
pub(super) const WORKER: &str = "grind-worker";
pub(super) const REVIEWER: &str = "grind-reviewer";
pub(super) const INTEGRATOR: &str = "grind-integrator";
pub(super) const LANDER: &str = "grind-lander";

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
fn inspect_ok(program: &str, words: &[String]) -> bool {
    if !INSPECT.contains(&program) {
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
}

impl RunFacts {
    pub(super) fn from_json(value: &Value) -> Self {
        Self {
            parallel: value.get("mode").and_then(Value::as_str) == Some("parallel"),
            ci: value.get("ci").and_then(Value::as_bool).unwrap_or(false),
        }
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

/// Whether `agent_type` is a capped `/grind` role.
pub(super) fn is_grind_role(agent_type: &str) -> bool {
    matches!(
        agent_type,
        PLANNER | WORKER | REVIEWER | INTEGRATOR | LANDER
    )
}

/// The denial reason for a shell `command` from `role`, or `None` to allow.
pub(super) fn shell_reason(role: &str, command: &str, run: &RunFacts) -> Option<String> {
    let statements = match statement_words(command) {
        Ok(statements) => statements,
        Err(construct) => return Some(format!("{role} may not use {construct}")),
    };
    statements
        .iter()
        .find_map(|words| statement_reason(role, words, run))
}

fn statement_reason(role: &str, words: &[String], run: &RunFacts) -> Option<String> {
    let program = program_name(&words[0]);
    let deny = |what: &str| Some(format!("{role} may not run `{what}`"));
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
                    [a, b, ..] if a == "worktree" && b == "add" && run.parallel => None,
                    [a, b, ..] if a == "worktree" && b == "add" => {
                        Some(format!("{role} may create worktrees only in parallel mode"))
                    }
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
        WORKER | REVIEWER => Some(format!(
            "{role} may only run read-only `gh` (issue/pr/run view|list, search); it cannot build, \
             lint or test"
        )),
        _ => deny(&program),
    }
}

fn program_name(word: &str) -> String {
    let word = word.trim_matches(&['\'', '"'][..]);
    let bare = word.rsplit(['/', '\\']).next().unwrap_or(word);
    bare.strip_suffix(".exe").unwrap_or(bare).to_string()
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
        RunFacts { parallel, ci }
    }

    fn allowed(role: &str, command: &str, facts: &RunFacts) -> bool {
        shell_reason(role, command, facts).is_none()
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
            "\"$CLUD_EXE\" tool run github/pr_merge_watch.py --pr 5",
            &facts
        ));
        assert!(allowed(LANDER, "gh pr merge 5 --admin --squash", &facts));
        assert!(allowed(LANDER, "gh run view 99 --log-failed", &facts));
        assert!(!allowed(LANDER, "cargo test", &facts));
        assert!(!allowed(LANDER, "git commit -am fix", &facts));
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
    fn run_facts_parse_mode_and_ci() {
        let facts = RunFacts::from_json(&serde_json::json!({"mode": "parallel", "ci": true}));
        assert_eq!(facts, run(true, true));
        assert_eq!(
            RunFacts::from_json(&serde_json::json!({"mode": "sequential"})),
            run(false, false)
        );
    }
}
