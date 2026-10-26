//! `.claude/worktrees/` is not a place for work (#1962, #1971).
//!
//! Claude Code used to create its isolated-agent worktrees there, and agents
//! took the directory as a place to drop notes and checkouts of their own.
//! Nothing reclaims it, and a checkout inside the repo is walked by every
//! linter and build. clud's `WorktreeCreate` hook now builds Claude Code's
//! worktrees under `~/.clud/worktrees` (see `claude_worktree_hook.rs`), so this
//! guard refuses everything else that would create the directory or a new
//! entry in it: shell commands (`mkdir`, `touch`, `cp`/`mv`, `tee`, an output
//! redirect, `git worktree add`, `git clone`), read from the evaluated syntax
//! tree, and the file tools (`Write`, `Edit`, ...).
//!
//! Any path with consecutive `.claude/worktrees` components counts, so the
//! rule covers the repo's and the home directory's alike. Work *inside* an
//! existing checkout there (one with a `.git` entry) stays allowed, so a
//! session already running in one is not broken mid-task; cleanup (removing
//! or moving entries out) writes nothing there and is never refused. A
//! target whose value is only known at run time is not judged.

use super::block_bad_cmd_shell::parse_command_with_home;
use super::*;
use crate::shell_ast::{Invocation, Value as Word};

/// Rule id for `CLUD_BAD_CMD_OVERRIDE=claude-worktrees-dir:<reason>`.
pub(super) const RULE_ID: &str = "claude-worktrees-dir";

const FILE_TOOLS: &[&str] = &["Write", "Edit", "MultiEdit", "NotebookEdit"];

/// The refusal for this tool call, or `None` to allow it.
pub(super) fn reason(
    tool_name: &str,
    tool_input: Option<&Value>,
    command: &str,
    dialect: ShellDialect,
    cwd: &Path,
) -> Option<String> {
    let root = crate::gc::worktree_root::worktree_root();
    reason_in(
        tool_name,
        tool_input,
        command,
        dialect,
        cwd,
        root.as_deref(),
    )
}

/// [`reason`] with clud's worktree root given explicitly; a refused
/// `git worktree add` is handed the same command rewritten to land there.
pub(super) fn reason_in(
    tool_name: &str,
    tool_input: Option<&Value>,
    command: &str,
    dialect: ShellDialect,
    cwd: &Path,
    root: Option<&Path>,
) -> Option<String> {
    let base = cwd.to_string_lossy();
    let (target, rewrite) = if FILE_TOOLS.contains(&tool_name) {
        file_tool_target(tool_input?, &base)
            .filter(|path| is_forbidden(path))
            .map(|path| (path, None))
    } else if block_bad_cmd_gate::gates_tool(tool_name) && !command.trim().is_empty() {
        shell_target(command, dialect, cwd, root)
    } else {
        None
    }?;
    if let Some(override_reason) = accepted_override_reason(RULE_ID) {
        append_log(&format!(
            "BAD_CMD_OVERRIDE accepted rule={RULE_ID} reason={override_reason:?} path={target:?}"
        ));
        return None;
    }
    let advice = rewrite.map(|rewrite| rewrite.advice()).unwrap_or_default();
    Some(refusal(&target) + &advice)
}

fn refusal(target: &str) -> String {
    format!(
        "[{RULE_ID}] `{target}` is inside `.claude/worktrees/`, which is not a place for work: \
         nothing there is ever reclaimed, and a checkout inside the repo is walked by every \
         linter and build. Use a worktree only when the work needs one (#1971); then create it \
         with `git worktree add ~/.clud/worktrees/<repo>-wt-<name> -b <branch>` (Claude Code's own \
         isolated agents are placed there automatically). Put notes and scratch files in the \
         session scratchpad. {}",
        override_hint(RULE_ID)
    )
}

fn file_tool_target(input: &Value, cwd: &str) -> Option<String> {
    let path = ["file_path", "notebook_path", "path"]
        .iter()
        .find_map(|key| input.get(key).and_then(Value::as_str))?;
    Some(crate::shell_ast::join_path(cwd, path))
}

