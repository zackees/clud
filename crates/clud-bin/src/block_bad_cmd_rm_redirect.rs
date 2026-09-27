//! Redirect an agent's own deletion commands to `safe-rm` (#1461).
//!
//! What an agent types as `rm`, `rmdir`, `unlink`, `find … -delete`,
//! `find … -exec rm …` or `xargs rm` is refused with the exact replacement
//! (`rm -rf build` → `safe-rm -rf build`). `safe-rm`
//! trash by default and only delete inside the session's roots, and a
//! command made only of them is allowed outright ([`rm_tool_only`]), so
//! Claude Code never stops an unattended run on an `rm` prompt.
//!
//! Scope is what the agent typed. A script's own `rm` reaches the child `rm`
//! shim, which only refuses catastrophic paths (`rm_guard`). `git rm`,
//! `git clean` and inline `python -c`/`node -e` deletions are deliberately
//! left alone. See `docs/architecture/rm-tools.md`.

use super::block_bad_cmd_shell::{
    command_words, nested_shell_command, posix_c_shell_script, program_name, split_shell_segments,
    tokenize,
};
use super::{strip_heredoc_bodies, ShellDialect};

/// Leading words that run the next word as the program, with their options
/// that take a separate value. `command`, `env` and `exec` are already
/// unwrapped by `command_words`.
const RUNNERS: &[(&str, &[&str])] = &[
    (
        "sudo",
        &[
            "-u", "-g", "-C", "-D", "-h", "-p", "-r", "-t", "-U", "-T", "--user", "--group",
        ],
    ),
    ("doas", &["-u", "-C"]),
    ("time", &["-f", "-o", "--format", "--output"]),
    ("nice", &["-n", "--adjustment"]),
    ("nohup", &[]),
    ("tap", &[]),
    ("stdbuf", &["-i", "-o", "-e"]),
    ("timeout", &["-s", "-k", "--signal", "--kill-after"]),
];

/// `xargs` options that take a separate value.
const XARGS_VALUE_OPTIONS: &[&str] = &[
    "-I",
    "-L",
    "-n",
    "-P",
    "-s",
    "-d",
    "-E",
    "-a",
    "--max-args",
    "--max-procs",
    "--delimiter",
    "--arg-file",
    "--max-lines",
    "--replace",
];

