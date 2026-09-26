use super::*;

fn args(list: &[&str]) -> Vec<String> {
    list.iter().map(|s| s.to_string()).collect()
}

struct World {
    _tmp: tempfile::TempDir,
    root: PathBuf,
    trash: PathBuf,
    audit: PathBuf,
    home: PathBuf,
}

fn world() -> World {
    let tmp = tempfile::tempdir().unwrap();
    let base = std::fs::canonicalize(tmp.path()).unwrap();
    let root = base.join("repo");
    let home = base.join("home");
    for dir in [&root, &home] {
        std::fs::create_dir_all(dir).unwrap();
    }
    World {
        trash: base.join("trash"),
        audit: base.join("audit"),
        root,
        home,
        _tmp: tmp,
    }
}

impl World {
    fn ctx(&self) -> Context {
        Context {
            cwd: self.root.clone(),
            home: Some(self.home.clone()),
            trash_root: self.trash.clone(),
            audit_dir: Some(self.audit.clone()),
            session_id: Some("session-1".into()),
            role: "agent".into(),
            register: false,
            now: SystemTime::now(),
        }
    }

    fn roots(&self) -> Roots {
        Roots::fixed(vec![self.root.clone()], true)
    }

    fn run(&self, kind: Kind, list: &[&str]) -> (i32, String, String) {
        let options = parse_args(&args(list)).unwrap().unwrap();
        let (mut out, mut err) = (Vec::new(), Vec::new());
        let code = run_with(
            kind,
            &options,
            &self.ctx(),
            &mut self.roots(),
            &mut out,
            &mut err,
        );
        (
            code,
            String::from_utf8(out).unwrap(),
            String::from_utf8(err).unwrap(),
        )
    }

    fn entries(&self) -> Vec<PathBuf> {
        std::fs::read_dir(&self.trash)
            .map(|d| d.filter_map(Result::ok).map(|e| e.path()).collect())
            .unwrap_or_default()
    }

    fn audit_records(&self) -> Vec<serde_json::Value> {
        std::fs::read_dir(&self.audit)
            .map(|d| {
                d.filter_map(Result::ok)
                    .flat_map(|e| {
                        std::fs::read_to_string(e.path())
                            .unwrap()
                            .lines()
                            .map(|l| serde_json::from_str(l).unwrap())
                            .collect::<Vec<_>>()
                    })
                    .collect()
            })
            .unwrap_or_default()
    }
}

#[test]
fn argv0_names_select_the_command() {
    assert_eq!(Kind::from_program_name("rm-file"), Some(Kind::File));
    assert_eq!(Kind::from_program_name("rm-dir"), Some(Kind::Dir));
    assert_eq!(Kind::from_program_name("rm-file.exe"), Some(Kind::File));
    assert_eq!(Kind::from_program_name("rm-dir.exe"), Some(Kind::Dir));
    assert_eq!(Kind::from_program_name("rm"), None);
    assert_eq!(Kind::from_program_name("clud"), None);
}

#[test]
fn only_three_flags_parse() {
    let parsed = parse_args(&args(&[
        "--purge",
        "--tracked",
        "--dry-run",
        "--",
        "a",
        "-b",
    ]))
    .unwrap()
    .unwrap();
    assert!(parsed.purge && parsed.tracked && parsed.dry_run);
    assert_eq!(parsed.paths, args(&["a", "-b"]));
    // After the first path, everything is a path: a file named `--purge`
    // from a glob cannot switch the call to purge.
    let later = parse_args(&args(&["a", "--purge", "--"])).unwrap().unwrap();
    assert!(!later.purge);
    assert_eq!(later.paths, args(&["a", "--purge", "--"]));
    assert_eq!(parse_args(&args(&["--help"])).unwrap(), None);
    assert!(parse_args(&args(&["-rf", "x"])).is_err());
    assert!(parse_args(&args(&["--force", "x"])).is_err());
    assert!(parse_args(&args(&["--purge"])).is_err(), "no paths");
}

