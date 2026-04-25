//! Auto-installer for the bundled `clud-*` skills.
//!
//! On every `clud` launch we ensure each entry in [`BUNDLED_SKILLS`] is
//! present at `~/.claude/skills/<name>/SKILL.md` and matches the version
//! baked into this binary via `include_str!`.
//!
//! Three states per skill:
//! - **Missing** — write the embedded copy, log a one-line install notice.
//! - **Matches modulo whitespace** — silent no-op.
//! - **Diverges semantically** — warn on stderr; do NOT overwrite. The user
//!   has edited the working copy and either wants to check the changes back
//!   into the source repo or revert. Either way, blind overwrite would lose
//!   their work.
//!
//! Each skill's source-of-truth lives at `skills/<name>/SKILL.md` in the repo
//! and is embedded at compile time, so a fresh `clud` install always carries
//! the current canonical copy of every bundled skill.
//!
//! All errors are non-fatal. A skill-install hiccup never breaks the launch
//! path — at worst the user sees a `[clud] note: ...` line and continues.

use std::path::{Path, PathBuf};

/// One bundled skill: the name (`clud-pr`) and the canonical SKILL.md
/// content baked into the binary at compile time.
struct Skill {
    name: &'static str,
    content: &'static str,
}

/// Every skill `clud` ships and auto-installs. Adding another skill is a
/// one-line entry here plus a new `skills/<name>/SKILL.md` file.
const BUNDLED_SKILLS: &[Skill] = &[
    Skill {
        name: "clud-pr",
        content: include_str!("../../../skills/clud-pr/SKILL.md"),
    },
    Skill {
        name: "clud-pr-merge",
        content: include_str!("../../../skills/clud-pr-merge/SKILL.md"),
    },
];

/// Run the install/check for every bundled skill on launch. Cheap on the
/// steady state (one stat + one read per skill). Failures degrade silently
/// to a stderr note.
pub fn ensure_installed() {
    for skill in BUNDLED_SKILLS {
        ensure_skill_installed(skill);
    }
}

fn ensure_skill_installed(skill: &Skill) {
    let Some(path) = target_path(skill.name) else {
        return;
    };
    match classify(&path, skill.content) {
        Existing::Missing => write_install(&path, skill),
        Existing::Matches => {}
        Existing::Diverges => warn_diverges(&path),
        Existing::Unreadable(err) => {
            eprintln!("[clud] note: could not read {}: {err}", path.display());
        }
    }
}

#[derive(Debug)]
enum Existing {
    Missing,
    Matches,
    Diverges,
    Unreadable(std::io::Error),
}

fn classify(path: &Path, embedded: &str) -> Existing {
    match std::fs::read_to_string(path) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Existing::Missing,
        Err(e) => Existing::Unreadable(e),
        Ok(content) => {
            if normalize(&content) == normalize(embedded) {
                Existing::Matches
            } else {
                Existing::Diverges
            }
        }
    }
}

