//! Owner-sanctioned effective-PATH contract (#1183). Never execute source.
use super::*;

pub(super) fn identity_reason(path_env: &str, trusted: &Path) -> Result<(), String> {
    // Relative/empty components depend on shell cwd, including subsequent cd.
    // Refuse them instead of resolving against the hook process's cwd.
    if path_env.is_empty() || std::env::split_paths(path_env).any(|p| !p.is_absolute()) {
        return Err(
            "rm identity: effective PATH is missing or contains relative/empty entries".into(),
        );
    }
    let found = crate::shim_resolve::which("rm", path_env)
        .ok_or("rm identity: no executable rm on the effective PATH")?;
    let expected = std::fs::read(trusted)
        .map_err(|e| format!("rm identity: packaged clud-shim is unreadable: {e}"))?;
    let actual = std::fs::read(&found)
        .map_err(|e| format!("rm identity: selected rm is unreadable: {e}"))?;
    if expected.is_empty() || actual != expected {
        return Err(format!(
            "rm identity: {} is not byte-identical to packaged clud-shim",
            found.display()
        ));
    }
    Ok(())
}

#[cfg(test)]
pub(super) fn source_reason(command: &str) -> Result<(), String> {
    source_reason_with_tap(command, false, false)
}

fn source_reason_with_tap(
    command: &str,
    trusted_tap: bool,
    rg_configured: bool,
) -> Result<(), String> {
    let refuse = || "rm identity: command changes or bypasses provable shim resolution".to_string();
    // A single plain subshell inherits PATH. Check every inner statement with
    // the same rules; extra/nested parentheses remain unsupported.
    if let Some(inner) = command
        .trim()
        .strip_prefix('(')
        .and_then(|s| s.strip_suffix(')'))
    {
        if inner.contains(['(', ')']) {
            return Err(refuse());
        }
        return source_reason_with_tap(inner, trusted_tap, rg_configured);
    }
    if contains_removal_in_command_substitution(command) {
        return Err(
            "rm identity: a command substitution runs rm, which could resolve to \
                    something other than clud's shim; run the removal as its own command \
                    with literal paths (rm-file / rm-dir)"
                .into(),
        );
    }
    // #1305: a backtick the scanner cannot prove inert only matters when the
    // command could run a removal through it. Prose in an issue title or a
    // grep pattern cannot change which rm runs.
    let has_backticks = command.as_bytes().contains(&96);
    let opaque_backticks = contains_active_backtick_substitution(command)
        || (has_backticks && !literal_backticks_are_data_only(command, rg_configured));
    if opaque_backticks
        && (contains_unquoted_removal_program(command)
            || contains_unquoted_dynamic_shell_program(command))
    {
        return Err(
            "rm identity: a backtick substitution appears in a command that runs rm \
                    or a nested shell; use $(...) or run the removal as its own command"
                .into(),
        );
    }
    // Opaque shell structure matters only when it can execute a removal. A
    // command substitution used to obtain documentation, or a for loop that
    // prints values, cannot alter rm resolution. Conversely, keep failing
    // closed when an unquoted removal program appears inside syntax we do not
    // model statement-by-statement.
    let statements = match block_bad_cmd_rm_vars::identity_statements(command) {
        Ok(statements) => statements,
        Err(())
            if !contains_unquoted_removal_program(command)
                && !contains_unquoted_dynamic_shell_program(command) =>
        {
            return Ok(())
        }
        Err(()) => return Err(refuse()),
    };
    for segment in statements {
        // Every `$(...)` body was checked for a removal above; mask it so
        // `n=$(basename $u)` is one assignment word, not `$u)` as a program
        // (#1305). The mask keeps its `$`, so a substitution in program
        // position is still refused below.
        let masked = mask_command_substitutions(segment);
        let words = match shell_words::split(masked.trim()) {
            Ok(words) => words,
            // `shell_words` intentionally does not model Bash's ANSI-C
            // `$'...'` arguments. The statement scanner above does, and can
            // already establish that this opaque argument cannot execute a
            // removal or a dynamic shell program. Treating a prose body as
            // executable source here was the #1228 regression.
            Err(_)
                if !contains_unquoted_removal_program(segment)
                    && !contains_unquoted_dynamic_shell_program(segment) =>
            {
                continue;
            }
            Err(_) => return Err(refuse()),
        };
        let mut index = 0;
        while words.get(index).is_some_and(|w| is_env_assignment(w)) {
            let name = words[index].split('=').next().unwrap_or_default();
            if resolution_variable(name) {
                return Err(refuse());
            }
            index += 1;
        }
        // Packaged tap's CommandSpec::Argv forwards this exact environment.
        // Its identity must also be established before treating it as transparent.
        if trusted_tap
            && words
                .get(index)
                .is_some_and(|w| w == "tap" || w == "tap.exe")
        {
            index += 1;
        }
        let Some(program) = words.get(index) else {
            continue;
        };
        let base = program_name(program);
        if program == "."
            || program.contains([
                '$', '`', '\\', '(', ')', '{', '}', '*', '?', '[', ']', '~', '!',
            ])
        {
            return Err(refuse());
        }
        if base == "rm" && !matches!(program.as_str(), "rm" | "rm.exe") {
            return Err(refuse());
        }
        if matches!(
            base.as_str(),
            "hash"
                | "alias"
                | "unalias"
                | "source"
                | "."
                | "enable"
                | "function"
                | "eval"
                | "builtin"
                | "command"
                | "trap"
                | "bind"
                | "complete"
                | "compgen"
                | "fc"
                | "read"
                | "mapfile"
                | "readarray"
                | "getopts"
                | "declare"
                | "typeset"
                | "local"
                | "let"
        ) {
            return Err(refuse());
        }
        // A shell `-c` body executes in a new interpreter context. This
        // checker cannot prove that startup state preserves the verified rm
        // resolution, so keep that wrapper fail-closed.
        if matches!(base.as_str(), "bash" | "sh" | "zsh")
            && words[index + 1..].iter().any(|word| word == "-c")
        {
            return Err(refuse());
        }
        if matches!(
            base.as_str(),
            "export"
                | "unset"
                | "readonly"
                | "declare"
                | "typeset"
                | "local"
                | "read"
                | "mapfile"
                | "readarray"
                | "getopts"
        ) && words[index + 1..].iter().any(|w| {
            w.contains(['$', '[', ']'])
                || resolution_variable(
                    w.split('=')
                        .next()
                        .unwrap_or_default()
                        .trim_end_matches('+'),
                )
                || (w.starts_with('-') && w.contains('n'))
        }) {
            return Err(refuse());
        }
        // printf -v and %n assign shell variables, including PATH. Only a
        // statically known format with ordinary output conversions is provable.
        if base == "printf" {
            let format_index = index
                + if words.get(index + 1).is_some_and(|w| w == "--") {
                    2
                } else {
                    1
                };
            if words[index + 1..].iter().any(|w| w.starts_with("-v"))
                || !words
                    .get(format_index)
                    .is_some_and(|w| printf_format_is_output_only(w))
            {
                return Err(refuse());
            }
        }
        if !matches!(base.as_str(), "echo" | "printf")
            && words[index + 1..].iter().any(|w| {
                w.split_once('=')
                    .is_some_and(|(name, _)| resolution_variable(name.trim_end_matches('+')))
            })
        {
            return Err(refuse());
        }
        if matches!(base.as_str(), "env" | "exec" | "sudo" | "su")
            && words[index + 1..]
                .iter()
                .any(|w| w.contains(['$', '[', ']']))
        {
            return Err(refuse());
        }
        // Wrappers can select a different PATH, shell startup state, or an
        // internal applet. Only a direct rm command has the checked contract.
        if base != "rm"
            && !matches!(base.as_str(), "echo" | "printf")
            && !(base == "git" && words.get(index + 1).is_some_and(|w| w == "rm"))
            && words[index + 1..].iter().any(|w| program_name(w) == "rm")
        {
            return Err(refuse());
        }
        if matches!(base.as_str(), "env" | "sudo" | "su" | "exec" | "command")
            && words[index + 1..].iter().any(|w| {
                w.starts_with("PATH=") || w == "-p" || w == "-i" || w == "--ignore-environment"
            })
        {
            return Err(refuse());
        }
    }
    Ok(())
}

