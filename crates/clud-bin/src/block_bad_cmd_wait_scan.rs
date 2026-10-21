//! zackees/clud#1972: find the waiter behind a wrapper or inside an embedded
//! script, so `timeout 590 bash -c '<loop>'`, `nohup gh run watch`, a loop
//! written into `watch.sh` by a heredoc, and a Python `while True` +
//! `time.sleep` poll read the same as the bare loop they wrap.
//!
//! These helpers only locate scripts; `block_bad_cmd_wait_loop` judges
//! them. Commands come from the evaluated syntax tree (#1962): a `bash -c`,
//! `eval` or heredoc fed to a shell is already parsed into its commands
//! there, so what is left here is what the shell grammar cannot see into:
//! Python payloads, script files, and heredocs written to a script file.
//! See DD-169 in `docs/DESIGN_DECISIONS.md`.

use super::*;
use crate::shell_ast::Invocation;

/// Shells whose `-c` argument is a script.
const POSIX_C_SHELLS: &[&str] = &["bash", "sh", "zsh", "dash", "ksh"];

/// Largest script file read from disk for a `bash x.sh` / `./x.sh` /
/// `python x.py` run. Larger files are not scanned.
const MAX_SCRIPT_FILE_BYTES: u64 = 256 * 1024;

/// A command's words with every transparent wrapper program peeled off.
pub(super) struct Peeled {
    /// The real program and its arguments.
    pub(super) words: Vec<String>,
    /// A `timeout` wrapper was peeled: the command has its own deadline.
    pub(super) timed: bool,
}

/// Peel `VAR=val`, `timeout`, `nohup`, `env`, `stdbuf`, `nice`, `ionice`,
/// `setsid`, `xargs`, `command`, `exec`, `time`, `builtin` and the gate
/// prefix until the real program is first.
pub(super) fn peel_wrappers(words: &[String]) -> Peeled {
    let gate_prefix = block_bad_cmd_gate::gate_prefix();
    let mut words = words.to_vec();
    let mut timed = false;
    for _ in 0..12 {
        while words.first().is_some_and(|word| is_env_assignment(word)) {
            words.remove(0);
        }
        let Some(head) = words.first() else {
            break;
        };
        let head = program_name(head);
        let next = match head.as_str() {
            "timeout" => {
                timed = true;
                skip_options(&words, &["-s", "--signal", "-k", "--kill-after"])
                    .map(|rest| rest.get(1..).unwrap_or_default().to_vec())
            }
            "nohup" | "setsid" | "time" | "builtin" => skip_options(&words, &[]),
            "nice" => skip_options(&words, &["-n", "--adjustment"]),
            "ionice" => skip_options(&words, &["-c", "-n", "-p", "--class", "--classdata"]),
            "stdbuf" => skip_options(&words, &["-i", "-o", "-e"]),
            "xargs" => skip_options(&words, XARGS_VALUE_OPTIONS),
            "env" => unwrap_env(&words),
            "command" => unwrap_command(&words),
            "exec" => unwrap_exec(&words),
            _ if words.len() > 1 && head == gate_prefix => Some(words[1..].to_vec()),
            _ => None,
        };
        match next {
            Some(rest) if !rest.is_empty() => words = rest,
            _ => break,
        }
    }
    Peeled { words, timed }
}

const XARGS_VALUE_OPTIONS: &[&str] = &[
    "-n",
    "-P",
    "-I",
    "-L",
    "-d",
    "-E",
    "-s",
    "-a",
    "--max-args",
    "--max-procs",
    "--delimiter",
    "--arg-file",
    "--max-lines",
    "--max-chars",
];

/// The words after the wrapper program and its options; `value_options`
/// take a separate value word.
fn skip_options(words: &[String], value_options: &[&str]) -> Option<Vec<String>> {
    let mut index = 1usize;
    while let Some(word) = words.get(index) {
        if word == "--" {
            index += 1;
            break;
        }
        if !word.starts_with('-') || word == "-" {
            break;
        }
        index += if value_options.contains(&word.as_str()) {
            2
        } else {
            1
        };
    }
    Some(words.get(index..)?.to_vec())
}

/// What an embedded script is written in.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(super) enum ScriptKind {
    Shell(ShellDialect),
    Python,
}

fn is_python(head: &str) -> bool {
    matches!(head, "python" | "python3" | "py" | "pypy" | "pypy3") // python-name-lint: allow (matches agents' interpreter names)
}

