use super::*;

fn act(name: &str, binds: &[&str]) -> ActContainer {
    ActContainer {
        name: name.to_string(),
        binds: binds.iter().map(PathBuf::from).collect(),
    }
}

#[test]
fn only_act_containers_bound_to_a_missing_clud_worktree_are_orphaned() {
    let roots = [
        PathBuf::from("/h/.clud/worktrees"),
        PathBuf::from("/h/.clud/tmp-wt"),
    ];
    let live = PathBuf::from("/h/.clud/worktrees/repo-wt-live");
    let exists = |p: &Path| p == live.as_path() || p.starts_with("/h/dev");
    let containers = [
        act(
            "act-CI-gone",
            &["/h/.clud/tmp-wt/zccache-wt-1909", "/var/run/docker.sock"],
        ),
        act("act-CI-live", &["/h/.clud/worktrees/repo-wt-live"]),
        act("act-CI-user", &["/h/dev/zccache"]),
        act("act-CI-user-gone", &["/h/dev/missing"]),
        act("postgres", &["/h/.clud/worktrees/repo-wt-gone"]),
        act("act-root-itself", &["/h/.clud/worktrees"]),
    ];
    assert_eq!(
        orphaned(&containers, &roots, &exists),
        vec!["act-CI-gone".to_string()]
    );
}

#[test]
fn inspect_output_parses_names_and_bind_sources_only() {
    let out = "/act-CI-x\t[{\"Type\":\"bind\",\"Source\":\"/h/.clud/tmp-wt/a\"},{\"Type\":\"volume\",\"Source\":\"/var/lib/docker/volumes/v\"}]\n";
    assert_eq!(
        parse_inspect(out),
        Some(vec![act("act-CI-x", &["/h/.clud/tmp-wt/a"])])
    );
}

#[test]
fn malformed_inspect_output_removes_nothing() {
    assert_eq!(parse_inspect("act-x no-tab"), None);
    assert_eq!(parse_inspect("act-x\tnot-json"), None);
}
