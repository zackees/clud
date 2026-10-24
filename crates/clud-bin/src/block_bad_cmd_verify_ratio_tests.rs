use super::*;

#[test]
fn calls_are_classified_by_tool_and_command() {
    assert_eq!(classify("Edit", ""), Some(Class::Edit));
    assert_eq!(classify("Write", ""), Some(Class::Edit));
    assert_eq!(
        classify("Bash", "soldr cargo clippy --workspace --all-targets"),
        Some(Class::Verify)
    );
    assert_eq!(
        classify("Bash", "cargo test --release -p core"),
        Some(Class::Verify)
    );
    assert_eq!(
        classify("Bash", "bosn ci run --workspace . --wait"),
        Some(Class::Verify)
    );
    assert_eq!(
        classify("Bash", "sed -i 's/a/b/' x.rs && cargo build"),
        Some(Class::Edit)
    );
    assert_eq!(classify("Bash", "git status"), None);
    assert_eq!(classify("Read", ""), None);
}

fn run(ledger: &mut Ledger, classes: &str, ratio: u32) -> Vec<Counts> {
    let mut notes = Vec::new();
    for letter in classes.chars() {
        let class = if letter == 'e' {
            Class::Edit
        } else {
            Class::Verify
        };
        let (next, note) = decide(ledger, class, ratio);
        *ledger = next;
        notes.extend(note);
    }
    notes
}

#[test]
fn verification_crowding_out_edits_trips_one_note_then_stays_quiet() {
    let mut ledger = Ledger::default();
    // #1988's session: one edit, then the whole gate, over and over.
    let notes = run(&mut ledger, "evvvvevvvvevv", DEFAULT_RATIO);
    assert_eq!(
        notes,
        vec![Counts {
            edits: 3,
            verifies: 10
        }]
    );
    // The same pattern right after the note is not repeated within a window.
    assert!(run(&mut ledger, "vvvvvvvvvvvvvvvvvvvvvvvvvvvvv", DEFAULT_RATIO).is_empty());
    assert_eq!(run(&mut ledger, "v", DEFAULT_RATIO).len(), 1);
}

#[test]
fn a_balanced_loop_a_short_session_or_ratio_zero_never_notes() {
    let mut ledger = Ledger::default();
    assert!(run(&mut ledger, &"evv".repeat(20), DEFAULT_RATIO).is_empty());
    let mut ledger = Ledger::default();
    assert!(run(&mut ledger, "vvvvvvvvv", DEFAULT_RATIO).is_empty());
    let mut ledger = Ledger::default();
    assert!(run(&mut ledger, &"v".repeat(60), 0).is_empty());
}

#[test]
fn the_window_keeps_only_the_newest_calls() {
    let mut ledger = Ledger::default();
    run(&mut ledger, &"e".repeat(40), DEFAULT_RATIO);
    assert_eq!(ledger.window.len(), WINDOW);
}

#[test]
fn the_ratio_comes_from_env_then_settings_then_default() {
    let settings = serde_json::json!({"hooks": {"verify_edit_ratio": 5}});
    assert_eq!(ratio_from(Some("0"), Some(&settings)), 0);
    assert_eq!(ratio_from(None, Some(&settings)), 5);
    assert_eq!(ratio_from(Some("junk"), None), DEFAULT_RATIO);
}

#[test]
fn state_round_trips_per_session_and_fails_open_without_one() {
    let dir = tempfile::tempdir().unwrap();
    for _ in 0..MIN_VERIFY - 1 {
        assert_eq!(observe_at(dir.path(), "s1", Class::Verify, 3), None);
    }
    assert!(observe_at(dir.path(), "s1", Class::Verify, 3).is_some());
    assert_eq!(observe_at(dir.path(), "s2", Class::Verify, 3), None);
    assert_eq!(observe_at(dir.path(), "", Class::Verify, 3), None);
}

#[test]
fn the_note_is_context_not_a_permission_decision() {
    let json = context_json(&note_text(
        Counts {
            edits: 1,
            verifies: 12,
        },
        3,
    ));
    let output = &json["hookSpecificOutput"];
    assert!(output.get("permissionDecision").is_none());
    assert!(output["additionalContext"]
        .as_str()
        .unwrap()
        .contains("12 lint/build/test/CI calls"));
}