#[test]
fn roots_come_from_the_env_or_the_git_checkout() {
    let w = world();
    let nested = w.root.join("src/deep");
    std::fs::create_dir_all(&nested).unwrap();
    // No env: the nearest checkout, found by its `.git`.
    std::fs::create_dir_all(w.root.join(".git")).unwrap();
    let roots = Roots::resolve(None, &nested);
    assert_eq!(roots.roots, vec![w.root.clone()]);
    assert!(!roots.from_env);
    // Outside a repo: the cwd.
    let roots = Roots::resolve(None, &w.home);
    assert_eq!(roots.roots, vec![w.home.clone()]);
    // The env wins, relative and missing entries are dropped.
    let value =
        std::env::join_paths([w.home.clone(), PathBuf::from("rel"), w.root.join("missing")])
            .unwrap();
    let roots = Roots::resolve(Some(&value), &nested);
    assert_eq!(roots.roots, vec![w.home.clone()]);
    assert!(roots.from_env);
    // Set but empty is treated as unset.
    assert!(!Roots::resolve(Some(OsStr::new("")), &nested).from_env);
}

#[test]
fn refuses_roots_home_outside_and_git_metadata() {
    let w = world();
    std::fs::create_dir_all(w.root.join(".git")).unwrap();
    std::fs::write(w.home.join("notes"), b"x").unwrap();
    let mut roots = w.roots();
    let refused =
        |raw: &str, roots: &mut Roots| resolve(raw, &w.root, Some(&w.home), roots).unwrap_err();
    assert!(refused(w.root.to_str().unwrap(), &mut roots).contains("root itself"));
    assert!(refused(".", &mut roots).contains("name it directly"));
    assert!(refused("sub/.", &mut roots).contains("name it directly"));
    assert!(refused("./", &mut roots).contains("name it directly"));
    assert!(refused(".git/objects", &mut roots).contains("git metadata"));
    assert!(refused(".GIT/HEAD", &mut roots).contains("git metadata"));
    assert!(refused("sub/..", &mut roots).contains("name it directly"));
    assert!(refused("/", &mut roots).contains("name it directly"));
    assert!(refused(".git", &mut roots).contains("git metadata"));
    assert!(refused(w.home.join("notes").to_str().unwrap(), &mut roots).contains("outside"));
    assert!(refused("", &mut roots).contains("empty"));
    // Home and its ancestors are never roots, however the session started.
    let value = std::env::join_paths([w.home.clone(), w.root.clone()]).unwrap();
    let from_home = Roots::resolve_with_home(Some(&value), &w.root, Some(&w.home));
    assert_eq!(from_home.roots, vec![w.root.clone()]);
    let parent = w.home.parent().unwrap().to_path_buf();
    assert!(Roots::resolve_with_home(None, &parent, Some(&w.home))
        .roots
        .is_empty());
    // Home and its ancestors are refused even when a root contains them.
    let mut wide = Roots::fixed(vec![w.home.parent().unwrap().to_path_buf()], true);
    assert!(
        resolve(w.home.to_str().unwrap(), &w.root, Some(&w.home), &mut wide)
            .unwrap_err()
            .contains("home")
    );
    // A missing path resolves as missing, not as an error.
    assert!(matches!(
        resolve("nope/deeper", &w.root, Some(&w.home), &mut roots),
        Ok(Resolved::Missing(_))
    ));
}

#[cfg(unix)]
#[test]
fn a_symlinked_parent_that_leaves_the_roots_is_refused_and_a_final_link_is_removed_as_a_link() {
    let w = world();
    let outside = w.home.join("outside");
    std::fs::create_dir_all(&outside).unwrap();
    std::fs::write(outside.join("keep"), b"precious").unwrap();
    std::os::unix::fs::symlink(&outside, w.root.join("escape")).unwrap();
    let (code, _, err) = w.run(Kind::File, &["escape/keep"]);
    assert_eq!(code, 1);
    assert!(err.contains("outside"), "{err}");
    assert!(outside.join("keep").exists());
    // `rm-dir escape` would follow nothing: the link is not a directory.
    let (code, _, err) = w.run(Kind::Dir, &["escape"]);
    assert_eq!(code, 1);
    assert!(err.contains("use rm-file"), "{err}");
    // `rm-file escape` trashes the link itself; the target survives.
    let (code, out, err) = w.run(Kind::File, &["escape"]);
    assert_eq!(code, 0, "{err}");
    assert!(out.starts_with("trashed"), "{out}");
    assert!(std::fs::symlink_metadata(w.root.join("escape")).is_err());
    assert!(outside.join("keep").exists());
}

