//! Issue #1486: the argv policy of the in-session `git` and `gh` aliases.
//!
//! Inside a clud session an agent's `git clone` or `git worktree add` would
//! land wherever it chose, outside every GC root, and never be reclaimed.
//! The session aliases refuse exactly the argv forms that create a new
//! checkout and redirect the agent to `safe-gh-clone` / `safe-gh-worktree`,
//! which write only into `~/.clud/tmp-wt` through the one ordinal allocator
//! ([`crate::gc::worktree_root::alloc_wt_path`]). Everything else passes to
//! the real binary byte for byte. See `docs/architecture/git-gh-redirect.md`.
//!
//! [`git_refusal`] and [`gh_refusal`] are pure. [`refuse`] is the one
//! side-effecting step: it **reserves** the example path before it prints
//! the message, so the printed path exists (DD-125).

use std::path::{Path, PathBuf};

/// The helper that replaces `git clone` / `gh repo clone`.
pub const SAFE_CLONE: &str = "safe-gh-clone";
/// The helper that replaces `git worktree add`.
pub const SAFE_WORKTREE: &str = "safe-gh-worktree";

/// Frozen first line of each refusal (stderr contract, like the other shims'
/// fixed diagnostics). Tests and agents grep these; change them only with a
/// DD.
pub const REFUSED_GIT_CLONE: &str = "git clone is redirected inside a clud session";
pub const REFUSED_GIT_WORKTREE_ADD: &str = "git worktree add is redirected inside a clud session";
pub const REFUSED_GH_REPO_CLONE: &str = "gh repo clone is redirected inside a clud session";
pub const REFUSED_GH_REPO_FORK_CLONE: &str =
    "gh repo fork --clone is redirected inside a clud session";
pub const REFUSED_GH_REPO_CREATE_CLONE: &str =
    "gh repo create --clone is redirected inside a clud session";

/// Exit status of a refusal. The command never ran.
pub const REFUSAL_EXIT_CODE: i32 = 2;

/// The allocator suffix a clone reservation uses: `<repo>-wt-clone`.
pub const CLONE_SUFFIX: &str = "clone";

/// A refused command, with what the message needs to build a real example.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Refusal {
    GitClone {
        source: Option<String>,
    },
    GitWorktreeAdd {
        /// Every `-C` value, in order; each is relative to the one before.
        dirs: Vec<String>,
        /// The `<path>` operand, when present.
        path: Option<String>,
        /// The `-b` / `-B` value, when present.
        branch: Option<String>,
        /// The `worktree add` arguments minus `<path>`, to carry into the
        /// example so the agent can copy it as is.
        rest: Vec<String>,
    },
    GhRepoClone {
        repo: Option<String>,
    },
    GhRepoForkClone {
        repo: Option<String>,
    },
    GhRepoCreateClone {
        name: Option<String>,
    },
}

impl Refusal {
    pub fn headline(&self) -> &'static str {
        match self {
            Refusal::GitClone { .. } => REFUSED_GIT_CLONE,
            Refusal::GitWorktreeAdd { .. } => REFUSED_GIT_WORKTREE_ADD,
            Refusal::GhRepoClone { .. } => REFUSED_GH_REPO_CLONE,
            Refusal::GhRepoForkClone { .. } => REFUSED_GH_REPO_FORK_CLONE,
            Refusal::GhRepoCreateClone { .. } => REFUSED_GH_REPO_CREATE_CLONE,
        }
    }
}

/// git's global options that take a separate value (`-C <path>`).
const GIT_GLOBAL_VALUE_OPTS: &[&str] = &[
    "-C",
    "-c",
    "--git-dir",
    "--work-tree",
    "--namespace",
    "--config-env",
    "--attr-source",
];

/// The subcommand index git itself would dispatch, after its global options
/// (`-C <p>`, `-c k=v`, `--git-dir[=]`, `--work-tree[=]`, `--no-pager`,
/// `-P`, `--bare`, `--namespace[=]`, `--exec-path[=]`, ...), plus every `-C`
/// value seen on the way.
fn git_subcommand<'a>(args: &[&'a str]) -> Option<(usize, Vec<&'a str>)> {
    let mut dirs = Vec::new();
    let mut i = 0;
    while i < args.len() {
        let word = args[i];
        if word == "--exec-path" {
            // Bare `--exec-path` prints the path; only `=` sets it.
            i += 1;
        } else if GIT_GLOBAL_VALUE_OPTS.contains(&word) {
            if word == "-C" {
                dirs.push(*args.get(i + 1)?);
            }
            i += 2;
        } else if word.starts_with('-') {
            i += 1;
        } else {
            return Some((i, dirs));
        }
    }
    None
}

