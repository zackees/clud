//! Bundle slash-command "skills" inside the `clud` binary and install them
//! into every supported backend's global skills directory on launch.
//!
//! Backends are listed in [`SKILL_BACKENDS`] — adding support for a new CLI
//! (OpenRouter, OpenCode, etc.) is one line: append a new [`SkillBackend`].
//! Each backend declares the home subdir used to detect that it is installed
//! (`.claude`, `.codex`, …) and the path under the user's home where its
//! skills live.
//!
//! We install only into a backend whose home subdir already exists — that way
//! users who only run one CLI don't get the other CLIs' skill directories
//! created in their home. Existing skill files are never overwritten, so user
//! edits survive. Codex reads skills from `~/.codex/skills/`, mirroring
//! Claude's `~/.claude/skills/` layout. Clud-managed copies that an older
//! build wrote to `~/.agents/skills/` are purged best-effort during Codex
//! global setup.
//!
//! The asset files live under `crates/clud-bin/assets/skills/` and are
//! embedded at compile time via `include_str!`, so the runtime needs no
//! filesystem access to read the source content.
//!
//! Errors are non-fatal: `main()` calls [`ensure_installed`], logs any
//! failure to stderr, and proceeds with launch.

use std::io;
use std::path::{Path, PathBuf};

use crate::backend::Backend;

const MANAGED_BY_CLUD_MARKER: &str = "managed-by: clud";

/// One bundled skill: the directory name and the literal `SKILL.md` body.
pub struct BundledSkill {
    pub name: &'static str,
    pub skill_md: &'static str,
}

/// All skills the binary ships with. Add new entries here when you bundle
/// another `assets/skills/<name>/SKILL.md`.
pub const BUNDLED_SKILLS: &[BundledSkill] = &[
    BundledSkill {
        name: "clud-loop",
        skill_md: include_str!("../assets/skills/clud-loop/SKILL.md"),
    },
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
        name: "clud-fix",
        skill_md: include_str!("../assets/skills/clud-fix/SKILL.md"),
    },
    BundledSkill {
        name: "clud-fix-quick",
        skill_md: include_str!("../assets/skills/clud-fix-quick/SKILL.md"),
    },
    BundledSkill {
        name: "clud-do",
        skill_md: include_str!("../assets/skills/clud-do/SKILL.md"),
    },
    BundledSkill {
        name: "clud-review",
        skill_md: include_str!("../assets/skills/clud-review/SKILL.md"),
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
    BundledSkill {
        name: "clud-improve",
        skill_md: include_str!("../assets/skills/clud-improve/SKILL.md"),
    },
    BundledSkill {
        name: "clud-docker-mac-x86",
        skill_md: include_str!("../assets/skills/clud-docker-mac-x86/SKILL.md"),
    },
];

/// One CLI backend that consumes `SKILL.md` files. Adding support for a
/// new tool is a one-line append to [`SKILL_BACKENDS`] — the on-disk layout
/// is the same as Claude Code's, just rooted under the tool's home subdir.
pub struct SkillBackend {
    /// Backend this install target belongs to.
    pub backend: Backend,
    /// Display name for log messages.
    pub name: &'static str,
    /// Path under the user's home dir where this backend stores config
    /// (e.g. `.claude`, `.codex`).
    pub home_subdir: &'static str,
    /// Optional override for the home-relative directory that contains
    /// this backend's skills, when it differs from `home_subdir`. Reserved
    /// for future backends whose skills live outside their config root.
    pub skills_home_subdir: Option<&'static str>,
    /// Path under the skills home dir where skill packages live (almost
    /// always `skills`, parameterized in case a future tool uses a different
    /// name).
    pub skills_subdir: &'static str,
}

