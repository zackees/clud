use super::*;

pub(super) fn split_pipeline_groups(command_text: &str, dialect: ShellDialect) -> Vec<Vec<String>> {
    let chars = command_text.chars().collect::<Vec<_>>();
    let mut groups = Vec::new();
    let mut group = Vec::new();
    let mut buf = String::new();
    let mut quote: Option<char> = None;
    let mut i = 0usize;

    let push_stage = |buf: &mut String, group: &mut Vec<String>| {
        let stage = buf.trim();
        if !stage.is_empty() {
            group.push(stage.to_string());
        }
        buf.clear();
    };
    let push_group = |group: &mut Vec<String>, groups: &mut Vec<Vec<String>>| {
        if !group.is_empty() {
            groups.push(std::mem::take(group));
        }
    };

    while i < chars.len() {
        let ch = chars[i];
        if let Some(q) = quote {
            buf.push(ch);
            if q != '\'' && is_shell_escape(ch, dialect) && i + 1 < chars.len() {
                buf.push(chars[i + 1]);
                i += 2;
                continue;
            }
            if ch == q {
                quote = None;
            }
            i += 1;
            continue;
        }
        if ch == '\'' || ch == '"' {
            quote = Some(ch);
            buf.push(ch);
            i += 1;
            continue;
        }
        if is_shell_escape(ch, dialect) && i + 1 < chars.len() {
            buf.push(ch);
            buf.push(chars[i + 1]);
            i += 2;
            continue;
        }
        if ch == '#' && dialect != ShellDialect::Cmd && is_shell_comment_start(&chars, i) {
            while i < chars.len() && !matches!(chars[i], '\r' | '\n') {
                i += 1;
            }
            continue;
        }

        let double_amp = ch == '&' && i + 1 < chars.len() && chars[i + 1] == '&';
        let double_pipe = ch == '|' && i + 1 < chars.len() && chars[i + 1] == '|';
        if ch == '|' && !double_pipe {
            push_stage(&mut buf, &mut group);
            i += 1;
            continue;
        }
        if matches!(ch, ';' | '\r' | '\n') || double_amp || double_pipe {
            push_stage(&mut buf, &mut group);
            push_group(&mut group, &mut groups);
            i += if double_amp || double_pipe { 2 } else { 1 };
            continue;
        }
        buf.push(ch);
        i += 1;
    }
    push_stage(&mut buf, &mut group);
    push_group(&mut group, &mut groups);
    groups
}

pub(super) fn split_shell_segments(command_text: &str, dialect: ShellDialect) -> Vec<String> {
    let chars = command_text.chars().collect::<Vec<_>>();
    let mut segments = Vec::new();
    let mut buf = String::new();
    let mut quote: Option<char> = None;
    let mut loop_header_paren_depth = 0usize;
    let mut i = 0usize;
    while i < chars.len() {
        let ch = chars[i];
        if let Some(q) = quote {
            buf.push(ch);
            if q != '\'' && is_shell_escape(ch, dialect) && i + 1 < chars.len() {
                buf.push(chars[i + 1]);
                i += 2;
                continue;
            }
            if ch == q {
                quote = None;
            }
            i += 1;
            continue;
        }

        if ch == '\'' || ch == '"' {
            quote = Some(ch);
            buf.push(ch);
            i += 1;
            continue;
        }
        if is_shell_escape(ch, dialect) && i + 1 < chars.len() {
            buf.push(ch);
            buf.push(chars[i + 1]);
            i += 2;
            continue;
        }
        if loop_header_paren_depth > 0 {
            buf.push(ch);
            if ch == '(' {
                loop_header_paren_depth += 1;
            } else if ch == ')' {
                loop_header_paren_depth -= 1;
            }
            i += 1;
            continue;
        }
        if ch == '(' && buf.trim().eq_ignore_ascii_case("for") {
            loop_header_paren_depth = 1;
            buf.push(ch);
            i += 1;
            continue;
        }
        if ch == '#' && dialect != ShellDialect::Cmd && is_shell_comment_start(&chars, i) {
            while i < chars.len() && !matches!(chars[i], '\r' | '\n') {
                i += 1;
            }
            continue;
        }

        let is_double_amp = ch == '&' && i + 1 < chars.len() && chars[i + 1] == '&';
        let is_double_pipe = ch == '|' && i + 1 < chars.len() && chars[i + 1] == '|';
        if matches!(ch, ';' | '|' | '\r' | '\n') || is_double_amp {
            let segment = buf.trim();
            if !segment.is_empty() {
                segments.push(segment.to_string());
            }
            buf.clear();
            i += if is_double_amp || is_double_pipe {
                2
            } else {
                1
            };
            continue;
        }

        buf.push(ch);
        i += 1;
    }

    let segment = buf.trim();
    if !segment.is_empty() {
        segments.push(segment.to_string());
    }
    segments
}

pub(super) fn is_shell_comment_start(chars: &[char], index: usize) -> bool {
    index == 0
        || chars[index - 1].is_whitespace()
        || matches!(chars[index - 1], ';' | '|' | '&' | '(' | ')')
}

