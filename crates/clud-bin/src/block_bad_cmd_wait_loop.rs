//! zackees/clud#1880: refuse hand-written waits in shell commands.
//!
//! Every hand-written wait invents a completion check on a stand-in (a process
//! list, a word in output, a status string) and nothing forces it to give up.
//! The harness's background-task notification tracks the real process exit,
//! so it is the only general way to wait. See
//! DD-168 in `docs/DESIGN_DECISIONS.md` for the `sleep` threshold.

use super::*;

/// Rule id for `CLUD_BAD_CMD_OVERRIDE=hand-written-wait:<reason>`.
pub(super) const WAIT_LOOP_RULE_ID: &str = "hand-written-wait";

/// The replacement every refusal names verbatim.
pub(super) const WAIT_REPLACEMENT: &str = "Never poll. To wait, run the command with `run_in_background` and wait for its completion notification, or use a waiting tool with its own deadline (`clud ci`, `bosn ci run --wait`, `pr_merge_watch`).";

/// Longest `sleep` allowed outside a loop and not chained to a status poll.
const MAX_STANDALONE_SLEEP_SECS: f64 = 60.0;

#[derive(Clone, Copy, PartialEq, Eq)]
enum LoopKind {
    /// `while`, `until`, `for ((;;))`, PowerShell `do`/`for (;;)`.
    Unbounded,
    /// A `for`/`foreach` over a list or a counted range.
    Bounded,
}

/// Words of one segment with leading shell control keywords removed, so the
/// body of `do sleep 5` reads as `sleep 5`.
fn body_words(segment: &str, dialect: ShellDialect) -> Vec<String> {
    let mut words = command_words(segment);
    loop {
        let Some(first) = words.first_mut() else {
            return words;
        };
        let trimmed = first.trim_start_matches(['(', '{']).to_string();
        if trimmed.is_empty() {
            words.remove(0);
            continue;
        }
        if trimmed.len() != first.len() {
            *first = trimmed;
        }
        let keyword = first.to_ascii_lowercase();
        let control = match dialect {
            ShellDialect::Posix => {
                matches!(
                    keyword.as_str(),
                    "do" | "then" | "else" | "elif" | "!" | "time"
                )
            }
            ShellDialect::PowerShell | ShellDialect::Cmd => false,
        };
        if !control {
            return words;
        }
        words.remove(0);
    }
}

fn loop_kind(segment: &str, words: &[String], dialect: ShellDialect) -> Option<LoopKind> {
    let head = program_name(words.first()?);
    let keyword = head.split_once('(').map_or(head.as_str(), |(name, _)| name);
    let compact: String = segment.chars().filter(|ch| !ch.is_whitespace()).collect();
    match dialect {
        ShellDialect::Posix => match keyword {
            "while" | "until" | "select" => Some(LoopKind::Unbounded),
            "for" if compact.starts_with("for((;;))") => Some(LoopKind::Unbounded),
            "for" => Some(LoopKind::Bounded),
            _ => None,
        },
        ShellDialect::PowerShell => match keyword {
            "while" | "do" => Some(LoopKind::Unbounded),
            "for" if compact.to_ascii_lowercase().starts_with("for(;;)") => {
                Some(LoopKind::Unbounded)
            }
            "for" | "foreach" => Some(LoopKind::Bounded),
            _ => None,
        },
        ShellDialect::Cmd => None,
    }
}

/// `Some(seconds)` for a `sleep`/`Start-Sleep` segment; the inner `None`
/// means the duration is not a literal (a variable, `infinity` counts as
/// infinite).
fn sleep_seconds(words: &[String]) -> Option<Option<f64>> {
    let head = program_name(words.first()?);
    match head.as_str() {
        "sleep" => Some(posix_sleep_seconds(&words[1..])),
        "start-sleep" => Some(powershell_sleep_seconds(&words[1..])),
        _ => None,
    }
}

fn posix_sleep_seconds(arguments: &[String]) -> Option<f64> {
    if arguments.is_empty() {
        return None;
    }
    let mut total = 0.0;
    for argument in arguments {
        if argument.eq_ignore_ascii_case("infinity") {
            return Some(f64::INFINITY);
        }
        let (number, scale) = match argument.chars().last()? {
            's' => (&argument[..argument.len() - 1], 1.0),
            'm' => (&argument[..argument.len() - 1], 60.0),
            'h' => (&argument[..argument.len() - 1], 3600.0),
            'd' => (&argument[..argument.len() - 1], 86400.0),
            _ => (argument.as_str(), 1.0),
        };
        total += number.parse::<f64>().ok()? * scale;
    }
    Some(total)
}

fn powershell_sleep_seconds(arguments: &[String]) -> Option<f64> {
    let mut scale = 1.0;
    let mut value = None;
    for argument in arguments {
        match argument.to_ascii_lowercase().as_str() {
            "}" | ")" => {}
            "-milliseconds" | "-m" => scale = 0.001,
            "-seconds" | "-s" => scale = 1.0,
            other => value = Some(other.parse::<f64>().ok()?),
        }
    }
    value.map(|seconds| seconds * scale)
}

