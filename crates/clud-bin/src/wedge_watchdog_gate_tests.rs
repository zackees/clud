use super::*;
use std::collections::HashMap;

const WINDOW: Duration = Duration::from_secs(10);

/// 10 s window in 100 ns ticks — one full core for the whole window.
const ONE_CORE: u64 = 100_000_000;

fn map(pairs: &[(u32, u64)]) -> HashMap<u32, u64> {
    pairs.iter().copied().collect()
}

#[test]
fn an_idle_subtree_does_not_arm_the_thread_walk() {
    // Every process used ~1% of a core. No thread inside them can be at
    // 90%, so the host-wide enumeration is pure waste.
    let prev = map(&[(10, 0), (11, 0), (12, 0)]);
    let cur = map(&[
        (10, ONE_CORE / 100),
        (11, ONE_CORE / 100),
        (12, ONE_CORE / 100),
    ]);
    assert!(!subtree_could_hide_a_hot_thread(
        &prev,
        &cur,
        Some(WINDOW),
        GATE_USER_PCT_THRESHOLD
    ));
}

#[test]
fn a_pinned_process_arms_the_thread_walk() {
    let prev = map(&[(10, 0), (11, 0)]);
    let cur = map(&[(10, ONE_CORE / 100), (11, ONE_CORE)]);
    assert!(subtree_could_hide_a_hot_thread(
        &prev,
        &cur,
        Some(WINDOW),
        GATE_USER_PCT_THRESHOLD
    ));
}

/// The gate must never be the reason a real wedge goes unseen. The
/// detector's own threshold is 90% of a core; the gate arms below that, so
/// a process sitting exactly at the detection threshold always gets
/// measured.
#[test]
fn the_gate_arms_below_the_detectors_own_threshold() {
    let prev = map(&[(10, 0)]);
    let at_detector_threshold = map(&[(10, (ONE_CORE as f64 * DEFAULT_USER_PCT_THRESHOLD) as u64)]);
    assert!(subtree_could_hide_a_hot_thread(
        &prev,
        &at_detector_threshold,
        Some(WINDOW),
        GATE_USER_PCT_THRESHOLD
    ));
}

#[test]
fn the_gate_fails_open_when_it_cannot_answer() {
    let populated = map(&[(10, ONE_CORE)]);
    // No wall delta yet (first tick).
    assert!(subtree_could_hide_a_hot_thread(
        &populated,
        &populated,
        None,
        GATE_USER_PCT_THRESHOLD
    ));
    // No baseline.
    assert!(subtree_could_hide_a_hot_thread(
        &HashMap::new(),
        &populated,
        Some(WINDOW),
        GATE_USER_PCT_THRESHOLD
    ));
    // Nothing readable this tick.
    assert!(subtree_could_hide_a_hot_thread(
        &populated,
        &HashMap::new(),
        Some(WINDOW),
        GATE_USER_PCT_THRESHOLD
    ));
    // Zero-width window — a division that would otherwise be meaningless.
    assert!(subtree_could_hide_a_hot_thread(
        &populated,
        &populated,
        Some(Duration::ZERO),
        GATE_USER_PCT_THRESHOLD
    ));
}

/// A pid seen for the first time this tick has no baseline, so its delta
/// is unknowable — it must arm the walk rather than be treated as quiet.
#[test]
fn a_process_new_this_tick_arms_the_walk() {
    let prev = map(&[(10, 0)]);
    let cur = map(&[(10, 0), (99, 0)]);
    assert!(subtree_could_hide_a_hot_thread(
        &prev,
        &cur,
        Some(WINDOW),
        GATE_USER_PCT_THRESHOLD
    ));
}

/// A gated-out window must reach the detector as an explicit *healthy*
/// observation, not as "no observation".
///
/// This pins the regression the gate nearly introduced. The loop does
/// `let Some(tick) = sampler.tick(..) else { continue }`, so if a cool
/// window returned `None` the streak would simply not advance — and would
/// not reset either. Eight hot windows, an idle stretch of any length,
/// then one more hot window would reach `Wedged` on nine *non-consecutive*
/// windows, which is exactly what the streak exists to prevent.
#[test]
fn an_idle_stretch_between_hot_windows_must_break_the_streak() {
    let window = Duration::from_secs(10);
    let hot = Sample {
        hottest_thread_user_delta: Duration::from_millis(9_500),
        wall_delta: window,
        io_write_delta: 0,
    };
    // What the gated path now emits.
    let gated_cool = Sample {
        hottest_thread_user_delta: Duration::ZERO,
        wall_delta: window,
        io_write_delta: 0,
    };

    let mut detector = WedgeDetector::new(WedgeDetectorCfg::default());
    for _ in 0..(DEFAULT_REQUIRED_STREAK - 1) {
        detector.observe(hot);
    }
    assert!(matches!(detector.observe(gated_cool), WedgeState::Healthy));
    assert!(
        matches!(detector.observe(hot), WedgeState::Suspect { streak: 1 }),
        "a cool window must restart the streak from scratch"
    );
}

// ─── descendants_of ────────────────────────────────────────────────

fn tree(pairs: &[(u32, &[u32])]) -> HashMap<u32, Vec<u32>> {
    pairs.iter().map(|(p, k)| (*p, k.to_vec())).collect()
}

#[test]
fn a_plain_tree_yields_every_descendant_once() {
    let children = tree(&[(1, &[2, 3]), (2, &[4]), (3, &[5])]);
    let mut got = descendants_of(&children, 1);
    got.sort_unstable();
    assert_eq!(got, vec![1, 2, 3, 4, 5]);
}

#[test]
fn a_root_with_no_children_is_just_itself() {
    assert_eq!(descendants_of(&HashMap::new(), 42), vec![42]);
}

/// The regression this walk's visited set exists for. Windows recycles
/// PIDs, so a Toolhelp parent-pid graph can name a descendant as its own
/// ancestor. Before #709 this looped forever, growing the output vector
/// until the process died — a hang, not a wrong answer.
#[test]
fn a_pid_reuse_cycle_terminates_instead_of_hanging() {
    let children = tree(&[(1, &[2]), (2, &[3]), (3, &[1])]);
    let mut got = descendants_of(&children, 1);
    got.sort_unstable();
    assert_eq!(got, vec![1, 2, 3], "each pid must appear exactly once");
}

#[test]
fn a_self_parented_pid_terminates() {
    let children = tree(&[(7, &[7])]);
    assert_eq!(descendants_of(&children, 7), vec![7]);
}

/// A diamond cannot occur in a real parent-pid graph (each pid has one
/// parent), but the walk must not double-count if the snapshot is torn.
#[test]
fn a_diamond_does_not_double_count() {
    let children = tree(&[(1, &[2, 3]), (2, &[4]), (3, &[4])]);
    let mut got = descendants_of(&children, 1);
    got.sort_unstable();
    assert_eq!(got, vec![1, 2, 3, 4]);
}