/// `git clone`'s options that take a separate value.
const GIT_CLONE_VALUE_OPTS: &[&str] = &[
    "-o",
    "--origin",
    "-b",
    "--branch",
    "-u",
    "--upload-pack",
    "--reference",
    "--reference-if-able",
    "--separate-git-dir",
    "--depth",
    "--shallow-since",
    "--shallow-exclude",
    "-c",
    "--config",
    "-j",
    "--jobs",
    "--filter",
    "--template",
    "--server-option",
    "--bundle-uri",
    "--ref-format",
];

/// The refusal for a `git` argv, or `None` to pass it through.
pub fn git_refusal(args: &[&str]) -> Option<Refusal> {
    let (index, dirs) = git_subcommand(args)?;
    let rest = &args[index + 1..];
    match args[index] {
        "clone" => Some(Refusal::GitClone {
            source: positionals(rest, GIT_CLONE_VALUE_OPTS).first().cloned(),
        }),
        "worktree" => {
            let sub = rest.iter().position(|w| !w.starts_with('-'))?;
            if rest[sub] != "add" {
                return None;
            }
            Some(worktree_add(
                dirs.iter().map(|d| d.to_string()).collect(),
                &rest[sub + 1..],
            ))
        }
        _ => None,
    }
}

fn worktree_add(dirs: Vec<String>, args: &[&str]) -> Refusal {
    let mut path = None;
    let mut branch = None;
    let mut rest = Vec::new();
    let mut options_done = false;
    let mut i = 0;
    while i < args.len() {
        let word = args[i];
        if !options_done && word == "--" {
            options_done = true;
            rest.push(word.to_string());
        } else if !options_done && matches!(word, "-b" | "-B" | "--reason") {
            rest.push(word.to_string());
            if let Some(value) = args.get(i + 1) {
                if word != "--reason" {
                    branch = Some(value.to_string());
                }
                rest.push(value.to_string());
                i += 1;
            }
        } else if !options_done && (word.starts_with("-b") || word.starts_with("-B")) {
            branch = Some(word[2..].to_string());
            rest.push(word.to_string());
        } else if !options_done && word.starts_with('-') {
            rest.push(word.to_string());
        } else if path.is_none() {
            path = Some(word.to_string());
        } else {
            rest.push(word.to_string());
        }
        i += 1;
    }
    Refusal::GitWorktreeAdd {
        dirs,
        path,
        branch,
        rest,
    }
}

/// Non-option words of `args`, skipping the value of each option in
/// `value_opts` and stopping at `--`.
fn positionals(args: &[&str], value_opts: &[&str]) -> Vec<String> {
    let mut out = Vec::new();
    let mut i = 0;
    while i < args.len() {
        let word = args[i];
        if word == "--" {
            break;
        }
        if value_opts.contains(&word) {
            i += 2;
            continue;
        }
        if !word.starts_with('-') {
            out.push(word.to_string());
        }
        i += 1;
    }
    out
}

/// `gh` flags that take a separate value on the `repo` commands.
const GH_VALUE_OPTS: &[&str] = &[
    "-R",
    "--repo",
    "--hostname",
    "--org",
    "--fork-name",
    "--remote-name",
    "-d",
    "--description",
    "-g",
    "--gitignore",
    "-l",
    "--license",
    "-p",
    "--template",
    "-s",
    "--source",
    "-t",
    "--team",
    "--homepage",
    "-u",
    "--upstream-branch",
];

/// Whether a `--clone` flag in `args` (before `--`) asks for a clone.
/// `--clone=<v>` counts unless `v` is one of cobra's false spellings.
fn asks_for_clone(args: &[&str], short_c: bool) -> bool {
    args.iter().take_while(|w| **w != "--").any(|word| {
        if *word == "--clone" || (short_c && *word == "-c") {
            return true;
        }
        match word.strip_prefix("--clone=") {
            Some(value) => !matches!(value, "false" | "False" | "FALSE" | "f" | "F" | "0"),
            None => false,
        }
    })
}

