//! Issue #1610: decision table for the launch-banner tmp-wt size warning and
//! the cache round trip. Every test works under a `tempdir`; none touches the
//! real `~/.clud`.

use super::*;
use tempfile::tempdir;

const GIB: u64 = 1 << 30;
const NOW: i64 = 1_800_000_000;

fn cached(check: SizeCheck, age_secs: i64) -> CachedTreeSize {
    CachedTreeSize {
        checked_unix: NOW - age_secs,
        warn_bytes: 50 * GIB,
        check,
    }
}

/// One row per reason: (cache, warn_bytes, expected verdict).
#[test]
fn banner_decision_table() {
    let over = cached(SizeCheck::Over(60 * GIB), 60);
    let rows: Vec<(&str, Option<CachedTreeSize>, u64, BannerVerdict)> = vec![
        (
            "over threshold, fresh",
            Some(over),
            50 * GIB,
            BannerVerdict::Warn { bytes: 60 * GIB },
        ),
        (
            "disabled wins even when over",
            Some(over),
            0,
            BannerVerdict::Skip(SkipReason::Disabled),
        ),
        (
            "no cache shows nothing",
            None,
            50 * GIB,
            BannerVerdict::Skip(SkipReason::NoCache),
        ),
        (
            "stale cache shows nothing",
            Some(cached(SizeCheck::Over(60 * GIB), MAX_CACHE_AGE_SECS + 1)),
            50 * GIB,
            BannerVerdict::Skip(SkipReason::Stale),
        ),
        (
            "exactly at max age is still fresh",
            Some(cached(SizeCheck::Over(60 * GIB), MAX_CACHE_AGE_SECS)),
            50 * GIB,
            BannerVerdict::Warn { bytes: 60 * GIB },
        ),
        (
            "timestamp from the future is not trusted",
            Some(cached(SizeCheck::Over(60 * GIB), -(FUTURE_SKEW_SECS + 1))),
            50 * GIB,
            BannerVerdict::Skip(SkipReason::FutureTimestamp),
        ),
        (
            "under threshold",
            Some(cached(SizeCheck::Under(10 * GIB), 60)),
            50 * GIB,
            BannerVerdict::Skip(SkipReason::Under),
        ),
        (
            "threshold raised past the cached lower bound",
            Some(over),
            100 * GIB,
            BannerVerdict::Skip(SkipReason::Under),
        ),
        (
            "threshold lowered below an exact under total",
            Some(cached(SizeCheck::Under(10 * GIB), 60)),
            5 * GIB,
            BannerVerdict::Warn { bytes: 10 * GIB },
        ),
        (
            "budget exhausted: unproven, stay quiet",
            Some(cached(SizeCheck::Unknown, 60)),
            50 * GIB,
            BannerVerdict::Skip(SkipReason::Unknown),
        ),
    ];
    for (name, cache, warn, expected) in rows {
        assert_eq!(
            banner_decision(cache.as_ref(), warn, NOW),
            expected,
            "row: {name}"
        );
    }
}

#[test]
fn banner_line_names_size_threshold_path_and_setting() {
    let root = Path::new("/h/.clud/tmp-wt");
    let line = banner_line(root, 60 * GIB, 50 * GIB);
    assert!(!line.contains('\n'), "one line: {line}");
    assert!(line.contains("60.0 GiB"), "{line}");
    assert!(line.contains("50.0 GiB"), "{line}");
    assert!(line.contains(&root.display().to_string()), "{line}");
    assert!(line.contains("worktrees.warn_bytes"), "{line}");
}

#[test]
fn cache_round_trips_and_lives_beside_tmp_wt() {
    let home = tempdir().unwrap();
    let root = crate::gc::worktree_root::worktree_root_for(home.path());
    let path = cache_path_for(&root);
    assert_eq!(path.parent(), root.parent());
    for check in [SizeCheck::Over(7), SizeCheck::Under(3), SizeCheck::Unknown] {
        let entry = cached(check, 0);
        write_cache(&path, &entry).unwrap();
        assert_eq!(read_cache(&path), Some(entry));
    }
}

#[test]
fn missing_or_corrupt_cache_reads_as_none() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("tmp-wt-size.json");
    assert_eq!(read_cache(&path), None);
    std::fs::write(&path, b"{not json").unwrap();
    assert_eq!(read_cache(&path), None);
    std::fs::write(&path, br#"{"checked_unix":1,"state":"weird","bytes":1}"#).unwrap();
    assert_eq!(read_cache(&path), None);
}

#[test]
fn launch_warning_reads_cache_only_and_is_failure_silent() {
    let home = tempdir().unwrap();
    let root = crate::gc::worktree_root::worktree_root_for(home.path());
    // No cache, no settings, no tmp-wt dir: nothing, no panic.
    assert_eq!(launch_warning_at(home.path(), NOW), None);
    write_cache(
        &cache_path_for(&root),
        &cached(SizeCheck::Over(60 * GIB), 60),
    )
    .unwrap();
    let line = launch_warning_at(home.path(), NOW).expect("fresh over cache warns");
    assert!(line.contains("60.0 GiB"), "{line}");
    // warn_bytes = 0 in settings silences it.
    let settings = crate::clud_settings::settings_path_at(home.path());
    std::fs::write(&settings, br#"{"worktrees":{"warn_bytes":0}}"#).unwrap();
    assert_eq!(launch_warning_at(home.path(), NOW), None);
}

#[test]
fn refresh_cache_records_a_bounded_check() {
    let home = tempdir().unwrap();
    let root = crate::gc::worktree_root::worktree_root_for(home.path());
    std::fs::create_dir_all(&root).unwrap();
    std::fs::write(root.join("blob"), vec![0u8; 200]).unwrap();
    refresh_cache(&root, 100, NOW).unwrap();
    let entry = read_cache(&cache_path_for(&root)).unwrap();
    assert_eq!(entry.checked_unix, NOW);
    assert_eq!(entry.warn_bytes, 100);
    assert!(matches!(entry.check, SizeCheck::Over(b) if b > 100));
}