/// Positional (non-option) arguments after the program.
fn positionals(words: &[String]) -> Vec<&String> {
    words
        .iter()
        .skip(1)
        .take_while(|word| !matches!(word.as_str(), ">" | ">>" | "<" | "|" | "&"))
        .filter(|word| !word.starts_with('-') && !word.contains('>') && !word.contains('<'))
        .collect()
}

/// The script a peeled command runs from its own argument: `bash -c`,
/// `sh -c`, `zsh -c`, `eval`, PowerShell `-Command`, and `python -c`.
pub(super) fn inline_script(
    words: &[String],
    dialect: ShellDialect,
) -> Option<(ScriptKind, String)> {
    let _ = dialect;
    let head = program_name(words.first()?);
    if is_python(&head) {
        let index = words.iter().position(|word| word == "-c")?;
        return Some((ScriptKind::Python, words.get(index + 1)?.clone()));
    }
    if head == "eval" {
        return Some((ScriptKind::Shell(ShellDialect::Posix), words[1..].join(" ")));
    }
    if contains_str(POSIX_C_SHELLS, &head) {
        // Only a short option cluster carrying `c` (`-c`, `-lc`) takes the
        // script; a script path such as `./watch.sh` is not one.
        let index = words.iter().position(|word| {
            word.starts_with('-') && !word.starts_with("--") && word.contains('c')
        })?;
        return Some((
            ScriptKind::Shell(ShellDialect::Posix),
            words.get(index + 1..)?.join(" "),
        ));
    }
    if matches!(head.as_str(), "pwsh" | "powershell") {
        let index = words
            .iter()
            .position(|word| matches!(word.to_ascii_lowercase().as_str(), "-command" | "-c"))?;
        return Some((
            ScriptKind::Shell(ShellDialect::PowerShell),
            words.get(index + 1..)?.join(" "),
        ));
    }
    None
}

/// The `time.sleep(N)` of a loop-free `python -c` used as a shell sleep:
/// `Some(Some(N))`, `Some(None)` for a non-literal duration, `None` when
/// the payload does not sleep (or loops, which `python_wait` judges).
pub(super) fn python_sleep_seconds(words: &[String]) -> Option<Option<f64>> {
    let (kind, script) = inline_script(words, ShellDialect::Posix)?;
    if kind != ScriptKind::Python || python_has_loop(&script) {
        return None;
    }
    let after = script.split("sleep(").nth(1)?;
    let literal: String = after.chars().take_while(|ch| *ch != ')').collect();
    Some(literal.trim().parse::<f64>().ok())
}

/// The script file a peeled command runs (`bash x.sh`, `./x.sh`,
/// `python x.py`), when its path is literal and the file exists.
pub(super) fn script_file(words: &[String], cwd: Option<&Path>) -> Option<(ScriptKind, String)> {
    let first = words.first()?;
    let head = program_name(first);
    let (kind, path) = if contains_str(POSIX_C_SHELLS, &head) {
        (
            ScriptKind::Shell(ShellDialect::Posix),
            positionals(words).first()?.to_string(),
        )
    } else if is_python(&head) {
        (ScriptKind::Python, positionals(words).first()?.to_string())
    } else if first.contains('/') && (first.ends_with(".sh") || first.ends_with(".bash")) {
        (ScriptKind::Shell(ShellDialect::Posix), first.clone())
    } else if first.contains('/') && first.ends_with(".py") {
        (ScriptKind::Python, first.clone())
    } else {
        return None;
    };
    if path.contains(['$', '`', '*', '?']) || path.starts_with('~') {
        return None;
    }
    let path = Path::new(&path);
    let resolved = if path.is_absolute() {
        path.to_path_buf()
    } else {
        cwd?.join(path)
    };
    let metadata = std::fs::metadata(&resolved).ok()?;
    if !metadata.is_file() || metadata.len() > MAX_SCRIPT_FILE_BYTES {
        return None;
    }
    Some((kind, std::fs::read_to_string(&resolved).ok()?))
}

/// How a heredoc body is used: executed now (`bash <<EOF`, `python - <<EOF`)
/// or written to a script file (`cat > watch.sh <<EOF`).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(super) enum HeredocUse {
    Fed,
    Written,
}

/// Every heredoc body in `text` that is a script: fed to an interpreter or
/// written by `cat`/`tee` to a `.sh`/`.py` file (or a file whose body starts
/// with a shell or Python shebang). Other heredocs (commit messages, issue
/// bodies) are data and are not returned.
#[cfg(test)]
pub(super) fn heredoc_scripts(text: &str) -> Vec<(HeredocUse, ScriptKind, String)> {
    parse_command(text, ShellDialect::Posix, None)
        .invocations
        .iter()
        .filter_map(heredoc_script)
        .collect()
}