/// The refusal for a `gh` argv, or `None` to pass it through.
pub fn gh_refusal(args: &[&str]) -> Option<Refusal> {
    let words = positionals(args, GH_VALUE_OPTS);
    if words.first().map(String::as_str) != Some("repo") {
        return None;
    }
    let operand = words.get(2).cloned();
    match words.get(1).map(String::as_str) {
        Some("clone") => Some(Refusal::GhRepoClone { repo: operand }),
        Some("fork") if asks_for_clone(args, false) => {
            Some(Refusal::GhRepoForkClone { repo: operand })
        }
        Some("create") if asks_for_clone(args, true) => {
            Some(Refusal::GhRepoCreateClone { name: operand })
        }
        _ => None,
    }
}

/// A repo slug usable as one path component: the last component of a repo
/// spec (`owner/repo`, a URL, `git@host:owner/repo.git`, a local path),
/// without `.git`, with anything outside `[A-Za-z0-9._-]` replaced by `-`.
pub fn slug_from_spec(spec: &str) -> String {
    let trimmed = spec.trim_end_matches(['/', '\\']);
    let last = trimmed.rsplit(['/', '\\', ':']).next().unwrap_or(trimmed);
    let last = last.strip_suffix(".git").unwrap_or(last);
    sanitize(last)
}

/// One allocator-safe path component.
pub fn sanitize(raw: &str) -> String {
    let cleaned: String = raw
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-') {
                c
            } else {
                '-'
            }
        })
        .collect();
    let cleaned = cleaned.trim_matches(|c| c == '.' || c == '-').to_string();
    if cleaned.is_empty() {
        "repo".to_string()
    } else {
        cleaned
    }
}

/// The repo slug for a `worktree add` run in `dir`: the main checkout's
/// directory name, found by walking up to the first `.git`. A linked
/// worktree's `.git` file points into `<main>/.git/worktrees/<name>`, so the
/// slug is the main repo's even when run from another worktree. A
/// `<repo>-wt-<suffix>` name is cut back to `<repo>`.
pub fn repo_slug_for_dir(dir: &Path) -> String {
    let mut current = Some(dir);
    while let Some(candidate) = current {
        let dot_git = candidate.join(".git");
        if dot_git.is_dir() {
            return slug_from_dir_name(candidate);
        }
        if dot_git.is_file() {
            let main = std::fs::read_to_string(&dot_git)
                .ok()
                .and_then(|text| {
                    text.lines()
                        .find_map(|l| l.strip_prefix("gitdir:"))
                        .map(|p| candidate.join(p.trim()))
                })
                .and_then(|gitdir| {
                    // <main>/.git/worktrees/<name> -> <main>
                    gitdir
                        .parent()
                        .and_then(Path::parent)
                        .and_then(Path::parent)
                        .map(Path::to_path_buf)
                });
            return slug_from_dir_name(main.as_deref().unwrap_or(candidate));
        }
        current = candidate.parent();
    }
    slug_from_dir_name(dir)
}

fn slug_from_dir_name(dir: &Path) -> String {
    let name = dir
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    let name = match name.find(crate::gc::worktree_root::WT_INFIX) {
        Some(at) if at > 0 => name[..at].to_string(),
        _ => name,
    };
    sanitize(&name)
}

/// The suffix for a `worktree add` reservation: the first digit run of the
/// branch (an issue number, `feat/1486-x` -> `1486`), else of the requested
/// path's name, else that name itself, else `new`.
pub fn worktree_suffix(branch: Option<&str>, path: Option<&str>) -> String {
    let path_name = path.map(slug_from_spec);
    for text in [branch, path_name.as_deref()].into_iter().flatten() {
        let digits: String = text
            .chars()
            .skip_while(|c| !c.is_ascii_digit())
            .take_while(char::is_ascii_digit)
            .collect();
        if !digits.is_empty() {
            return digits;
        }
    }
    match path_name {
        Some(name) if name != "repo" => name,
        _ => "new".to_string(),
    }
}

/// POSIX-quote one word for a copyable example.
pub fn shell_quote(word: &str) -> String {
    let plain = !word.is_empty()
        && word
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || "-_./:=@%+,".contains(c));
    if plain {
        word.to_string()
    } else {
        format!("'{}'", word.replace('\'', r"'\''"))
    }
}

