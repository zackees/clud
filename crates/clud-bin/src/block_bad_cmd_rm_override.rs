//! Protect user settings while distinguishing executable writes from data.
//! A protected name in an issue search or scratch heredoc is not a write.
//! Unknown operations that mention the protected setting still fail closed.

use std::io::Read;
use std::path::Path;

use sha2::{Digest, Sha256};

use crate::shell_ast::{Invocation, Value};

/// Programs that only read their operands when run without a redirection.
const READ_ONLY: &[&str] = &[
    "cat", "less", "more", "head", "tail", "grep", "egrep", "fgrep", "rg", "jq", "wc", "ls",
    "stat", "file", "diff", "bat", "echo", "printf", "test", "[", "true",
];

pub(super) const REFUSAL: &str =
    "raw writes to user settings and safe_rm.extra_roots are refused. \
    Use `clud settings set /key JSON --reason TEXT` for an ordinary change, or \
    `clud settings roots JSON --reason TEXT` for a separate deletion-root change. \
    The user must approve the exact operation in its native desktop window; \
    environment variables, scripts, and claimed approval metadata are not authority";

pub(super) fn reason_for_payload(payload: &super::HookPayloadView) -> Option<String> {
    let home = crate::home::user_home();
    let tool = payload
        .tool_name
        .rsplit('.')
        .next()
        .unwrap_or_default()
        .to_ascii_lowercase();
    if super::block_bad_cmd_gate::gates_tool(&tool) {
        return reason_in(
            &payload.command,
            super::shell_dialect_for_payload(payload),
            Some(&payload.cwd),
            home.as_deref(),
        );
    }
    native_file_reason(payload, &tool, home.as_deref())
}

fn native_file_reason(
    payload: &super::HookPayloadView,
    tool: &str,
    home: Option<&Path>,
) -> Option<String> {
    if tool == "apply_patch" {
        return apply_patch_reason(payload, home);
    } else if matches!(tool, "write" | "edit" | "multiedit" | "notebookedit") {
        let path = payload.tool_input.as_ref().and_then(|input| {
            ["file_path", "notebook_path", "path"]
                .iter()
                .find_map(|key| input.get(key).and_then(serde_json::Value::as_str))
        });
        if path.is_none_or(|path| native_target_is_protected(path, payload, home)) {
            return Some(REFUSAL.into());
        }
    }
    None
}

/// Refusal for a patch whose destination cannot be established. Distinct from
/// [`REFUSAL`]: nothing here says the patch touches user settings, only that
/// it cannot be shown not to (#2289, #2222). Still fails closed.
pub(super) const UNVERIFIABLE_PATCH: &str =
    "could not parse the apply_patch payload (no `*** Add/Update/Delete File:` or \
    `*** Move to:` target found); refusing because the target cannot be verified \
    against the protected user settings file. Retry with a well-formed patch";

const NO_HOME: &str =
    "the user home directory could not be resolved, so apply_patch targets cannot be \
    verified against the protected user settings file; refusing";

const PATCH_MARKERS: [&str; 4] = [
    "*** Add File: ",
    "*** Update File: ",
    "*** Delete File: ",
    "*** Move to: ",
];

/// The patch text from the shapes Codex sends: a bare string,
/// `{"patch"|"input"|"command": "<patch>"}`, or a `command` argv array such as
/// `["apply_patch", "<patch>"]` (the element holding `*** Begin Patch`, else the last string).
fn patch_text(input: &serde_json::Value) -> Option<&str> {
    if let Some(text) = input.as_str() {
        return Some(text);
    }
    ["patch", "input", "command"].iter().find_map(|key| {
        let value = input.get(key)?;
        value.as_str().or_else(|| {
            let all: Vec<&str> = value
                .as_array()?
                .iter()
                .filter_map(serde_json::Value::as_str)
                .collect();
            all.iter()
                .copied()
                .find(|text| text.contains("*** Begin Patch"))
                .or_else(|| all.last().copied())
        })
    })
}

fn patch_targets(patch: &str) -> Vec<&str> {
    patch
        .lines()
        .filter_map(|line| {
            let line = line.trim_start();
            PATCH_MARKERS
                .iter()
                .find_map(|prefix| line.strip_prefix(prefix))
        })
        .map(|path| path.trim_end_matches(['\r', '\n']))
        .collect()
}