/// Whitespace-tolerant equality. Collapses runs of whitespace (incl. CRLF
/// vs LF differences) into single spaces and trims the ends. So `"a  b\r\n"`
/// and `"a b"` compare equal — exactly what we want when the OS rewrites
/// line endings or a user re-formats trailing blanks.
fn normalize(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn target_path(skill_name: &str) -> Option<PathBuf> {
    let home = home_dir()?;
    Some(
        home.join(".claude")
            .join("skills")
            .join(skill_name)
            .join("SKILL.md"),
    )
}

fn home_dir() -> Option<PathBuf> {
    #[cfg(windows)]
    {
        std::env::var_os("USERPROFILE").map(PathBuf::from)
    }
    #[cfg(not(windows))]
    {
        std::env::var_os("HOME").map(PathBuf::from)
    }
}

fn write_install(path: &Path, skill: &Skill) {
    if let Some(parent) = path.parent() {
        if let Err(e) = std::fs::create_dir_all(parent) {
            eprintln!(
                "[clud] note: could not create skill dir {}: {e}",
                parent.display()
            );
            return;
        }
    }
    if let Err(e) = std::fs::write(path, skill.content) {
        eprintln!(
            "[clud] note: could not install /{} skill at {}: {e}",
            skill.name,
            path.display()
        );
        return;
    }
    eprintln!(
        "[clud] installed /{} skill at {}",
        skill.name,
        path.display()
    );
}

fn warn_diverges(path: &Path) {
    eprintln!(
        "[clud] note: {} diverges from the version embedded in clud; \
         check in your changes to the clud repo or revert.",
        path.display()
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    /// Embedded copy of the canonical clud-pr skill — used as the
    /// embedded-content reference in the install state-transition tests.
    /// Picking the first bundled skill keeps the tests aligned with
    /// production: any compile-time breakage in the include path fails
    /// here too.
    fn ref_skill() -> &'static Skill {
        &BUNDLED_SKILLS[0]
    }

    /// Mirror of [`ensure_skill_installed`] but routed at a caller-supplied
    /// path. Lets us unit-test the three state transitions without touching
    /// the real home directory.
    fn ensure_installed_at(path: &Path, skill: &Skill) -> Existing {
        let state = classify(path, skill.content);
        match &state {
            Existing::Missing => write_install(path, skill),
            Existing::Matches => {}
            Existing::Diverges => warn_diverges(path),
            Existing::Unreadable(_) => {}
        }
        state
    }

    #[test]
    fn normalize_collapses_whitespace_runs() {
        assert_eq!(normalize("a  b\n\nc"), "a b c");
        assert_eq!(normalize("a b c"), "a b c");
    }

    #[test]
    fn normalize_handles_crlf_vs_lf() {
        // The Windows scenario: file checked out with CRLF line endings,
        // EMBEDDED content baked in with LF. Whitespace normalization
        // must report these as equal so we don't warn on a checkout
        // artifact.
        let crlf = "line1\r\nline2\r\n";
        let lf = "line1\nline2\n";
        assert_eq!(normalize(crlf), normalize(lf));
    }

    #[test]
    fn missing_file_triggers_install() {
        let tmp = TempDir::new().unwrap();
        let skill = ref_skill();
        let target = tmp.path().join("skills").join(skill.name).join("SKILL.md");
        assert!(matches!(
            ensure_installed_at(&target, skill),
            Existing::Missing
        ));
        let written = fs::read_to_string(&target).unwrap();
        assert_eq!(written, skill.content);
    }

    #[test]
    fn identical_content_is_noop() {
        let tmp = TempDir::new().unwrap();
        let skill = ref_skill();
        let target = tmp.path().join("SKILL.md");
        fs::write(&target, skill.content).unwrap();
        let mtime_before = fs::metadata(&target).unwrap().modified().unwrap();

        let state = ensure_installed_at(&target, skill);
        assert!(matches!(state, Existing::Matches));
        let mtime_after = fs::metadata(&target).unwrap().modified().unwrap();
        assert_eq!(
            mtime_before, mtime_after,
            "Matches state must not rewrite the file"
        );
    }

    #[test]
    fn whitespace_only_diff_is_noop() {
        // CRLF on disk, LF embedded — must NOT trigger overwrite or
        // a divergence warning. This is the most common false-positive
        // we have to suppress on Windows checkouts.
        let tmp = TempDir::new().unwrap();
        let skill = ref_skill();
        let target = tmp.path().join("SKILL.md");
        let crlf = skill.content.replace('\n', "\r\n");
        fs::write(&target, &crlf).unwrap();

        let state = ensure_installed_at(&target, skill);
        assert!(
            matches!(state, Existing::Matches),
            "CRLF-vs-LF must classify as Matches, got {state:?}"
        );

        // File content stays as the user (or the OS) wrote it. We do
        // not silently rewrite to LF.
        let after = fs::read_to_string(&target).unwrap();
        assert_eq!(after, crlf);
    }

    #[test]
    fn semantic_diff_warns_and_preserves_user_edits() {
        let tmp = TempDir::new().unwrap();
        let skill = ref_skill();
        let target = tmp.path().join("SKILL.md");
        let user_version = format!("{}\n\n## Local addition\nMy custom rule.\n", skill.content);
        fs::write(&target, &user_version).unwrap();

        let state = ensure_installed_at(&target, skill);
        assert!(
            matches!(state, Existing::Diverges),
            "added content must classify as Diverges, got {state:?}"
        );

        // Critical: the user's edit is preserved, not overwritten.
        let after = fs::read_to_string(&target).unwrap();
        assert_eq!(after, user_version, "Diverges branch must NOT overwrite");
    }

    #[test]
    fn install_creates_missing_parent_directories() {
        let tmp = TempDir::new().unwrap();
        let skill = ref_skill();
        // Three nested levels none of which exist yet.
        let target = tmp.path().join("a").join("b").join("c").join("SKILL.md");
        assert!(!target.parent().unwrap().exists());

        let state = ensure_installed_at(&target, skill);
        assert!(matches!(state, Existing::Missing));
        assert!(target.exists(), "install must create the file");
        assert!(
            target.parent().unwrap().is_dir(),
            "install must create parent dirs"
        );
    }

    #[test]
    fn every_bundled_skill_has_real_content() {
        // Compile-time + content guard: if someone deletes a skill file
        // the include_str! still compiles to "" — assert every entry has
        // real frontmatter so a missing file is caught here, not by the
        // user reading an empty SKILL.md from their home dir.
        assert!(!BUNDLED_SKILLS.is_empty(), "no bundled skills?");
        for skill in BUNDLED_SKILLS {
            assert!(
                skill.content.len() > 100,
                "skill {} has suspiciously short content ({}b)",
                skill.name,
                skill.content.len()
            );
            let frontmatter_marker = format!("name: {}", skill.name);
            assert!(
                skill.content.contains(&frontmatter_marker),
                "skill {} content missing 'name: {}' frontmatter — bad include_str path?",
                skill.name,
                skill.name
            );
        }
    }

    #[test]
    fn bundle_includes_clud_pr_and_clud_pr_merge() {
        // The two skills wired up so far. Adding more is fine; this test
        // just guards against accidental removal of either.
        let names: Vec<&str> = BUNDLED_SKILLS.iter().map(|s| s.name).collect();
        assert!(names.contains(&"clud-pr"), "clud-pr missing from bundle");
        assert!(
            names.contains(&"clud-pr-merge"),
            "clud-pr-merge missing from bundle"
        );
    }

    #[test]
    fn bundled_skills_have_unique_names() {
        // Two entries with the same name would silently overwrite each
        // other on disk — guard against typos at compile-test time.
        let mut names: Vec<&str> = BUNDLED_SKILLS.iter().map(|s| s.name).collect();
        names.sort();
        let len_before = names.len();
        names.dedup();
        assert_eq!(
            names.len(),
            len_before,
            "BUNDLED_SKILLS contains duplicate names"
        );
    }

    #[test]
    fn install_pass_processes_every_bundled_skill() {
        // Drive the whole bundle against an isolated tmp tree by simulating
        // each entry's per-skill install. This is the multi-skill analog
        // of `missing_file_triggers_install` and confirms that adding a
        // skill to BUNDLED_SKILLS actually causes it to land on disk.
        let tmp = TempDir::new().unwrap();
        for skill in BUNDLED_SKILLS {
            let target = tmp.path().join("skills").join(skill.name).join("SKILL.md");
            assert!(matches!(
                ensure_installed_at(&target, skill),
                Existing::Missing
            ));
            let on_disk = fs::read_to_string(&target).unwrap();
            assert_eq!(on_disk, skill.content);
        }
    }
}