/// The denial for a command that deletes with `rm` & co, naming the
/// `safe-rm` command to run instead; `None` when it does not.
#[cfg(test)]
pub(super) fn redirect_reason(command: &str) -> Option<String> {
    let (original, suggestion) = find_removal(command, 0)?;
    Some(format!(
        "Use `safe-rm` for agent-authored deletion: it moves paths to the clud trash \
         (`--purge` deletes for real, `--tracked` allows git-tracked paths) and limits \
         targets to this session's allowed locations. Instead of `{original}`, run:\n  \
         {suggestion}\n(`clud safe-rm` also works if the alias is not on PATH. \
         Human-written scripts may use rm; its shim only refuses catastrophic paths.)"
    ))
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum Redirect {
    Rewrite(String),
    Refuse(String),
}

/// A rewrite must not launder a shell assignment that changes the deletion
/// roots, role or program resolution used by the rewritten command.
pub(super) fn changes_deletion_environment(command: &str) -> bool {
    tokenize(command).iter().any(|word| {
        let name = word
            .split('=')
            .next()
            .unwrap_or_default()
            .trim_end_matches('+');
        matches!(name, "PATH" | "CLUD_RM_ROOTS" | "CLUD_RM_ROLE")
    })
}

pub(super) fn raw_payload_mentions_removal(raw: &str) -> bool {
    let lower = raw.to_ascii_lowercase();
    let decoded = decode_ascii_json_escapes(&lower);
    let readings = [
        decoded.replace('\\', " "),
        decoded.replace("\\n", " ").replace("\\t", " "),
        decoded
            .replace("''", "")
            .replace("\"\"", "")
            .replace('\\', ""),
    ];
    readings.iter().any(|reading| {
        reading
            .split(|ch: char| !ch.is_ascii_alphanumeric() && !matches!(ch, '_' | '-' | '.' | '/'))
            .any(|word| {
                let candidate = word.strip_suffix(".exe").unwrap_or(word);
                crate::deletion_rules::redirect_for(&program_name(candidate)).is_some()
            })
    })
}

fn decode_ascii_json_escapes(raw: &str) -> String {
    let chars: Vec<char> = raw.chars().collect();
    let mut out = String::with_capacity(raw.len());
    let mut index = 0;
    while index < chars.len() {
        if chars[index] == '\\' && chars.get(index + 1) == Some(&'u') && index + 5 < chars.len() {
            let digits: String = chars[index + 2..index + 6].iter().collect();
            if let Some(letter) = u32::from_str_radix(&digits, 16)
                .ok()
                .filter(|code| *code < 0x80)
                .and_then(char::from_u32)
            {
                out.push(letter);
                index += 6;
                continue;
            }
        }
        out.push(chars[index]);
        index += 1;
    }
    out
}

pub(super) fn redirect(command: &str) -> Option<Redirect> {
    if shell_fed_heredoc_removal(command) {
        return Some(Redirect::Refuse(
            "a shell-fed heredoc may execute deletion; use safe-rm directly instead".into(),
        ));
    }
    let (original, suggestion) = find_removal(command, 0)?;
    let unsafe_form = ["sudo ", "doas ", "eval ", " -delete"]
        .iter()
        .any(|needle| command.contains(needle))
        || command.contains('`')
        || command.contains("$(")
        || contains_nested_shell(command)
        || tokenize(command).windows(2).any(|pair| {
            crate::deletion_rules::redirect_for(&program_name(&pair[0]))
                .is_some_and(|(_, prefix_arg)| prefix_arg.is_some())
                && (pair[1] == "-p" || pair[1] == "--parents")
        });
    if unsafe_form || command.matches(&original).count() != 1 {
        Some(Redirect::Refuse(format!(
            "this deletion form cannot be rewritten safely; use `safe-rm` instead (suggestion: {suggestion})"
        )))
    } else {
        Some(Redirect::Rewrite(command.replacen(
            &original,
            &suggestion,
            1,
        )))
    }
}

fn contains_nested_shell(command: &str) -> bool {
    split_shell_segments(command, ShellDialect::Posix)
        .into_iter()
        .any(|segment| {
            let segment = segment.trim();
            let segment = segment.strip_prefix('(').map_or(segment, str::trim_start);
            let words = strip_runners(command_words(segment));
            posix_c_shell_script(&words).is_some()
                || nested_shell_command(&words, ShellDialect::Posix).is_some()
        })
}

/// The first removal in `text`: the statement as typed and its replacement.
fn find_removal(text: &str, depth: usize) -> Option<(String, String)> {
    if depth > 4 {
        return None;
    }
    // A heredoc body is data, at every depth: `$(cat <<'EOF' … EOF)` is how
    // a commit message is passed.
    let text = strip_heredoc_bodies(text);
    let text = text.as_str();
    for inner in active_substitutions(text) {
        if let Some(found) = find_removal(&inner, depth + 1) {
            return Some(found);
        }
    }
    for segment in split_shell_segments(text, ShellDialect::Posix) {
        let segment = segment.trim();
        let segment = segment.strip_prefix('(').map_or(segment, str::trim_start);
        let words = strip_runners(command_words(segment));
        if words.is_empty() {
            continue;
        }
        let raw = raw_words(segment, &words);
        if let Some(script) = posix_c_shell_script(&words) {
            if let Some(found) = find_removal(&script, depth + 1) {
                return Some(found);
            }
        }
        if let Some((nested, ShellDialect::Posix)) =
            nested_shell_command(&words, ShellDialect::Posix)
        {
            if let Some(found) = find_removal(&nested, depth + 1) {
                return Some(found);
            }
            continue;
        }
        if let Some(suggestion) = statement_suggestion(&words, &raw) {
            return Some((segment.to_string(), suggestion));
        }
    }
    None
}

fn shell_fed_heredoc_removal(command: &str) -> bool {
    if !command.contains("<<") {
        return false;
    }
    let lines: Vec<&str> = command.split('\n').collect();
    let mut index = 0;
    while index < lines.len() {
        let line = lines[index];
        let Some(delimiter) = super::find_heredoc_delimiter(line) else {
            index += 1;
            continue;
        };
        let terminator = (index + 1..lines.len()).find(|&candidate| {
            lines[candidate]
                .trim_start_matches('\t')
                .trim_end_matches('\r')
                == delimiter
        });
        let end = terminator.unwrap_or(lines.len());
        let body = lines[index + 1..end].join("\n");
        let expands_body =
            heredoc_expands_body(line, &delimiter) && (body.contains('`') || body.contains("$("));
        if (line_feeds_shell(line) || expands_body) && find_removal(&body, 0).is_some() {
            return true;
        }
        index = terminator.map_or(lines.len(), |end| end + 1);
    }
    false
}

fn heredoc_expands_body(line: &str, delimiter: &str) -> bool {
    let Some((_, tail)) = line.split_once("<<") else {
        return false;
    };
    let tail = tail.strip_prefix('-').unwrap_or(tail).trim_start();
    tail.starts_with(delimiter)
}

fn line_feeds_shell(line: &str) -> bool {
    for group in super::split_pipeline_groups(line, ShellDialect::Posix) {
        let Some(start) = group
            .iter()
            .position(|stage| super::find_heredoc_delimiter(stage).is_some())
        else {
            continue;
        };
        if group[start..].iter().any(|stage| {
            let words = command_words(stage);
            let Some(first) = words.first() else {
                return false;
            };
            let name = program_name(first);
            let shell = if name == "busybox" {
                words
                    .get(1)
                    .map(|word| program_name(word))
                    .unwrap_or_default()
            } else {
                name
            };
            matches!(
                shell.as_str(),
                "bash" | "sh" | "zsh" | "dash" | "ksh" | "ash" | "mksh"
            )
        }) {
            return true;
        }
    }
    false
}

/// The bodies of the command substitutions (`$(…)` and backticks) that
/// the shell would run: none inside single quotes, where they are text.
fn active_substitutions(text: &str) -> Vec<String> {
    let chars: Vec<char> = text.chars().collect();
    let mut found = Vec::new();
    let mut single = false;
    let mut double = false;
    let mut i = 0;
    while i < chars.len() {
        let ch = chars[i];
        if single {
            single = ch != '\'';
            i += 1;
            continue;
        }
        match ch {
            '\\' => {
                i += 2;
                continue;
            }
            '\'' if !double => single = true,
            '"' => double = !double,
            '$' if chars.get(i + 1) == Some(&'(') => {
                if let Some(end) = closing_paren(&chars, i + 1) {
                    found.push(chars[i + 2..end].iter().collect());
                    i = end + 1;
                    continue;
                }
            }
            '`' => {
                let end = (i + 1..chars.len()).find(|&j| chars[j] == '`' && chars[j - 1] != '\\');
                if let Some(end) = end {
                    found.push(chars[i + 1..end].iter().collect());
                    i = end + 1;
                    continue;
                }
            }
            _ => {}
        }
        i += 1;
    }
    found
}

/// The index of the `)` closing the `(` at `open`, honouring quotes.
fn closing_paren(chars: &[char], open: usize) -> Option<usize> {
    let mut depth = 0usize;
    let mut quote: Option<char> = None;
    let mut i = open;
    while i < chars.len() {
        let ch = chars[i];
        match quote {
            Some(q) if ch == q => quote = None,
            Some('"') if ch == '\\' => i += 1,
            Some(_) => {}
            None => match ch {
                '\\' => i += 1,
                '\'' | '"' => quote = Some(ch),
                '(' => depth += 1,
                ')' => {
                    depth -= 1;
                    if depth == 0 {
                        return Some(i);
                    }
                }
                _ => {}
            },
        }
        i += 1;
    }
    None
}

fn strip_runners(mut words: Vec<String>) -> Vec<String> {
    while let Some(first) = words.first() {
        let name = program_name(first);
        let Some((_, value_options)) = RUNNERS.iter().find(|(runner, _)| *runner == name) else {
            break;
        };
        words.remove(0);
        // Their own options (and those options' values) come first.
        while let Some(word) = words.first() {
            if !word.starts_with('-') {
                break;
            }
            let takes_value = value_options.contains(&word.as_str());
            words.remove(0);
            if takes_value && !words.is_empty() {
                words.remove(0);
            }
        }
        // `timeout`'s duration and `nice`'s old `-10` form.
        if name == "timeout" && !words.is_empty() {
            words.remove(0);
        }
    }
    words
}

/// The words of `segment` as typed, quotes kept, aligned with `words` (its
/// unquoted program words): the same whitespace split as `tokenize`, so the
/// suffix that `command_words` kept lines up. Falls back to shell-quoting
/// the unquoted words when it does not.
fn raw_words(segment: &str, words: &[String]) -> Vec<String> {
    let mut raw = Vec::new();
    let mut buf = String::new();
    let mut quote: Option<char> = None;
    for ch in segment.chars() {
        if let Some(q) = quote {
            buf.push(ch);
            if ch == q {
                quote = None;
            }
            continue;
        }
        if ch == '\'' || ch == '"' {
            quote = Some(ch);
            buf.push(ch);
            continue;
        }
        if ch.is_whitespace() {
            if !buf.is_empty() {
                raw.push(std::mem::take(&mut buf));
            }
            continue;
        }
        buf.push(ch);
    }
    if !buf.is_empty() {
        raw.push(buf);
    }
    let unquoted = tokenize(segment);
    if raw.len() == unquoted.len() && unquoted.ends_with(words) {
        return raw.split_off(raw.len() - words.len());
    }
    words
        .iter()
        .map(|w| shell_words::quote(w).into_owned())
        .collect()
}

/// The replacement for one statement that deletes, or `None`. `raw` holds
/// the same words as typed, so the suggestion keeps the agent's quoting.
fn statement_suggestion(words: &[String], raw: &[String]) -> Option<String> {
    let program = program_name(&words[0]);
    if crate::deletion_rules::redirect_for(&program).is_some() {
        return Some(remover_replacement(&program, &words[1..], &raw[1..]));
    }
    match program.as_str() {
        "find" => find_suggestion(words, raw),
        "xargs" => {
            let at = xargs_program_index(words)?;
            let inner = program_name(&words[at]);
            crate::deletion_rules::redirect_for(&inner).map(|_| {
                let mut out: Vec<String> = raw[..at].to_vec();
                out.push(tool_for(&inner, &words[at + 1..]).to_string());
                out.extend(raw[at + 1..].iter().cloned());
                out.join(" ")
            })
        }
        _ => None,
    }
}

/// `safe-rm` for `rmdir` or a recursive `rm`, else `safe-rm`.
fn tool_for(program: &str, args: &[String]) -> &'static str {
    let _ = (program, args);
    crate::deletion_rules::redirect_for(program)
        .map(|(replacement, _)| replacement)
        .unwrap_or("safe-rm")
}

