//! Bundle slash-command "skills" inside the `clud` binary and install them
//! into every supported backend's global skills directory on launch.
//!
//! Backends are listed in [`SKILL_BACKENDS`] — adding support for a new CLI
//! (OpenRouter, OpenCode, etc.) is one line: append a new [`SkillBackend`].
//! Each backend declares the home subdir it lives under (`.claude`,
//! `.codex`, …) and the path under that where its skills live.
//!
//! We install only into a backend whose home subdir already exists — that
//! way users who only run one CLI don't get the other CLIs' directories
//! created in their home. Existing skill files are never overwritten, so
//! user edits survive.
//!
//! The asset files live under `crates/clud-bin/assets/skills/` and are
//! embedded at compile time via `include_str!`, so the runtime needs no
//! filesystem access to read the source content.
//!
//! Errors are non-fatal: `main()` calls [`ensure_installed`], logs any
//! failure to stderr, and proceeds with launch.

use std::io;
use std::path::{Path, PathBuf};

/// One bundled skill: the directory name and the literal `SKILL.md` body.
pub struct BundledSkill {
    pub name: &'static str,
    pub skill_md: &'static str,
}

/// All skills the binary ships with. Add new entries here when you bundle
/// another `assets/skills/<name>/SKILL.md`.
pub const BUNDLED_SKILLS: &[BundledSkill] = &[
    BundledSkill {
        name: "clud-issue",
        skill_md: include_str!("../assets/skills/clud-issue/SKILL.md"),
    },
    BundledSkill {
        name: "clud-issue-triage",
        skill_md: include_str!("../assets/skills/clud-issue-triage/SKILL.md"),
    },
    BundledSkill {
        name: "clud-pr",
        skill_md: include_str!("../assets/skills/clud-pr/SKILL.md"),
    },
    BundledSkill {
        name: "clud-tag-release",
        skill_md: include_str!("../assets/skills/clud-tag-release/SKILL.md"),
    },
    BundledSkill {
        name: "clud-docker-rust-app-dev",
        skill_md: include_str!("../assets/skills/clud-docker-rust-app-dev/SKILL.md"),
    },
    BundledSkill {
        name: "clud-windows-trash",
        skill_md: include_str!("../assets/skills/clud-windows-trash/SKILL.md"),
    },
    BundledSkill {
        name: "clud-extern-repos",
        skill_md: include_str!("../assets/skills/clud-extern-repos/SKILL.md"),
    },
];

/// One CLI backend that consumes `SKILL.md` files. Adding support for a
/// new tool is a one-line append to [`SKILL_BACKENDS`] — the on-disk layout
/// is the same as Claude Code's, just rooted under the tool's home subdir.
pub struct SkillBackend {
    /// Display name for log messages.
    pub name: &'static str,
    /// Path under the user's home dir where this backend stores config
    /// (e.g. `.claude`, `.codex`).
    pub home_subdir: &'static str,
    /// Path under `home_subdir` where skill packages live (almost always
    /// `skills`, parameterized in case a future tool uses a different
    /// name).
    pub skills_subdir: &'static str,
}

impl SkillBackend {
    /// Resolved skills dir for this backend, given a home dir.
    pub fn skills_dir(&self, home: &Path) -> PathBuf {
        home.join(self.home_subdir).join(self.skills_subdir)
    }

    /// True when this backend's home subdir exists as a directory under
    /// `home` — used to gate installs so we don't auto-create a backend
    /// root the user hasn't installed.
    pub fn root_exists(&self, home: &Path) -> bool {
        home.join(self.home_subdir).is_dir()
    }
}

/// Backends we install bundled skills into. To support a new CLI: confirm
/// it loads `SKILL.md`-format playbooks from a per-tool skills dir, then
/// append a `SkillBackend { ... }` entry here.
pub const SKILL_BACKENDS: &[SkillBackend] = &[
    SkillBackend {
        name: "Claude Code",
        home_subdir: ".claude",
        skills_subdir: "skills",
    },
    SkillBackend {
        name: "Codex",
        home_subdir: ".codex",
        skills_subdir: "skills",
    },
    // To add a new backend (e.g. OpenRouter CLI):
    // SkillBackend {
    //     name: "OpenRouter",
    //     home_subdir: ".openrouter",
    //     skills_subdir: "skills",
    // },
];

#[derive(Debug)]
pub enum InstallError {
    NoHomeDir,
    Io(io::Error),
}

impl std::fmt::Display for InstallError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            InstallError::NoHomeDir => write!(f, "could not resolve user home directory"),
            InstallError::Io(e) => write!(f, "io error: {e}"),
        }
    }
}

impl std::error::Error for InstallError {}