fn apply_patch_reason(payload: &super::HookPayloadView, home: Option<&Path>) -> Option<String> {
    let targets = payload
        .tool_input
        .as_ref()
        .and_then(patch_text)
        .map(patch_targets)
        .unwrap_or_default();
    if targets.is_empty() {
        return Some(UNVERIFIABLE_PATCH.into());
    }
    let Some(home) = home else {
        return Some(NO_HOME.into());
    };
    targets
        .iter()
        .any(|path| protected_file(path, &payload.cwd.to_string_lossy(), home))
        .then(|| REFUSAL.into())
}

fn native_target_is_protected(
    path: &str,
    payload: &super::HookPayloadView,
    home: Option<&Path>,
) -> bool {
    home.is_none_or(|home| protected_file(path, &payload.cwd.to_string_lossy(), home))
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
    let script = super::block_bad_cmd_shell::parse_command_with_home(command, dialect, cwd, home);
    let writes_settings = script.invocations.iter().any(|invocation| {
        protected_operand(invocation, home)
            || invocation.redirects.iter().any(|redirect| {
                redirect.writes_file()
                    && home.is_none_or(|home| !safe_destination(&redirect.target, invocation, home))
            })
    });
    if writes_settings
        || (names_override
            && (script.has_error
                || home.is_none()
                || !script.invocations.iter().all(read_only_invocation)))
    {
        Some(REFUSAL.to_string())
    } else {
        None
    }
}