/// What a refusal reserves: `(slug, suffix)` for the allocator, and the
/// directory the command would have run in.
pub fn reservation_for(refusal: &Refusal, cwd: &Path) -> (String, String) {
    match refusal {
        Refusal::GitClone { source } => (
            source
                .as_deref()
                .map_or_else(|| "repo".to_string(), slug_from_spec),
            CLONE_SUFFIX.to_string(),
        ),
        Refusal::GhRepoClone { repo } | Refusal::GhRepoForkClone { repo } => (
            repo.as_deref()
                .map_or_else(|| repo_slug_for_dir(cwd), slug_from_spec),
            CLONE_SUFFIX.to_string(),
        ),
        Refusal::GhRepoCreateClone { name } => (
            name.as_deref()
                .map_or_else(|| "repo".to_string(), slug_from_spec),
            CLONE_SUFFIX.to_string(),
        ),
        Refusal::GitWorktreeAdd {
            dirs, path, branch, ..
        } => {
            let mut dir = cwd.to_path_buf();
            for d in dirs {
                dir = dir.join(d);
            }
            (
                repo_slug_for_dir(&dir),
                worktree_suffix(branch.as_deref(), path.as_deref()),
            )
        }
    }
}

/// The full refusal text. `reserved` is the path the allocator already
/// created, or the error that stopped it.
pub fn refusal_message(
    refusal: &Refusal,
    slug: &str,
    suffix: &str,
    reserved: &Result<PathBuf, String>,
) -> String {
    let mut out = String::new();
    out.push_str(refusal.headline());
    out.push('\n');
    let (usage, example_head, tail): (String, Vec<String>, Vec<String>) = match refusal {
        Refusal::GitWorktreeAdd { rest, .. } => (
            format!(
                "  use: {SAFE_WORKTREE} <repo-slug> <issue-number> [--path <reserved-dir>] \
                 [<git worktree add options>...]"
            ),
            vec![SAFE_WORKTREE.into(), slug.into(), suffix.into()],
            rest.clone(),
        ),
        Refusal::GitClone { source } | Refusal::GhRepoClone { repo: source } => (
            format!(
                "  use: {SAFE_CLONE} <owner/repo | url> [--path <reserved-dir>] \
                 [<git clone options>...]"
            ),
            vec![
                SAFE_CLONE.into(),
                source.clone().unwrap_or_else(|| "<owner/repo>".into()),
            ],
            Vec::new(),
        ),
        Refusal::GhRepoForkClone { .. } | Refusal::GhRepoCreateClone { .. } => (
            format!(
                "  use: run it again without --clone, then: {SAFE_CLONE} <owner/repo> \
                 [--path <reserved-dir>]"
            ),
            vec![SAFE_CLONE.into(), "<owner/repo>".into()],
            Vec::new(),
        ),
    };
    out.push_str(&usage);
    out.push('\n');
    match reserved {
        Ok(path) => {
            let shown = path.display().to_string();
            let mut example: Vec<String> = example_head
                .iter()
                .map(|w| {
                    if w.starts_with('<') {
                        w.clone()
                    } else {
                        shell_quote(w)
                    }
                })
                .collect();
            example.push("--path".into());
            example.push(shell_quote(&shown));
            example.extend(tail.iter().map(|w| shell_quote(w)));
            out.push_str(&format!("  e.g. {}\n", example.join(" ")));
            let name = path
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_default();
            out.push_str(&format!(
                "  -> {shown}  (reserved now; without --path a call takes the next free \
                 ordinal, e.g. {name}-2)\n"
            ));
        }
        Err(error) => {
            let base = format!("{slug}{}{suffix}", crate::gc::worktree_root::WT_INFIX);
            out.push_str(&format!(
                "  e.g. {} {}  ->  ~/.clud/tmp-wt/{base}  (could not reserve it now: {error})\n",
                example_head.join(" "),
                tail.join(" ")
            ));
        }
    }
    out
}

/// Reserve the example path, print the refusal to stderr, and return
/// [`REFUSAL_EXIT_CODE`]. The refused command is never run.
pub fn refuse(refusal: &Refusal) -> i32 {
    let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    let (slug, suffix) = reservation_for(refusal, &cwd);
    let reserved =
        crate::gc::worktree_root::alloc_wt_path(&slug, &suffix).map_err(|error| error.to_string());
    eprint!("{}", refusal_message(refusal, &slug, &suffix, &reserved));
    REFUSAL_EXIT_CODE
}

#[cfg(test)]
#[path = "git_gh_policy_tests.rs"]
mod tests;
