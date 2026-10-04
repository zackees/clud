//! Agents cannot set the safe-rm root override (#1668, DD-137).
//!
//! `safe_rm.extra_roots` widens where `safe-rm` may delete, so only the user
//! may write it, by hand, in `~/.clud/settings.json`. This check refuses any
//! shell command that names that settings file (or the `extra_roots` key)
//! unless every statement in it is a plain read with no output redirection.
//! It is the same text-scan mechanism that refuses a deletion command
//! assigning `CLUD_RM_ROOTS`, `TMPDIR`, `TEMP` or `TMP`
//! (`block_bad_cmd_rm_redirect::changes_deletion_environment`), extended to
//! the setting's file: there is no environment-variable form of the
//! override to assign. It fails closed: an unrecognized program touching the
//! file is refused, not allowed.

/// Programs that only read their operands when run without a redirection.
const READ_ONLY: &[&str] = &[
    "cat", "less", "more", "head", "tail", "grep", "egrep", "fgrep", "rg", "jq", "wc", "ls",
    "stat", "file", "diff", "bat", "echo", "printf", "test", "[", "true",
];

pub(super) const REFUSAL: &str = "agents may not write ~/.clud/settings.json or the \
     safe_rm.extra_roots override: it widens where safe-rm may delete, so only the user sets it, \
     by editing the file by hand. Ask the user to add the path and a reason, or leave the path \
     and report it";

/// The refusal for `command`, or `None` when it cannot write the override.
pub(super) fn reason(command: &str) -> Option<String> {
    let normalized: String = command
        .to_ascii_lowercase()
        .chars()
        .filter(|c| !matches!(c, '\'' | '"' | '\\'))
        .collect();
    // A repo's own `.clud/settings.json` is never read for the override, so
    // only a spelling that can reach the user's home counts.
    let reaches_home = ["~", "home", "userprofile", "/users/", "/root/"]
        .iter()
        .any(|marker| normalized.contains(marker));
    let names_override = normalized.contains("extra_roots")
        || (normalized.contains(".clud") && normalized.contains("settings.json") && reaches_home);
    if !names_override || only_reads(command) {
        return None;
    }
    Some(REFUSAL.to_string())
}

/// Whether every statement of `command` is a [`READ_ONLY`] program and no
/// output is redirected anywhere but `/dev/null` or another descriptor.
fn only_reads(command: &str) -> bool {
    let mut statements: Vec<Vec<String>> = vec![Vec::new()];
    let mut word = String::new();
    let mut quote: Option<char> = None;
    let mut redirect_pending = false;
    let mut chars = command.chars().peekable();
    while let Some(ch) = chars.next() {
        if let Some(q) = quote {
            if ch == q {
                quote = None;
            } else if ch == '\\' && q == '"' {
                if let Some(next) = chars.next() {
                    word.push(next);
                }
            } else {
                word.push(ch);
            }
            continue;
        }
        match ch {
            '\'' | '"' => quote = Some(ch),
            '\\' => {
                if let Some(next) = chars.next() {
                    word.push(next);
                }
            }
            '>' => {
                // `2>` / `&>`: the descriptor digits are not a word.
                if word.chars().all(|c| c.is_ascii_digit() || c == '&') {
                    word.clear();
                }
                if !finish(&mut word, &mut statements, &mut redirect_pending) {
                    return false;
                }
                if chars.peek() == Some(&'>') {
                    chars.next();
                }
                if chars.peek() == Some(&'&') {
                    // `>&2` duplicates a descriptor; it writes no file.
                    chars.next();
                    while chars.peek().is_some_and(char::is_ascii_digit) {
                        chars.next();
                    }
                } else {
                    redirect_pending = true;
                }
            }
            ';' | '&' | '|' | '\n' | '\r' | '(' | ')' | '`' => {
                if !finish(&mut word, &mut statements, &mut redirect_pending) {
                    return false;
                }
                statements.push(Vec::new());
            }
            c if c.is_whitespace() => {
                if !finish(&mut word, &mut statements, &mut redirect_pending) {
                    return false;
                }
            }
            c => word.push(c),
        }
    }
    if !finish(&mut word, &mut statements, &mut redirect_pending) || redirect_pending {
        return false;
    }
    statements.iter().all(|words| {
        let Some(program) = words
            .iter()
            .find(|w| !super::block_bad_cmd_shell::is_env_assignment(w))
        else {
            return true;
        };
        let name = super::block_bad_cmd_shell::program_name(program);
        READ_ONLY.contains(&name.as_str()) || program == "["
    })
}

/// Close the current word. A redirection target that is not `/dev/null`
/// means the command writes, which is reported as `false`.
fn finish(word: &mut String, statements: &mut [Vec<String>], pending: &mut bool) -> bool {
    if word.is_empty() {
        return true;
    }
    let text = std::mem::take(word);
    if std::mem::take(pending) {
        return text == "/dev/null";
    }
    if let Some(last) = statements.last_mut() {
        last.push(text);
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn commands_that_write_the_settings_file_are_refused() {
        for command in [
            "echo '{}' > ~/.clud/settings.json",
            "printf x >> $HOME/.clud/settings.json",
            "jq '.safe_rm.extra_roots=[{\"path\":\"/srv\"}]' ~/.clud/settings.json > /x/s && mv /x/s ~/.clud/settings.json",
            "cat new.json | tee ~/.clud/settings.json",
            "sed -i 's/a/b/' ~/.clud/settings.json",
            "sed -n 'w /home/u/.clud/settings.json' notes.txt",
            "python -c \"open('/home/u/.clud/settings.json','w').write('{}')\"",
            "cp evil.json ~/.clud/settings.json",
            "cd ~/.clud && echo x > settings.json",
            "perl -pi -e 's/x/y/' \"$HOME/.clud/settings.json\"",
            "node -e 'require(\"fs\").writeFileSync(process.env.HOME+\"/.clud/settings.json\",\"{}\")'",
            "dd if=x of=~/.clud/settings.json",
            "clud settings set safe_rm.extra_roots /srv",
            "CLUD_ALLOW_ALL_CMDS=1 cp x ~/.clud/settings.json",
            "echo '{\"safe_rm\":{\"extra_roots\":[]}}' > /home/u/.clud/settings.json",
        ] {
            let got = reason(command);
            assert!(got.is_some(), "{command}");
            assert!(got.unwrap().contains("safe_rm.extra_roots"), "{command}");
        }
    }

    #[test]
    fn reads_and_unrelated_commands_are_allowed() {
        for command in [
            "cat ~/.clud/settings.json",
            "jq .safe_rm ~/.clud/settings.json",
            "grep extra_roots ~/.clud/settings.json 2>/dev/null",
            "clud settings --list",
            "safe-rm -r /srv/out",
            "echo settings.json > notes.txt",
            "cargo test",
            "cat ./settings.json",
            // The repo layer is never read for the override.
            "jq '.bash.block_cd=false' .clud/settings.json > t && mv t .clud/settings.json",
        ] {
            assert_eq!(reason(command), None, "{command}");
        }
    }
}