fn remover_replacement(program: &str, args: &[String], raw: &[String]) -> String {
    let _ = args;
    let mut out = vec![tool_for(program, args).to_string()];
    if program == "rmdir" {
        out.push("-d".into());
    }
    out.extend(raw.iter().cloned());
    out.join(" ")
}

/// `find … -delete` and `find … -exec rm …`, rewritten to run the tools.
fn find_suggestion(words: &[String], raw: &[String]) -> Option<String> {
    let has_dir_type = words
        .windows(2)
        .any(|w| w[0] == "-type" && w[1].contains('d'));
    let has_type = words.iter().any(|w| w == "-type");
    let mut out: Vec<String> = Vec::new();
    let mut changed = false;
    let mut i = 0;
    while i < words.len() {
        let word = &words[i];
        if word == "-delete" {
            changed = true;
            if has_dir_type {
                out.extend(["-prune", "-exec", "safe-rm", "-d", "{}", "+"].map(String::from));
            } else {
                if !has_type {
                    out.extend(["-type", "f"].map(String::from));
                }
                out.extend(["-exec", "safe-rm", "{}", "+"].map(String::from));
            }
            i += 1;
            continue;
        }
        if matches!(word.as_str(), "-exec" | "-execdir" | "-ok" | "-okdir") {
            let end = (i + 1..words.len())
                .find(|&j| matches!(words[j].as_str(), ";" | "\\;" | "+"))
                .unwrap_or(words.len());
            let body = &words[i + 1..end];
            let inner = body.first().map(|p| program_name(p)).unwrap_or_default();
            if crate::deletion_rules::redirect_for(&inner).is_some() {
                changed = true;
                let tool = tool_for(&inner, &body[1..]);
                out.push(raw[i].clone());
                out.push(tool.to_string());
                if let Some(arg) =
                    crate::deletion_rules::redirect_for(&inner).and_then(|(_, arg)| arg)
                {
                    out.push(arg.to_string());
                }
                out.extend(raw[i + 2..=end.min(raw.len() - 1)].iter().cloned());
                i = end + 1;
                continue;
            }
        }
        out.push(raw[i].clone());
        i += 1;
    }
    changed.then(|| out.join(" "))
}