pub(super) fn is_shell_escape(ch: char, dialect: ShellDialect) -> bool {
    matches!(
        (ch, dialect),
        ('\\', ShellDialect::Posix) | ('`', ShellDialect::PowerShell) | ('^', ShellDialect::Cmd)
    )
}

pub(super) fn tokenize(segment: &str) -> Vec<String> {
    let mut words = Vec::new();
    let mut buf = String::new();
    let mut quote: Option<char> = None;
    for ch in segment.chars() {
        if let Some(q) = quote {
            if ch == q {
                quote = None;
            } else {
                buf.push(ch);
            }
            continue;
        }
        if ch == '\'' || ch == '"' {
            quote = Some(ch);
            continue;
        }
        if ch.is_whitespace() {
            if !buf.is_empty() {
                words.push(std::mem::take(&mut buf));
            }
            continue;
        }
        buf.push(ch);
    }
    if !buf.is_empty() {
        words.push(buf);
    }
    words
}

pub(super) fn program_name(word: &str) -> String {
    let cleaned = word.trim().trim_matches(&['\'', '"'][..]);
    crate::path_norm::file_stem_any_separator(cleaned)
        .unwrap_or_default()
        .to_ascii_lowercase()
}

pub(super) fn command_words(segment: &str) -> Vec<String> {
    let mut words = tokenize(segment);
    while words
        .first()
        .is_some_and(|word| ["&", "call"].contains(&word.as_str()))
    {
        words.remove(0);
    }
    while words.first().is_some_and(|word| is_env_assignment(word)) {
        words.remove(0);
    }
    unwrap_transparent_wrappers(&words).unwrap_or_default()
}

pub(super) fn is_env_assignment(word: &str) -> bool {
    let mut chars = word.chars();
    let Some(first) = chars.next() else {
        return false;
    };
    if !(first == '_' || first.is_ascii_alphabetic()) {
        return false;
    }
    for ch in chars {
        if ch == '=' {
            return true;
        }
        if !(ch == '_' || ch.is_ascii_alphanumeric()) {
            return false;
        }
    }
    false
}

pub(super) fn resolve_uv_run_tool(words: &[String]) -> Option<String> {
    if words.len() < 3 || program_name(&words[0]) != "uv" || words[1] != "run" {
        return None;
    }
    let mut i = 2usize;
    while i < words.len() {
        let word = &words[i];
        if word == "--" {
            i += 1;
            break;
        }
        if word == "--script" && i + 1 < words.len() {
            return Some(words[i + 1].clone());
        }
        if let Some(value) = word.strip_prefix("--script=") {
            return Some(value.to_string());
        }
        if !word.starts_with('-') {
            break;
        }
        let consumes_value = (!word.contains('=') && contains_str(UV_RUN_OPTIONS_WITH_VALUE, word))
            || contains_str(UV_RUN_SHORT_OPTIONS_WITH_VALUE, word);
        if consumes_value {
            i += 2;
        } else {
            i += 1;
        }
    }
    words.get(i).cloned()
}

pub(super) fn nested_shell_command(
    words: &[String],
    current_dialect: ShellDialect,
) -> Option<(String, ShellDialect)> {
    let first = program_name(words.first()?);
    if !contains_str(SHELL_WRAPPERS, &first) {
        return None;
    }
    if first == "eval" {
        if words.len() > 1 {
            return Some((words[1..].join(" "), current_dialect));
        }
        return None;
    }
    if first == "cmd" {
        for (i, word) in words.iter().enumerate().skip(1) {
            if ["/c", "/k", "/r"].contains(&word.to_ascii_lowercase().as_str())
                && i + 1 < words.len()
            {
                return Some((words[i + 1..].join(" "), ShellDialect::Cmd));
            }
        }
        return None;
    }
    if first == "powershell" || first == "pwsh" {
        for (i, word) in words.iter().enumerate().skip(1) {
            if ["-command", "-c", "/c"].contains(&word.to_ascii_lowercase().as_str())
                && i + 1 < words.len()
            {
                return Some((words[i + 1..].join(" "), ShellDialect::PowerShell));
            }
        }
        return None;
    }

    for (i, word) in words.iter().enumerate().skip(1) {
        let option = word.to_ascii_lowercase();
        let option = option.trim_start_matches('-');
        if option.contains('c') && i + 1 < words.len() {
            return Some((words[i + 1..].join(" "), ShellDialect::Posix));
        }
    }
    None
}

/// POSIX shells that execute a script passed via `-c`. Broader than
/// [`SHELL_WRAPPERS`] because #1082 must also reach `dash`/`ksh` (and the
/// busybox multiplexers), which are not among the dialect-selecting wrappers.
pub(super) const POSIX_C_SHELLS: &[&str] =
    &["bash", "sh", "zsh", "dash", "ksh", "ash", "mksh", "busybox"];

