//! Bundled Claude Code agent definitions and workflow scripts.
//!
//! The `/grind` DAG needs more than skills: capped agent types under
//! `~/.claude/agents/` and the `grind-run` workflow under `~/.claude/workflows/`.
//! Both are Claude-only file kinds, so this installer targets `~/.claude`
//! alone and only when that directory already exists.
//!
//! Ownership follows the skill installer (`skills::install_to`): a missing
//! file is written, a file whose `managed-by: clud` marker was stripped is the
//! user's and is never touched, a stale managed copy is refreshed, and a
//! current one is not rewritten.

use std::io;
use std::path::Path;

const MANAGED_BY_CLUD_MARKER: &str = "managed-by: clud";

/// One bundled file: its path relative to `~/.claude`, and its body.
pub struct BundledClaudeFile {
    pub rel_path: &'static str,
    pub body: &'static str,
}

/// Every bundled agent definition and workflow script.
pub const BUNDLED_CLAUDE_FILES: &[BundledClaudeFile] = &[
    BundledClaudeFile {
        rel_path: "agents/grind-planner.md",
        body: include_str!("../assets/agents/grind-planner.md"),
    },
    BundledClaudeFile {
        rel_path: "agents/grind-worker.md",
        body: include_str!("../assets/agents/grind-worker.md"),
    },
    BundledClaudeFile {
        rel_path: "agents/grind-reviewer.md",
        body: include_str!("../assets/agents/grind-reviewer.md"),
    },
    BundledClaudeFile {
        rel_path: "agents/grind-integrator.md",
        body: include_str!("../assets/agents/grind-integrator.md"),
    },
    BundledClaudeFile {
        rel_path: "agents/grind-lander.md",
        body: include_str!("../assets/agents/grind-lander.md"),
    },
    BundledClaudeFile {
        rel_path: "workflows/grind-run.js",
        body: include_str!("../assets/workflows/grind-run.js"),
    },
];

/// Files clud used to install and has since retired or renamed. Each is
/// deleted only while it still carries the `managed-by: clud` marker, so a
/// user's own file at the same path is never touched.
///
/// `workflows/grind.js` was renamed to `grind-run.js`: a workflow named
/// `grind` listed a second `/grind` beside the router skill, and picking it
/// skipped the router's questions.
pub const PURGED_CLAUDE_FILES: &[&str] = &["workflows/grind.js"];

/// Result of one install pass.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct InstallReport {
    pub installed: Vec<&'static str>,
    pub skipped_existing: Vec<&'static str>,
    pub refreshed: Vec<&'static str>,
    pub purged: Vec<&'static str>,
}

/// Install [`BUNDLED_CLAUDE_FILES`] under `home/.claude`, or do nothing when
/// Claude Code is not installed there.
pub fn ensure_installed_at(home: &Path) -> io::Result<Option<InstallReport>> {
    let root = home.join(".claude");
    if !root.is_dir() {
        return Ok(None);
    }
    let mut report = install_to(&root, BUNDLED_CLAUDE_FILES)?;
    report.purged = purge_retired(&root, PURGED_CLAUDE_FILES);
    Ok(Some(report))
}