/// The heredoc `invocation` reads, when it is a script; see
/// [`heredoc_scripts`].
pub(super) fn heredoc_script(invocation: &Invocation) -> Option<(HeredocUse, ScriptKind, String)> {
    let body = invocation.heredoc.as_ref()?;
    let peeled = peel_wrappers(&rule_words(invocation));
    let head = program_name(peeled.words.first()?);
    let reads_stdin = positionals(&peeled.words)
        .iter()
        .all(|word| word.as_str() == "-");
    if contains_str(POSIX_C_SHELLS, &head) && reads_stdin {
        return Some((
            HeredocUse::Fed,
            ScriptKind::Shell(ShellDialect::Posix),
            body.clone(),
        ));
    }
    if is_python(&head) && reads_stdin {
        return Some((HeredocUse::Fed, ScriptKind::Python, body.clone()));
    }
    if !matches!(head.as_str(), "cat" | "tee") {
        return None;
    }
    let target = write_target(invocation, &peeled.words);
    let shebang = body.lines().next().unwrap_or_default();
    let kind =
        if target.ends_with(".py") || (shebang.starts_with("#!") && shebang.contains("python")) {
            ScriptKind::Python
        } else if target.ends_with(".sh")
            || target.ends_with(".bash")
            || (shebang.starts_with("#!") && shebang.contains("sh"))
        {
            ScriptKind::Shell(ShellDialect::Posix)
        } else {
            return None;
        };
    Some((HeredocUse::Written, kind, body.clone()))
}

/// The file `cat > FILE` / `tee FILE` writes; empty when none is named or
/// its name is only known at run time.
fn write_target(invocation: &Invocation, words: &[String]) -> String {
    let redirected = invocation
        .redirects
        .iter()
        .filter(|redirect| redirect.writes_file())
        .find_map(|redirect| redirect.target.as_literal());
    if let Some(target) = redirected {
        return target.to_string();
    }
    if program_name(words.first().map_or("", String::as_str)) == "tee" {
        return positionals(words)
            .into_iter()
            .next()
            .cloned()
            .unwrap_or_default();
    }
    String::new()
}

/// Python source with triple-quoted strings removed: an edit script that
/// carries replacement code in `'''...'''` is data, not a loop.
fn strip_triple_quoted(source: &str) -> String {
    let mut out = String::with_capacity(source.len());
    let mut rest = source;
    loop {
        let next = [rest.find("'''"), rest.find("\"\"\"")]
            .into_iter()
            .flatten()
            .min();
        let Some(start) = next else {
            out.push_str(rest);
            return out;
        };
        out.push_str(&rest[..start]);
        let quote = &rest[start..start + 3];
        let after = &rest[start + 3..];
        match after.find(quote) {
            Some(end) => rest = &after[end + 3..],
            None => return out,
        }
    }
}

fn python_has_loop(source: &str) -> bool {
    strip_triple_quoted(source).lines().any(|line| {
        let line = line.trim_start();
        (line.starts_with("while ") || line.starts_with("while(") || line.starts_with("for "))
            && line.contains(':')
    })
}

/// A Python script that loops and sleeps: `Some(poll)`, where `poll` says it
/// also reads GitHub PR/check/run state (or blocks on a native watcher).
pub(super) fn python_wait(source: &str) -> Option<bool> {
    let code = strip_triple_quoted(source);
    if !python_has_loop(&code) || !code.contains("sleep(") {
        return None;
    }
    let normalized: String = code
        .chars()
        .map(|ch| {
            if matches!(ch, '\'' | '"' | ',' | '[' | ']' | '(' | ')') {
                ' '
            } else {
                ch
            }
        })
        .collect();
    Some(mentions_gh_status(&normalized))
}

/// True when `text` runs a GitHub status read or a native watcher anywhere,
/// including inside a command substitution. One linear token scan.
pub(super) fn mentions_gh_status(text: &str) -> bool {
    // Split on quotes and substitution delimiters too: `"$(gh run view 1)"`
    // is one shell word but runs `gh`.
    let words: Vec<String> = text
        .split(|ch: char| {
            ch.is_whitespace() || matches!(ch, '"' | '\'' | '(' | ')' | '`' | ';' | '|' | '&')
        })
        .filter(|word| !word.is_empty())
        .map(str::to_string)
        .collect();
    words.iter().enumerate().any(|(index, word)| {
        program_name(word) == "gh"
            && (gh_poll_target(&words[index..]) || native_gh_waiter(&words[index..]))
    })
}