/// The first path a command would create inside `.claude/worktrees/`, and,
/// when it is a `git worktree add`, the same command rewritten to land in
/// clud's worktree `root`. `~` is pinned to the home that `root` lives in.
fn shell_target(
    command: &str,
    dialect: ShellDialect,
    cwd: &Path,
    root: Option<&Path>,
) -> Option<(String, Option<WorktreeAddRewrite>)> {
    let home = root.and_then(Path::parent).and_then(Path::parent);
    let script = parse_command_with_home(command, dialect, Some(cwd), home);
    let fallback = cwd.to_string_lossy();
    script.invocations.iter().find_map(|invocation| {
        let base = invocation.cwd.as_literal().unwrap_or(&fallback);
        let target = created_paths(invocation, cwd)
            .into_iter()
            .map(|path| crate::shell_ast::join_path(base, &path))
            .find(|path| is_forbidden(path))?;
        let rewrite = root.and_then(|root| {
            let repo = repo_name(&target, base, home);
            rewrite_worktree_add(invocation, Some(cwd), root, &repo)
        });
        Some((target, rewrite))
    })
}

/// The repo a `.claude/worktrees` path belongs to: the directory holding
/// `.claude`, unless that is the home directory (whose `.claude` belongs to
/// no repo), in which case the directory the command runs in.
fn repo_name(target: &str, run_in: &str, home: Option<&Path>) -> String {
    let parts: Vec<&str> = target.split(['/', '\\']).collect();
    let owner = parts
        .windows(2)
        .position(|pair| pair[0] == ".claude" && pair[1] == "worktrees")
        .filter(|at| *at > 0)
        .map(|at| &parts[..at]);
    let in_home =
        owner.is_some_and(|owner| home.is_some_and(|home| Path::new(&owner.join("/")) == home));
    match owner.and_then(|owner| owner.last()) {
        Some(name) if !in_home && !name.is_empty() => (*name).to_string(),
        _ => Path::new(run_in)
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_default(),
    }
}

/// Paths `invocation` would create or write, as written (relative paths
/// resolve against its cwd). Unknown values are skipped.
fn created_paths(invocation: &Invocation, cwd: &Path) -> Vec<String> {
    let mut paths: Vec<String> = invocation
        .redirects
        .iter()
        .filter(|redirect| redirect.writes_file())
        .filter_map(|redirect| redirect.target.as_literal().map(str::to_string))
        .collect();
    let argv = invocation.effective_argv();
    let Some(program) = invocation.effective_program() else {
        return paths;
    };
    let args = literal_args(&argv[1..]);
    match program.as_str() {
        "mkdir" | "md" | "touch" | "tee" => paths.extend(positionals(&args)),
        "cp" | "mv" | "ln" | "install" | "rsync" | "copy" | "move" => {
            paths.extend(copy_destination(&args));
        }
        "new-item" | "ni" | "set-content" | "add-content" | "out-file" | "copy-item"
        | "move-item" | "cpi" | "mi" => paths.extend(cmdlet_target(&args)),
        "dd" => paths.extend(
            args.iter()
                .filter_map(|a| a.strip_prefix("of=").map(str::to_string)),
        ),
        "git" => {
            if let Some(Ok(capture)) = detect_git_path(invocation, Some(cwd)) {
                paths.push(capture.path.to_string_lossy().into_owned());
            }
        }
        _ => {}
    }
    paths
}

/// Literal arguments; an unknown one becomes an empty placeholder so
/// positions still line up but it never matches a path.
fn literal_args(words: &[Word]) -> Vec<String> {
    words
        .iter()
        .map(|word| word.as_literal().unwrap_or_default().to_string())
        .collect()
}

fn positionals(args: &[String]) -> Vec<String> {
    let mut out = Vec::new();
    let mut options_done = false;
    for arg in args {
        if !options_done && arg == "--" {
            options_done = true;
        } else if options_done || !arg.starts_with('-') {
            out.push(arg.clone());
        }
    }
    out.retain(|arg| !arg.is_empty());
    out
}

