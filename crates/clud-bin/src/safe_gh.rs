//! Issue #1486: `safe-gh-clone` and `safe-gh-worktree`, the commands the
//! in-session `git` / `gh` aliases redirect clones and `worktree add` to.
//!
//! Both are argv\[0\] names of the one `clud` binary (DD-121) with a
//! `Native` registry row, so they work in and out of a session. Each writes
//! only into `~/.clud/tmp-wt`: into a directory the caller already reserved
//! (`--path`, printed by a refusal) or into a fresh one from
//! [`crate::gc::worktree_root::alloc_wt_path`]. They run the **real** `git` /
//! `gh` ([`crate::shim_registry::real_program`]), never the session alias,
//! and print the absolute destination as the last line of stdout after the
//! real command succeeds. See `docs/architecture/git-gh-redirect.md`.

use std::path::{Path, PathBuf};

use crate::git_gh_policy::{self as policy, SAFE_CLONE, SAFE_WORKTREE};

const USAGE_CLONE: &str =
    "usage: safe-gh-clone <owner/repo | url | path> [--path <reserved-dir>] [<git clone options>...]";
const USAGE_WORKTREE: &str = "usage: safe-gh-worktree <repo-slug> <issue-number> [--path <reserved-dir>] [<git worktree add options>...]";

/// Parsed helper arguments: positionals in order, the `--path` value, and
/// everything else (options for the real command) in order.
#[derive(Debug, Default, PartialEq, Eq)]
struct Parsed {
    positionals: Vec<String>,
    path: Option<String>,
    rest: Vec<String>,
    help: bool,
}

/// `want` leading positionals, then everything else as pass-through. A
/// `--path <p>` / `--path=<p>` anywhere before `--` is the reservation.
fn parse(args: &[String], want: usize) -> Result<Parsed, String> {
    let mut parsed = Parsed::default();
    let mut i = 0;
    let mut after_dashdash = false;
    while i < args.len() {
        let word = args[i].as_str();
        if !after_dashdash && word == "--" {
            after_dashdash = true;
        } else if !after_dashdash && word == "--path" {
            let value = args
                .get(i + 1)
                .ok_or_else(|| "--path needs a value".to_string())?;
            parsed.path = Some(value.clone());
            i += 1;
        } else if let Some(value) = word.strip_prefix("--path=").filter(|_| !after_dashdash) {
            parsed.path = Some(value.to_string());
        } else if !after_dashdash
            && matches!(word, "-h" | "--help")
            && parsed.positionals.is_empty()
        {
            parsed.help = true;
        } else if parsed.positionals.len() < want && !word.starts_with('-') {
            parsed.positionals.push(word.to_string());
        } else {
            parsed.rest.push(word.to_string());
        }
        i += 1;
    }
    Ok(parsed)
}

/// A `--path` must be an existing, empty directory directly inside the
/// worktree root: exactly what a refusal reserved.
fn validate_reserved(path: &str, root: &Path) -> Result<PathBuf, String> {
    let path = PathBuf::from(path);
    let canon = std::fs::canonicalize(&path)
        .map_err(|error| format!("--path {}: {error}", path.display()))?;
    let root = std::fs::canonicalize(root).unwrap_or_else(|_| root.to_path_buf());
    if canon.parent() != Some(root.as_path()) {
        return Err(format!(
            "--path {} is not a directory directly inside {}",
            path.display(),
            root.display()
        ));
    }
    let empty = std::fs::read_dir(&canon)
        .map(|mut entries| entries.next().is_none())
        .map_err(|error| format!("--path {}: {error}", path.display()))?;
    if !empty {
        return Err(format!(
            "--path {} is not empty; it is already in use",
            path.display()
        ));
    }
    Ok(canon)
}

/// The destination: the validated reservation, or a fresh allocation.
fn destination(
    reserved: Option<&str>,
    root: &Path,
    slug: &str,
    suffix: &str,
) -> Result<(PathBuf, bool), String> {
    match reserved {
        Some(path) => validate_reserved(path, root).map(|p| (p, false)),
        None => crate::gc::worktree_root::alloc_wt_path_in(root, slug, suffix)
            .map(|p| (p, true))
            .map_err(|error| error.to_string()),
    }
}

/// Whether `spec` names something `git clone` takes directly (a URL, an
/// scp-style address, or an existing local path) rather than a GitHub
/// `owner/repo` for `gh repo clone`.
fn is_git_source(spec: &str) -> bool {
    spec.contains("://") || spec.starts_with("git@") || Path::new(spec).exists()
}