/// Active legacy command substitutions remain fail-closed. Quoting and escaping
/// are modeled; comments and other opaque shell constructs stay fail-closed.
fn contains_active_backtick_substitution(command: &str) -> bool {
    #[derive(Clone, Copy, PartialEq, Eq)]
    enum Quote {
        Single,
        AnsiC,
        Double,
    }

    let bytes = command.as_bytes();
    let mut quote = None::<Quote>;
    let mut index = 0;
    while index < bytes.len() {
        let byte = bytes[index];
        match quote {
            Some(Quote::Single) => {
                if byte == b'\'' {
                    quote = None;
                }
                index += 1;
            }
            Some(Quote::AnsiC) => match byte {
                b'\\' => {
                    index = (index + 2).min(bytes.len());
                }
                b'\'' => {
                    quote = None;
                    index += 1;
                }
                _ => index += 1,
            },
            Some(Quote::Double) => match byte {
                b'\\' => {
                    if bytes
                        .get(index + 1)
                        .is_some_and(|next| matches!(next, b'$' | 96 | b'"' | b'\\' | b'\n'))
                    {
                        index += 2;
                    } else {
                        index += 1;
                    }
                }
                b'"' => {
                    quote = None;
                    index += 1;
                }
                96 => return true,
                _ => index += 1,
            },
            None => match byte {
                b'\\' => {
                    if bytes.get(index + 1) == Some(&b'\n') {
                        index += 2;
                    } else {
                        index = (index + 2).min(bytes.len());
                    }
                }
                b'\'' => {
                    quote = Some(if is_ansi_c_quote_start(bytes, index) {
                        Quote::AnsiC
                    } else {
                        Quote::Single
                    });
                    index += 1;
                }
                b'"' => {
                    quote = Some(Quote::Double);
                    index += 1;
                }
                b'<' if bytes.get(index + 1) == Some(&b'<')
                    && bytes.get(index + 2) != Some(&b'<') =>
                {
                    // Here-doc bodies have different quote rules; without
                    // parsing delimiters, fail closed on any later backtick.
                    if bytes[index + 2..].contains(&96) {
                        return true;
                    }
                    index += 2;
                }
                96 => return true,
                _ => index += 1,
            },
        }
    }
    false
}