#[test]
fn trash_by_default_keeps_paths_relative_to_their_root_with_a_manifest() {
    let w = world();
    std::fs::create_dir_all(w.root.join("build/obj")).unwrap();
    std::fs::write(w.root.join("build/obj/a.o"), b"obj").unwrap();
    std::fs::write(w.root.join("notes.txt"), b"note").unwrap();
    let (code, out, err) = w.run(Kind::Dir, &["build"]);
    assert_eq!(code, 0, "{err}");
    assert!(out.contains("trashed"), "{out}");
    assert!(!w.root.join("build").exists());
    let entries = w.entries();
    assert_eq!(entries.len(), 1, "one call is one trash entry");
    let entry = &entries[0];
    let name = entry.file_name().unwrap().to_string_lossy().into_owned();
    assert!(name.ends_with("-build"), "{name}");
    let root_name = w.root.file_name().unwrap();
    assert_eq!(
        std::fs::read(entry.join(root_name).join("build/obj/a.o")).unwrap(),
        b"obj"
    );
    let manifest: serde_json::Value =
        serde_json::from_slice(&std::fs::read(entry.join(TRASH_MANIFEST)).unwrap()).unwrap();
    assert_eq!(manifest["command"], "rm-dir");
    assert_eq!(
        manifest["items"][0]["origin"],
        w.root.join("build").display().to_string()
    );
    // rm-file on a file, into a second entry.
    let (code, _, _) = w.run(Kind::File, &["notes.txt"]);
    assert_eq!(code, 0);
    assert_eq!(w.entries().len(), 2);
}

#[test]
fn purge_deletes_and_dry_run_changes_nothing() {
    let w = world();
    std::fs::write(w.root.join("a"), b"a").unwrap();
    std::fs::create_dir_all(w.root.join("d/e")).unwrap();
    let (code, out, _) = w.run(Kind::File, &["--dry-run", "a"]);
    assert_eq!(code, 0);
    assert!(out.starts_with("would-trash"), "{out}");
    let (_, out, _) = w.run(Kind::Dir, &["--dry-run", "--purge", "d"]);
    assert!(out.starts_with("would-purge"), "{out}");
    assert!(w.root.join("a").exists() && w.root.join("d").exists());
    assert!(w.entries().is_empty());
    let (code, out, _) = w.run(Kind::File, &["--purge", "a"]);
    assert_eq!(code, 0);
    assert!(out.starts_with("purged"), "{out}");
    let (code, _, _) = w.run(Kind::Dir, &["--purge", "d"]);
    assert_eq!(code, 0);
    assert!(!w.root.join("a").exists() && !w.root.join("d").exists());
    assert!(w.entries().is_empty(), "purge never trashes");
}

#[test]
fn wrong_kind_gets_a_hint_and_batches_keep_going() {
    let w = world();
    std::fs::create_dir_all(w.root.join("dir/inner")).unwrap();
    std::fs::write(w.root.join("file"), b"f").unwrap();
    std::fs::write(w.root.join("other"), b"o").unwrap();
    let (code, _, err) = w.run(Kind::File, &["dir", "other"]);
    assert_eq!(code, 1);
    assert!(err.contains("is a directory; use rm-dir"), "{err}");
    assert!(
        !w.root.join("other").exists(),
        "the rest of the batch still runs"
    );
    let (code, _, err) = w.run(Kind::Dir, &["file"]);
    assert_eq!(code, 1);
    assert!(err.contains("use rm-file"), "{err}");
    // `find -exec rm-dir {} +` order: the parent first, then its child.
    let (code, _, err) = w.run(Kind::Dir, &["dir", "dir/inner"]);
    assert_eq!(
        code, 0,
        "a path inside an already removed dir is skipped: {err}"
    );
    // `find -depth` order: the child first, then its parent.
    std::fs::create_dir_all(w.root.join("deep/obj")).unwrap();
    let (code, _, err) = w.run(Kind::Dir, &["deep/obj", "deep"]);
    assert_eq!(code, 0, "the parent takes over its listed child: {err}");
    assert!(!w.root.join("deep").exists());
    let (code, _, err) = w.run(Kind::File, &["missing"]);
    assert_eq!(code, 1);
    assert!(err.contains("no such file"), "{err}");
}