/// The real program, never the session alias.
fn program(name: &str) -> String {
    crate::shim_registry::real_program(name)
        .map(|p| p.to_string_lossy().into_owned())
        .unwrap_or_else(|| name.to_string())
}

/// `gh repo clone` runs `git` by PATH lookup, which inside a session would
/// hit the alias and be refused. Put the real `git`'s directory first.
fn child_env() -> Vec<(String, String)> {
    let mut env: Vec<(String, String)> = std::env::vars().collect();
    if let Some(dir) = crate::shim_registry::real_program("git")
        .and_then(|git| git.parent().map(Path::to_path_buf))
    {
        crate::shim_session::prepend_to_path(&mut env, &dir);
    }
    env
}

fn run_real(argv: Vec<String>) -> Result<i32, String> {
    let child =
        crate::subprocess::ManagedSubprocess::start(argv.clone(), None, child_env(), false, None)
            .map_err(|error| format!("cannot start {}: {error}", argv[0]))?;
    child.wait(None)
}

/// Release a directory this call allocated when the real command failed,
/// only if it is still empty, so a failure does not leak a reservation.
fn release_if_ours(dest: &Path, allocated: bool) {
    if allocated {
        let _ = std::fs::remove_dir(dest);
    }
}

fn finish(label: &str, dest: &Path, allocated: bool, outcome: Result<i32, String>) -> i32 {
    match outcome {
        Ok(0) => {
            println!("{}", dest.display());
            0
        }
        Ok(code) => {
            release_if_ours(dest, allocated);
            code
        }
        Err(error) => {
            release_if_ours(dest, allocated);
            eprintln!("{label}: {error}");
            1
        }
    }
}

fn worktree_root() -> Result<PathBuf, String> {
    crate::gc::worktree_root::worktree_root()
        .ok_or_else(|| "no home directory; cannot resolve ~/.clud/tmp-wt".to_string())
}

/// `safe-gh-clone <owner/repo | url | path> [--path <reserved>] [opts...]`.
pub fn run_clone(args: &[String]) -> i32 {
    let parsed = match parse(args, 1) {
        Ok(parsed) => parsed,
        Err(error) => return usage(SAFE_CLONE, USAGE_CLONE, &error),
    };
    if parsed.help {
        println!("{USAGE_CLONE}");
        return 0;
    }
    let Some(source) = parsed.positionals.first() else {
        return usage(SAFE_CLONE, USAGE_CLONE, "missing <owner/repo | url | path>");
    };
    let root = match worktree_root() {
        Ok(root) => root,
        Err(error) => return usage(SAFE_CLONE, USAGE_CLONE, &error),
    };
    let slug = policy::slug_from_spec(source);
    let (dest, allocated) =
        match destination(parsed.path.as_deref(), &root, &slug, policy::CLONE_SUFFIX) {
            Ok(found) => found,
            Err(error) => return usage(SAFE_CLONE, USAGE_CLONE, &error),
        };
    let dest_arg = dest.to_string_lossy().into_owned();
    let argv = if is_git_source(source) {
        let mut argv = vec![program("git"), "clone".into()];
        argv.extend(parsed.rest.iter().cloned());
        argv.extend([source.clone(), dest_arg]);
        argv
    } else {
        let mut argv = vec![
            program("gh"),
            "repo".into(),
            "clone".into(),
            source.clone(),
            dest_arg,
        ];
        if !parsed.rest.is_empty() {
            argv.push("--".into());
            argv.extend(parsed.rest.iter().cloned());
        }
        argv
    };
    finish(SAFE_CLONE, &dest, allocated, run_real(argv))
}

/// `safe-gh-worktree <repo-slug> <issue-number> [--path <reserved>] [opts...]`,
/// run inside the repository. The options go to `git worktree add` after the
/// destination (`-b feat/x origin/main`).
pub fn run_worktree(args: &[String]) -> i32 {
    let parsed = match parse(args, 2) {
        Ok(parsed) => parsed,
        Err(error) => return usage(SAFE_WORKTREE, USAGE_WORKTREE, &error),
    };
    if parsed.help {
        println!("{USAGE_WORKTREE}");
        return 0;
    }
    let [slug, issue] = parsed.positionals.as_slice() else {
        return usage(
            SAFE_WORKTREE,
            USAGE_WORKTREE,
            "missing <repo-slug> <issue-number>",
        );
    };
    let root = match worktree_root() {
        Ok(root) => root,
        Err(error) => return usage(SAFE_WORKTREE, USAGE_WORKTREE, &error),
    };
    let slug = policy::slug_from_spec(slug);
    let (dest, allocated) = match destination(parsed.path.as_deref(), &root, &slug, issue) {
        Ok(found) => found,
        Err(error) => return usage(SAFE_WORKTREE, USAGE_WORKTREE, &error),
    };
    let mut argv = vec![
        program("git"),
        "worktree".into(),
        "add".into(),
        dest.to_string_lossy().into_owned(),
    ];
    argv.extend(parsed.rest.iter().cloned());
    finish(SAFE_WORKTREE, &dest, allocated, run_real(argv))
}