fn is_ansi_c_quote_start(bytes: &[u8], quote_index: usize) -> bool {
    if quote_index == 0 || bytes[quote_index - 1] != b'$' {
        return false;
    }
    let mut cursor = quote_index - 1;
    let mut backslashes = 0;
    while cursor > 0 && bytes[cursor - 1] == b'\\' {
        backslashes += 1;
        cursor -= 1;
    }
    backslashes % 2 == 0
}

fn literal_backticks_are_data_only(command: &str, rg_configured: bool) -> bool {
    let Ok(statements) = block_bad_cmd_rm_vars::identity_statements(command) else {
        return false;
    };
    !statements.is_empty()
        && statements.iter().all(|statement| {
            let Ok(words) = shell_words::split(statement.trim()) else {
                return false;
            };
            let Some(program) = words.first().map(|word| program_name(word)) else {
                return false;
            };
            match program.as_str() {
                "rg" => {
                    // Require this first so it cannot be consumed as another
                    // option's value or treated as a path after `--`.
                    let no_config = words.get(1).is_some_and(|word| word == "--no-config");
                    let has_config = words
                        .iter()
                        .any(|word| word == "--config" || word.starts_with("--config="));
                    let has_preprocessor = words.iter().any(|word| {
                        word == "--pre" || word == "--pre-glob" || word.starts_with("--pre=")
                    });
                    !has_preprocessor && !has_config && (no_config || !rg_configured)
                }
                "gh" => {
                    let has_body = words.iter().any(|word| {
                        matches!(word.as_str(), "-b" | "--body" | "-F" | "--body-file")
                    });
                    let launches_editor_or_browser = words.iter().any(|word| {
                        matches!(word.as_str(), "-e" | "--editor" | "-w" | "--web")
                            || word.starts_with("--editor=")
                    });
                    has_body
                        && !launches_editor_or_browser
                        && words.get(1).is_some_and(|word| word == "issue")
                        && words.get(2).is_some_and(|word| word == "comment")
                }
                "echo" | "printf" => true,
                _ => false,
            }
        })
}

