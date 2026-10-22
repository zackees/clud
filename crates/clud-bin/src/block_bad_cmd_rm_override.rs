//! Protect user settings while distinguishing executable writes from data.
//! A protected name in an issue search or scratch heredoc is not a write.
//! Unknown operations that mention the protected setting still fail closed.

use std::path::Path;

use crate::shell_ast::{Invocation, Value};

/// Programs that only read their operands when run without a redirection.
const READ_ONLY: &[&str] = &[
    "cat", "less", "more", "head", "tail", "grep", "egrep", "fgrep", "rg", "jq", "wc", "ls",
    "stat", "file", "diff", "bat", "echo", "printf", "test", "[", "true",
];

pub(super) const REFUSAL: &str = "agents may not write ~/.clud/settings.json or the \
     safe_rm.extra_roots override: it widens where safe-rm may delete, so only the user sets it, \
     by editing the file by hand. Ask the user to add the path and a reason, or leave the path \
     and report it";

pub(super) fn reason_for_payload(payload: &super::HookPayloadView) -> Option<String> {
    let home = crate::home::user_home();
    reason_in(
        &payload.command,
        super::shell_dialect_for_tool(&payload.tool_name),
        Some(&payload.cwd),
        home.as_deref(),
    )
}

fn reason_in(
    command: &str,
    dialect: super::ShellDialect,
    cwd: Option<&Path>,
    home: Option<&Path>,
) -> Option<String> {
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
    if !names_override || does_not_write_settings(command, dialect, cwd, home) {
        return None;
    }
    Some(REFUSAL.to_string())
}

fn does_not_write_settings(
    command: &str,
    dialect: super::ShellDialect,
    cwd: Option<&Path>,
    home: Option<&Path>,
) -> bool {
    let Some(home) = home else {
        return false;
    };
    let script =
        super::block_bad_cmd_shell::parse_command_with_home(command, dialect, cwd, Some(home));
    !script.has_error
        && script.invocations.iter().all(|invocation| {
            let writes = invocation.redirects.iter().any(|redirect| {
                redirect.writes_file() && !safe_destination(&redirect.target, invocation, home)
            });
            !writes && read_only_invocation(invocation)
        })
}

fn safe_destination(target: &Value, invocation: &Invocation, home: &Path) -> bool {
    let Some(target) = target.as_literal() else {
        return false;
    };
    if target == "/dev/null" {
        return true;
    }
    let Some(cwd) = invocation.cwd.as_literal() else {
        return false;
    };
    let destination = crate::shell_ast::join_path(cwd, target);
    let settings = crate::clud_settings::settings_path_at(home);
    let canonical = |path: &Path| {
        crate::path_norm::canonicalize_plain(path).unwrap_or_else(|_| path.to_path_buf())
    };
    canonical(Path::new(&destination)) != canonical(&settings)
}

fn read_only_invocation(invocation: &Invocation) -> bool {
    let words = invocation.effective_argv();
    let Some(program) = words.first() else {
        return true;
    };
    let Some(program) = program.as_literal() else {
        return false;
    };
    let name = super::block_bad_cmd_shell::program_name(program);
    READ_ONLY.contains(&name.as_str())
        || program == "["
        || forge_read(&name, words)
        || numeric_sed_read(&name, words)
}

fn forge_read(program: &str, words: &[Value]) -> bool {
    program == "gh"
        && words.get(1).and_then(Value::as_literal) == Some("issue")
        && matches!(
            words.get(2).and_then(Value::as_literal),
            Some("list" | "view")
        )
}

fn numeric_sed_read(program: &str, words: &[Value]) -> bool {
    program == "sed"
        && words.get(1).and_then(Value::as_literal) == Some("-n")
        && words
            .get(2)
            .and_then(Value::as_literal)
            .and_then(|script| script.strip_suffix('p'))
            .is_some_and(|range| {
                range
                    .chars()
                    .all(|c| c.is_ascii_digit() || matches!(c, ',' | '$'))
            })
        && words.iter().skip(3).all(|word| {
            word.as_literal()
                .is_some_and(|operand| !operand.starts_with('-'))
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn reason(command: &str) -> Option<String> {
        reason_in(
            command,
            crate::block_bad_cmd::ShellDialect::Posix,
            None,
            Some(Path::new("/home/u")),
        )
    }

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
            "gh issue list --repo zackees/clud --search 'safe_rm.extra_roots ~/.clud/settings.json'",
            "cat > issue-draft.md <<'EOF'\nDocument ~/.clud/settings.json and safe_rm.extra_roots.\nEOF",
            "sed -n '1,175p' crates/clud-bin/src/rm_tool_extra_roots.rs",
            // The repo layer is never read for the override.
            "jq '.bash.block_cd=false' .clud/settings.json > t && mv t .clud/settings.json",
        ] {
            assert_eq!(reason(command), None, "{command}");
        }
    }
}
