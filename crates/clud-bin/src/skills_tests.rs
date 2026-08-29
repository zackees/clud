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
    serde_yaml::from_str(frontmatter_yaml(skill.name, skill.skill_md))
        .unwrap_or_else(|err| panic!("skill {} has invalid YAML frontmatter: {err}", skill.name))
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
fn refreshes_a_stale_clud_managed_copy() {
    // The bug this closes: a user who installed clud-issue months ago
    // kept the interview-era body forever, because install skipped any
    // existing file. Editing the bundled skill reached nobody.
    let dir = tempdir().unwrap();
    let managed = [BundledSkill {
        name: "alpha",
        skill_md: "<!-- managed-by: clud -->\nnew body\n",
    }];
    let alpha_dir = dir.path().join("alpha");
    std::fs::create_dir_all(&alpha_dir).unwrap();
    std::fs::write(
        alpha_dir.join("SKILL.md"),
        "<!-- managed-by: clud -->\nold body\n",
    )
    .unwrap();

    let report = install_to(dir.path(), &managed).unwrap();

    assert_eq!(report.refreshed, vec!["alpha"]);
    assert!(report.installed.is_empty());
    assert!(report.skipped_existing.is_empty());
    assert_eq!(
        std::fs::read_to_string(alpha_dir.join("SKILL.md")).unwrap(),
        "<!-- managed-by: clud -->\nnew body\n"
    );

    // Second pass is quiet.
    let again = install_to(dir.path(), &managed).unwrap();
    assert!(again.refreshed.is_empty());
    assert_eq!(again.skipped_existing, vec!["alpha"]);
}

#[test]
fn refresh_never_touches_a_copy_whose_marker_the_user_removed() {
    let dir = tempdir().unwrap();
    let managed = [BundledSkill {
        name: "alpha",
        skill_md: "<!-- managed-by: clud -->\nnew body\n",
    }];
    let alpha_dir = dir.path().join("alpha");
    std::fs::create_dir_all(&alpha_dir).unwrap();
    std::fs::write(alpha_dir.join("SKILL.md"), "mine now\n").unwrap();

    let report = install_to(dir.path(), &managed).unwrap();

    assert_eq!(report.skipped_existing, vec!["alpha"]);
    assert!(report.refreshed.is_empty());
    assert_eq!(
        std::fs::read_to_string(alpha_dir.join("SKILL.md")).unwrap(),
        "mine now\n",
        "dropping the marker is how a user claims ownership"
    );
}