/// True when an unquoted shell word names the protected removal executable.
/// Arguments are data: `rg 'rm identity'` is not an invocation, while
/// `for x; do rm x; done` remains relevant even though the flat statement
/// analyzer intentionally refuses to model control flow.
fn contains_unquoted_removal_program(command: &str) -> bool {
    contains_unquoted_removal_program_at_depth(command, 0)
}

/// `eval` and a nested shell can execute quoted text. If the statement splitter
/// cannot model surrounding control flow, their presence is still enough to
/// make the PATH contract unprovable.
fn contains_unquoted_dynamic_shell_program(command: &str) -> bool {
    let mut quote = None::<char>;
    let mut escaped = false;
    let mut word = String::new();
    let mut at_program_start = true;
    let mut quoted_program = false;
    // Depth inside an unquoted `${...}` parameter expansion, which belongs
    // to the current word: its braces are not a brace group (#1305).
    let mut expansion_depth = 0usize;
    for character in command.chars() {
        if quote.is_none() && !escaped {
            if expansion_depth > 0 {
                word.push(character);
                match character {
                    '{' => expansion_depth += 1,
                    '}' => expansion_depth -= 1,
                    _ => {}
                }
                continue;
            }
            if character == '{' && word.ends_with('$') {
                word.push(character);
                expansion_depth = 1;
                continue;
            }
        }
        if escaped {
            if quote.is_none() {
                word.push(character);
            }
            escaped = false;
            continue;
        }
        match quote {
            Some('\'') if character == '\'' => quote = None,
            Some('\'') => {}
            Some('"') if character == '"' => quote = None,
            Some('"') if character == '\\' => escaped = true,
            Some('"') => {}
            Some(_) => unreachable!(),
            None if character == '\'' || character == '"' => {
                quoted_program = at_program_start && word.is_empty();
                quote = Some(character);
            }
            None if character == '\\' => escaped = true,
            None if character.is_whitespace() || ";|&(){}<>".contains(character) => {
                if opaque_program_word_changes_identity(&word, at_program_start, quoted_program) {
                    return true;
                }
                if matches!(word.as_str(), "then" | "do" | "else" | "elif") {
                    at_program_start = true;
                } else if at_program_start && !word.is_empty() && !is_env_assignment(&word) {
                    at_program_start = false;
                }
                word.clear();
                quoted_program = false;
                if ";|&(){}<>".contains(character) {
                    at_program_start = true;
                }
            }
            None => word.push(character),
        }
    }
    opaque_program_word_changes_identity(&word, at_program_start, quoted_program)
}

fn opaque_program_word_changes_identity(
    word: &str,
    at_program_start: bool,
    quoted_program: bool,
) -> bool {
    if !at_program_start || is_env_assignment(word) {
        return false;
    }
    if quoted_program {
        return true;
    }
    if word.is_empty() {
        return false;
    }
    let program = program_name(word);
    matches!(
        program.as_str(),
        "eval"
            | "source"
            | "."
            | "bash"
            | "sh"
            | "zsh"
            | "env"
            | "exec"
            | "sudo"
            | "su"
            | "command"
            | "xargs"
            | "busybox"
    ) || program.contains(['$', '*', '?', '[', ']', '{', '}', '~', '`'])
        || word.contains('`')
}

/// `segment` with each `$(...)` outside single quotes replaced by `$_`.
fn mask_command_substitutions(segment: &str) -> String {
    let bytes = segment.as_bytes();
    let mut out = String::with_capacity(segment.len());
    let mut single = false;
    let mut index = 0;
    while index < bytes.len() {
        let byte = bytes[index];
        if byte == b'\'' {
            single = !single;
        } else if !single && byte == b'$' && bytes.get(index + 1) == Some(&b'(') {
            if let Some(end) = closing_paren(bytes, index + 1) {
                out.push_str("$_");
                index = end;
                continue;
            }
        }
        let width = segment[index..].chars().next().map_or(1, char::len_utf8);
        out.push_str(&segment[index..index + width]);
        index += width;
    }
    out
}