/// Delete each retired file that is still clud-managed; report what went.
pub fn purge_retired(root: &Path, retired: &[&'static str]) -> Vec<&'static str> {
    retired
        .iter()
        .copied()
        .filter(|rel| {
            let path = root.join(rel);
            std::fs::read_to_string(&path).is_ok_and(|body| body.contains(MANAGED_BY_CLUD_MARKER))
                && std::fs::remove_file(&path).is_ok()
        })
        .collect()
}

pub fn install_to(root: &Path, files: &[BundledClaudeFile]) -> io::Result<InstallReport> {
    let mut report = InstallReport::default();
    for file in files {
        let path = root.join(file.rel_path);
        match std::fs::read_to_string(&path) {
            // Only a missing file is ours to write. Any other read failure
            // (permissions, non-UTF-8 content) may be a user's file.
            Err(error) if error.kind() != io::ErrorKind::NotFound => return Err(error),
            Err(_) => {
                if let Some(parent) = path.parent() {
                    std::fs::create_dir_all(parent)?;
                }
                std::fs::write(&path, file.body)?;
                report.installed.push(file.rel_path);
            }
            Ok(existing) if !existing.contains(MANAGED_BY_CLUD_MARKER) => {
                report.skipped_existing.push(file.rel_path);
            }
            Ok(existing) if normalize(&existing) == normalize(file.body) => {
                report.skipped_existing.push(file.rel_path);
            }
            Ok(_) => {
                std::fs::write(&path, file.body)?;
                report.refreshed.push(file.rel_path);
            }
        }
    }
    Ok(report)
}

fn normalize(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_bundled_file_carries_the_marker() {
        for file in BUNDLED_CLAUDE_FILES {
            assert!(
                file.body.contains(MANAGED_BY_CLUD_MARKER),
                "{} must carry the managed-by marker",
                file.rel_path
            );
        }
    }

    #[test]
    fn bundled_includes_the_grind_roles_and_workflow() {
        let paths: Vec<_> = BUNDLED_CLAUDE_FILES.iter().map(|f| f.rel_path).collect();
        for role in ["planner", "worker", "reviewer", "integrator", "lander"] {
            let path = format!("agents/grind-{role}.md");
            assert!(paths.contains(&path.as_str()), "missing {path}");
        }
        assert!(paths.contains(&"workflows/grind-run.js"));
        assert!(!paths.contains(&"workflows/grind.js"));
    }

    /// The agent `name:` must match its file, or the workflow's
    /// `agentType: 'grind-<role>'` resolves to nothing.
    #[test]
    fn agent_names_match_their_files() {
        for file in BUNDLED_CLAUDE_FILES {
            let Some(stem) = file
                .rel_path
                .strip_prefix("agents/")
                .and_then(|p| p.strip_suffix(".md"))
            else {
                continue;
            };
            assert!(
                file.body.contains(&format!("\nname: {stem}\n")),
                "{} frontmatter name must be {stem}",
                file.rel_path
            );
        }
    }

    /// Every capped role declares its tools, and reviewers get exactly the
    /// worker's.
    #[test]
    fn capped_roles_declare_tool_lists() {
        let body = |role: &str| {
            BUNDLED_CLAUDE_FILES
                .iter()
                .find(|f| f.rel_path == format!("agents/grind-{role}.md"))
                .unwrap()
                .body
        };
        for role in ["planner", "worker", "reviewer", "integrator", "lander"] {
            assert!(
                body(role).contains("\ntools: "),
                "grind-{role} needs tools:"
            );
        }
        let tools = |role: &str| {
            body(role)
                .lines()
                .find(|l| l.starts_with("tools: "))
                .unwrap()
                .to_string()
        };
        assert_eq!(tools("reviewer"), tools("worker"));
        assert!(!body("lander").contains("Edit"));
        assert!(!body("planner").contains("Edit"));
    }

    #[test]
    fn install_is_idempotent_and_respects_user_ownership() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        let first = install_to(root, BUNDLED_CLAUDE_FILES).unwrap();
        assert_eq!(first.installed.len(), BUNDLED_CLAUDE_FILES.len());
        let second = install_to(root, BUNDLED_CLAUDE_FILES).unwrap();
        assert!(second.installed.is_empty() && second.refreshed.is_empty());

        let owned = root.join("workflows/grind-run.js");
        std::fs::write(&owned, "// my own grind").unwrap();
        let third = install_to(root, BUNDLED_CLAUDE_FILES).unwrap();
        assert!(third.skipped_existing.contains(&"workflows/grind-run.js"));
        assert_eq!(std::fs::read_to_string(&owned).unwrap(), "// my own grind");
    }

    /// No workflow may share a skill's name, or Claude Code lists two
    /// commands under one `/name`.
    #[test]
    fn workflow_names_never_collide_with_skills() {
        for file in BUNDLED_CLAUDE_FILES {
            let Some(stem) = file
                .rel_path
                .strip_prefix("workflows/")
                .and_then(|p| p.strip_suffix(".js"))
            else {
                continue;
            };
            assert!(
                file.body.contains(&format!("name: '{stem}'")),
                "{} meta name must be {stem}",
                file.rel_path
            );
            assert!(
                !crate::skills::BUNDLED_SKILLS.iter().any(|s| s.name == stem),
                "workflow {stem} collides with the bundled skill of the same name"
            );
        }
    }

    #[test]
    fn retired_grind_workflow_is_purged_but_user_copies_survive() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join(".claude");
        std::fs::create_dir_all(root.join("workflows")).unwrap();
        let old = root.join("workflows/grind.js");

        std::fs::write(&old, "// managed-by: clud\nold").unwrap();
        let report = ensure_installed_at(dir.path()).unwrap().unwrap();
        assert_eq!(report.purged, vec!["workflows/grind.js"]);
        assert!(!old.exists());
        assert!(root.join("workflows/grind-run.js").is_file());

        std::fs::write(&old, "// my own grind").unwrap();
        let report = ensure_installed_at(dir.path()).unwrap().unwrap();
        assert!(report.purged.is_empty());
        assert!(old.exists());
    }

    #[test]
    fn missing_claude_home_installs_nothing() {
        let dir = tempfile::tempdir().unwrap();
        assert!(ensure_installed_at(dir.path()).unwrap().is_none());
    }
}