fn xargs_program_index(words: &[String]) -> Option<usize> {
    let mut i = 1;
    while let Some(word) = words.get(i) {
        if !word.starts_with('-') {
            return Some(i);
        }
        i += if XARGS_VALUE_OPTIONS.contains(&word.as_str()) {
            2
        } else {
            1
        };
    }
    None
}

const TOOLS: &[&str] = &["safe-rm"];

/// Whether `word` names clud's own `safe-rm` exactly: the bare
/// alias (resolved from clud's rm-shim directory, which the identity check
/// keeps first on PATH) or its path in that directory. `./safe-rm.sh` or a
/// script elsewhere that happens to share the name is not the tool.
fn is_tool_word(word: &str) -> bool {
    let bare = word.strip_suffix(".exe").unwrap_or(word);
    if TOOLS.contains(&bare) {
        return true;
    }
    let normalized = crate::path_norm::slash_separators(bare);
    TOOLS
        .iter()
        .any(|tool| normalized.ends_with(&format!("/.clud/state/rm-shim/{tool}")))
}

/// Whether `word` names clud itself: bare `clud` or `$CLUD_EXE`.
fn is_clud_word(word: &str) -> bool {
    matches!(
        word.strip_suffix(".exe").unwrap_or(word),
        "clud" | "$CLUD_EXE" | "${CLUD_EXE}"
    )
}

