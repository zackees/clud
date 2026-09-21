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
    source_reason_with_tap(command, false)
}

fn source_reason_with_tap(command: &str, trusted_tap: bool) -> Result<(), String> {
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
        return source_reason_with_tap(inner, trusted_tap);
    }
    if command.contains('`') || contains_removal_in_command_substitution(command) {
        return Err(refuse());
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
        let words = shell_words::split(segment.trim()).map_err(|_| refuse())?;
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
    for character in command.chars() {
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
    ) || program.contains(['$', '*', '?', '[', ']', '{', '}', '~'])
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
            None if byte.is_ascii_whitespace() || b";|&(){}<>".contains(&byte) => {
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
    source_reason_with_tap(command, trusted_tap)?;
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
