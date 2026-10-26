use super::*;

const NOW: u64 = 10_000_000;
const HOUR: u64 = 3600;

fn generation(name: &str) -> Generation {
    Generation {
        repo_id: "repo".to_string(),
        workspace_id: "ws".to_string(),
        stack: "rust".to_string(),
        role: "target".to_string(),
        scope_digest: name.to_string(),
        last_used: NOW,
        size_bytes: 100,
        leased: false,
        newest: false,
        workspace_done: false,
        live_cwd_activity: None,
    }
}

fn idle(mut g: Generation, hours: u64) -> Generation {
    g.last_used = NOW - hours * HOUR;
    g
}

fn eligible(g: &Generation) -> bool {
    matches!(
        age_verdict(g, NOW, &Policy::default()),
        Verdict::Eligible(_)
    )
}

#[test]
fn a_lease_protects_whatever_the_age_or_done_state() {
    let mut g = idle(generation("a"), 1000);
    g.leased = true;
    g.workspace_done = true;
    assert_eq!(tier(&g), Tier::Leased);
    assert_eq!(
        age_verdict(&g, NOW, &Policy::default()),
        Verdict::Protected("leased".to_string())
    );
}

#[test]
fn superseded_generations_expire_after_24h_since_last_use() {
    assert!(!eligible(&idle(generation("a"), 23)));
    assert!(eligible(&idle(generation("a"), 24)));
}

#[test]
fn an_aged_generation_is_eligible_the_moment_its_last_lease_ends() {
    let mut g = idle(generation("a"), 48);
    g.leased = true;
    assert!(
        !eligible(&g),
        "a long build is never collected while leased"
    );
    g.leased = false;
    assert!(eligible(&g));
}

#[test]
fn the_newest_generation_stays_warm_for_two_weeks() {
    let mut g = idle(generation("a"), 13 * 24);
    g.newest = true;
    assert_eq!(tier(&g), Tier::Warm);
    assert!(
        !eligible(&g),
        "a warm cache outlives the superseded 24h cap"
    );
    assert!(eligible(&idle(g, 14 * 24)));
}

#[test]
fn a_done_workspace_is_eligible_at_once() {
    let mut g = generation("a");
    g.newest = true;
    g.workspace_done = true;
    assert_eq!(tier(&g), Tier::Done);
    assert!(eligible(&g));
}

#[test]
fn live_cwd_protection_expires_with_session_activity() {
    let mut g = idle(generation("a"), 30 * 24);
    g.newest = true;
    g.live_cwd_activity = Some(NOW - HOUR);
    assert!(matches!(
        age_verdict(&g, NOW, &Policy::default()),
        Verdict::Protected(_)
    ));
    g.live_cwd_activity = Some(NOW - 25 * HOUR);
    assert!(eligible(&g), "an idle forgotten terminal stops protecting");
}

#[test]
fn eviction_order_is_done_then_superseded_then_warm() {
    let mut done = idle(generation("done"), 1);
    done.workspace_done = true;
    let old_superseded = idle(generation("old"), 5);
    let new_superseded = idle(generation("new"), 2);
    let mut warm = idle(generation("warm"), 100);
    warm.newest = true;
    let mut leased = idle(generation("leased"), 1000);
    leased.leased = true;
    let mut pinned = idle(generation("pinned"), 1000);
    pinned.newest = true;
    pinned.live_cwd_activity = Some(NOW);
    let all = [warm, new_superseded, leased, done, pinned, old_superseded];
    let names: Vec<&str> = eviction_order(&all, NOW, &Policy::default())
        .into_iter()
        .map(|i| all[i].scope_digest.as_str())
        .collect();
    assert_eq!(names, ["done", "old", "new", "warm"]);
}

#[test]
fn equal_age_ties_go_to_the_largest_first() {
    let mut small = idle(generation("small"), 30);
    small.size_bytes = 10;
    let mut large = idle(generation("large"), 30);
    large.size_bytes = 1000;
    let all = [small, large];
    assert_eq!(eviction_order(&all, NOW, &Policy::default()), [1, 0]);
}

#[test]
fn count_pressure_trims_each_repo_stack_to_its_limit() {
    let mut warm = idle(generation("warm"), 1);
    warm.newest = true;
    let mut other_stack = idle(generation("node"), 50);
    other_stack.stack = "node".to_string();
    let all = [
        warm,
        idle(generation("s1"), 3),
        idle(generation("s2"), 2),
        other_stack,
    ];
    let evicted = relieve_count(&all, NOW, &Policy::default(), 2);
    assert_eq!(
        evicted,
        [1],
        "rust holds 3 of 2: drop its oldest superseded only"
    );
}

#[test]
fn byte_pressure_evicts_down_the_order_until_the_deficit_is_freed() {
    let mut warm = idle(generation("warm"), 1);
    warm.newest = true;
    let all = [warm, idle(generation("s1"), 3), idle(generation("s2"), 2)];
    let relief = relieve_bytes(&all, NOW, &Policy::default(), 150);
    assert_eq!(relief.evict, [1, 2]);
    assert_eq!(relief.freed_bytes, 200);
    assert_eq!(relief.compaction_advisory, None);
}

#[test]
fn byte_pressure_beyond_reclaimable_stops_and_advises_compaction() {
    let mut leased = generation("leased");
    leased.leased = true;
    leased.size_bytes = 10_000;
    let all = [leased, idle(generation("s1"), 3)];
    let relief = relieve_bytes(&all, NOW, &Policy::default(), 500);
    assert!(
        relief.evict.is_empty(),
        "no grinding against slack GC cannot move"
    );
    assert!(relief
        .compaction_advisory
        .as_deref()
        .is_some_and(|advice| advice.contains("only 100 are reclaimable")));
}