impl SkillBackend {
    /// Resolved skills dir for this backend, given a home dir.
    pub fn skills_dir(&self, home: &Path) -> PathBuf {
        home.join(self.skills_home_subdir.unwrap_or(self.home_subdir))
            .join(self.skills_subdir)
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
        backend: Backend::Claude,
        name: "Claude Code",
        home_subdir: ".claude",
        skills_home_subdir: None,
        skills_subdir: "skills",
    },
    SkillBackend {
        backend: Backend::Codex,
        name: "Codex",
        home_subdir: ".codex",
        skills_home_subdir: None,
        skills_subdir: "skills",
    },
    // To add a new backend (e.g. OpenRouter CLI):
    // SkillBackend {
    //     backend: Backend::OpenRouter,
    //     name: "OpenRouter",
    //     home_subdir: ".openrouter",
    //     skills_home_subdir: None,
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

/// Result of the legacy Codex skill cleanup pass.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct LegacyPurgeReport {
    pub removed: Vec<&'static str>,
    pub preserved: Vec<&'static str>,
    pub failed: Vec<&'static str>,
}

/// Compatibility helper that installs bundled skills into every backend whose
/// home subdir exists. Production launch setup calls
/// [`ensure_installed_for_backend`] for the selected backend instead. Returns
/// one `(backend, report)` per backend actually written to. Returns
/// [`InstallError::NoHomeDir`] only when the home dir itself cannot be
/// resolved.
pub fn ensure_installed() -> Result<Vec<(&'static SkillBackend, InstallReport)>, InstallError> {
    let home = home_dir().ok_or(InstallError::NoHomeDir)?;
    ensure_installed_at(&home)
}

pub fn ensure_installed_at(
    home: &Path,
) -> Result<Vec<(&'static SkillBackend, InstallReport)>, InstallError> {
    let _ = purge_stale_agents_skills(home, BUNDLED_SKILLS);
    let mut results = Vec::new();
    for backend in active_backends(home) {
        let report = install_to(&backend.skills_dir(home), BUNDLED_SKILLS)?;
        results.push((backend, report));
    }
    Ok(results)
}

pub fn ensure_installed_for_backend(
    backend: Backend,
) -> Result<Option<(&'static SkillBackend, InstallReport)>, InstallError> {
    let home = home_dir().ok_or(InstallError::NoHomeDir)?;
    ensure_installed_for_backend_at(&home, backend)
}

pub fn ensure_installed_for_backend_at(
    home: &Path,
    backend: Backend,
) -> Result<Option<(&'static SkillBackend, InstallReport)>, InstallError> {
    if matches!(backend, Backend::Codex) {
        let _ = purge_stale_agents_skills(home, BUNDLED_SKILLS);
    }
    let Some(skill_backend) = backend_for(backend) else {
        return Ok(None);
    };
    if !skill_backend.root_exists(home) {
        return Ok(None);
    }
    let report = install_to(&skill_backend.skills_dir(home), BUNDLED_SKILLS)?;
    Ok(Some((skill_backend, report)))
}

pub fn backend_for(backend: Backend) -> Option<&'static SkillBackend> {
    SKILL_BACKENDS.iter().find(|b| b.backend == backend)
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