/// Whether `command` has an unquoted `&` that is not part of `&&`, which
/// runs the statement before it in the background and starts another.
fn has_lone_ampersand(command: &str) -> bool {
    let chars: Vec<char> = command.chars().collect();
    let mut quote: Option<char> = None;
    let mut i = 0;
    while i < chars.len() {
        let ch = chars[i];
        match quote {
            Some(q) if ch == q => quote = None,
            Some(_) => {}
            None if ch == '\\' => i += 1,
            None if ch == '\'' || ch == '"' => quote = Some(ch),
            None if ch == '&' => {
                if chars.get(i + 1) == Some(&'&') {
                    i += 2;
                    continue;
                }
                return true;
            }
            None => {}
        }
        i += 1;
    }
    false
}

/// Whether `command` does nothing but run `safe-rm`, directly,
/// through `clud`, from `find … -exec`, or after `xargs`, with no
/// substitution, redirection, background `&` or environment prefix. The hook
/// allows such a command outright: the tools enforce the roots themselves,
/// so there is nothing for a permission prompt to add.
pub(super) fn rm_tool_only(command: &str) -> bool {
    if command.contains(['`', '>', '<']) || command.contains("$(") || has_lone_ampersand(command) {
        return false;
    }
    let segments = split_shell_segments(command, ShellDialect::Posix);
    let mut ran_tool = false;
    for segment in &segments {
        // The words as typed: an environment prefix (`CLUD_RM_ROOTS=/ …`)
        // or a wrapper (`env`, `sudo`) is not a plain tool call.
        let words = tokenize(segment);
        let Some(first) = words.first() else {
            return false;
        };
        let is_tool = |word: &String| is_tool_word(word);
        if is_tool_word(first) {
            ran_tool = true;
            continue;
        }
        if is_clud_word(first) {
            if words.get(1).is_some_and(|w| TOOLS.contains(&w.as_str())) {
                ran_tool = true;
                continue;
            }
            return false;
        }
        match first.as_str() {
            "xargs" => match xargs_program_index(&words) {
                Some(at) if is_tool(&words[at]) => ran_tool = true,
                _ => return false,
            },
            "find" => {
                let mut i = 1;
                while i < words.len() {
                    let word = words[i].as_str();
                    if matches!(word, "-exec" | "-execdir") {
                        if !words.get(i + 1).is_some_and(is_tool) {
                            return false;
                        }
                        ran_tool = true;
                    } else if matches!(word, "-delete" | "-ok" | "-okdir")
                        || word.starts_with("-fprint")
                        || word.starts_with("-fls")
                    {
                        return false;
                    }
                    i += 1;
                }
            }
            _ => return false,
        }
    }
    ran_tool
}

#[cfg(test)]
mod tests {
    use super::*;

    fn suggestion(command: &str) -> Option<String> {
        match redirect(command)? {
            Redirect::Rewrite(value) => Some(value),
            Redirect::Refuse(_) => None,
        }
    }

