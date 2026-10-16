//! #1956, #1957: paths under `$HOME` outside the roots. Single files are
//! trashed, directories only inside the tool data dirs; personal folders and
//! denylisted credentials are refused.

use super::*;

fn file(w: &World, rel: &str) -> PathBuf {
    let path = w.home.join(rel);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(&path, b"x").unwrap();
    path
}

#[test]
fn a_single_file_under_home_outside_the_roots_is_trashed() {
    let w = world();
    let desktop = file(&w, ".local/share/applications/x.desktop");
    let (code, _, err) = w.run(Kind::File, &[desktop.to_str().unwrap()]);
    assert_eq!(code, 0, "{err}");
    assert!(!desktop.exists());
    assert!(desktop.parent().unwrap().exists(), "only the file goes");
    assert_eq!(w.entries().len(), 1, "it is trashed, not purged");
    let record = w.audit_records().pop().unwrap().to_string();
    assert!(record.contains(home_files::GRANT_REASON), "{record}");
}

#[test]
fn a_memory_file_the_agent_wrote_is_trashed() {
    let w = world();
    let note = file(&w, ".claude/projects/-home-u-dev-x/memory/wrong.md");
    let (code, _, err) = w.run(Kind::File, &[note.to_str().unwrap()]);
    assert_eq!(code, 0, "{err}");
    assert!(!note.exists());
}

#[test]
fn a_directory_inside_a_tool_data_dir_is_trashed_but_not_the_dir_itself() {
    let w = world();
    file(&w, ".claude/projects/-a/memory/m.md");
    let project = w.home.join(".claude/projects/-a");
    let (code, _, err) = w.run(Kind::File, &["-r", project.to_str().unwrap()]);
    assert_eq!(code, 0, "{err}");
    assert!(!project.exists());
    let claude = w.home.join(".claude");
    let (code, _, err) = w.run(Kind::File, &["-r", claude.to_str().unwrap()]);
    assert_eq!(code, 1, "{err}");
    assert!(claude.exists());
}

#[test]
fn a_directory_elsewhere_under_home_is_still_refused() {
    let w = world();
    let inside = file(&w, "dev/project/f");
    let dir = inside.parent().unwrap();
    for (kind, flags) in [(Kind::File, "-r"), (Kind::Dir, "-f")] {
        let (code, _, err) = w.run(kind, &[flags, dir.to_str().unwrap()]);
        assert_eq!(code, 1, "{err}");
        assert!(err.contains("outside the allowed roots"), "{err}");
    }
    assert!(inside.exists());
}

#[test]
fn personal_folders_are_refused_even_for_single_files() {
    let w = world();
    for rel in ["Documents/taxes.pdf", "desktop/note.txt", "Pictures/a/b.png"] {
        let path = file(&w, rel);
        let (code, _, err) = w.run(Kind::File, &[path.to_str().unwrap()]);
        assert_eq!(code, 1, "{rel}: {err}");
        assert!(err.contains("personal data folder"), "{rel}: {err}");
        assert!(path.exists(), "{rel}");
    }
}

#[test]
fn denylisted_paths_and_dirs_containing_them_are_refused_by_name() {
    let w = world();
    for rel in [
        ".ssh/id_ed25519",
        ".bashrc",
        ".git-credentials",
        ".claude/.credentials.json",
        ".clud/settings.json",
    ] {
        let path = file(&w, rel);
        let (code, _, err) = w.run(Kind::File, &[path.to_str().unwrap()]);
        assert_eq!(code, 1, "{rel}: {err}");
        assert!(err.contains("home denylist"), "{rel}: {err}");
        assert!(path.exists(), "{rel}");
    }
    file(&w, ".config/gh/hosts.yml");
    let config = w.home.join(".config");
    let (code, _, err) = w.run(Kind::File, &["-r", config.to_str().unwrap()]);
    assert_eq!(code, 1, "{err}");
    assert!(err.contains("home denylist"), "{err}");
}

#[test]
fn a_file_outside_home_and_the_roots_is_still_refused() {
    let w = world();
    let outside = w.home.parent().unwrap().join("elsewhere.txt");
    std::fs::write(&outside, b"x").unwrap();
    let (code, _, err) = w.run(Kind::File, &[outside.to_str().unwrap()]);
    assert_eq!(code, 1);
    assert!(err.contains("outside the allowed roots"), "{err}");
    assert!(outside.exists());
}

#[test]
fn home_itself_is_never_deleted() {
    let w = world();
    let (code, _, err) = w.run(Kind::File, &["-rf", w.home.to_str().unwrap()]);
    assert_eq!(code, 1, "{err}");
    assert!(w.home.exists());
}

#[test]
fn rule_tables_match_whole_components_case_insensitively() {
    use std::path::Path;
    assert_eq!(home_files::denied(Path::new(".SSH/config")), Some(".ssh"));
    assert_eq!(home_files::denied(Path::new(".sshx/config")), None);
    assert_eq!(home_files::denied(Path::new("notes/.bashrc")), None);
    assert_eq!(home_files::denied(Path::new(".config")), Some(".config/gh"));
    assert_eq!(home_files::personal(Path::new("documents/x")), Some("Documents"));
    assert_eq!(home_files::personal(Path::new("Documentsx/x")), None);
    assert!(home_files::dir_allowed(Path::new(".cache/pip")));
    assert!(!home_files::dir_allowed(Path::new(".cache")));
    assert!(!home_files::dir_allowed(Path::new("dev/x")));
}