/// `tail -f`/`-F`/`--follow` and PowerShell `Get-Content -Wait`. A
/// `timeout`-wrapped follow has `timeout` as its head and never reaches here.
fn follows_output(words: &[String]) -> bool {
    let Some(head) = words.first().map(|word| program_name(word)) else {
        return false;
    };
    let arguments = &words[1..];
    match head.as_str() {
        "tail" => arguments.iter().any(|argument| {
            argument == "--follow"
                || argument.starts_with("--follow=")
                || (argument.starts_with('-')
                    && !argument.starts_with("--")
                    && argument.contains(['f', 'F']))
        }),
        "get-content" | "gc" => arguments
            .iter()
            .any(|argument| argument.eq_ignore_ascii_case("-wait")),
        _ => false,
    }
}

/// True when `text` runs a GitHub status poll target anywhere, including
/// inside a command substitution (`p=$(gh pr checks 1)`). One linear token
/// scan: recursing per substitution is quadratic on nested input.
fn mentions_gh_poll_target(text: &str) -> bool {
    // Split on quotes and substitution delimiters too: `"$(gh run view 1)"`
    // is one shell word but runs `gh`.
    let words: Vec<String> = text
        .split(|ch: char| {
            ch.is_whitespace() || matches!(ch, '"' | '\'' | '(' | ')' | '`' | ';' | '|' | '&')
        })
        .filter(|word| !word.is_empty())
        .map(str::to_string)
        .collect();
    words
        .iter()
        .enumerate()
        .any(|(index, word)| program_name(word) == "gh" && gh_poll_target(&words[index..]))
}

/// What the command does that counts as a hand-written wait, if anything.
fn hand_written_wait(command_text: &str, dialect: ShellDialect) -> Option<(&'static str, bool)> {
    let stripped = strip_heredoc_bodies(command_text);
    let mut unbounded = false;
    let mut bounded = false;
    let mut sleeps = Vec::new();
    let mut follow = false;
    let mut watch = false;
    for segment in split_shell_segments(&stripped, dialect) {
        let words = body_words(&segment, dialect);
        match loop_kind(&segment, &words, dialect) {
            Some(LoopKind::Unbounded) => unbounded = true,
            Some(LoopKind::Bounded) => bounded = true,
            None => {}
        }
        if let Some(seconds) = sleep_seconds(&words) {
            sleeps.push(seconds);
        }
        follow |= follows_output(&words);
        watch |= words
            .first()
            .is_some_and(|word| program_name(word) == "watch");
    }
    let poll = (unbounded || !sleeps.is_empty()) && mentions_gh_poll_target(&stripped);
    let what = if unbounded {
        "a `while`/`until`/infinite `for` loop"
    } else if bounded && !sleeps.is_empty() {
        "a `for` loop that sleeps between iterations"
    } else if follow {
        "`tail -f`/`--follow` without `timeout`"
    } else if watch {
        "`watch`"
    } else if poll && !sleeps.is_empty() {
        "a `sleep` chained to a GitHub status poll"
    } else if sleeps
        .iter()
        .any(|seconds| seconds.is_none_or(|s| s > MAX_STANDALONE_SLEEP_SECS))
    {
        "a `sleep` longer than 60 s, or of non-literal length"
    } else {
        return None;
    };
    Some((what, poll))
}

/// The refusal for a hand-written wait, unless an audited
/// `CLUD_BAD_CMD_OVERRIDE=hand-written-wait:<reason>` accepts it.
pub(super) fn hand_written_wait_reason(
    command_text: &str,
    dialect: ShellDialect,
    evaluation: &mut CommandEvaluation,
) -> Option<String> {
    let (what, poll) = hand_written_wait(command_text, dialect)?;
    if let Some(override_reason) = accepted_override_reason(WAIT_LOOP_RULE_ID) {
        evaluation.log_messages.push(format!(
            "BAD_CMD_OVERRIDE accepted rule={WAIT_LOOP_RULE_ID} reason={override_reason:?} command={command_text:?}"
        ));
        return None;
    }
    let pr_hint = if poll {
        format!(" For a PR, run `{PR_WATCH_REPLACEMENT}` in the background.")
    } else {
        String::new()
    };
    Some(format!(
        "[{WAIT_LOOP_RULE_ID}] Refused {what}. Hand-written waits check a stand-in (a process list, a word in output, a status string) instead of the real completion event and never give up; such loops have run for 7 hours (zackees/clud#1880). {WAIT_REPLACEMENT}{pr_hint} To intentionally bypass this rule for this one command, set the real environment variable {BAD_CMD_OVERRIDE_ENV}=\"{WAIT_LOOP_RULE_ID}:<your reason for needing the raw command>\" for this tool call (not text prepended to the command itself) and re-run the exact same command unchanged."
    ))
}