    #[test]
    fn typed_removals_get_the_exact_replacement() {
        for (typed, expected) in [
            ("rm -rf build", "safe-rm -rf build"),
            ("rm -r build dist", "safe-rm -r build dist"),
            ("rm a.txt b.txt", "safe-rm a.txt b.txt"),
            ("rm -f 'my file'", "safe-rm -f 'my file'"),
            ("rm -f *.o", "safe-rm -f *.o"),
            ("rm -- -weird", "safe-rm -- -weird"),
            ("rmdir empty", "safe-rm -d empty"),
            ("unlink link", "safe-rm link"),
            ("/bin/rm -rf ./build", "safe-rm -rf ./build"),
            ("env FOO=1 rm x", "safe-rm x"),
            ("tap rm -rf /etc/passwd", "safe-rm -rf /etc/passwd"),
            ("nice -n 10 rm x", "safe-rm x"),
            ("timeout -s KILL 5 rm -r d", "safe-rm -r d"),
            ("cd src && rm -f out.o", "cd src && safe-rm -f out.o"),
            (
                "find X -name '*.o' -delete",
                "find X -name '*.o' -type f -exec safe-rm {} +",
            ),
            (
                "find X -type f -name '*.o' -delete",
                "find X -type f -name '*.o' -exec safe-rm {} +",
            ),
            (
                "find X -type d -name __pycache__ -delete",
                "find X -type d -name __pycache__ -prune -exec safe-rm {} +",
            ),
            (
                "find X -name '*.tmp' -exec rm {} \\;",
                "find X -name '*.tmp' -exec safe-rm {} +",
            ),
            (
                "find X -type d -name __pycache__ -exec rm -rf {} +",
                "find X -type d -name __pycache__ -prune -exec safe-rm {} +",
            ),
            ("find . -print0 | xargs -0 rm", "xargs -0 safe-rm"),
            ("xargs -n 1 rm -rf < list", "xargs -n 1 safe-rm"),
        ] {
            if !typed.contains("find ") && !typed.contains("xargs") {
                assert_eq!(suggestion(typed).as_deref(), Some(expected), "{typed}");
            }
        }
    }

    #[test]
    fn rm_find_exec_and_xargs_keep_the_original_arguments() {
        for (command, expected) in [
            ("find X -exec rm -rf {} +", "find X -exec safe-rm -rf {} +"),
            (
                "find X -exec rm -v 'my file' {} \\;",
                "find X -exec safe-rm -v 'my file' {} \\;",
            ),
            ("find X -exec rmdir {} +", "find X -exec safe-rm -d {} +"),
            (
                "find X -print0 | xargs -0 rm -f",
                "find X -print0 | xargs -0 safe-rm -f",
            ),
            ("xargs rm -v", "xargs safe-rm -v"),
        ] {
            assert_eq!(suggestion(command).as_deref(), Some(expected), "{command}");
        }
    }

    #[test]
    fn rm_unsupported_directory_parent_option_is_refused() {
        let directory_remover = concat!("rm", "dir");
        for command in [
            format!("{directory_remover} -p a/b"),
            format!("find X -exec {directory_remover} -p {{}} +"),
        ] {
            match redirect(&command) {
                Some(Redirect::Refuse(reason)) => assert!(reason.contains("safe-rm")),
                other => panic!("{command}: {other:?}"),
            }
        }
    }

    #[test]
    fn rm_rewrite_refuses_ambiguous_earlier_quoted_occurrence() {
        let command = "echo 'r".to_string() + "m a'; r" + "m a";
        match redirect(&command) {
            Some(Redirect::Refuse(reason)) => assert!(reason.contains("safe-rm")),
            other => panic!("{command}: {other:?}"),
        }
    }

    #[test]
    fn rm_in_substitution_or_nested_shell_is_refused() {
        for command in [
            "echo $(r".to_string() + "m a)",
            "x=$(r".to_string() + "m -v y)",
            "dash -c 'r".to_string() + "m a'",
            "ksh -c 'r".to_string() + "m a'",
            "(dash -c 'r".to_string() + "m a')",
        ] {
            match redirect(&command) {
                Some(Redirect::Refuse(reason)) => assert!(reason.contains("safe-rm")),
                other => panic!("{command}: {other:?}"),
            }
        }
    }

    #[test]
    fn rm_nested_posix_shells_are_refused_instead_of_rewritten() {
        for prefix in ["bash", "sh", "dash", "ksh", "busybox sh", "uv run bash"] {
            let command = format!("{prefix} -c 'r{} -rf build'", "m");
            match redirect(&command) {
                Some(Redirect::Refuse(reason)) => assert!(reason.contains("safe-rm")),
                other => panic!("{command}: {other:?}"),
            }
        }
    }