impl From<io::Error> for InstallError {
    fn from(e: io::Error) -> Self {
        InstallError::Io(e)
    }
}

/// Result of an install pass into a single backend's skills dir.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct InstallReport {
    pub installed: Vec<&'static str>,
    pub skipped_existing: Vec<&'static str>,
}

/// Install bundled skills into every backend whose home subdir exists.
/// Returns one `(backend, report)` per backend actually written to.
/// Returns [`InstallError::NoHomeDir`] only when the home dir itself
/// cannot be resolved.
pub fn ensure_installed() -> Result<Vec<(&'static SkillBackend, InstallReport)>, InstallError> {
    let home = home_dir().ok_or(InstallError::NoHomeDir)?;
    let mut results = Vec::new();
    for backend in active_backends(&home) {
        let report = install_to(&backend.skills_dir(&home), BUNDLED_SKILLS)?;
        results.push((backend, report));
    }
    Ok(results)
}

/// All `SkillBackend`s whose home subdir currently exists under `home` —
/// i.e. the backends the user has installed.
pub fn active_backends(home: &Path) -> Vec<&'static SkillBackend> {
    SKILL_BACKENDS
        .iter()
        .filter(|b| b.root_exists(home))
        .collect()
}

/// Install the given skills into `base/<name>/SKILL.md`. Writes only when
/// the target file is missing.
pub fn install_to(base: &Path, skills: &[BundledSkill]) -> Result<InstallReport, InstallError> {
    let mut report = InstallReport::default();
    for skill in skills {
        let skill_dir = base.join(skill.name);
        let skill_md = skill_dir.join("SKILL.md");
        if skill_md.exists() {
            report.skipped_existing.push(skill.name);
            continue;
        }
        std::fs::create_dir_all(&skill_dir)?;
        std::fs::write(&skill_md, skill.skill_md)?;
        report.installed.push(skill.name);
    }
    Ok(report)
}

