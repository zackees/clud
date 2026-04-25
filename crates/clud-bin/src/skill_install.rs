//! Auto-installer for the bundled `/clud-pr` skill.
//!
//! On every `clud` launch we ensure `~/.claude/skills/clud-pr/SKILL.md` exists
//! and matches the version baked into this binary via `include_str!`.
//!
//! Three states:
//! - **Missing** — write the embedded copy, log a one-line install notice.
//! - **Matches modulo whitespace** — silent no-op.
//! - **Diverges semantically** — warn on stderr; do NOT overwrite. The user
//!   has edited the working copy and either wants to check the changes back
//!   into the source repo or revert. Either way, blind overwrite would lose
//!   their work.
//!
//! The skill source-of-truth lives at `skills/clud-pr/SKILL.md` in the repo
//! and is embedded at compile time, so a fresh `clud` install always carries
//! the current canonical copy.
//!
//! All errors are non-fatal. A skill-install hiccup never breaks the launch
//! path — at worst the user sees a `[clud] note: ...` line and continues.

use std::path::{Path, PathBuf};

const EMBEDDED_SKILL: &str = include_str!("../../../skills/clud-pr/SKILL.md");
const SKILL_NAME: &str = "clud-pr";

/// Run the install/check on every launch. Cheap on the steady state
/// (one stat + one read). Failures degrade silently to a stderr note.
pub fn ensure_installed() {
    let Some(path) = target_path() else {
        return;
    };
    match classify(&path) {
        Existing::Missing => write_install(&path),
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

fn classify(path: &Path) -> Existing {
    match std::fs::read_to_string(path) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Existing::Missing,
        Err(e) => Existing::Unreadable(e),
        Ok(content) => {
            if normalize(&content) == normalize(EMBEDDED_SKILL) {
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

fn target_path() -> Option<PathBuf> {
    let home = home_dir()?;
    Some(
        home.join(".claude")
            .join("skills")
            .join(SKILL_NAME)
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

fn write_install(path: &Path) {
    if let Some(parent) = path.parent() {
        if let Err(e) = std::fs::create_dir_all(parent) {
            eprintln!(
                "[clud] note: could not create skill dir {}: {e}",
                parent.display()
            );
            return;
        }
    }
    if let Err(e) = std::fs::write(path, EMBEDDED_SKILL) {
        eprintln!(
            "[clud] note: could not install /{} skill at {}: {e}",
            SKILL_NAME,
            path.display()
        );
        return;
    }
    eprintln!(
        "[clud] installed /{} skill at {}",
        SKILL_NAME,
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

    /// Mirror of [`ensure_installed`] but routed at a caller-supplied path
    /// instead of `~/.claude/skills/...`. Lets us unit-test the three
    /// state transitions without touching the real home directory.
    fn ensure_installed_at(path: &Path) -> Existing {
        let state = classify(path);
        match &state {
            Existing::Missing => write_install(path),
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
        // EMBEDDED_SKILL baked in with LF. Whitespace normalization
        // must report these as equal so we don't warn on a checkout
        // artifact.
        let crlf = "line1\r\nline2\r\n";
        let lf = "line1\nline2\n";
        assert_eq!(normalize(crlf), normalize(lf));
    }

    #[test]
    fn missing_file_triggers_install() {
        let tmp = TempDir::new().unwrap();
        let target = tmp.path().join("skills").join("clud-pr").join("SKILL.md");
        assert!(matches!(ensure_installed_at(&target), Existing::Missing));
        let written = fs::read_to_string(&target).unwrap();
        assert_eq!(written, EMBEDDED_SKILL);
    }

    #[test]
    fn identical_content_is_noop() {
        let tmp = TempDir::new().unwrap();
        let target = tmp.path().join("SKILL.md");
        fs::write(&target, EMBEDDED_SKILL).unwrap();
        let mtime_before = fs::metadata(&target).unwrap().modified().unwrap();

        // Sleep a couple of ms? Not needed — Matches state takes the
        // no-op branch, so the file is never opened for write. We
        // assert by reading content unchanged AND that classify ==
        // Matches.
        let state = ensure_installed_at(&target);
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
        let target = tmp.path().join("SKILL.md");
        let crlf = EMBEDDED_SKILL.replace('\n', "\r\n");
        fs::write(&target, &crlf).unwrap();

        let state = ensure_installed_at(&target);
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
        let target = tmp.path().join("SKILL.md");
        let user_version = format!("{EMBEDDED_SKILL}\n\n## Local addition\nMy custom rule.\n");
        fs::write(&target, &user_version).unwrap();

        let state = ensure_installed_at(&target);
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
        // Three nested levels none of which exist yet.
        let target = tmp.path().join("a").join("b").join("c").join("SKILL.md");
        assert!(!target.parent().unwrap().exists());

        let state = ensure_installed_at(&target);
        assert!(matches!(state, Existing::Missing));
        assert!(target.exists(), "install must create the file");
        assert!(
            target.parent().unwrap().is_dir(),
            "install must create parent dirs"
        );
    }

    #[test]
    fn embedded_skill_is_nonempty() {
        // Compile-time guard: if someone deletes skills/clud-pr/SKILL.md
        // the include_str! still compiles to "" — assert the bake is real.
        assert!(EMBEDDED_SKILL.len() > 100, "EMBEDDED_SKILL looks empty");
        assert!(
            EMBEDDED_SKILL.contains("name: clud-pr"),
            "EMBEDDED_SKILL missing frontmatter — bad include_str path?"
        );
    }
}