fn protected_operand(invocation: &Invocation, home: Option<&Path>) -> bool {
    if read_only_invocation(invocation) {
        return false;
    }
    let Some(home) = home else {
        return false;
    };
    let mutator = matches!(
        invocation.effective_program().as_deref(),
        Some("cp" | "mv" | "tee" | "install" | "dd" | "set-content" | "add-content" | "out-file")
    );
    let Some(cwd) = invocation.cwd.as_literal() else {
        return mutator;
    };
    invocation.effective_argv().iter().skip(1).any(|word| {
        word.as_literal().map_or(mutator, |word| {
            // A known protected operand on an unknown writer is refused too:
            // passing a target to a script does not establish its authority.
            protected_file(word.strip_prefix("of=").unwrap_or(word), cwd, home)
        })
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
    !protected_file(target, cwd, home)
}

fn protected_file(target: &str, cwd: &str, home: &Path) -> bool {
    let expanded = if let Some(relative) = target
        .strip_prefix("~/")
        .or_else(|| target.strip_prefix("~\\"))
    {
        home.join(relative).to_string_lossy().into_owned()
    } else {
        target.to_string()
    };
    let destination = if crate::platform::host::WINDOWS || !expanded.contains('\\') {
        Path::new(cwd).join(&expanded)
    } else {
        crate::shell_ast::join_path(cwd, &expanded).into()
    };
    let settings = crate::clud_settings::settings_path_at(home);
    let canonical = |path: &Path| {
        crate::path_norm::display_slash(&canonical_creation_target(path)).to_ascii_lowercase()
    };
    canonical(&destination) == canonical(&settings)
}

fn canonical_creation_target(path: &Path) -> std::path::PathBuf {
    let mut ancestor = path;
    let mut missing = Vec::new();
    loop {
        if let Ok(mut canonical) = crate::path_norm::canonicalize_plain(ancestor) {
            for component in missing.iter().rev() {
                canonical.push(component);
            }
            return Path::new(&crate::shell_ast::join_path(
                "",
                &canonical.to_string_lossy(),
            ))
            .into();
        }
        let (Some(parent), Some(component)) = (ancestor.parent(), ancestor.file_name()) else {
            return path.to_path_buf();
        };
        missing.push(component);
        ancestor = parent;
    }
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
        || controlled_settings_command(invocation)
}

fn controlled_settings_command(invocation: &Invocation) -> bool {
    let words = &invocation.words;
    if !invocation.env.is_empty()
        || words.get(1).and_then(Value::as_literal) != Some("settings")
        || !matches!(
            words.get(2).and_then(Value::as_literal),
            Some("set" | "roots")
        )
    {
        return false;
    }
    let Some(program) = words.first().and_then(Value::as_literal) else {
        return false;
    };
    if super::block_bad_cmd_shell::program_name(program) != "clud" {
        return false;
    }
    let Ok(current) = std::env::current_exe() else {
        return false;
    };
    let candidate = if program.contains(['/', '\\']) {
        invocation
            .cwd
            .as_literal()
            .map(|cwd| std::path::PathBuf::from(crate::shell_ast::join_path(cwd, program)))
    } else {
        std::env::var_os("PATH").and_then(|path| {
            std::env::split_paths(&path)
                .flat_map(|directory| {
                    [
                        directory.join(program),
                        directory.join(format!("{program}.exe")),
                    ]
                })
                .find(|path| path.is_file())
        })
    };
    let Some(candidate) = candidate else {
        return false;
    };
    if matches!(
        (crate::path_norm::canonicalize_plain(&candidate), crate::path_norm::canonicalize_plain(&current)),
        (Ok(candidate), Ok(current)) if candidate == current
    ) {
        return true;
    }
    if std::fs::metadata(&candidate)
        .ok()
        .map(|metadata| metadata.len())
        != std::fs::metadata(&current)
            .ok()
            .map(|metadata| metadata.len())
    {
        return false;
    }
    // Installed and runtime-cache copies can have different paths. Only an
    // exact copy of this scanner's executable gets the controlled exception.
    matches!((executable_digest(&candidate), executable_digest(&current)), (Ok(candidate), Ok(current)) if candidate == current)
}

fn executable_digest(path: &Path) -> std::io::Result<[u8; 32]> {
    let mut file = std::fs::File::open(path)?;
    let mut digest = Sha256::new();
    let mut buffer = [0_u8; 64 * 1024];
    loop {
        let count = file.read(&mut buffer)?;
        if count == 0 {
            break;
        }
        digest.update(&buffer[..count]);
    }
    Ok(digest.finalize().into())
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

    #[test]
    fn relative_mutators_cannot_write_home_settings() {
        let home = tempfile::tempdir().unwrap();
        for command in [
            "cp staged.json .clud/settings.json",
            "mv staged.json .clud/settings.json",
            "tee .clud/settings.json < staged.json",
        ] {
            assert!(
                reason_in(
                    command,
                    crate::block_bad_cmd::ShellDialect::Posix,
                    Some(home.path()),
                    Some(home.path()),
                )
                .is_some(),
                "{command}"
            );
        }
    }

    #[test]
    fn missing_settings_through_parent_alias_are_protected() {
        let directory = tempfile::tempdir().unwrap();
        let home = directory.path().join("home");
        std::fs::create_dir(&home).unwrap();
        let alias = crate::platform::path_norm::test_alias(&home, &directory.path().join("alias"));
        assert!(protected_file(
            &alias.join(".clud/settings.json").to_string_lossy(),
            &directory.path().to_string_lossy(),
            &home,
        ));
    }

    fn patch_payload(
        cwd: &Path,
        input: serde_json::Value,
    ) -> crate::block_bad_cmd::HookPayloadView {
        crate::block_bad_cmd::HookPayloadView {
            tool_name: "apply_patch".into(),
            command: String::new(),
            cwd: cwd.to_path_buf(),
            tool_input: Some(input),
            agent_id: None,
            agent_type: None,
            session_id: None,
            transcript_path: None,
        }
    }

    fn patch_reason(home: &Path, input: serde_json::Value) -> Option<String> {
        native_file_reason(&patch_payload(home, input), "apply_patch", Some(home))
    }

    const ORDINARY: &str = "*** Begin Patch\n*** Update File: memories/MEMORY.md\n@@\n-a\n+b\n\
        *** Update File: profiles/x/memories/MEMORY.md\n@@\n-a\n+b\n\
        *** Add File: src/new.rs\n+fn main() {}\n*** End Patch";

    #[test]
    fn ordinary_patches_are_allowed_in_every_payload_shape() {
        let home = tempfile::tempdir().unwrap();
        for input in [
            serde_json::json!(ORDINARY),
            serde_json::json!({"patch": ORDINARY}),
            serde_json::json!({"input": ORDINARY}),
            serde_json::json!({"command": ORDINARY}),
            serde_json::json!({"command": ["apply_patch", ORDINARY]}),
            serde_json::json!({"command": ["apply_patch", ORDINARY, "extra"]}),
        ] {
            assert_eq!(patch_reason(home.path(), input.clone()), None, "{input}");
        }
    }

    #[test]
    fn crlf_and_indented_marker_lines_are_understood() {
        let home = tempfile::tempdir().unwrap();
        let crlf = ORDINARY.replace('\n', "\r\n");
        assert_eq!(patch_reason(home.path(), serde_json::json!(crlf)), None);
        let indented = ORDINARY.replace("\n*** ", "\n  *** ");
        assert_eq!(patch_reason(home.path(), serde_json::json!(indented)), None);
        let settings = crate::clud_settings::settings_path_at(home.path());
        let protected = format!(
            "*** Begin Patch\r\n  *** Update File: {}\r\n@@\r\n-a\r\n+b\r\n*** End Patch",
            settings.display()
        );
        assert_eq!(
            patch_reason(home.path(), serde_json::json!(protected)).as_deref(),
            Some(REFUSAL)
        );
    }

    #[test]
    fn relative_and_absolute_ordinary_paths_are_allowed() {
        let home = tempfile::tempdir().unwrap();
        let absolute = home.path().join("repo/src/lib.rs");
        let patch = format!(
            "*** Begin Patch\n*** Update File: ./src/lib.rs\n@@\n-a\n+b\n\
             *** Update File: {}\n@@\n-a\n+b\n*** End Patch",
            absolute.display()
        );
        assert_eq!(patch_reason(home.path(), serde_json::json!(patch)), None);
    }

    #[test]
    fn patches_touching_the_protected_file_are_refused() {
        let home = tempfile::tempdir().unwrap();
        let settings = crate::clud_settings::settings_path_at(home.path());
        let shown = settings.display();
        let relative = settings
            .strip_prefix(home.path())
            .unwrap()
            .display()
            .to_string();
        for patch in [
            format!("*** Begin Patch\n*** Update File: {shown}\n@@\n-a\n+b\n*** End Patch"),
            format!("*** Begin Patch\n*** Update File: {relative}\n@@\n-a\n+b\n*** End Patch"),
            format!("*** Begin Patch\n*** Delete File: {shown}\n*** End Patch"),
            format!(
                "*** Begin Patch\n*** Update File: src/a.rs\n@@\n-a\n+b\n\
                 *** Update File: {shown}\n@@\n-a\n+b\n*** End Patch"
            ),
            format!(
                "*** Begin Patch\n*** Update File: src/a.rs\n*** Move to: {shown}\n@@\n-a\n+b\n*** End Patch"
            ),
        ] {
            for input in [
                serde_json::json!(patch),
                serde_json::json!({"command": ["apply_patch", patch]}),
            ] {
                assert_eq!(
                    patch_reason(home.path(), input).as_deref(),
                    Some(REFUSAL),
                    "{patch}"
                );
            }
        }
    }

    #[test]
    fn unparseable_patches_fail_closed_with_an_accurate_message() {
        let home = tempfile::tempdir().unwrap();
        for input in [
            serde_json::json!({}),
            serde_json::json!(42),
            serde_json::json!({"command": []}),
            serde_json::json!({"command": [1, 2]}),
            serde_json::json!("*** Begin Patch\n@@\n+No target header\n*** End Patch"),
        ] {
            let got = patch_reason(home.path(), input.clone()).expect("fail closed");
            assert_eq!(got, UNVERIFIABLE_PATCH, "{input}");
            assert!(!got.contains("extra_roots"), "{input}");
        }
    }

    #[test]
    fn unresolved_home_is_reported_as_such() {
        let cwd = tempfile::tempdir().unwrap();
        let payload = patch_payload(cwd.path(), serde_json::json!(ORDINARY));
        assert_eq!(
            native_file_reason(&payload, "apply_patch", None).as_deref(),
            Some(NO_HOME)
        );
    }

    fn reason(command: &str) -> Option<String> {
        reason_in(
            command,
            crate::block_bad_cmd::ShellDialect::Posix,
            Some(Path::new("/repo")),
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
