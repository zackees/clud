//! Detect a repo's `./lint` and `./test` entry scripts for `/grind` (#1336).
//!
//! The `/grind` router carries a `` !`clud grind-scripts` `` line. Claude Code
//! runs it when the skill loads and hands the output to the model verbatim,
//! so this module renders the facts the router needs to ask its one
//! "run them before each push?" question: which scripts exist, the command
//! that runs each one on this platform, and the files to read to discover
//! the scripts' user-facing modes.
//!
//! Nothing here ever executes a script: a script that ignores `--help` would
//! start the whole suite. Modes are found by the router reading the source.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

/// One candidate script found at the repo root.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScriptFile {
    /// File name relative to the repo root, e.g. `lint`, `test.bat`.
    pub name: String,
    /// Whether the file has an executable bit (always `false` on Windows).
    pub executable: bool,
    /// The script's text, used to find what it delegates to.
    pub contents: String,
}

/// Everything [`render`] needs, gathered from the filesystem by [`gather`].
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ScriptFacts {
    /// Candidate scripts present at the repo root.
    pub files: Vec<ScriptFile>,
    /// Render for Windows (prefers `.bat` / `.ps1`).
    pub windows: bool,
    /// Repo-relative paths (forward slashes) that exist; delegation targets
    /// referenced by a script are only listed when they are in this set.
    pub existing: BTreeSet<String>,
}

/// A chosen script and the command that runs it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Chosen {
    pub name: String,
    pub command: String,
    /// The script itself plus the local files it delegates to.
    pub read_for_modes: Vec<String>,
}

const KINDS: [&str; 2] = ["lint", "test"];

fn candidates(kind: &str, windows: bool) -> Vec<String> {
    let exts: &[&str] = if windows {
        &[".bat", ".ps1", "", ".sh"]
    } else {
        &["", ".sh"]
    };
    exts.iter().map(|e| format!("{kind}{e}")).collect()
}

/// The command that runs `file` on this platform, without executing it.
pub fn run_command(file: &ScriptFile, windows: bool) -> String {
    let name = &file.name;
    if name.ends_with(".bat") {
        format!("cmd /c {name}")
    } else if name.ends_with(".ps1") {
        format!("pwsh -File {name}")
    } else if file.executable && !windows {
        format!("./{name}")
    } else {
        format!("bash ./{name}")
    }
}

/// Pick the platform-native script for `kind` (`lint` or `test`).
pub fn choose(facts: &ScriptFacts, kind: &str) -> Option<Chosen> {
    let file = candidates(kind, facts.windows)
        .into_iter()
        .find_map(|c| facts.files.iter().find(|f| f.name == c))?;
    let mut read_for_modes = vec![file.name.clone()];
    for target in delegation_targets(&file.contents) {
        if target != file.name
            && facts.existing.contains(&target)
            && !read_for_modes.contains(&target)
        {
            read_for_modes.push(target);
        }
    }
    Some(Chosen {
        name: file.name.clone(),
        command: run_command(file, facts.windows),
        read_for_modes,
    })
}

/// Local paths a script's text may hand off to: path-like tokens
/// (`ci/test.py`, `./ci/x.sh`) and `python -m pkg.mod` modules
/// (`ci/test.py`, `ci/test/__main__.py`). Callers filter by existence.
pub fn delegation_targets(contents: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let mut push = |s: String| {
        if !out.contains(&s) {
            out.push(s);
        }
    };
    for line in contents.lines() {
        let line = line.trim();
        if line.starts_with('#') && !line.starts_with("#!") {
            continue;
        }
        let tokens: Vec<&str> = line
            .split(|c: char| c.is_whitespace() || matches!(c, '"' | '\'' | '`' | ';' | '(' | ')'))
            .filter(|t| !t.is_empty())
            .collect();
        for (i, raw) in tokens.iter().enumerate() {
            if *raw == "-m" {
                if let Some(module) = tokens.get(i + 1) {
                    if module
                        .chars()
                        .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '.')
                        && !module.is_empty()
                    {
                        let base = module.replace('.', "/");
                        push(format!("{base}.py"));
                        push(format!("{base}/__main__.py"));
                    }
                }
                continue;
            }
            if let Some(path) = normalize_path(raw) {
                push(path);
            }
        }
    }
    out
}