/// Extract the script text passed to a POSIX shell via `-c`, first unwrapping
/// leading launchers (`uv run …`, `busybox`, `find … -exec … \;`). Returns
/// only the script operand (word after `-c`), so the caller can run the rm
/// resolver against it — the recursion in [`nested_shell_command`] reaches the
/// denylist path but never the rm-variable resolver, which is exactly the
/// #1082 bypass (`dash -c 'rm -rf "$V"/'` and friends sailed through).
pub(super) fn posix_c_shell_script(words: &[String]) -> Option<String> {
    let unwrapped = strip_command_launchers(words);
    let words: &[String] = unwrapped.as_deref().unwrap_or(words);
    let first = program_name(words.first()?);
    // `busybox sh -c …` invokes the applet named by the *next* word; a bare
    // `busybox -c` is not a shell invocation.
    if first == "busybox" {
        return None;
    }
    if !contains_str(POSIX_C_SHELLS, &first) {
        return None;
    }
    for (i, word) in words.iter().enumerate().skip(1) {
        // Stop at the script operand; anything before `-c` is a shell option.
        let option = word.to_ascii_lowercase();
        let option = option.trim_start_matches('-');
        if word.starts_with('-') && option.contains('c') && i + 1 < words.len() {
            return Some(words[i + 1].clone());
        }
    }
    None
}

/// Peel leading command launchers so [`posix_c_shell_script`] sees the real
/// shell head. Handles `busybox <applet …>`, `uv run [opts] <cmd …>`, and
/// `find … -exec <cmd …> \;`/`+`. Returns `None` when a launcher is present
/// but its wrapped command cannot be located.
fn strip_command_launchers(words: &[String]) -> Option<Vec<String>> {
    let mut current = words.to_vec();
    for _ in 0..8 {
        let first = program_name(current.first()?);
        match first.as_str() {
            "busybox" => {
                // `busybox sh -c …` → `sh -c …`.
                current = current.get(1..)?.to_vec();
                if current.is_empty() {
                    return None;
                }
            }
            "uv" if current.get(1).map(String::as_str) == Some("run") => {
                current = uv_run_remaining(&current)?;
            }
            "find" => {
                current = find_exec_command(&current)?;
            }
            _ => return Some(current),
        }
    }
    None
}

/// The argv `uv run [options] -- <cmd> [args]` ultimately launches. Mirrors
/// [`resolve_uv_run_tool`]'s option-skipping but returns the whole remaining
/// slice rather than only the tool word. `--script <file>` runs a file, not a
/// shell head we can extract, so it yields `None`.
fn uv_run_remaining(words: &[String]) -> Option<Vec<String>> {
    let mut i = 2usize;
    while i < words.len() {
        let word = &words[i];
        if word == "--" {
            i += 1;
            break;
        }
        if word == "--script" || word.starts_with("--script=") {
            return None;
        }
        if !word.starts_with('-') {
            break;
        }
        let consumes_value = (!word.contains('=') && contains_str(UV_RUN_OPTIONS_WITH_VALUE, word))
            || contains_str(UV_RUN_SHORT_OPTIONS_WITH_VALUE, word);
        if consumes_value {
            i += 2;
        } else {
            i += 1;
        }
    }
    let rest = words.get(i..)?.to_vec();
    if rest.is_empty() {
        None
    } else {
        Some(rest)
    }
}

/// The command `find … -exec <cmd …> ;`/`+` would run, as a word slice. The
/// terminator (`;`, an escaped `\;`, or `+`) and everything after it are
/// dropped.
fn find_exec_command(words: &[String]) -> Option<Vec<String>> {
    let mut i = 1usize;
    while i < words.len() {
        if matches!(words[i].as_str(), "-exec" | "-execdir" | "-ok" | "-okdir") {
            let start = i + 1;
            let mut j = start;
            while j < words.len() && !matches!(words[j].as_str(), ";" | "\\;" | "+") {
                j += 1;
            }
            if j > start {
                return Some(words[start..j].to_vec());
            }
            return None;
        }
        i += 1;
    }
    None
}

pub(super) fn python_rust_hybrid_root(cwd: Option<&Path>) -> Option<PathBuf> {
    let anchor = cwd?.canonicalize().ok()?;
    for candidate in std::iter::once(anchor.as_path()).chain(anchor.ancestors().skip(1)) {
        if candidate.join("pyproject.toml").is_file() && candidate.join("Cargo.toml").is_file() {
            return Some(candidate.to_path_buf());
        }
    }
    None
}

pub(super) fn hybrid_bypass_warning(hybrid_root: &Path) -> String {
    format!(
        "\x1b[33mWARNING: AUTO COMPILING RUST because of uv run\n\
CLUD_UV_RUST_ALLOW_ALL=1 is set, so the auto-sync gate at {} was bypassed.\n\
DIRECTIVE TO AGENT: the next `uv run` in this project root will trigger a full native rebuild (can take minutes). \
If you don't need a fresh build, pass `--no-sync` (use existing venv), `--no-project` (pure-Python script), or \
`--frozen` (lock to existing lockfile) to skip the auto-sync. If you DO need a clean rebuild, prefer `./test` \
(or `bash ./test`) - the canonical full-build entrypoint.\x1b[0m",
        hybrid_root.display()
    )
}

pub(super) fn contains_str(haystack: &[&str], needle: &str) -> bool {
    haystack.iter().any(|item| item == &needle)
}