fn usage(label: &str, usage: &str, error: &str) -> i32 {
    eprintln!("{label}: {error}\n{usage}");
    2
}

#[cfg(test)]
mod tests {
    use super::*;

    fn strings(words: &[&str]) -> Vec<String> {
        words.iter().map(|w| w.to_string()).collect()
    }

    #[test]
    fn parse_takes_leading_positionals_path_and_the_rest_in_order() {
        let parsed = parse(
            &strings(&[
                "clud",
                "432",
                "--path",
                "/r/clud-wt-432",
                "-b",
                "feat/x",
                "origin/main",
            ]),
            2,
        )
        .unwrap();
        assert_eq!(parsed.positionals, strings(&["clud", "432"]));
        assert_eq!(parsed.path.as_deref(), Some("/r/clud-wt-432"));
        assert_eq!(parsed.rest, strings(&["-b", "feat/x", "origin/main"]));
        let parsed = parse(&strings(&["--path=/p", "x", "--depth", "1"]), 1).unwrap();
        assert_eq!(parsed.path.as_deref(), Some("/p"));
        assert_eq!(parsed.positionals, strings(&["x"]));
        assert_eq!(parsed.rest, strings(&["--depth", "1"]));
        assert!(parse(&strings(&["x", "--path"]), 1).is_err());
        assert!(parse(&strings(&["--help"]), 1).unwrap().help);
    }

    #[test]
    fn a_reservation_must_be_an_empty_dir_directly_in_the_root() {
        let home = tempfile::tempdir().unwrap();
        let root = crate::gc::worktree_root::ensure_worktree_root_at(home.path()).unwrap();
        let good = crate::gc::worktree_root::alloc_wt_path_in(&root, "clud", "432").unwrap();
        assert_eq!(
            validate_reserved(&good.to_string_lossy(), &root).unwrap(),
            std::fs::canonicalize(&good).unwrap()
        );
        std::fs::write(good.join("f"), b"x").unwrap();
        assert!(validate_reserved(&good.to_string_lossy(), &root)
            .unwrap_err()
            .contains("not empty"));
        let nested = root.join("a").join("b");
        std::fs::create_dir_all(&nested).unwrap();
        assert!(validate_reserved(&nested.to_string_lossy(), &root)
            .unwrap_err()
            .contains("directly inside"));
        let outside = home.path().join("elsewhere");
        std::fs::create_dir_all(&outside).unwrap();
        assert!(validate_reserved(&outside.to_string_lossy(), &root).is_err());
        assert!(validate_reserved(&root.join("missing").to_string_lossy(), &root).is_err());
    }

    /// #1486 acceptance 6: without `--path`, consecutive calls allocate
    /// `clud-wt-432` then `clud-wt-432-2`, each existing on return.
    #[test]
    fn destination_allocates_the_next_ordinal() {
        let home = tempfile::tempdir().unwrap();
        let root = crate::gc::worktree_root::worktree_root_for(home.path());
        let (first, allocated) = destination(None, &root, "clud", "432").unwrap();
        assert!(allocated && first.is_dir());
        let (second, _) = destination(None, &root, "clud", "432").unwrap();
        assert!(second.is_dir());
        assert_eq!(first, root.join("clud-wt-432"));
        assert_eq!(second, root.join("clud-wt-432-2"));
        let (reused, allocated) =
            destination(Some(&first.to_string_lossy()), &root, "clud", "432").unwrap();
        assert!(!allocated, "a --path reservation is not ours to release");
        assert_eq!(reused, std::fs::canonicalize(&first).unwrap());
    }

    #[test]
    fn sources_split_between_git_and_gh() {
        assert!(is_git_source("https://github.com/zackees/clud"));
        assert!(is_git_source("git@github.com:zackees/clud.git"));
        assert!(is_git_source(&std::env::temp_dir().to_string_lossy()));
        assert!(!is_git_source("zackees/clud-definitely-not-a-local-path"));
    }
}