fn normalize_path(raw: &str) -> Option<String> {
    let t = raw.trim_end_matches([',', '&', '|']);
    let t = t.replace('\\', "/");
    let t = t.strip_prefix("./").unwrap_or(&t).to_string();
    if t.is_empty()
        || t.starts_with('/')
        || t.starts_with('-')
        || t.starts_with('$')
        || t.starts_with('#')
        || t.contains("..")
        || t.contains("://")
        || t.contains('=')
        || t.contains(':')
    {
        return None;
    }
    let has_ext = [".py", ".sh", ".bash", ".bat", ".ps1", ".js", ".ts", ".mjs"]
        .iter()
        .any(|e| t.ends_with(e));
    (t.contains('/') || has_ext).then_some(t)
}

/// The rendered block for the `/grind` router.
pub fn render(facts: &ScriptFacts) -> String {
    let mut out = String::from("## Lint and test scripts (rendered by `clud grind-scripts`)\n\n");
    let found: Vec<(&str, Chosen)> = KINDS
        .iter()
        .filter_map(|k| choose(facts, k).map(|c| (*k, c)))
        .collect();
    if found.is_empty() {
        out.push_str(
            "No ./lint or ./test detected at the repo root. Skip the scripts question; the planner's verify commands stand.\n",
        );
        return out;
    }
    for (kind, chosen) in &found {
        out.push_str(&format!("- {kind}: `{}`\n", chosen.command));
    }
    let mut reads: Vec<String> = Vec::new();
    for (_, chosen) in &found {
        for p in &chosen.read_for_modes {
            if !reads.contains(p) {
                reads.push(p.clone());
            }
        }
    }
    out.push_str(&format!(
        "- read for modes: {}\n\n",
        reads
            .iter()
            .map(|p| format!("`{p}`"))
            .collect::<Vec<_>>()
            .join(", ")
    ));
    out.push_str(
        "Read each listed file to find the scripts' user-facing modes: flags the script accepts that change what runs, such as `--integration` or `--full`. Ignore flags a script passes to its own tools (`uv`, `cargo`, `pip`, `pytest` options). Offer at most four modes, each with a short description from the script's help text or comments. Never execute the scripts to discover modes.\n\n",
    );
    let names: Vec<String> = found
        .iter()
        .map(|(_, c)| format!("`./{}`", c.name))
        .collect();
    let question = format!("Found {}. Run them before each push?", names.join(" and "));
    let run_it = if found.len() == 1 {
        "Run it"
    } else {
        "Run them"
    };
    let question = question.replace("Run them", run_it);
    out.push_str(&format!(
        "Ask the user exactly once per run, as one combined question:\n\n> {question}\n"
    ));
    if found.len() == 2 {
        out.push_str(
            "> - Lint and test\n> - Lint and test `<mode>` (one option per mode found, with its description)\n> - Lint only / Test only\n",
        );
    } else {
        let (kind, _) = &found[0];
        let cap = if *kind == "lint" { "Lint" } else { "Test" };
        out.push_str(&format!(
            "> - {cap}\n> - {cap} `<mode>` (one option per mode found, with its description)\n"
        ));
    }
    out.push_str("> - Neither (use the planner's verify commands)\n");
    out
}

fn repo_root(cwd: &Path) -> PathBuf {
    cwd.ancestors()
        .find(|d| d.join(".git").exists())
        .map(Path::to_path_buf)
        .unwrap_or_else(|| cwd.to_path_buf())
}

#[cfg(unix)]
fn is_executable(meta: &std::fs::Metadata) -> bool {
    use std::os::unix::fs::PermissionsExt;
    meta.permissions().mode() & 0o111 != 0
}

#[cfg(not(unix))]
fn is_executable(_meta: &std::fs::Metadata) -> bool {
    false
}

/// Gather [`ScriptFacts`] for the repo at `root`. Reads files; runs nothing.
pub fn gather(root: &Path, windows: bool) -> ScriptFacts {
    let mut files = Vec::new();
    for kind in KINDS {
        for name in candidates(kind, true) {
            let path = root.join(&name);
            let Ok(meta) = std::fs::metadata(&path) else {
                continue;
            };
            if !meta.is_file() {
                continue;
            }
            let contents = std::fs::read_to_string(&path).unwrap_or_default();
            files.push(ScriptFile {
                name,
                executable: is_executable(&meta),
                contents,
            });
        }
    }
    let mut existing = BTreeSet::new();
    for file in &files {
        for target in delegation_targets(&file.contents) {
            if root.join(&target).is_file() {
                existing.insert(target);
            }
        }
    }
    ScriptFacts {
        files,
        windows,
        existing,
    }
}

