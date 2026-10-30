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
    reason_for_payload_in(payload, crate::home::user_home())
}

fn reason_for_payload_in(
    payload: &super::HookPayloadView,
    home: Option<std::path::PathBuf>,
) -> Option<String> {
    let tool = payload
        .tool_name
        .rsplit('.')
        .next()
        .unwrap_or_default()
        .to_ascii_lowercase();
    if super::block_bad_cmd_gate::gates_tool(&tool) {
        let dialect = super::shell_dialect_for_payload(payload);
        if let Some(verdict) =
            nested_patch_reason(&payload.command, dialect, &payload.cwd, home.as_deref())
        {
            return verdict;
        }
        return reason_in(
            &payload.command,
            dialect,
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
/// it cannot be shown not to (#2289, #2222). Still fails closed. Every patch
/// refusal opens with a stable bracketed reason code (#2285).
pub(super) const UNRECOGNIZED_PAYLOAD: &str =
    "[unrecognized-payload] the apply_patch input has no recognizable patch text \
    (expected a string, or a `patch`/`input`/`cmd`/`command` field); refusing because \
    the target cannot be verified against the protected user settings file";

pub(super) const NO_TARGETS: &str =
    "[no-targets] the apply_patch text has no `*** Add/Update/Delete File:` or \
    `*** Move to:` header; refusing because the target cannot be verified \
    against the protected user settings file. Retry with a well-formed patch";

const NO_HOME: &str =
    "[no-home] the user home directory could not be resolved, so apply_patch targets cannot be \
    verified against the protected user settings file; refusing";

const PATCH_MARKERS: [&str; 4] = [
    "*** Add File: ",
    "*** Update File: ",
    "*** Delete File: ",
    "*** Move to: ",
];

/// Every string that may carry patch text in the shapes Codex sends: a bare
/// string, `{"patch"|"input"|"cmd"|"command": "<patch>"}`, or a `command` argv
/// array such as `["apply_patch", "<patch>"]`. All of them are collected so a
/// decoy candidate cannot hide a protected target.
fn patch_texts(input: &serde_json::Value) -> Vec<&str> {
    let mut texts = Vec::new();
    if let Some(text) = input.as_str() {
        texts.push(text);
    }
    for key in ["patch", "input", "cmd", "command"] {
        match input.get(key) {
            Some(serde_json::Value::String(text)) => texts.push(text),
            Some(serde_json::Value::Array(items)) => {
                texts.extend(items.iter().filter_map(serde_json::Value::as_str));
            }
            _ => {}
        }
    }
    texts
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

/// Decide a patch from its candidate texts. The reason never echoes patch
/// content: only the protected file's own name.
fn patch_verdict(texts: &[&str], cwd: &str, home: Option<&Path>) -> Option<String> {
    if texts.is_empty() {
        return Some(UNRECOGNIZED_PAYLOAD.into());
    }
    let targets: Vec<&str> = texts.iter().copied().flat_map(patch_targets).collect();
    if targets.is_empty() {
        return Some(NO_TARGETS.into());
    }
    let Some(home) = home else {
        return Some(NO_HOME.into());
    };
    // Codex trims header lines, so the trimmed spelling is what gets written.
    targets
        .iter()
        .any(|path| protected_file(path, cwd, home) || protected_file(path.trim(), cwd, home))
        .then(|| protected_target_reason(home))
}

fn protected_target_reason(home: &Path) -> String {
    let name = crate::clud_settings::settings_path_at(home)
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_default();
    format!("[protected-target] the patch targets protected `{name}`; {REFUSAL}")
}

fn apply_patch_reason(payload: &super::HookPayloadView, home: Option<&Path>) -> Option<String> {
    let texts = payload
        .tool_input
        .as_ref()
        .map(patch_texts)
        .unwrap_or_default();
    patch_verdict(&texts, &payload.cwd.to_string_lossy(), home)
}

/// Codex may deliver `apply_patch` through a shell-style tool
/// (`apply_patch <<'EOF' ... EOF`, optionally after a `cd`). Such a command is
/// judged by the patch's targets, not by whether its text mentions the settings
/// file: a source file may legitimately discuss it. `None` means the command is
/// not a plain nested patch and the general shell rules apply.
fn nested_patch_reason(
    command: &str,
    dialect: super::ShellDialect,
    cwd: &Path,
    home: Option<&Path>,
) -> Option<Option<String>> {
    let script =
        super::block_bad_cmd_shell::parse_command_with_home(command, dialect, Some(cwd), home);
    if script.has_error {
        return None;
    }
    let mut patches = Vec::new();
    for invocation in &script.invocations {
        match invocation.effective_program().as_deref() {
            Some("cd") if invocation.redirects.iter().all(|r| !r.writes_file()) => {}
            Some("apply_patch" | "applypatch")
                if invocation.env.is_empty()
                    && invocation.redirects.iter().all(|r| !r.writes_file()) =>
            {
                patches.push(invocation);
            }
            _ => return None,
        }
    }
    let [patch] = patches.as_slice() else {
        return None;
    };
    let work = patch.cwd.as_literal()?;
    // The parser may attach the heredoc body to the `cd` that precedes the patch.
    let mut texts: Vec<&str> = script
        .invocations
        .iter()
        .filter_map(|invocation| invocation.heredoc.as_deref())
        .collect();
    texts.extend(patch.words.iter().skip(1).filter_map(Value::as_literal));
    Some(patch_verdict(&texts, work, home))
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
        let path = crate::path_norm::win32_file_name(path);
        crate::path_norm::display_slash(&canonical_creation_target(&path)).to_ascii_lowercase()
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
        assert_protected(patch_reason(home.path(), serde_json::json!(protected)));
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
                assert_protected(patch_reason(home.path(), input));
            }
        }
    }

    #[test]
    fn trailing_whitespace_on_a_protected_target_is_refused() {
        let home = tempfile::tempdir().unwrap();
        let settings = crate::clud_settings::settings_path_at(home.path());
        for pad in [" ", "\t", "  \t "] {
            let patch = format!(
                "*** Begin Patch\n*** Update File: {}{pad}\n@@\n-a\n+b\n*** End Patch",
                settings.display()
            );
            assert_protected(patch_reason(home.path(), serde_json::json!(patch)));
        }
    }

    #[test]
    fn decoy_candidates_do_not_hide_a_protected_target() {
        let home = tempfile::tempdir().unwrap();
        let settings = crate::clud_settings::settings_path_at(home.path());
        let protected = format!("*** Update File: {}\n@@\n-a\n+b", settings.display());
        let wrapped = format!("*** Begin Patch\n{protected}\n*** End Patch");
        for input in [
            serde_json::json!({"patch": ORDINARY, "command": wrapped}),
            serde_json::json!({"input": ORDINARY, "patch": wrapped}),
            serde_json::json!({"command": ["apply_patch", ORDINARY, wrapped]}),
            serde_json::json!({"command": ["apply_patch", ORDINARY, protected]}),
        ] {
            assert_protected(patch_reason(home.path(), input));
        }
    }

    #[test]
    fn multiple_ordinary_candidates_are_still_allowed() {
        let home = tempfile::tempdir().unwrap();
        let other = "*** Begin Patch\n*** Add File: src/other.rs\n+x\n*** End Patch";
        for input in [
            serde_json::json!({"patch": ORDINARY, "command": other}),
            serde_json::json!({"command": ["apply_patch", ORDINARY, other]}),
        ] {
            assert_eq!(patch_reason(home.path(), input.clone()), None, "{input}");
        }
    }

    fn assert_protected(got: Option<String>) {
        let got = got.expect("protected target must be refused");
        assert!(got.starts_with("[protected-target]"), "{got}");
        assert!(
            got.contains("settings.json") && got.contains(REFUSAL),
            "{got}"
        );
    }

    #[test]
    fn unrecognized_payloads_fail_closed_with_their_own_code() {
        let home = tempfile::tempdir().unwrap();
        for input in [
            serde_json::json!({}),
            serde_json::json!(42),
            serde_json::json!({"command": []}),
            serde_json::json!({"command": [1, 2]}),
        ] {
            let got = patch_reason(home.path(), input.clone()).expect("fail closed");
            assert_eq!(got, UNRECOGNIZED_PAYLOAD, "{input}");
            assert!(!got.contains("extra_roots"), "{input}");
        }
    }

    #[test]
    fn headerless_patches_fail_closed_with_their_own_code() {
        let home = tempfile::tempdir().unwrap();
        for input in [
            serde_json::json!("*** Begin Patch\n@@\n+No target header\n*** End Patch"),
            serde_json::json!({"cmd": "apply_patch <<'EOF'\n@@\n+x\nEOF"}),
        ] {
            let got = patch_reason(home.path(), input.clone()).expect("fail closed");
            assert_eq!(got, NO_TARGETS, "{input}");
            assert!(!got.contains("+No target"), "{input}");
        }
    }

    fn nested(home: &Path, tool: &str, input: serde_json::Value) -> Option<String> {
        let mut payload = patch_payload(home, input);
        payload.tool_name = tool.into();
        payload.command = payload
            .tool_input
            .as_ref()
            .and_then(|v| v.get("cmd").or_else(|| v.get("command")))
            .and_then(serde_json::Value::as_str)
            .unwrap_or_default()
            .into();
        reason_for_payload_in(&payload, Some(home.to_path_buf()))
    }

    #[test]
    fn nested_exec_command_patches_are_judged_by_their_targets() {
        let home = tempfile::tempdir().unwrap();
        let settings = crate::clud_settings::settings_path_at(home.path());
        // Source text that merely discusses the protected file is ordinary.
        let source = "*** Begin Patch\n*** Update File: src/lib.rs\n@@\n\
            -// reads ~/.clud/settings.json and extra_roots\n+// ok\n*** End Patch";
        let ordinary = format!("apply_patch <<'EOF'\n{source}\nEOF");
        let with_cd = format!(
            "cd {} && apply_patch <<'EOF'\n{source}\nEOF",
            home.path().display()
        );
        for (tool, key, command) in [
            ("exec_command", "cmd", &ordinary),
            ("exec_command", "cmd", &with_cd),
            ("shell_command", "command", &ordinary),
            ("apply_patch", "command", &ordinary),
        ] {
            let input = serde_json::json!({ key: command });
            assert_eq!(nested(home.path(), tool, input), None, "{tool} {command}");
        }
        let hit = format!(
            "apply_patch <<'EOF'\n*** Begin Patch\n*** Update File: {}\n@@\n-a\n+b\n*** End Patch\nEOF",
            settings.display()
        );
        assert_protected(nested(
            home.path(),
            "exec_command",
            serde_json::json!({"cmd": hit}),
        ));
        let headerless = "apply_patch <<'EOF'\n@@\n+x\nEOF";
        assert_eq!(
            nested(
                home.path(),
                "exec_command",
                serde_json::json!({"cmd": headerless})
            )
            .as_deref(),
            Some(NO_TARGETS)
        );
        // A relative target resolves against the `cd` directory, not the payload cwd.
        let relative = format!(
            "cd {} && apply_patch <<'EOF'\n*** Begin Patch\n*** Update File: settings.json\n@@\n-a\n+b\n*** End Patch\nEOF",
            settings.parent().unwrap().display()
        );
        assert_protected(nested(
            home.path(),
            "exec_command",
            serde_json::json!({"cmd": relative}),
        ));
    }

    #[test]
    fn direct_apply_patch_reaches_the_gate_through_the_entry_point() {
        let home = tempfile::tempdir().unwrap();
        let payload = patch_payload(home.path(), serde_json::json!({"command": ORDINARY}));
        assert_eq!(
            reason_for_payload_in(&payload, Some(home.path().to_path_buf())),
            None
        );
        let garbage = patch_payload(home.path(), serde_json::json!(7));
        assert_eq!(
            reason_for_payload_in(&garbage, Some(home.path().to_path_buf())).as_deref(),
            Some(UNRECOGNIZED_PAYLOAD)
        );
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