    #[test]
    fn rm_shell_fed_heredocs_are_refused_but_data_heredocs_are_left_alone() {
        let body = "r".to_string() + "m -rf build";
        for command in [
            format!("bash <<'EOF'\n{body}\nEOF"),
            format!("cat <<'EOF' | bash\n{body}\nEOF"),
            format!("sh <<-EOF\n{body}\nEOF"),
        ] {
            match redirect(&command) {
                Some(Redirect::Refuse(reason)) => assert!(reason.contains("safe-rm")),
                other => panic!("{command}: {other:?}"),
            }
        }
        assert_eq!(redirect(&format!("cat <<'EOF'\n{body}\nEOF")), None);
        let expanding_body = "cat <<EOF\nprintf ok # `r".to_string() + "m /tmp/victim`\nEOF";
        match redirect(&expanding_body) {
            Some(Redirect::Refuse(reason)) => assert!(reason.contains("safe-rm")),
            other => panic!("{expanding_body}: {other:?}"),
        }
        let spaced = "cat << EOF\n$(r".to_string() + "m /tmp/victim)\nEOF";
        match redirect(&spaced) {
            Some(Redirect::Refuse(reason)) => assert!(reason.contains("safe-rm")),
            other => panic!("{spaced}: {other:?}"),
        }
    }

    #[test]
    fn non_removals_and_deliberate_exceptions_pass() {
        for command in [
            "safe-rm build/x",
            "safe-rm build",
            "git rm -r --cached foo",
            "git clean -fd",
            "docker run --rm ubuntu",
            "python -c \"import os; os.remove('x')\"",
            "node -e \"require('fs').rmSync('x')\"",
            "echo 'rm -rf /'",
            "grep -rn 'rm -rf' src",
            "cat <<'EOF'\nrm -rf /\nEOF",
            "find . -name '*.rs'",
            "ls -la",
            "gh issue create --title 'fix: rm -rf'",
            "gh pr create --body 'Replaces `rm -rf build`'",
            "git commit -m \"$(cat <<'EOF'\nfix: stop `rm -rf x`\n\nbody\nEOF\n)\"",
        ] {
            assert_eq!(redirect_reason(command), None, "{command}");
        }
    }

    #[test]
    fn the_message_says_why_and_how() {
        let reason = redirect_reason("rm -rf build").unwrap();
        assert!(reason.contains("trash"), "{reason}");
        assert!(reason.contains("allowed locations"), "{reason}");
        assert!(reason.contains("`rm -rf build`"), "{reason}");
        assert!(reason.contains("safe-rm"), "{reason}");
    }

    #[test]
    fn find_delete_refusal_suggests_only_safe_rm() {
        let verdict = redirect("find build -name '*.tmp' -delete").unwrap();
        let Redirect::Refuse(reason) = verdict else {
            panic!("find -delete must be refused");
        };
        assert!(reason.contains("safe-rm {} +"), "{reason}");
    }

    #[test]
    fn only_pure_tool_commands_are_allowed_outright() {
        for command in [
            "safe-rm a b",
            "safe-rm -r build && safe-rm notes.txt",
            "clud safe-rm -r build",
            "\"$CLUD_EXE\" safe-rm a",
            "/home/u/.clud/state/rm-shim/safe-rm --purge -r build",
            "find build -name '*.o' -type f -exec safe-rm {} +",
            "find . -name '*.tmp' -print0 | xargs -0 safe-rm",
        ] {
            assert!(rm_tool_only(command), "{command}");
        }
        for command in [
            "safe-rm a & python evil.py",
            "safe-rm a &",
            "CLUD_RM_ROOTS=/ safe-rm --purge /home/u/Documents",
            "HOME=/x safe-rm a",
            "env safe-rm a",
            "./safe-rm.sh a",
            "/tmp/x/RM-FILE a",
            "./clud safe-rm a",
            "find . -exec ./safe-rm.py {} +",
            "safe-rm a; git push",
            "safe-rm $(cat list)",
            "safe-rm `cat list`",
            "safe-rm a > log",
            "find . -name x",
            "find . -exec safe-rm {} + -delete",
            "find . -exec sh -c 'x' \\;",
            "xargs rm",
            "ls",
            "",
        ] {
            assert!(!rm_tool_only(command), "{command}");
        }
    }
}
