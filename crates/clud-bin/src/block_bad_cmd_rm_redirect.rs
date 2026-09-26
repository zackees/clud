//! Redirect an agent's own deletion commands to `rm-file` / `rm-dir` (#1340).
//!
//! What an agent types as `rm`, `rmdir`, `unlink`, `find … -delete`,
//! `find … -exec rm …` or `xargs rm` is refused with the exact replacement
//! (`rm -rf build` → `rm-dir build`), so the agent learns the command
//! instead of having it rewritten behind its back. `rm-file` / `rm-dir`
//! trash by default and only delete inside the session's roots, and a
//! command made only of them is allowed outright ([`rm_tool_only`]), so
//! Claude Code never stops an unattended run on an `rm` prompt.
//!
//! Scope is what the agent typed. A script's own `rm` reaches the child `rm`
//! shim, which allows it inside the session's roots (`rm_guard`). `git rm`,
//! `git clean` and inline `python -c`/`node -e` deletions are deliberately
//! left alone. See `docs/architecture/rm-tools.md`.

use super::block_bad_cmd_shell::{
    command_words, nested_shell_command, posix_c_shell_script, program_name, split_shell_segments,
    tokenize,
};
use super::{strip_heredoc_bodies, ShellDialect};

const REMOVERS: &[&str] = &["rm", "rmdir", "unlink"];

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
/// `rm-file` / `rm-dir` command to run instead; `None` when it does not.
pub(super) fn redirect_reason(command: &str) -> Option<String> {
    let (original, suggestion) = find_removal(command, 0)?;
    Some(format!(
        "clud routes deletion through `rm-file` (files) and `rm-dir` (directories) (#1340): \
         they move paths to the clud trash (`--purge` deletes for real, `--tracked` allows \
         git-tracked paths) and only inside this session's roots, and they run without an rm \
         prompt. Instead of `{original}`, run:\n  {suggestion}\n(`clud rm-file` / `clud \
         rm-dir` work the same if the aliases are not on PATH. Scripts you run, such as \
         ./test or make, may still use rm inside the session's roots.)"
    ))
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
    if REMOVERS.contains(&program.as_str()) {
        return Some(remover_replacement(&program, &words[1..], &raw[1..]));
    }
    match program.as_str() {
        "find" => find_suggestion(words, raw),
        "xargs" => {
            let at = xargs_program_index(words)?;
            let inner = program_name(&words[at]);
            REMOVERS.contains(&inner.as_str()).then(|| {
                let mut out: Vec<String> = raw[..at].to_vec();
                out.push(tool_for(&inner, &words[at + 1..]).to_string());
                out.join(" ")
            })
        }
        _ => None,
    }
}

/// `rm-dir` for `rmdir` or a recursive `rm`, else `rm-file`.
fn tool_for(program: &str, args: &[String]) -> &'static str {
    let recursive = program == "rmdir"
        || args.iter().take_while(|a| a.as_str() != "--").any(|a| {
            a == "--recursive"
                || (a.starts_with('-') && !a.starts_with("--") && a.contains(['r', 'R']))
        });
    if recursive {
        "rm-dir"
    } else {
        "rm-file"
    }
}