fn home_dir() -> Option<PathBuf> {
    #[cfg(windows)]
    {
        if let Some(p) = std::env::var_os("USERPROFILE") {
            if !p.is_empty() {
                return Some(PathBuf::from(p));
            }
        }
    }
    if let Some(p) = std::env::var_os("HOME") {
        if !p.is_empty() {
            return Some(PathBuf::from(p));
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde::Deserialize;
    use tempfile::tempdir;

    #[derive(Debug, Deserialize)]
    struct SkillFrontmatter {
        name: String,
        description: String,
        #[serde(default)]
        triggers: Vec<String>,
    }

    fn frontmatter_yaml<'a>(skill_name: &str, skill_md: &'a str) -> &'a str {
        let Some(after_open) = skill_md
            .strip_prefix("---\r\n")
            .or_else(|| skill_md.strip_prefix("---\n"))
        else {
            panic!("skill {skill_name} must start with YAML frontmatter");
        };
        let Some(end) = after_open.find("\n---") else {
            panic!("skill {skill_name} missing closing YAML frontmatter marker");
        };
        &after_open[..end]
    }

    fn parse_frontmatter(skill: &BundledSkill) -> SkillFrontmatter {
        serde_yaml::from_str(frontmatter_yaml(skill.name, skill.skill_md)).unwrap_or_else(|err| {
            panic!("skill {} has invalid YAML frontmatter: {err}", skill.name)
        })
    }

    fn fake_skills() -> Vec<BundledSkill> {
        vec![
            BundledSkill {
                name: "alpha",
                skill_md: "alpha body\n",
            },
            BundledSkill {
                name: "beta",
                skill_md: "beta body\n",
            },
        ]
    }

    #[test]
    fn installs_when_missing() {
        let dir = tempdir().unwrap();
        let report = install_to(dir.path(), &fake_skills()).unwrap();
        assert_eq!(report.installed, vec!["alpha", "beta"]);
        assert!(report.skipped_existing.is_empty());
        assert_eq!(
            std::fs::read_to_string(dir.path().join("alpha/SKILL.md")).unwrap(),
            "alpha body\n"
        );
        assert_eq!(
            std::fs::read_to_string(dir.path().join("beta/SKILL.md")).unwrap(),
            "beta body\n"
        );
    }

    #[test]
    fn skips_existing_and_preserves_user_edits() {
        let dir = tempdir().unwrap();
        let alpha_dir = dir.path().join("alpha");
        std::fs::create_dir_all(&alpha_dir).unwrap();
        std::fs::write(alpha_dir.join("SKILL.md"), "USER EDIT").unwrap();

        let report = install_to(dir.path(), &fake_skills()).unwrap();
        assert_eq!(report.installed, vec!["beta"]);
        assert_eq!(report.skipped_existing, vec!["alpha"]);
        assert_eq!(
            std::fs::read_to_string(alpha_dir.join("SKILL.md")).unwrap(),
            "USER EDIT",
            "existing user content must not be overwritten"
        );
    }

    #[test]
    fn idempotent_second_pass_is_a_noop() {
        let dir = tempdir().unwrap();
        let first = install_to(dir.path(), &fake_skills()).unwrap();
        assert_eq!(first.installed.len(), 2);
        let second = install_to(dir.path(), &fake_skills()).unwrap();
        assert!(second.installed.is_empty());
        assert_eq!(second.skipped_existing, vec!["alpha", "beta"]);
    }

    #[test]
    fn creates_missing_parent_dirs() {
        let dir = tempdir().unwrap();
        let nested = dir.path().join("a/b/c");
        let report = install_to(&nested, &fake_skills()).unwrap();
        assert_eq!(report.installed, vec!["alpha", "beta"]);
        assert!(nested.join("alpha/SKILL.md").exists());
    }

    /// The bundled assets must be non-empty — `include_str!` would fail at
    /// build time on a missing file, but a 0-byte file would silently ship.
    #[test]
    fn bundled_skills_are_non_empty() {
        assert!(!BUNDLED_SKILLS.is_empty());
        for s in BUNDLED_SKILLS {
            assert!(!s.skill_md.trim().is_empty(), "skill {} is empty", s.name);
            assert!(
                s.skill_md.contains("managed-by: clud"),
                "skill {} missing managed-by marker",
                s.name
            );
        }
    }

    #[test]
    fn bundled_skill_frontmatter_is_valid_yaml() {
        assert!(!BUNDLED_SKILLS.is_empty());
        for skill in BUNDLED_SKILLS {
            let frontmatter = parse_frontmatter(skill);
            assert_eq!(
                frontmatter.name, skill.name,
                "skill {} frontmatter name must match BUNDLED_SKILLS entry",
                skill.name
            );
            assert!(
                !frontmatter.description.trim().is_empty(),
                "skill {} missing frontmatter description",
                skill.name
            );
            assert!(
                !frontmatter.triggers.is_empty(),
                "skill {} missing frontmatter triggers",
                skill.name
            );
        }
    }

    #[test]
    fn bundled_includes_all_known_skills() {
        let names: Vec<&str> = BUNDLED_SKILLS.iter().map(|s| s.name).collect();
        assert!(names.contains(&"clud-issue"));
        assert!(names.contains(&"clud-issue-triage"));
        assert!(names.contains(&"clud-pr"));
        assert!(names.contains(&"clud-tag-release"));
        assert!(names.contains(&"clud-docker-rust-app-dev"));
        assert!(names.contains(&"clud-windows-trash"));
        assert!(names.contains(&"clud-extern-repos"));
    }

    #[test]
    fn skill_backends_include_claude_and_codex() {
        let names: Vec<&str> = SKILL_BACKENDS.iter().map(|b| b.home_subdir).collect();
        assert!(names.contains(&".claude"));
        assert!(names.contains(&".codex"));
    }

    #[test]
    fn active_backends_returns_all_when_all_roots_exist() {
        let home = tempdir().unwrap();
        for b in SKILL_BACKENDS {
            std::fs::create_dir_all(home.path().join(b.home_subdir)).unwrap();
        }
        let active = active_backends(home.path());
        assert_eq!(active.len(), SKILL_BACKENDS.len());
    }

    #[test]
    fn active_backends_filters_to_existing_roots() {
        let home = tempdir().unwrap();
        // Only the first backend is installed.
        std::fs::create_dir_all(home.path().join(SKILL_BACKENDS[0].home_subdir)).unwrap();
        let active = active_backends(home.path());
        assert_eq!(active.len(), 1);
        assert_eq!(active[0].home_subdir, SKILL_BACKENDS[0].home_subdir);
    }

    #[test]
    fn active_backends_empty_when_nothing_installed() {
        let home = tempdir().unwrap();
        assert!(active_backends(home.path()).is_empty());
    }

    /// A file (not a directory) at a backend's home path must not register
    /// as installed — `is_dir()` filters it out.
    #[test]
    fn active_backends_ignores_non_directory_at_root() {
        let home = tempdir().unwrap();
        std::fs::write(
            home.path().join(SKILL_BACKENDS[0].home_subdir),
            b"not a dir",
        )
        .unwrap();
        assert!(active_backends(home.path()).is_empty());
    }

    #[test]
    fn skills_dir_resolves_under_backend_root() {
        let home = tempdir().unwrap();
        let backend = SkillBackend {
            name: "Test",
            home_subdir: ".testtool",
            skills_subdir: "skills",
        };
        assert_eq!(
            backend.skills_dir(home.path()),
            home.path().join(".testtool").join("skills")
        );
    }
}