/// Remove stale clud-managed skill copies from `~/.agents/skills/`.
///
/// Clud used to install Codex skills under `~/.agents/skills/` based on an
/// expected cross-vendor skill location. Codex actually loads from
/// `~/.codex/skills/`, so the agents-dir copies were inert duplicates. The
/// cleanup is deliberately best effort and conservative: it only touches
/// directories named after currently bundled clud skills, and only removes
/// a `SKILL.md` that still carries the clud ownership marker. Any unrelated
/// files in the skill directory are left in place.
pub fn purge_stale_agents_skills(home: &Path, skills: &[BundledSkill]) -> LegacyPurgeReport {
    let stale_dir = home.join(".agents").join("skills");
    let mut report = LegacyPurgeReport::default();
    if !stale_dir.is_dir() {
        return report;
    }

    for skill in skills {
        let skill_dir = stale_dir.join(skill.name);
        let skill_md = skill_dir.join("SKILL.md");
        if !skill_md.is_file() {
            continue;
        }
        let body = match std::fs::read_to_string(&skill_md) {
            Ok(body) => body,
            Err(_) => {
                report.failed.push(skill.name);
                continue;
            }
        };
        if !body.contains(MANAGED_BY_CLUD_MARKER) {
            report.preserved.push(skill.name);
            continue;
        }
        match std::fs::remove_file(&skill_md) {
            Ok(()) => {
                report.removed.push(skill.name);
                let _ = std::fs::remove_dir(&skill_dir);
            }
            Err(_) => report.failed.push(skill.name),
        }
    }

    let _ = std::fs::remove_dir(&stale_dir);
    report
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
        assert!(names.contains(&"clud-loop"));
        assert!(names.contains(&"clud-issue"));
        assert!(names.contains(&"clud-issue-triage"));
        assert!(names.contains(&"clud-pr"));
        assert!(names.contains(&"clud-fix"));
        assert!(names.contains(&"clud-tag-release"));
        assert!(names.contains(&"clud-docker-rust-app-dev"));
        assert!(names.contains(&"clud-windows-trash"));
        assert!(names.contains(&"clud-extern-repos"));
        assert!(names.contains(&"clud-improve"));
        assert!(names.contains(&"clud-docker-mac-x86"));
    }

    #[test]
    fn bundled_skills_include_red_green_rule() {
        for skill in BUNDLED_SKILLS {
            assert!(
                skill.skill_md.contains("RED -> GREEN"),
                "skill {} must include the RED -> GREEN code-change rule",
                skill.name
            );
        }
    }

    #[test]
    fn clud_improve_files_concrete_reports_without_generic_prompt() {
        let skill = BUNDLED_SKILLS
            .iter()
            .find(|skill| skill.name == "clud-improve")
            .expect("clud-improve must be bundled")
            .skill_md;

        for required in [
            "Concrete report means file directly",
            "Bare manual invocation asks once",
            "If the skill was auto-selected and the current user message already contains a concrete clud report, use that message as the report.",
        ] {
            assert!(
                skill.contains(required),
                "clud-improve skill missing argument-aware filing guidance: {required}"
            );
        }
    }

    #[test]
    fn clud_pr_teardown_requires_process_audit() {
        let skill = BUNDLED_SKILLS
            .iter()
            .find(|skill| skill.name == "clud-pr")
            .expect("clud-pr must be bundled")
            .skill_md;

        for required in [
            "audit live processes before removing the worktree",
            "stop only that exact process tree before cleanup",
            "do not use a blind `rm -rf` retry loop",
        ] {
            assert!(
                skill.contains(required),
                "clud-pr skill missing process-audit teardown guidance: {required}"
            );
        }
        assert!(
            !skill.contains("Follow the **Tear down** retry pattern"),
            "clud-pr skill must not recommend blind retry teardown"
        );
    }

    #[test]
    fn clud_loop_skill_uses_codex_native_orchestration() {
        let skill = BUNDLED_SKILLS
            .iter()
            .find(|skill| skill.name == "clud-loop")
            .expect("clud-loop must be bundled")
            .skill_md;

        for required in [
            "Foreground In-Chat Orchestration",
            "main Codex agent is the single orchestrator",
            "status: DONE | PARTIAL | BLOCKED | FAILED | NOOP",
            "LOOP_DETECTED",
            "Legacy External Process Mode",
            "Do not run `clud --codex loop` for normal foreground in-chat work.",
        ] {
            assert!(
                skill.contains(required),
                "clud-loop skill missing required Codex-native loop guidance: {required}"
            );
        }
    }

    #[test]
    fn clud_fix_skill_owns_issue_goal_and_meta_burndown() {
        let skill = BUNDLED_SKILLS
            .iter()
            .find(|skill| skill.name == "clud-fix")
            .expect("clud-fix must be bundled")
            .skill_md;

        for required in [
            "/goal $clud-fix <issue-or-issue-url>",
            "Complete meta issue #N",
            "every child issue closed/validated",
            "parent checklist updated",
            "parent issue closed",
            ".clud/fix/<owner>__<repo>__issue-<num>.json",
            "Delegated `clud-pr` work must not invoke a nested `/goal`",
            "Claude And Codex Parity",
        ] {
            assert!(
                skill.contains(required),
                "clud-fix skill missing required orchestration guidance: {required}"
            );
        }

        assert!(
            !skill.contains("clud-pr-merge"),
            "clud-fix must not depend on the retired merge skill"
        );
    }

    #[test]
    fn clud_pr_skill_supports_delegated_mode_without_nested_goal() {
        let skill = BUNDLED_SKILLS
            .iter()
            .find(|skill| skill.name == "clud-pr")
            .expect("clud-pr must be bundled")
            .skill_md;

        for required in [
            "Delegated Mode",
            "Do not invoke `/goal`",
            "When called by [[clud-fix]], do not set or replace `/goal`",
            "Return structured evidence",
        ] {
            assert!(
                skill.contains(required),
                "clud-pr skill missing delegated-mode guidance: {required}"
            );
        }
    }

    #[test]
    fn skill_backends_include_claude_and_codex() {
        let backends: Vec<(Backend, &str, &str, Option<&str>)> = SKILL_BACKENDS
            .iter()
            .map(|b| (b.backend, b.name, b.home_subdir, b.skills_home_subdir))
            .collect();
        assert!(backends.contains(&(Backend::Claude, "Claude Code", ".claude", None)));
        assert!(backends.contains(&(Backend::Codex, "Codex", ".codex", None)));
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
            backend: Backend::Claude,
            name: "Test",
            home_subdir: ".testtool",
            skills_home_subdir: None,
            skills_subdir: "skills",
        };
        assert_eq!(
            backend.skills_dir(home.path()),
            home.path().join(".testtool").join("skills")
        );
    }

    #[test]
    fn codex_root_installs_to_codex_skills_dir() {
        let home = tempdir().unwrap();
        std::fs::create_dir_all(home.path().join(".codex")).unwrap();

        let codex = ensure_installed_for_backend_at(home.path(), Backend::Codex)
            .unwrap()
            .expect("codex backend should be active");

        assert_eq!(
            codex.0.skills_dir(home.path()),
            home.path().join(".codex/skills")
        );
        assert!(home.path().join(".codex/skills/clud-pr/SKILL.md").exists());
        assert!(!home.path().join(".agents/skills/clud-pr/SKILL.md").exists());
    }

    #[test]
    fn codex_install_writes_bundled_skill_bodies_byte_for_byte() {
        let home = tempdir().unwrap();
        std::fs::create_dir_all(home.path().join(".codex")).unwrap();

        ensure_installed_for_backend_at(home.path(), Backend::Codex)
            .unwrap()
            .expect("codex backend should be active");

        for skill_name in ["clud-pr", "clud-fix"] {
            let expected = BUNDLED_SKILLS
                .iter()
                .find(|s| s.name == skill_name)
                .unwrap_or_else(|| panic!("{skill_name} must be bundled"))
                .skill_md;
            let written = std::fs::read_to_string(
                home.path()
                    .join(".codex/skills")
                    .join(skill_name)
                    .join("SKILL.md"),
            )
            .unwrap();
            assert_eq!(written, expected);
        }
    }

    #[test]
    fn purges_managed_stale_agents_skill_copies() {
        let home = tempdir().unwrap();
        let stale_skill = home.path().join(".agents/skills/alpha");
        std::fs::create_dir_all(&stale_skill).unwrap();
        std::fs::write(stale_skill.join("SKILL.md"), "<!-- managed-by: clud -->\n").unwrap();

        let report = purge_stale_agents_skills(home.path(), &fake_skills());

        assert_eq!(report.removed, vec!["alpha"]);
        assert!(report.preserved.is_empty());
        assert!(report.failed.is_empty());
        assert!(!stale_skill.exists());
    }

    #[test]
    fn stale_agents_purge_preserves_unrelated_and_user_authored_content() {
        let home = tempdir().unwrap();
        let stale_root = home.path().join(".agents/skills");
        let custom_skill = stale_root.join("custom");
        let edited_bundled_skill = stale_root.join("alpha");
        let bundled_with_extra = stale_root.join("beta");
        std::fs::create_dir_all(&custom_skill).unwrap();
        std::fs::create_dir_all(&edited_bundled_skill).unwrap();
        std::fs::create_dir_all(&bundled_with_extra).unwrap();
        std::fs::write(
            custom_skill.join("SKILL.md"),
            "<!-- managed-by: clud -->\ncustom\n",
        )
        .unwrap();
        std::fs::write(edited_bundled_skill.join("SKILL.md"), "USER EDIT\n").unwrap();
        std::fs::write(
            bundled_with_extra.join("SKILL.md"),
            "<!-- managed-by: clud -->\n",
        )
        .unwrap();
        std::fs::write(bundled_with_extra.join("notes.txt"), "keep me\n").unwrap();

        let report = purge_stale_agents_skills(home.path(), &fake_skills());

        assert_eq!(report.removed, vec!["beta"]);
        assert_eq!(report.preserved, vec!["alpha"]);
        assert!(custom_skill.join("SKILL.md").exists());
        assert_eq!(
            std::fs::read_to_string(edited_bundled_skill.join("SKILL.md")).unwrap(),
            "USER EDIT\n"
        );
        assert!(!bundled_with_extra.join("SKILL.md").exists());
        assert!(bundled_with_extra.join("notes.txt").exists());
    }

    #[test]
    fn stale_agents_purge_is_idempotent() {
        let home = tempdir().unwrap();
        let stale_skill = home.path().join(".agents/skills/alpha");
        std::fs::create_dir_all(&stale_skill).unwrap();
        std::fs::write(stale_skill.join("SKILL.md"), "<!-- managed-by: clud -->\n").unwrap();

        let first = purge_stale_agents_skills(home.path(), &fake_skills());
        let second = purge_stale_agents_skills(home.path(), &fake_skills());

        assert_eq!(first.removed, vec!["alpha"]);
        assert!(second.removed.is_empty());
        assert!(second.preserved.is_empty());
        assert!(second.failed.is_empty());
    }

    /// Codex install must not touch a pre-existing `~/.codex/skills/<name>/SKILL.md`
    /// that the user has hand-edited.
    #[test]
    fn codex_install_preserves_user_edited_skill_at_new_path() {
        let home = tempdir().unwrap();
        let clud_pr_dir = home.path().join(".codex/skills/clud-pr");
        std::fs::create_dir_all(&clud_pr_dir).unwrap();
        std::fs::write(clud_pr_dir.join("SKILL.md"), "USER EDIT\n").unwrap();

        ensure_installed_for_backend_at(home.path(), Backend::Codex)
            .unwrap()
            .expect("codex backend should be active");

        assert_eq!(
            std::fs::read_to_string(clud_pr_dir.join("SKILL.md")).unwrap(),
            "USER EDIT\n",
            "existing user content under ~/.codex/skills/ must not be overwritten"
        );
    }

    /// Codex install must clean up stale clud-managed copies from `~/.agents/skills/`.
    #[test]
    fn codex_install_purges_stale_agents_skills() {
        let home = tempdir().unwrap();
        std::fs::create_dir_all(home.path().join(".codex")).unwrap();
        let stale = home.path().join(".agents/skills/clud-pr");
        std::fs::create_dir_all(&stale).unwrap();
        std::fs::write(stale.join("SKILL.md"), "<!-- managed-by: clud -->\nstale\n").unwrap();

        ensure_installed_for_backend_at(home.path(), Backend::Codex)
            .unwrap()
            .expect("codex backend should be active");

        assert!(
            !stale.exists(),
            "stale ~/.agents/skills/ copy must be purged"
        );
    }

    /// The `skills_home_subdir` field overrides the default skills root.
    /// Kept as a unit test of `SkillBackend::skills_dir` so the field's
    /// contract stays exercised even when no shipped backend uses it.
    #[test]
    fn skills_dir_honors_skills_home_subdir_override() {
        let home = tempdir().unwrap();
        let backend = SkillBackend {
            backend: Backend::Codex,
            name: "Test",
            home_subdir: ".sometool",
            skills_home_subdir: Some(".agents"),
            skills_subdir: "skills",
        };
        assert_eq!(
            backend.skills_dir(home.path()),
            home.path().join(".agents").join("skills")
        );
        assert!(!backend.root_exists(home.path()));
        std::fs::create_dir_all(home.path().join(".sometool")).unwrap();
        assert!(backend.root_exists(home.path()));
    }
}
