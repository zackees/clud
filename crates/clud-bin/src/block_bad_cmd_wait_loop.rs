//! zackees/clud#1880: refuse hand-written waits in shell commands.
//!
//! Every hand-written wait invents a completion check on a stand-in (a process
//! list, a word in output, a status string) and nothing forces it to give up.
//! The harness's background-task notification tracks the real process exit,
//! so it is the only general way to wait. See
//! DD-168 in `docs/DESIGN_DECISIONS.md` for the `sleep` threshold.

use super::block_bad_cmd_wait_scan::{
    heredoc_scripts, inline_script, mentions_gh_status, peel_wrappers, python_sleep_seconds,
    python_wait, script_file, HeredocUse, Peeled, ScriptKind,
};
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
        if dialect == ShellDialect::Posix && keyword == "case" {
            // #1972: `case "$s" in UNKNOWN*) sleep 5` keeps its arm's body.
            let after_in = words
                .iter()
                .position(|word| word == "in")
                .map_or(1, |i| i + 1);
            words.drain(..after_in.min(words.len()));
            continue;
        }
        let control = match dialect {
            ShellDialect::Posix => {
                matches!(
                    keyword.as_str(),
                    "do" | "then" | "else" | "elif" | "!" | "time"
                ) || is_case_pattern(&keyword)
            }
            ShellDialect::PowerShell | ShellDialect::Cmd => false,
        };
        if !control {
            return words;
        }
        words.remove(0);
    }
}

/// A `case` arm's pattern word (`*)`, `completed*)`, `pass|fail)`), which
/// starts the segment that holds the arm's first command.
fn is_case_pattern(word: &str) -> bool {
    word.len() > 1 && word.ends_with(')') && !word.contains('(')
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

/// Deepest embedded script (a `bash -c` inside a heredoc inside ...) read.
const MAX_SCRIPT_DEPTH: usize = 4;

/// One hand-written wait the scanner found.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct Finding {
    pub(super) what: &'static str,
    /// It reads or blocks on GitHub PR, check or run state.
    pub(super) poll: bool,
}

/// What a scan may look at beyond the command text.
#[derive(Clone, Copy)]
pub(super) struct ScanContext<'a> {
    /// Resolves a relative script path (`bash ./watch.sh`).
    pub(super) cwd: Option<&'a Path>,
    /// `git.pr_wait_fail_fast`: refuse a bare native watcher outside a loop.
    pub(super) native_gate: bool,
    /// A clud gh-shim session, whose `gh pr checks --watch` is canonical.
    pub(super) shim_active: bool,
}

/// Facts collected over one command's segments.
#[derive(Default)]
struct SegmentFacts {
    unbounded: bool,
    bounded: bool,
    sleeps: Vec<Option<f64>>,
    follow: bool,
    watch: bool,
    /// A native watcher (`gh run watch`, `gh pr checks --watch`), canonical or not.
    any_native: bool,
    /// A native watcher that is not the gh-shim's canonical PR watch.
    raw_native: bool,
}

/// The shim's own `gh pr checks --watch`, routed to the fail-fast waiter.
fn canonical_shimmed_watch(words: &[String], shim_active: bool) -> bool {
    shim_active
        && gh_positionals(words.get(1..).unwrap_or(&[])).starts_with(&["pr", "checks"])
        && words
            .iter()
            .any(|word| word == "--watch" || word.starts_with("--watch="))
}

impl SegmentFacts {
    fn record(&mut self, segment: &str, dialect: ShellDialect, scan: ScanContext<'_>) -> Peeled {
        let words = body_words(segment, dialect);
        match loop_kind(segment, &words, dialect) {
            Some(LoopKind::Unbounded) => self.unbounded = true,
            Some(LoopKind::Bounded) => self.bounded = true,
            None => {}
        }
        let peeled = peel_wrappers(&words);
        if let Some(seconds) =
            sleep_seconds(&peeled.words).or_else(|| python_sleep_seconds(&peeled.words))
        {
            self.sleeps.push(seconds);
        }
        // `timeout 60 tail -f` has its own deadline (DD-168).
        self.follow |= !peeled.timed && follows_output(&peeled.words);
        self.watch |= peeled
            .words
            .first()
            .is_some_and(|word| program_name(word) == "watch");
        if native_gh_waiter(&peeled.words) {
            self.any_native = true;
            self.raw_native |= !canonical_shimmed_watch(&peeled.words, scan.shim_active);
        }
        peeled
    }

    fn verdict(&self, stripped: &str, native_gate: bool) -> Option<Finding> {
        let looping = self.unbounded || self.bounded;
        let has_sleep = !self.sleeps.is_empty();
        let poll =
            self.any_native || ((self.unbounded || has_sleep) && mentions_gh_status(stripped));
        let what = if self.any_native && looping {
            "a native GitHub watcher (`gh run watch`, `gh pr checks --watch`) retried in a loop"
        } else if self.unbounded {
            "a `while`/`until`/infinite `for` loop"
        } else if self.bounded && has_sleep {
            "a `for` loop that sleeps between iterations"
        } else if self.raw_native && native_gate {
            "a native GitHub watcher (`gh run watch`, or `gh pr checks --watch` outside a clud gh-shim session)"
        } else if self.follow {
            "`tail -f`/`--follow` without `timeout`"
        } else if self.watch {
            "`watch`"
        } else if poll && has_sleep {
            "a `sleep` chained to a GitHub status poll"
        } else if self
            .sleeps
            .iter()
            .any(|seconds| seconds.is_none_or(|s| s > MAX_STANDALONE_SLEEP_SECS))
        {
            "a `sleep` longer than 60 s, or of non-literal length"
        } else {
            return None;
        };
        Some(Finding { what, poll })
    }
}

