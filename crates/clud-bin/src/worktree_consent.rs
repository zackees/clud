//! Git worktrees need the user's consent (#1984).
//!
//! Agents made hundreds of worktrees a week, each a cold Rust `target/`,
//! often right after the user said not to: a subagent never sees the user's
//! prompt. So creation is refused unless this session opted in, and the
//! opt-in comes from places an agent cannot write:
//!
//! - the user's own prompt: the `UserPromptSubmit` hook ([`handle_prompt`])
//!   sets the daemon's `worktrees` flag for the payload's `session_id`
//!   ([`crate::daemon::session_flag_set`]);
//! - the launch environment: `CLUD_ALLOW_WORKTREES=1`, inherited by every
//!   hook from clud's environment. An agent writing the same assignment in a
//!   Bash command changes only that command's environment, not the hook's;
//! - `clud --worktree`, which clud turns into that variable at launch;
//! - an active `/grind` run, whose own caps govern its worktrees.
//!
//! [`refusal`] is asked by the PreToolUse scanner (Bash `git worktree add`,
//! `EnterWorktree`, `Agent` with `isolation: "worktree"`) and by the
//! `WorktreeCreate` handler, which every Claude Code worktree goes through.
//! A session the daemon cannot answer for is refused: this control fails
//! closed. It is not overridable through `CLUD_BAD_CMD_OVERRIDE`.

use serde_json::Value;

/// Launch-environment opt-in for the whole session.
pub const ALLOW_ENV: &str = "CLUD_ALLOW_WORKTREES";

/// The daemon session-flag key.
pub const FLAG_KEY: &str = "worktrees";

/// The harness event that carries the user's prompt.
pub const USER_PROMPT_SUBMIT_EVENT: &str = "UserPromptSubmit";

const NEGATIONS: &[&str] = &[
    "no", "not", "never", "without", "avoid", "dont", "don't", "do not", "stop", "nor",
];
/// Verbs that grant, not merely mention: "can you remove the worktrees" or
/// "deal with the stale worktrees" must not count.
const ALLOW_BEFORE: &[&str] = &["use", "using", "allow", "allowed", "create", "enable"];
const ALLOW_AFTER: &[&str] = &["ok", "okay", "allowed", "fine", "enabled"];

/// Whether `prompt` says yes to worktrees. A mention is not a yes: "no
/// worktrees", "don't use the git work tree" and "without a worktree" all
/// say no, and "branch" says nothing about worktrees.
#[must_use]
pub fn says_yes(prompt: &str) -> bool {
    let lower = prompt.to_lowercase();
    lower.split(['.', '!', '?', ';', '\n']).any(clause_says_yes)
}

fn clause_says_yes(clause: &str) -> bool {
    let words: Vec<String> = clause
        .replace("work-tree", "worktree")
        .replace("work tree", "worktree")
        .split(|c: char| !(c.is_alphanumeric() || c == '\''))
        .filter(|word| !word.is_empty())
        .map(str::to_string)
        .collect();
    words.iter().enumerate().any(|(index, word)| {
        if !word.starts_with("worktree") {
            return false;
        }
        let before = &words[index.saturating_sub(6)..index];
        let after = &words[index + 1..words.len().min(index + 3)];
        let negated = before.iter().any(|w| is_negation(w))
            || before
                .windows(2)
                .any(|pair| pair[0] == "do" && pair[1] == "not");
        !negated
            && (before.iter().any(|w| ALLOW_BEFORE.contains(&w.as_str()))
                || after.iter().any(|w| ALLOW_AFTER.contains(&w.as_str())))
    })
}

fn is_negation(word: &str) -> bool {
    NEGATIONS.contains(&word) || word.ends_with("n't")
}

/// Whether a non-shell tool call creates a worktree: `EnterWorktree`, or an
/// `Agent`/`Task` call with `isolation: "worktree"`.
#[must_use]
pub fn tool_creates_worktree(tool_name: &str, tool_input: Option<&Value>) -> bool {
    match tool_name {
        "EnterWorktree" => true,
        "Agent" | "Task" => {
            tool_input
                .and_then(|input| input.get("isolation"))
                .and_then(Value::as_str)
                == Some("worktree")
        }
        _ => false,
    }
}

/// Handle `UserPromptSubmit`: record consent when the prompt says yes.
/// Always exits 0 and prints nothing; stdout here would be added to the
/// conversation, and exit 2 would erase the user's prompt.
#[must_use]
pub fn handle_prompt(stdin: &str) -> i32 {
    let Ok(payload) = serde_json::from_str::<Value>(stdin) else {
        return 0;
    };
    let prompt = payload.get("prompt").and_then(Value::as_str).unwrap_or("");
    let session = payload.get("session_id").and_then(Value::as_str);
    let Some(session) = session.filter(|id| crate::grind_facts::valid_session_id(id)) else {
        return 0;
    };
    if !says_yes(prompt) {
        return 0;
    }
    let outcome = crate::daemon::default_state_dir().and_then(|dir| {
        crate::daemon::session_flag_set(&dir, session, FLAG_KEY, Value::Bool(true))
    });
    if let Err(error) = outcome {
        eprintln!("[clud worktrees] could not record consent: {error}");
    }
    0
}