/// A command substitution executes its body even when it appears inside a
/// quoted argument. The ordinary word scan intentionally ignores quoted data,
/// so inspect substitution bodies separately.
fn contains_removal_in_command_substitution(command: &str) -> bool {
    let bytes = command.as_bytes();
    let mut index = 0;
    let mut quote = None::<u8>;
    let mut escaped = false;
    while index < bytes.len() {
        let byte = bytes[index];
        if escaped {
            escaped = false;
            index += 1;
            continue;
        }
        match quote {
            Some(b'\'') if byte == b'\'' => quote = None,
            Some(b'\'') => {}
            Some(b'\"') if byte == b'\"' => quote = None,
            Some(b'\"') if byte == b'\\' => escaped = true,
            Some(b'\"') if byte == b'$' && bytes.get(index + 1) == Some(&b'(') => {
                let Some(end) = closing_paren(bytes, index + 1) else {
                    return true;
                };
                let inner = &command[index + 2..end - 1];
                if contains_unquoted_removal_program(inner)
                    || contains_removal_in_command_substitution(inner)
                {
                    return true;
                }
                index = end;
                continue;
            }
            Some(b'\"') => {}
            Some(_) => unreachable!(),
            None if byte == b'\'' || byte == b'\"' => quote = Some(byte),
            None if byte == b'\\' => escaped = true,
            None if byte == b'$' && bytes.get(index + 1) == Some(&b'(') => {
                let Some(end) = closing_paren(bytes, index + 1) else {
                    return true;
                };
                let inner = &command[index + 2..end - 1];
                if contains_unquoted_removal_program(inner)
                    || contains_removal_in_command_substitution(inner)
                {
                    return true;
                }
                index = end;
                continue;
            }
            None => {}
        }
        index += 1;
    }
    false
}

fn contains_unquoted_removal_program_at_depth(command: &str, depth: usize) -> bool {
    if depth >= 8 {
        return false;
    }
    let mut quote = None::<u8>;
    let mut escaped = false;
    let mut word = String::new();
    let finish_word = |word: &mut String| {
        let executable = word.trim_end_matches(".exe");
        let program = program_name(word);
        let is_removal = matches!(program.trim_end_matches(".exe"), "rm" | "rmdir")
            || matches!(executable, "rm" | "rmdir")
            || executable.ends_with("/rm")
            || executable.ends_with("/rmdir");
        word.clear();
        is_removal
    };
    let bytes = command.as_bytes();
    let mut index = 0;
    while index < bytes.len() {
        let byte = bytes[index];
        if escaped {
            if quote.is_none() {
                word.push(byte as char);
            }
            escaped = false;
            index += 1;
            continue;
        }
        match quote {
            Some(b'\'') if byte == b'\'' => quote = None,
            Some(b'\'') => {}
            Some(b'\"') if byte == b'\"' => quote = None,
            Some(b'\"') if byte == b'\\' => escaped = true,
            Some(b'\"') if byte == b'$' && bytes.get(index + 1) == Some(&b'(') => {
                let Some(end) = closing_paren(bytes, index + 1) else {
                    return true;
                };
                if contains_unquoted_removal_program_at_depth(
                    &command[index + 2..end - 1],
                    depth + 1,
                ) {
                    return true;
                }
                index = end;
                continue;
            }
            Some(b'\"') => {}
            Some(_) => unreachable!(),
            None if byte == b'\'' || byte == b'\"' => quote = Some(byte),
            None if byte == b'\\' => escaped = true,
            None if byte == b'$' && bytes.get(index + 1) == Some(&b'(') => {
                let Some(end) = closing_paren(bytes, index + 1) else {
                    return true;
                };
                if contains_unquoted_removal_program_at_depth(
                    &command[index + 2..end - 1],
                    depth + 1,
                ) {
                    return true;
                }
                index = end;
                continue;
            }
            // A backtick ends a word too, so `` #`rm x` `` still names rm.
            None if byte.is_ascii_whitespace() || b";|&(){}<>`".contains(&byte) => {
                if finish_word(&mut word) {
                    return true;
                }
            }
            None => word.push(byte as char),
        }
        index += 1;
    }
    finish_word(&mut word)
}