/// `cp a b dest`, `cp -t dest a b`, `--target-directory=dest`.
fn copy_destination(args: &[String]) -> Option<String> {
    for (index, arg) in args.iter().enumerate() {
        if arg == "-t" || arg == "--target-directory" {
            return args.get(index + 1).cloned();
        }
        if let Some(dir) = arg.strip_prefix("--target-directory=") {
            return Some(dir.to_string());
        }
    }
    let operands = positionals(args);
    (operands.len() >= 2).then(|| operands[operands.len() - 1].clone())
}

/// A PowerShell cmdlet's target: `-Path`/`-LiteralPath`/`-FilePath`/
/// `-Destination`, else its first positional argument.
fn cmdlet_target(args: &[String]) -> Option<String> {
    const TARGET: &[&str] = &["-path", "-literalpath", "-filepath", "-destination"];
    const VALUED: &[&str] = &[
        "-itemtype",
        "-type",
        "-value",
        "-encoding",
        "-name",
        "-inputobject",
    ];
    let mut positional = None;
    let mut index = 0;
    while let Some(arg) = args.get(index) {
        let lower = arg.to_ascii_lowercase();
        if TARGET.contains(&lower.as_str()) {
            return args.get(index + 1).cloned();
        }
        if VALUED.contains(&lower.as_str()) {
            index += 2;
            continue;
        }
        if !arg.starts_with('-') && !arg.is_empty() && positional.is_none() {
            positional = Some(arg.clone());
        }
        index += 1;
    }
    positional
}

/// Whether writing `path` would create the `.claude/worktrees` directory or
/// a new entry in it. A path inside an existing checkout there is fine.
fn is_forbidden(path: &str) -> bool {
    let parts: Vec<&str> = path.split(['/', '\\']).collect();
    let Some(at) = parts
        .windows(2)
        .position(|pair| pair[0] == ".claude" && pair[1] == "worktrees")
    else {
        return false;
    };
    let entry_at = at + 2;
    let Some(entry) = parts.get(entry_at).filter(|entry| !entry.is_empty()) else {
        return true;
    };
    let separator = if path.contains('\\') && !path.contains('/') {
        "\\"
    } else {
        "/"
    };
    let checkout = parts[..entry_at].join(separator) + separator + entry;
    !Path::new(&checkout).join(".git").exists()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_component_rule_finds_the_directory_anywhere() {
        assert!(is_forbidden("/repo/.claude/worktrees"));
        assert!(is_forbidden("/repo/.claude/worktrees/"));
        assert!(is_forbidden("/repo/.claude/worktrees/notes.md"));
        assert!(is_forbidden("/home/u/.claude/worktrees/x/y"));
        assert!(is_forbidden(r"C:\repo\.claude\worktrees\notes.md"));
        assert!(!is_forbidden("/repo/.claude/agents/x"));
        assert!(!is_forbidden("/repo/worktrees/x"));
        assert!(!is_forbidden("/repo/.claude-worktrees/x"));
    }

    #[test]
    fn copy_destinations_and_cmdlet_targets() {
        let args = |list: &[&str]| list.iter().map(|s| (*s).to_string()).collect::<Vec<_>>();
        assert_eq!(
            copy_destination(&args(&["-r", "a", "b", "dest"])).as_deref(),
            Some("dest")
        );
        assert_eq!(
            copy_destination(&args(&["-t", "dest", "a"])).as_deref(),
            Some("dest")
        );
        assert_eq!(copy_destination(&args(&["only"])), None);
        assert_eq!(
            cmdlet_target(&args(&["-ItemType", "Directory", r".claude\worktrees\x"])).as_deref(),
            Some(r".claude\worktrees\x")
        );
        assert_eq!(
            cmdlet_target(&args(&["-Path", "p", "-Value", "v"])).as_deref(),
            Some("p")
        );
    }
}