#[test]
fn tracked_paths_need_the_tracked_flag() {
    let w = world();
    let git = |a: &[&str]| crate::worktrees::run_git(&w.root, a).unwrap();
    git(&["init", "-q"]);
    std::fs::create_dir_all(w.root.join("src")).unwrap();
    std::fs::write(w.root.join("src/lib.rs"), b"code").unwrap();
    std::fs::write(w.root.join("scratch.txt"), b"tmp").unwrap();
    git(&["add", "src/lib.rs"]);
    let (code, _, err) = w.run(Kind::File, &["src/lib.rs"]);
    assert_eq!(code, 1);
    assert!(err.contains("git rm"), "{err}");
    let (code, _, err) = w.run(Kind::Dir, &["src"]);
    assert_eq!(code, 1);
    assert!(err.contains("git-tracked"), "{err}");
    assert!(w.root.join("src/lib.rs").exists());
    let (code, _, err) = w.run(Kind::File, &["scratch.txt"]);
    assert_eq!(code, 0, "untracked: {err}");
    let (code, _, err) = w.run(Kind::Dir, &["--tracked", "src"]);
    assert_eq!(code, 0, "{err}");
    assert!(!w.root.join("src").exists());
}

#[test]
fn every_call_writes_one_audit_record() {
    let w = world();
    std::fs::write(w.root.join("a"), b"a").unwrap();
    w.run(Kind::File, &["a", "/definitely/outside"]);
    let records = w.audit_records();
    assert_eq!(records.len(), 1);
    let r = &records[0];
    assert_eq!(r["command"], "rm-file");
    assert_eq!(r["session_id"], "session-1");
    assert_eq!(r["role"], "agent");
    assert_eq!(r["exit"], 1);
    assert_eq!(r["roots"][0], w.root.display().to_string());
    let actions: Vec<&str> = r["paths"]
        .as_array()
        .unwrap()
        .iter()
        .map(|p| p["action"].as_str().unwrap())
        .collect();
    assert_eq!(actions, vec!["refused", "trashed"]);
    assert!(r["paths"][1]["trash_path"].is_string());
}

#[test]
fn rm_trash_entries_expire_after_the_keep_window_and_quarantine_entries_are_not_kept() {
    let w = world();
    std::fs::write(w.root.join("a"), b"a").unwrap();
    w.run(Kind::File, &["a"]);
    let entry = w.entries().pop().unwrap();
    let now = SystemTime::now();
    assert!(keep_trash_entry(&entry, now));
    assert!(expired_trash_entries(&w.trash, now).is_empty());
    let later = now + TRASH_KEEP + Duration::from_secs(60);
    assert!(!keep_trash_entry(&entry, later));
    assert_eq!(expired_trash_entries(&w.trash, later), vec![entry]);
    // A `clud trash` quarantine dir has no manifest: never kept, never swept here.
    let quarantine = w.trash.join("20260101T000000Z-abcdef");
    std::fs::create_dir_all(&quarantine).unwrap();
    assert!(!keep_trash_entry(&quarantine, now));
    assert!(!expired_trash_entries(&w.trash, later).contains(&quarantine));
}

#[test]
fn session_roots_are_the_checkout_and_the_session_temp_dir() {
    let w = world();
    std::fs::create_dir_all(w.root.join(".git")).unwrap();
    std::fs::create_dir_all(w.root.join("sub")).unwrap();
    let value = session_roots_value(&w.root.join("sub")).unwrap();
    let roots: Vec<PathBuf> = std::env::split_paths(&value).collect();
    assert_eq!(roots[0], w.root);
}

#[cfg(unix)]
#[test]
fn cross_volume_copy_keeps_symlinks_as_links() {
    let w = world();
    let src = w.root.join("tree");
    std::fs::create_dir_all(src.join("d")).unwrap();
    std::fs::write(src.join("d/f"), b"f").unwrap();
    std::os::unix::fs::symlink("/etc/hostname", src.join("link")).unwrap();
    let dest = w.home.join("copy");
    copy_nofollow(&src, &dest).unwrap();
    assert_eq!(std::fs::read(dest.join("d/f")).unwrap(), b"f");
    assert!(std::fs::symlink_metadata(dest.join("link"))
        .unwrap()
        .file_type()
        .is_symlink());
}