fn closing_paren(bytes: &[u8], opener: usize) -> Option<usize> {
    let mut depth = 0usize;
    let mut quote = None::<u8>;
    let mut escaped = false;
    for (index, &byte) in bytes.iter().enumerate().skip(opener) {
        if escaped {
            escaped = false;
            continue;
        }
        match quote {
            Some(b'\'') if byte == b'\'' => quote = None,
            Some(b'\'') => continue,
            Some(b'\"') if byte == b'\"' => quote = None,
            Some(b'\"') if byte == b'\\' => escaped = true,
            Some(b'\"') => continue,
            Some(_) => unreachable!(),
            None if byte == b'\'' || byte == b'\"' => quote = Some(byte),
            None if byte == b'\\' => escaped = true,
            None if byte == b'(' => depth += 1,
            None if byte == b')' => {
                depth = depth.checked_sub(1)?;
                if depth == 0 {
                    return Some(index + 1);
                }
            }
            None => {}
        }
    }
    None
}

fn printf_format_is_output_only(format: &str) -> bool {
    if format.contains(['$', '*', '?', '[', ']', '{', '}', '~']) {
        return false;
    }
    let mut chars = format.chars();
    while let Some(ch) = chars.next() {
        if ch != '%' {
            continue;
        }
        let Some(mut conversion) = chars.next() else {
            return false;
        };
        if conversion == '%' {
            continue;
        }
        while conversion.is_ascii_digit() || " -+#.".contains(conversion) {
            let Some(next) = chars.next() else {
                return false;
            };
            conversion = next;
        }
        if !"bcdiouxXeEfFgGaAsqQ".contains(conversion) {
            return false;
        }
    }
    true
}

fn resolution_variable(name: &str) -> bool {
    matches!(
        name,
        "PATH"
            | "PATHEXT"
            | "ENV"
            | "BASH_ENV"
            | "SHELLOPTS"
            | "BASHOPTS"
            | "IFS"
            | "LD_PRELOAD"
            | "LD_LIBRARY_PATH"
            | "PROMPT_COMMAND"
            // #1340: where rm-file / rm-dir and the child shim may delete.
            | "CLUD_RM_ROOTS"
            | "CLUD_RM_ROLE"
    )
}

