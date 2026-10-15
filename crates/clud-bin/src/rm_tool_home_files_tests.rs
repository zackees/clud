//! #1956: a single file under `$HOME` outside the roots is trashed; a
//! directory there, or a denylisted file, is still refused.

use super::*;

#[test]
fn a_single_file_under_home_outside_the_roots_is_trashed() {
    let w = world();
    let apps = w.home.join(".local/share/applications");
    std::fs::create_dir_all(&apps).unwrap();
    let desktop = apps.join("x.desktop");
    std::fs::write(&desktop, b"[Desktop Entry]").unwrap();
    let (code, _, err) = w.run(Kind::File, &[desktop.to_str().unwrap()]);
    assert_eq!(code, 0, "{err}");
    assert!(!desktop.exists());
    assert!(apps.exists(), "only the file goes");
    assert_eq!(w.entries().len(), 1, "it is trashed, not purged");
    let record = w.audit_records().pop().unwrap().to_string();
    assert!(record.contains(home_files::GRANT_REASON), "{record}");
}

#[cfg(unix)]
#[test]
fn a_symlink_under_home_is_removed_as_a_link() {
    let w = world();
    let target = w.home.join("real");
    std::fs::write(&target, b"keep").unwrap();
    let link = w.home.join("link");
    std::os::unix::fs::symlink(&target, &link).unwrap();
    let (code, _, err) = w.run(Kind::File, &[link.to_str().unwrap()]);
    assert_eq!(code, 0, "{err}");
    assert!(std::fs::symlink_metadata(&link).is_err());
    assert!(target.exists());
}

#[test]
fn a_directory_under_home_outside_the_roots_is_still_refused() {
    let w = world();
    let dir = w.home.join("project");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("f"), b"x").unwrap();
    for (kind, flags) in [(Kind::File, "-r"), (Kind::Dir, "-f")] {
        let (code, _, err) = w.run(kind, &[flags, dir.to_str().unwrap()]);
        assert_eq!(code, 1, "{err}");
        assert!(err.contains("outside the allowed roots"), "{err}");
    }
    assert!(dir.join("f").exists());
}

#[test]
fn denylisted_home_files_are_refused_by_name() {
    let w = world();
    std::fs::create_dir_all(w.home.join(".ssh")).unwrap();
    for rel in [".ssh/id_ed25519", ".bashrc", ".git-credentials"] {
        let path = w.home.join(rel);
        std::fs::write(&path, b"secret").unwrap();
        let (code, _, err) = w.run(Kind::File, &[path.to_str().unwrap()]);
        assert_eq!(code, 1, "{rel}: {err}");
        assert!(err.contains("home denylist"), "{rel}: {err}");
        assert!(path.exists(), "{rel}");
    }
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
fn denylist_matches_whole_components_case_insensitively() {
    use std::path::Path;
    assert_eq!(home_files::denied(Path::new(".SSH/config")), Some(".ssh"));
    assert_eq!(home_files::denied(Path::new(".sshx/config")), None);
    assert_eq!(home_files::denied(Path::new("notes/.bashrc")), None);
    assert_eq!(
        home_files::denied(Path::new(".config/gh/hosts.yml")),
        Some(".config/gh")
    );
}