/// A script embedded in, or run by, one peeled command: `bash -c`, `eval`,
/// `python -c`, or a literal script file on disk. A file on disk may be a
/// repository's own script, so only a PR/CI wait in it counts.
fn embedded_wait(
    peeled: &Peeled,
    dialect: ShellDialect,
    scan: ScanContext<'_>,
    depth: usize,
) -> Option<Finding> {
    if let Some((kind, script)) = inline_script(&peeled.words, dialect) {
        if let Some(finding) = script_wait(kind, &script, scan, depth) {
            return Some(finding);
        }
    }
    let (kind, script) = script_file(&peeled.words, scan.cwd)?;
    script_wait(kind, &script, scan, depth).filter(|finding| finding.poll)
}

fn script_wait(
    kind: ScriptKind,
    script: &str,
    scan: ScanContext<'_>,
    depth: usize,
) -> Option<Finding> {
    match kind {
        ScriptKind::Shell(dialect) => hand_written_wait(script, dialect, scan, depth + 1),
        ScriptKind::Python => python_wait(script).map(|poll| Finding {
            what: "a Python loop that sleeps",
            poll,
        }),
    }
}

/// Heredoc bodies that are scripts: one fed to an interpreter is judged in
/// full, one written to a script file only for a PR/CI wait.
fn heredoc_wait(command_text: &str, scan: ScanContext<'_>, depth: usize) -> Option<Finding> {
    heredoc_scripts(command_text)
        .into_iter()
        .find_map(|(usage, kind, body)| {
            script_wait(kind, &body, scan, depth)
                .filter(|finding| usage == HeredocUse::Fed || finding.poll)
        })
}

/// What the command does that counts as a hand-written wait, if anything.
pub(super) fn hand_written_wait(
    command_text: &str,
    dialect: ShellDialect,
    scan: ScanContext<'_>,
    depth: usize,
) -> Option<Finding> {
    if depth > MAX_SCRIPT_DEPTH {
        return None;
    }
    if let Some(finding) = heredoc_wait(command_text, scan, depth) {
        return Some(finding);
    }
    let stripped = strip_heredoc_bodies(command_text);
    let mut facts = SegmentFacts::default();
    for segment in split_shell_segments(&stripped, dialect) {
        let peeled = facts.record(&segment, dialect, scan);
        if let Some(finding) = embedded_wait(&peeled, dialect, scan, depth) {
            return Some(finding);
        }
    }
    facts.verdict(&stripped, scan.native_gate)
}

/// How to run the blessed PR watcher, named in every PR/CI refusal.
pub(super) const PR_WATCH_USAGE: &str = "For a PR, run `clud tool run github/pr_merge_watch.py <PR> [--repo OWNER/NAME] [--timeout SECS]` with `run_in_background` and wait for its completion notification: it returns the moment a required check fails, review activity arrives, the PR merges or closes, or the timeout fires. For local CI use `clud ci` or `bosn ci run --wait`. Moving the loop into `timeout`, `nohup`, `bash -c`, `eval`, a Monitor call, a written script or Python is the same hand-written wait (zackees/clud#1972).";

/// The refusal for a hand-written wait, unless an audited
/// `CLUD_BAD_CMD_OVERRIDE=hand-written-wait:<reason>` accepts it.
pub(super) fn hand_written_wait_reason(
    command_text: &str,
    dialect: ShellDialect,
    scan: ScanContext<'_>,
    evaluation: &mut CommandEvaluation,
) -> Option<String> {
    let Finding { what, poll } = hand_written_wait(command_text, dialect, scan, 0)?;
    if let Some(override_reason) = accepted_override_reason(WAIT_LOOP_RULE_ID) {
        evaluation.log_messages.push(format!(
            "BAD_CMD_OVERRIDE accepted rule={WAIT_LOOP_RULE_ID} reason={override_reason:?} command={command_text:?}"
        ));
        return None;
    }
    let pr_hint = if poll {
        format!(" {PR_WATCH_USAGE}")
    } else {
        String::new()
    };
    Some(format!(
        "[{WAIT_LOOP_RULE_ID}] Refused {what}. Hand-written waits check a stand-in (a process list, a word in output, a status string) instead of the real completion event and never give up; such loops have run for 7 hours (zackees/clud#1880). {WAIT_REPLACEMENT}{pr_hint} To intentionally bypass this rule for this one command, set the real environment variable {BAD_CMD_OVERRIDE_ENV}=\"{WAIT_LOOP_RULE_ID}:<your reason for needing the raw command>\" for this tool call (not text prepended to the command itself) and re-run the exact same command unchanged."
    ))
}