/// The refusal for creating a worktree in `session_id`, or `None` to allow.
#[must_use]
pub fn refusal(session_id: Option<&str>) -> Option<String> {
    if crate::runner::unsafe_mode_enabled() {
        return None;
    }
    let env_allows = std::env::var(ALLOW_ENV).ok().as_deref() == Some("1");
    let grind_run = matches!(
        crate::grind_facts::lookup(session_id),
        crate::grind_facts::Lookup::Found(_)
    );
    decide(env_allows || grind_run, session_id, |session| {
        let dir = crate::daemon::default_state_dir()?;
        crate::daemon::session_flag_get(&dir, session, FLAG_KEY)
    })
}

fn decide(
    allowed_without_asking: bool,
    session_id: Option<&str>,
    lookup: impl FnOnce(&str) -> std::io::Result<Option<Value>>,
) -> Option<String> {
    if allowed_without_asking {
        return None;
    }
    let Some(session) = session_id.filter(|id| !id.is_empty()) else {
        return Some(denial("the hook payload names no session"));
    };
    match lookup(session) {
        Ok(Some(Value::Bool(true))) => None,
        Ok(_) => Some(denial("the user has not allowed them in this session")),
        Err(error) => Some(denial(&format!(
            "the clud daemon could not be asked ({error})"
        ))),
    }
}

fn denial(why: &str) -> String {
    format!(
        "Git worktrees are off for this session: {why}. Work in this checkout; a feature \
         branch (`git switch -c <branch>`) is fine. If you need a worktree, ask the user: \
         they allow it by saying so in a prompt (\"use worktrees\") or by launching with \
         {ALLOW_ENV}=1."
    )
}

/// `clud --worktree` / `-w` asks Claude Code for a worktree at startup,
/// before any prompt exists. That flag is the user's consent.
#[must_use]
pub fn launch_args_ask_for_worktree(passthrough: &[String]) -> bool {
    passthrough
        .iter()
        .any(|arg| arg == "-w" || arg == "--worktree" || arg.starts_with("--worktree="))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn prompts_that_say_yes() {
        for prompt in [
            "use worktrees for this",
            "you can use a git worktree",
            "You can use git work trees here.",
            "worktrees are ok",
            "worktrees ok",
            "allow worktrees",
            "Create a git worktree for issue 1234",
            "go ahead and use a work-tree",
        ] {
            assert!(says_yes(prompt), "{prompt:?}");
        }
    }

    #[test]
    fn prompts_that_say_no_or_nothing() {
        for prompt in [
            "Just do one agent - no worktrees, this should be very simple",
            "No Git work trees. Just do them one at a time sequentially",
            "Don't use the get work tree. Just do the one at a time in this repo",
            "do it without a worktree",
            "do not use worktrees",
            "never create a worktree",
            "find stale worktrees that we can purge",
            "clean up expired work trees",
            "put it on a branch and open a PR",
            "can you remove these worktrees",
            "make sure the worktrees are cleaned up",
            "deal with the stale worktrees",
            "stop using worktrees",
            "we can't use worktrees here",
            "",
        ] {
            assert!(!says_yes(prompt), "{prompt:?}");
        }
    }

    #[test]
    fn a_no_in_one_sentence_does_not_cancel_a_yes_in_another() {
        assert!(says_yes("No rush. You can use worktrees."));
        assert!(!says_yes("You can use branches. No worktrees."));
    }

    #[test]
    fn worktree_creating_tools() {
        assert!(tool_creates_worktree("EnterWorktree", None));
        assert!(tool_creates_worktree(
            "Agent",
            Some(&json!({"isolation": "worktree", "prompt": "x"}))
        ));
        assert!(!tool_creates_worktree(
            "Agent",
            Some(&json!({"prompt": "x"}))
        ));
        assert!(!tool_creates_worktree(
            "Bash",
            Some(&json!({"command": "ls"}))
        ));
    }

    #[test]
    fn consent_comes_from_the_daemon_flag() {
        let yes = |_: &str| Ok(Some(json!(true)));
        let unset = |_: &str| Ok(None);
        assert_eq!(decide(false, Some("s"), yes), None);
        assert!(decide(false, Some("s"), unset)
            .unwrap()
            .contains("has not allowed"));
    }

    #[test]
    fn an_unanswerable_session_is_refused() {
        let down = |_: &str| Err(std::io::Error::other("connection refused"));
        assert!(decide(false, Some("s"), down).unwrap().contains("daemon"));
        assert!(decide(false, None, |_| Ok(Some(json!(true))))
            .unwrap()
            .contains("no session"));
    }

    #[test]
    fn the_environment_or_a_grind_run_skips_the_lookup() {
        let never = |_: &str| -> std::io::Result<Option<Value>> { panic!("asked the daemon") };
        assert_eq!(decide(true, None, never), None);
    }

    #[test]
    fn the_launch_flag_is_consent() {
        let args = |list: &[&str]| list.iter().map(|s| (*s).to_string()).collect::<Vec<_>>();
        assert!(launch_args_ask_for_worktree(&args(&["--worktree"])));
        assert!(launch_args_ask_for_worktree(&args(&["-w", "name"])));
        assert!(launch_args_ask_for_worktree(&args(&["--worktree=feat"])));
        assert!(!launch_args_ask_for_worktree(&args(&["--model", "x"])));
    }
}
