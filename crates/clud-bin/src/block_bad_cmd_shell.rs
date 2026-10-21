//! The scanner's view of a command: parsed and evaluated by `shell_ast`
//! (#1962). Rules read [`Invocation`]s; nothing here splits text by hand.

use super::*;
use crate::shell_ast::{self, BaseEnv, Dialect, Invocation, Script};

fn ast_dialect(dialect: ShellDialect) -> Dialect {
    match dialect {
        ShellDialect::Posix => Dialect::Posix,
        ShellDialect::PowerShell => Dialect::PowerShell,
        ShellDialect::Cmd => Dialect::Cmd,
    }
}

/// Parse and evaluate `text` as `dialect` would run it from `cwd`, starting
/// from this process's environment (the hook inherits the agent shell's).
pub(super) fn parse_command(text: &str, dialect: ShellDialect, cwd: Option<&Path>) -> Script {
    parse_command_with_home(text, dialect, cwd, None)
}

/// [`parse_command`] with `~` / `$HOME` / `%USERPROFILE%` pinned to `home`.
pub(super) fn parse_command_with_home(
    text: &str,
    dialect: ShellDialect,
    cwd: Option<&Path>,
    home: Option<&Path>,
) -> Script {
    let mut base = BaseEnv::from_process(cwd.unwrap_or_else(|| Path::new("")));
    if let Some(home) = home {
        let home = home.to_string_lossy().into_owned();
        base = base
            .with_var("HOME", home.clone())
            .with_var("USERPROFILE", home);
    }
    shell_ast::parse(text, ast_dialect(dialect), &base)
}

/// The words rules match for one command: its argv text with the
/// transparent launchers `env`, `command` and `exec` removed. Unknown words
/// read as their source text (`$X`, `$(cmd)`), which is what the rules saw
/// before #1962 and what their messages quote. Empty when a launcher's
/// options cannot be read.
pub(super) fn rule_words(invocation: &Invocation) -> Vec<String> {
    unwrap_transparent_wrappers(&invocation.argv()).unwrap_or_default()
}

/// `env -S 'a b'` / `--split-string=...`: env splits the string itself, with
/// shell-like quoting, before running it.
pub(super) fn split_env_string(text: &str) -> Vec<String> {
    parse_command(text, ShellDialect::Posix, None)
        .invocations
        .first()
        .map(Invocation::argv)
        .unwrap_or_default()
}

pub(super) fn program_name(word: &str) -> String {
    let cleaned = word.trim().trim_matches(&['\'', '"'][..]);
    crate::path_norm::file_stem_any_separator(cleaned)
        .unwrap_or_default()
        .to_ascii_lowercase()
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
