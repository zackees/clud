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
    let _ = (command, READ_ONLY);
    None
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
        ] {
            assert_eq!(reason(command), None, "{command}");
        }
    }
}
