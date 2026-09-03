//! Guard: the self-invoking fixtures in this target must name tests that exist.
//!
//! Several tests here re-invoke the test binary with `--ignored --exact <id>`
//! to build a controlled process tree. libtest matches `--exact` against the
//! **module-qualified** id, and #1056 changed every id in this target from
//! `<fn>` to `<module>::<fn>` when it folded seven top-level test files into
//! modules of one executable.
//!
//! The call sites kept the pre-consolidation bare names. `--exact` then
//! matched nothing, so the child ran zero tests, exited 0, and never wrote the
//! pid file the parent was waiting on. The parent failed 30 seconds later with
//! `NotFound` on that file -- which reads as a spawn, Job-object or
//! containment failure and says nothing about a test name. Both Windows reaper
//! tests failed exactly that way on every run, on `main` and on every PR.
//!
//! This asserts the prefix without running anything: the surrounding target is
//! full of tests that scan for tagged descendants, so a guard that spawned a
//! child to ask libtest for its own list would be polluting the very thing
//! those tests measure.

/// The module-qualified prefix libtest uses for tests declared in `module`.
///
/// `module_path!()` is crate-rooted (`reaper::<module>`); libtest omits that
/// crate segment. Strip one leading segment when there is one, so this stays
/// correct whether or not the target is renamed.
pub fn libtest_module_prefix(module_path: &str) -> &str {
    module_path
        .split_once("::")
        .map_or(module_path, |(_crate, rest)| rest)
}

#[test]
fn the_prefix_helper_drops_only_the_crate_segment() {
    assert_eq!(
        libtest_module_prefix("reaper::tool_shell_lifecycle_windows"),
        "tool_shell_lifecycle_windows"
    );
    // Nested modules keep every segment below the crate.
    assert_eq!(
        libtest_module_prefix("reaper::a::b"),
        "a::b",
        "only the crate segment is libtest-implicit"
    );
    // A bare path has no crate segment to drop; returning "" would silently
    // build ids like `::name`.
    assert_eq!(libtest_module_prefix("solo"), "solo");
}

/// `module_path!()` really is crate-rooted here, which the helper assumes.
///
/// If a toolchain change made it relative, the helper would strip a real
/// module segment and every derived id would be wrong -- so the assumption is
/// asserted rather than trusted.
#[test]
fn module_path_is_crate_rooted_in_this_target() {
    let path = module_path!();
    assert!(
        path.contains("::"),
        "module_path!() = `{path}`, expected a crate-qualified path"
    );
    assert!(
        path.ends_with("fixture_ids"),
        "module_path!() = `{path}`, expected it to end with this module"
    );
}