pub(super) fn check(command: &str, path_env: &str) -> Result<(), String> {
    let trusted = crate::shim_install::packaged_shim().map_err(|e| format!("rm identity: {e}"))?;
    let trusted_tap = if command.contains("tap") {
        let name = if cfg!(windows) { "tap.exe" } else { "tap" };
        let packaged = trusted.with_file_name(name);
        crate::shim_resolve::which("tap", path_env)
            .and_then(|p| std::fs::read(p).ok())
            .zip(std::fs::read(packaged).ok())
            .is_some_and(|(actual, expected)| !expected.is_empty() && actual == expected)
    } else {
        false
    };
    // Refusing opaque source needs no binary IO. Every allowed command still
    // reaches the full byte comparison; there is no identity cache or bypass.
    source_reason_with_tap(
        command,
        trusted_tap,
        std::env::var_os("RIPGREP_CONFIG_PATH").is_some(),
    )?;
    identity_reason(path_env, &trusted)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn allows_exact_bytes_and_denies_missing_replaced_system() {
        let tmp = tempfile::tempdir().unwrap();
        let trusted = tmp.path().join("clud-shim");
        let rm = tmp.path().join(if cfg!(windows) { "rm.exe" } else { "rm" });
        std::fs::write(&trusted, b"packaged bytes").unwrap();
        let path = tmp.path().to_str().unwrap();
        assert!(identity_reason(path, &trusted).is_err());
        std::fs::copy(&trusted, &rm).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&rm, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        assert_eq!(identity_reason(path, &trusted), Ok(()));
        std::fs::write(&rm, b"replacement").unwrap();
        assert!(identity_reason(path, &trusted).is_err());
        assert!(identity_reason("", &trusted).is_err());
        assert!(identity_reason("/bin:/usr/bin", &trusted).is_err());
        assert!(identity_reason(path, &tmp.path().join("missing")).is_err());
    }
    #[test]
    fn source_contract_both_directions() {
        for source in [
            "rm -rf ./build",
            "V=/tmp/safe; rm -rf \"$V\"/",
            "echo 'rm -rf /'",
            "printf '%s' \"$V\"",
            "printf '%%n' PATH",
            "git rm -r --cached foo",
            "docker run --rm ubuntu",
            "(cd src && ls)",
            "rg -n 'rm identity' crates/clud-bin/src/block_bad_cmd_rm_identity.rs",
            "printf '%s' \"stream or a future field\"",
            "body=$(curl -fsSL https://example.invalid); printf '%s' \"$body\"",
            "for version in 1 2 3; do printf '%s\\n' \"$version\"; done",
            "gh issue comment 1229 --repo zackees/clud --body $'Burndown update\\n\\n- all platform CI green'",
        ] {
            assert!(source_reason(source).is_ok(), "{source}");
        }
        for source in [
            "/bin/rm -rf ./build",
            "env PATH=/bin rm x",
            "command -p rm x",
            "busybox rm x",
            "sudo rm x",
            "hash -p /bin/rm rm",
            "alias rm=/bin/rm",
            "PATH=/bin; rm x",
            "export PATH=/bin",
            "bash -c 'rm x'",
            "$REMOVE x",
            "source setup.sh",
            ". /tmp/setup",
            "builtin printf -v PATH /bin; rm ./build",
            "command printf -v PATH /bin; rm ./build",
            "trap ' PATH=/bin' DEBUG; rm ./build",
            "r? ./build",
            "NAME=PATH; export \"$NAME=/bin\"; rm ./build",
            "env BASH_ENV=/tmp/setup bash -c true",
            "printf -v PATH /bin; rm ./build",
            "OPT=-vPATH; printf \"$OPT\" /bin; rm ./build",
            "printf '%n' PATH; rm ./build",
            "printf '%5n' PATH; rm ./build",
            "printf -vPATH /bin; rm ./build",
            "read 'PATH[0]' <<< /bin; rm ./build",
            "read -aPATH <<< /bin; rm ./build",
            "declare -i P=0; P=PATH++; rm ./build",
            "declare -n P=PATH; P=/bin; rm ./build",
            "echo ok & /bin/rm ./build",
            "echo \"$(/bin/rm ./build)\"",
            "eval '/bin/rm victim'",
            "for x in 1; do eval '/bin/rm victim'; done",
            "for x in 1; do $F '/bin/rm victim'; done",
            "for x in 1; do \"$F\" '/bin/rm victim'; done",
            "for x in 1; do env $F victim; done",
            "echo `/bin/rm victim`",
            "(cd src && /bin/rm ./build)",
            "(PATH=/bin; rm ./build)",
            "(echo ok) && (/bin/rm ./build)",
        ] {
            assert!(source_reason(source).is_err(), "{source}");
        }
    }

    #[test]
    fn single_quoted_markdown_backticks_in_search_pattern_are_data() {
        // Reproduces the read-only search denied by the hook after a successful jq query.
        // Shell single quotes make the Markdown delimiters literal pattern text.
        let tick = char::from(96);
        let command = format!(
            "rg -l --glob '*.md' --glob '!projects/**' 'Run {tick}git diff @\\{{upstream\\}}\\.\\.\\.HEAD{tick}|high effort → 8 inline angles|Phase 0 — Gather the diff' /home/niteris/.claude /home/niteris/dev/fastled/.claude"
        );
        assert!(source_reason(&command).is_ok(), "{command}");
        assert!(!literal_backticks_are_data_only(&command, true));
        let no_config = command.replacen("rg -l", "rg --no-config -l", 1);
        assert!(literal_backticks_are_data_only(&no_config, true));
        let after_terminator = command.replacen(
            " /home/niteris/.claude",
            " -- --no-config /home/niteris/.claude",
            1,
        );
        assert!(!literal_backticks_are_data_only(&after_terminator, true));
        let consumed_as_value = command.replacen("rg -l", "rg -g --no-config -l", 1);
        assert!(!literal_backticks_are_data_only(&consumed_as_value, true));
        let preprocessed = command.replacen("rg -l", "rg --pre=printf -l", 1);
        assert!(!literal_backticks_are_data_only(&preprocessed, false));
        let issue_comment = format!(
            "gh issue comment 1298 --repo zackees/clud --body 'Findings include {tick}cache_health_fuse{tick} and {tick}quoted prose{tick}'"
        );
        assert!(source_reason(&issue_comment).is_ok(), "{issue_comment}");
        // #1305: backticks the scanner cannot prove inert are refused only
        // when the command could run rm; these run none.
        for command in [
            format!("gh issue comment 1298 --editor --attach image.png --body 'literal {tick}text{tick}'"),
            format!("gh issue comment 1298 --web --body 'literal {tick}text{tick}'"),
        ] {
            assert!(source_reason(&command).is_ok(), "{command}");
        }

        for command in [
            format!("printf '%s' \\{tick}literal\\{tick}"),
            format!(r#"printf '%s' "\{tick}literal\{tick}""#),
            "printf '%s' $'\\x60literal\\x60'".to_string(),
        ] {
            assert!(source_reason(&command).is_ok(), "{command}");
        }

        // An active backtick substitution that runs rm stays refused.
        for command in [
            format!("printf x\\ #{tick}rm /tmp/victim{tick}"),
            format!("printf $(echo x)#{tick}rm /tmp/victim{tick}"),
            format!("printf x\r#{tick}rm /tmp/victim{tick}"),
            format!("cat <<EOF\nprintf ok # {tick}rm /tmp/victim{tick}\nEOF"),
        ] {
            assert!(contains_active_backtick_substitution(&command), "{command}");
            assert!(source_reason(&command).is_err(), "{command}");
        }
        // #1305: one that runs no removal cannot change which rm runs.
        for command in [
            format!("echo {tick}PATH=/tmp{tick}"),
            format!("printf ok \\;# literal {tick}text{tick}"),
            format!("printf ok # literal {tick}text{tick}"),
        ] {
            assert!(contains_active_backtick_substitution(&command), "{command}");
            assert!(source_reason(&command).is_ok(), "{command}");
        }

        let dollar = char::from(36);
        let nested_expansion = format!(
            "unset CLUD_REVIEW_UNSET; printf '%s' {dollar}{{CLUD_REVIEW_UNSET:- #{tick}printf nested{tick}}}"
        );
        assert!(contains_active_backtick_substitution(&nested_expansion));
        assert!(source_reason(&nested_expansion).is_ok());
        let nested_removal = nested_expansion.replace("printf nested", "rm -rf x");
        assert!(source_reason(&nested_removal).is_err(), "{nested_removal}");

        let perl_program = format!("perl -e 'print {tick}printf nested{tick}'");
        assert!(source_reason(&perl_program).is_ok(), "{perl_program}");
        // A nested shell could run rm from the hidden text: still refused.
        let nested_shell = format!("env bash -c 'printf ok {tick}printf nested{tick}'");
        assert!(source_reason(&nested_shell).is_err(), "{nested_shell}");
    }

    /// #1305's four refused commands, verbatim in shape: none runs rm.
    #[test]
    fn issue_1305_harmless_commands_are_allowed() {
        let tick = char::from(96);
        for command in [
            format!(
                "gh issue create --title \"feat(openrouter): \\{tick}clud --openrouter <KEY>\\{tick} saves the key\" --body-file x.md"
            ),
            "for u in $(gh issue list --json url -q '.[].url'); do n=${u##*/}; gh api repos/o/r/issues/$n; done".to_string(),
            "u=$(gh issue create --title t --body b) && n=$(basename $u) && id=$(gh api repos/o/r/issues/$n --jq .id)".to_string(),
            format!("grep -rn '\\\\{tick}' docs"),
        ] {
            assert_eq!(source_reason(&command), Ok(()), "{command}");
        }
        // Real bypasses stay refused.
        for command in [
            format!("if :; then PATH=/bin {tick}printf r{tick}{tick}printf m{tick} -rf x; fi"),
            format!("if :; then /bin/{tick}printf rm{tick} -rf x; fi"),
            "CLUD_RM_ROOTS=/ ./test".to_string(),
            "export CLUD_RM_ROOTS=/".to_string(),
            "PATH=/bin rm x".to_string(),
            format!("{tick}which rm{tick} x"),
            "$(printf rm) file".to_string(),
        ] {
            assert!(source_reason(&command).is_err(), "{command}");
        }
    }

    #[test]
    fn unquoted_removal_detection_does_not_treat_data_as_code() {
        for command in [
            "rg -n 'rm identity' file",
            "printf '%s' \"stream or a future field\"",
            "printf '%s' \"rm identity\"",
        ] {
            assert!(!contains_unquoted_removal_program(command), "{command}");
        }
        for command in ["rm file", "/bin/rm file", "for x in 1; do rm x; done"] {
            assert!(contains_unquoted_removal_program(command), "{command}");
        }
    }
}
