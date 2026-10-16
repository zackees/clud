use super::*;

fn write(path: &Path, len: usize) {
    std::fs::write(path, vec![b'x'; len]).unwrap();
}

#[test]
fn current_aliases_are_clean_and_stale_or_retired_ones_are_reported() {
    let dir = tempfile::tempdir().unwrap();
    let reference = dir.path().join("clud-ref");
    write(&reference, 10);
    let own = fingerprint(&reference).unwrap();
    let fresh = dir.path().join(crate::shim_registry::file_name("fresh"));
    write(&fresh, 10);
    let stale = dir.path().join(crate::shim_registry::file_name("stale"));
    write(&stale, 7);
    let retired = dir.path().join(crate::shim_registry::file_name("old-alias"));
    write(&retired, 10);
    let names = ["fresh", "stale", "absent"].map(String::from).to_vec();
    let findings = inspect_dir(dir.path(), &names, &["old-alias", "older-alias"], &own);
    assert_eq!(
        findings,
        vec![
            Finding::Stale {
                path: stale,
                found: format!("{}+7", env!("CARGO_PKG_VERSION")),
            },
            Finding::Retired { path: retired },
        ]
    );
}

#[test]
fn findings_read_as_one_line_each() {
    let line = Finding::Retired {
        path: PathBuf::from("/h/.clud/state/shims/old-alias"),
    }
    .to_string();
    assert_eq!(line, "retired alias /h/.clud/state/shims/old-alias remains");
}