#[test]
fn bundled_skills_all_carry_the_ownership_marker() {
    // Refresh keys on the marker; a bundled skill without one would be
    // installed and then never updated again.
    for skill in BUNDLED_SKILLS {
        assert!(
            skill.skill_md.contains(MANAGED_BY_CLUD_MARKER),
            "skill {} is missing the `{MANAGED_BY_CLUD_MARKER}` marker",
            skill.name
        );
    }
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

/// The regression test for #844. `idempotent_second_pass_is_a_noop` above
/// proves the *algorithm* is idempotent using synthetic skills; this proves
/// the *shipped bundle* is, which is where the bug actually lived. A body
/// that fails to round-trip through write-then-read — missing marker, or
/// on-disk bytes that don't compare equal to `skill_md` — makes every launch
/// rewrite the file and print a green `[clud] updated /<name>` that changed
/// nothing. That ran for months against real users.
#[test]
fn real_bundle_install_is_idempotent() {
    let dir = tempdir().unwrap();

    let first = install_to(dir.path(), BUNDLED_SKILLS).unwrap();
    assert_eq!(first.installed.len(), BUNDLED_SKILLS.len());
    assert!(first.refreshed.is_empty());

    let second = install_to(dir.path(), BUNDLED_SKILLS).unwrap();
    assert!(
        second.installed.is_empty(),
        "reinstalled on second pass: {:?}",
        second.installed
    );
    assert!(
        second.refreshed.is_empty(),
        "second pass reported a refresh with nothing changed: {:?}",
        second.refreshed
    );
    assert_eq!(second.skipped_existing.len(), BUNDLED_SKILLS.len());
}

/// A CRLF-vs-LF difference is not a real change. Without the whitespace
/// tolerance in `install_to`, a home whose checkout or editor rewrote line
/// endings would be "refreshed" on every single launch — the same
/// user-visible symptom as #844, from a different cause.
#[test]
fn line_ending_drift_is_not_a_refresh() {
    let dir = tempdir().unwrap();
    let skills = fake_skills();
    install_to(dir.path(), &skills).unwrap();

    let path = dir.path().join("alpha/SKILL.md");
    let crlf = std::fs::read_to_string(&path)
        .unwrap()
        .replace('\n', "\r\n");
    std::fs::write(&path, &crlf).unwrap();

    let report = install_to(dir.path(), &skills).unwrap();
    assert!(
        report.refreshed.is_empty(),
        "line-ending drift was treated as a real change: {:?}",
        report.refreshed
    );
    assert_eq!(
        std::fs::read_to_string(&path).unwrap(),
        crlf,
        "a no-op pass must not rewrite the file"
    );
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
    assert!(names.contains(&"clud-fix-quick"));
    assert!(names.contains(&"clud-review"));
    assert!(names.contains(&"clud-tag-release"));
    assert!(names.contains(&"clud-docker-rust-app-dev"));
    assert!(names.contains(&"clud-windows-trash"));
    assert!(names.contains(&"clud-extern-repos"));
    assert!(names.contains(&"clud-omarchy"));
    assert!(names.contains(&"clud-improve"));
    assert!(names.contains(&"clud-docker-mac-x86"));
    assert!(names.contains(&"clud-docker-recover"));
}

/// Issue #531: the Docker-recovery skill must trigger on the failure
/// modes from the incident (engine pipe absent, WSL/Docker startup
/// failures, Docker VM disk/memory questions) and must carry the
/// non-destructive, config-driven storage guidance from the follow-up
/// comment. Locks the load-bearing guarantees into the embedded body.
#[test]
fn clud_docker_recover_skill_is_non_destructive_and_config_driven() {
    let skill = BUNDLED_SKILLS
        .iter()
        .find(|skill| skill.name == "clud-docker-recover")
        .expect("clud-docker-recover must be bundled")
        .skill_md;

    for required in [
        "clud tool run docker/docker_recover.py doctor",
        "read-only",
        "CustomWslDistroDir",
        "DataFolder",
        "never compacts, prunes, deletes, resets, or",
        "images and volumes",
        "attempts = 10, interval = 2s",
    ] {
        assert!(
            skill.contains(required),
            "clud-docker-recover skill missing required guidance: {required:?}"
        );
    }
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

/// The worktree-teardown guarantees this pins used to live in `clud-pr`.
/// That skill was retired (superseded by the harness `/goal` hook), and
/// `clud-git` is now the single source of truth for the worktree / branch /
/// process-audit playbook — so the guardrail follows the playbook rather
/// than retiring with its old host. The failure this prevents is real and
/// Windows-specific: a blind `rm -rf` retry loop papers over a process
/// holding a file open instead of identifying and stopping it.
#[test]
fn clud_git_teardown_requires_process_audit() {
    let skill = BUNDLED_SKILLS
        .iter()
        .find(|skill| skill.name == "clud-git")
        .expect("clud-git must be bundled")
        .skill_md;

    for required in [
        "Process audit before destructive removal.",
        "Never blind-loop `rm -rf`.",
        "clud trash",
    ] {
        assert!(
            skill.contains(required),
            "clud-git skill missing process-audit teardown guidance: {required}"
        );
    }
}

#[test]
fn retired_skills_are_not_also_bundled() {
    // A name in both lists would install and then immediately purge on
    // every launch, or purge-then-reinstall depending on call order.
    let bundled: Vec<&str> = BUNDLED_SKILLS.iter().map(|s| s.name).collect();
    for retired in PURGED_BUNDLED_SKILLS {
        assert!(
            !bundled.contains(retired),
            "{retired} is retired but still bundled"
        );
    }
}

#[test]
fn clud_loop_is_retired() {
    assert!(
        PURGED_BUNDLED_SKILLS.contains(&"clud-loop"),
        "clud-loop must stay in the purge list so upgrades clean old homes"
    );
}

#[test]
fn purge_removes_retired_skill_from_every_backend() {
    let home = tempdir().unwrap();
    let mut dirs = Vec::new();
    for backend in SKILL_BACKENDS {
        let dir = backend.skills_dir(home.path()).join("clud-loop");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("SKILL.md"),
            "---\nname: clud-loop\n---\n<!-- managed-by: clud -->\n",
        )
        .unwrap();
        dirs.push(dir);
    }

    let report = purge_retired_bundled_skills(home.path(), &["clud-loop"]);

    assert_eq!(report.removed.len(), SKILL_BACKENDS.len());
    assert!(report.failed.is_empty());
    for dir in dirs {
        assert!(!dir.exists(), "{} should be gone", dir.display());
    }
}

#[test]
fn purge_preserves_a_skill_the_user_owns() {
    let home = tempdir().unwrap();
    let dir = SKILL_BACKENDS[0].skills_dir(home.path()).join("clud-loop");
    std::fs::create_dir_all(&dir).unwrap();
    // No `managed-by: clud` marker: the user hand-wrote or edited this.
    std::fs::write(dir.join("SKILL.md"), "---\nname: clud-loop\n---\nmine\n").unwrap();

    let report = purge_retired_bundled_skills(home.path(), &["clud-loop"]);

    assert_eq!(report.preserved, vec!["clud-loop"]);
    assert!(report.removed.is_empty());
    assert!(dir.join("SKILL.md").is_file());
}

#[test]
fn purge_is_idempotent_and_quiet_when_nothing_is_installed() {
    let home = tempdir().unwrap();
    let report = purge_retired_bundled_skills(home.path(), PURGED_BUNDLED_SKILLS);
    assert!(report.removed.is_empty());
    assert!(report.preserved.is_empty());
    assert!(report.failed.is_empty());
}

/// `clud-pr`, `clud-fix`, `clud-do` and `clud-pr-merge` were retired because
/// the harness `/goal` Stop hook does their orchestration natively. They stay
/// in the purge list so upgrades clean the copies already sitting in user
/// homes — dropping a name from here would strand it there forever, which is
/// the leak [`PURGED_BUNDLED_SKILLS`] exists to close.
///
/// `clud-pr-merge` is the one that needs the explicit pin: it was never in
/// [`BUNDLED_SKILLS`], only in the deleted `skill_install.rs` registry, so
/// nothing else in this file would notice it going missing.
#[test]
fn goal_superseded_skills_stay_retired() {
    for retired in ["clud-pr", "clud-fix", "clud-do", "clud-pr-merge"] {
        assert!(
            PURGED_BUNDLED_SKILLS.contains(&retired),
            "{retired} must stay in the purge list so upgrades clean old homes"
        );
    }
}

/// Two entries with the same `name` would race to write the same
/// `<name>/SKILL.md`, and which one won would depend on registry order.
/// Ported from the deleted `skill_install.rs`, which was the only registry
/// that checked this.
#[test]
fn bundled_skill_names_are_unique() {
    let mut names: Vec<&str> = BUNDLED_SKILLS.iter().map(|s| s.name).collect();
    names.sort_unstable();
    let before = names.len();
    names.dedup();
    assert_eq!(
        before,
        names.len(),
        "duplicate skill name in BUNDLED_SKILLS"
    );
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
    assert!(home
        .path()
        .join(".codex/skills/clud-issue/SKILL.md")
        .exists());
    assert!(!home
        .path()
        .join(".agents/skills/clud-issue/SKILL.md")
        .exists());
}

#[test]
fn codex_install_writes_bundled_skill_bodies_byte_for_byte() {
    let home = tempdir().unwrap();
    std::fs::create_dir_all(home.path().join(".codex")).unwrap();

    ensure_installed_for_backend_at(home.path(), Backend::Codex)
        .unwrap()
        .expect("codex backend should be active");

    for skill_name in ["clud-issue", "clud-review"] {
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
    // Must name a *currently bundled* skill. Pointed at a retired name this
    // test passes vacuously — nothing would have written there anyway.
    let skill_dir = home.path().join(".codex/skills/clud-issue");
    std::fs::create_dir_all(&skill_dir).unwrap();
    std::fs::write(skill_dir.join("SKILL.md"), "USER EDIT\n").unwrap();

    ensure_installed_for_backend_at(home.path(), Backend::Codex)
        .unwrap()
        .expect("codex backend should be active");

    assert_eq!(
        std::fs::read_to_string(skill_dir.join("SKILL.md")).unwrap(),
        "USER EDIT\n",
        "existing user content under ~/.codex/skills/ must not be overwritten"
    );
}

/// Codex install must clean up stale clud-managed copies from `~/.agents/skills/`.
#[test]
fn codex_install_purges_stale_agents_skills() {
    let home = tempdir().unwrap();
    std::fs::create_dir_all(home.path().join(".codex")).unwrap();
    let stale = home.path().join(".agents/skills/clud-issue");
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