/// `clud grind-scripts`: print the rendered block; always exit 0 so the
/// harness shows the model the text.
pub fn run() -> i32 {
    let cwd = std::env::current_dir().unwrap_or_else(|_| ".".into());
    let root = repo_root(&cwd);
    print!("{}", render(&gather(&root, cfg!(windows))));
    0
}

#[cfg(test)]
mod tests {
    use super::*;

    fn file(name: &str, executable: bool, contents: &str) -> ScriptFile {
        ScriptFile {
            name: name.into(),
            executable,
            contents: contents.into(),
        }
    }

    fn facts(files: Vec<ScriptFile>, windows: bool) -> ScriptFacts {
        ScriptFacts {
            files,
            windows,
            existing: BTreeSet::new(),
        }
    }

    #[test]
    fn executable_pair_runs_directly_l1() {
        let f = facts(
            vec![
                file("lint", true, "#!/bin/bash\n"),
                file("test", true, "#!/bin/bash\n"),
            ],
            false,
        );
        let out = render(&f);
        assert!(out.contains("- lint: `./lint`"), "{out}");
        assert!(out.contains("- test: `./test`"), "{out}");
        assert!(out.contains("Found `./lint` and `./test`. Run them before each push?"));
        assert!(out.contains("Never execute the scripts"));
    }

    #[test]
    fn non_executable_runs_through_bash_l2() {
        let f = facts(
            vec![file("lint", false, ""), file("test", false, "")],
            false,
        );
        let out = render(&f);
        assert!(out.contains("- lint: `bash ./lint`"), "{out}");
        assert!(out.contains("- test: `bash ./test`"), "{out}");
    }

    #[test]
    fn delegation_target_is_listed_l3() {
        let mut f = facts(
            vec![file(
                "test",
                false,
                "#!/bin/bash\nset -e\nuv run --frozen python ci/test.py \"$@\"\n",
            )],
            false,
        );
        f.existing.insert("ci/test.py".into());
        let out = render(&f);
        assert!(
            out.contains("- read for modes: `test`, `ci/test.py`"),
            "{out}"
        );
        assert!(
            out.contains("Found `./test`. Run it before each push?"),
            "{out}"
        );
    }

    #[test]
    fn python_dash_m_and_missing_targets() {
        let targets = delegation_targets("python -m ci.test \"$@\"\nexec ./ci/x.sh\n");
        assert!(targets.contains(&"ci/test.py".to_string()));
        assert!(targets.contains(&"ci/x.sh".to_string()));
        // Not in `existing`: not listed.
        let f = facts(vec![file("test", true, "exec ./ci/x.sh\n")], false);
        assert_eq!(choose(&f, "test").unwrap().read_for_modes, vec!["test"]);
    }

    #[test]
    fn only_bat_on_linux_detects_nothing_l4() {
        let f = facts(vec![file("test.bat", false, "@echo off\n")], false);
        let out = render(&f);
        assert!(out.contains("No ./lint or ./test detected"), "{out}");
        assert!(out.contains("Skip the scripts question"));
        assert!(!out.contains("Run them before each push"));
    }

    #[test]
    fn windows_prefers_bat_then_ps1() {
        let f = facts(
            vec![
                file("lint", true, ""),
                file("lint.ps1", false, ""),
                file("lint.bat", false, ""),
                file("test.ps1", false, ""),
                file("test", false, ""),
            ],
            true,
        );
        assert_eq!(choose(&f, "lint").unwrap().command, "cmd /c lint.bat");
        assert_eq!(choose(&f, "test").unwrap().command, "pwsh -File test.ps1");
    }

    #[test]
    fn windows_falls_back_to_bash() {
        let f = facts(
            vec![file("lint", true, ""), file("test.sh", false, "")],
            true,
        );
        assert_eq!(choose(&f, "lint").unwrap().command, "bash ./lint");
        assert_eq!(choose(&f, "test").unwrap().command, "bash ./test.sh");
    }

    #[test]
    fn sh_fallback_and_extensionless_preferred() {
        let f = facts(vec![file("test.sh", true, "")], false);
        assert_eq!(choose(&f, "test").unwrap().command, "./test.sh");
        let f = facts(
            vec![file("test.sh", true, ""), file("test", false, "")],
            false,
        );
        assert_eq!(choose(&f, "test").unwrap().command, "bash ./test");
    }

    #[test]
    fn never_ps1_on_non_windows() {
        let f = facts(vec![file("lint.ps1", false, "")], false);
        assert!(choose(&f, "lint").is_none());
    }
}