fn remover_replacement(program: &str, args: &[String], raw: &[String]) -> String {
    let mut operands: Vec<String> = Vec::new();
    let mut dashed = false;
    let mut options = true;
    for (arg, typed) in args.iter().zip(raw) {
        if options && arg == "--" {
            options = false;
            continue;
        }
        if options && arg.starts_with('-') && arg != "-" {
            continue;
        }
        dashed |= arg.starts_with('-');
        operands.push(typed.clone());
    }
    let mut out = vec![tool_for(program, args).to_string()];
    if dashed {
        out.push("--".into());
    }
    out.extend(operands);
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
                out.extend(["-prune", "-exec", "rm-dir", "{}", "+"].map(String::from));
            } else {
                if !has_type {
                    out.extend(["-type", "f"].map(String::from));
                }
                out.extend(["-exec", "rm-file", "{}", "+"].map(String::from));
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
            if REMOVERS.contains(&inner.as_str()) {
                changed = true;
                let tool = tool_for(&inner, &body[1..]);
                if tool == "rm-dir" {
                    out.push("-prune".into());
                }
                out.extend([raw[i].clone(), tool.to_string(), "{}".into(), "+".into()]);
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

const TOOLS: &[&str] = &["rm-file", "rm-dir"];

/// Whether `word` names clud's own `rm-file` / `rm-dir` exactly: the bare
/// alias (resolved from clud's rm-shim directory, which the identity check
/// keeps first on PATH) or its path in that directory. `./rm-file.sh` or a
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

/// Whether `command` does nothing but run `rm-file` / `rm-dir`, directly,
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
        let reason = redirect_reason(command)?;
        let line = reason.lines().nth(1)?.trim().to_string();
        Some(line)
    }

    #[test]
    fn typed_removals_get_the_exact_replacement() {
        for (typed, expected) in [
            ("rm -rf build", "rm-dir build"),
            ("rm -r build dist", "rm-dir build dist"),
            ("rm a.txt b.txt", "rm-file a.txt b.txt"),
            ("rm -f 'my file'", "rm-file 'my file'"),
            ("rm -rf \"$UNSET\"/*", "rm-dir \"$UNSET\"/*"),
            ("rm -f *.o", "rm-file *.o"),
            ("rm -- -weird", "rm-file -- -weird"),
            ("rmdir empty", "rm-dir empty"),
            ("unlink link", "rm-file link"),
            ("/bin/rm -rf ./build", "rm-dir ./build"),
            ("sudo rm -rf /tmp/x", "rm-dir /tmp/x"),
            ("env FOO=1 rm x", "rm-file x"),
            ("tap rm -rf /etc/passwd", "rm-dir /etc/passwd"),
            ("nice -n 10 rm x", "rm-file x"),
            ("timeout -s KILL 5 rm -r d", "rm-dir d"),
            ("sudo -u root rm x", "rm-file x"),
            ("echo \"`rm -f x`\"", "rm-file x"),
            ("cd src && rm -f out.o", "rm-file out.o"),
            ("bash -c 'rm -rf x'", "rm-dir x"),
            ("echo $(rm -f x)", "rm-file x"),
            (
                "find X -name '*.o' -delete",
                "find X -name '*.o' -type f -exec rm-file {} +",
            ),
            (
                "find X -type f -name '*.o' -delete",
                "find X -type f -name '*.o' -exec rm-file {} +",
            ),
            (
                "find X -type d -name __pycache__ -delete",
                "find X -type d -name __pycache__ -prune -exec rm-dir {} +",
            ),
            (
                "find X -name '*.tmp' -exec rm {} \\;",
                "find X -name '*.tmp' -exec rm-file {} +",
            ),
            (
                "find X -type d -name __pycache__ -exec rm -rf {} +",
                "find X -type d -name __pycache__ -prune -exec rm-dir {} +",
            ),
            ("find . -print0 | xargs -0 rm", "xargs -0 rm-file"),
            ("xargs -n 1 rm -rf < list", "xargs -n 1 rm-dir"),
        ] {
            assert_eq!(suggestion(typed).as_deref(), Some(expected), "{typed}");
        }
    }

    #[test]
    fn non_removals_and_deliberate_exceptions_pass() {
        for command in [
            "rm-file build/x",
            "rm-dir build",
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
        assert!(reason.contains("roots"), "{reason}");
        assert!(reason.contains("`rm -rf build`"), "{reason}");
        assert!(reason.contains("clud rm-file"), "{reason}");
    }

    #[test]
    fn only_pure_tool_commands_are_allowed_outright() {
        for command in [
            "rm-file a b",
            "rm-dir build && rm-file notes.txt",
            "clud rm-dir build",
            "\"$CLUD_EXE\" rm-file a",
            "/home/u/.clud/state/rm-shim/rm-dir --purge build",
            "find build -name '*.o' -type f -exec rm-file {} +",
            "find . -type d -name __pycache__ -prune -exec rm-dir {} +",
            "find . -name '*.tmp' -print0 | xargs -0 rm-file",
        ] {
            assert!(rm_tool_only(command), "{command}");
        }
        for command in [
            "rm-file a & python evil.py",
            "rm-file a &",
            "CLUD_RM_ROOTS=/ rm-dir --purge /home/u/Documents",
            "HOME=/x rm-file a",
            "env rm-file a",
            "./rm-file.sh a",
            "/tmp/x/RM-FILE a",
            "./clud rm-file a",
            "find . -exec ./rm-file.py {} +",
            "rm-file a; git push",
            "rm-file $(cat list)",
            "rm-file `cat list`",
            "rm-file a > log",
            "find . -name x",
            "find . -exec rm-file {} + -delete",
            "find . -exec sh -c 'x' \\;",
            "xargs rm",
            "ls",
            "",
        ] {
            assert!(!rm_tool_only(command), "{command}");
        }
    }
}
