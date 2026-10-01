//! Decision table for the repeated-identical-call guard (#1674).

use super::*;
use serde_json::json;

const N: u32 = DEFAULT_LIMIT;

/// Feed `keys` through the decision table, `gap` seconds apart, and return
/// the 1-based indices of the denied calls.
fn denied(keys: &[&str], gap: u64, limit: u32) -> Vec<usize> {
    let mut state: Option<Streak> = None;
    let mut out = Vec::new();
    for (index, key) in keys.iter().enumerate() {
        let (verdict, next) = decide(state.as_ref(), key, 1_000 + gap * index as u64, limit);
        if verdict != Verdict::Allow {
            out.push(index + 1);
        }
        state = Some(next);
    }
    out
}

#[test]
fn incident_294_identical_calls_are_denied_from_call_n_plus_one() {
    let keys = ["a"; 294];
    let expected: Vec<usize> = (N as usize + 1..=294).collect();
    assert_eq!(denied(&keys, 0, N), expected);
}

#[test]
fn incident_371_identical_calls_are_denied_from_call_n_plus_one() {
    let keys = ["a"; 371];
    assert_eq!(denied(&keys, 0, N).first(), Some(&(N as usize + 1)));
    assert_eq!(denied(&keys, 0, N).len(), 371 - N as usize);
}

#[test]
fn exactly_n_identical_calls_are_all_allowed() {
    assert!(denied(&["a"; N as usize], 0, N).is_empty());
}

#[test]
fn polling_loop_below_the_limit_is_never_denied() {
    // The longest sleep-paced poll seen in local transcripts was 138 calls.
    assert!(denied(&["poll"; 138], 2, N).is_empty());
}

#[test]
fn a_different_call_in_between_resets_the_streak() {
    let mut keys = vec!["a"; N as usize];
    keys.push("b");
    keys.extend(["a"; N as usize]);
    assert!(denied(&keys, 0, N).is_empty());
}

#[test]
fn interleaved_calls_never_build_a_streak() {
    let keys: Vec<&str> = (0..1_000).map(|i| if i % 2 == 0 { "a" } else { "b" }).collect();
    assert!(denied(&keys, 0, N).is_empty());
}

#[test]
fn an_idle_gap_longer_than_the_reset_window_starts_a_new_streak() {
    // A `/loop` tick every 10 minutes repeats one call indefinitely.
    assert!(denied(&["tick"; 1_000], IDLE_RESET_SECS + 1, N).is_empty());
    // At the window it still counts.
    assert_eq!(denied(&["a"; N as usize + 1], IDLE_RESET_SECS, N), vec![N as usize + 1]);
}

#[test]
fn a_denied_call_keeps_counting_so_a_blind_retry_stays_denied() {
    assert_eq!(denied(&["a"; 5], 0, 3), vec![4, 5]);
}

#[test]
fn limit_zero_disables_the_guard() {
    assert!(denied(&["a"; 1_000], 0, 0).is_empty());
}

#[test]
fn call_key_is_tool_scoped_and_ignores_object_key_order() {
    let one = json!({"command": "gh pr checks", "timeout": 5});
    let two: Value = serde_json::from_str(r#"{"timeout":5,"command":"gh pr checks"}"#).unwrap();
    assert_eq!(call_key("Bash", Some(&one)), call_key("Bash", Some(&two)));
    assert_ne!(call_key("Bash", Some(&one)), call_key("PowerShell", Some(&one)));
    assert_ne!(
        call_key("Bash", Some(&one)),
        call_key("Bash", Some(&json!({"command": "gh pr checks 1"})))
    );
    assert_eq!(call_key("Bash", Some(&one)).len(), 16);
}

#[test]
fn limit_resolution_env_then_settings_then_default() {
    let settings = json!({"hooks": {"repeat_call_limit": 40}});
    assert_eq!(limit_from(Some("7"), Some(&settings)), 7);
    assert_eq!(limit_from(Some("0"), Some(&settings)), 0);
    assert_eq!(limit_from(None, Some(&settings)), 40);
    assert_eq!(limit_from(Some("junk"), None), DEFAULT_LIMIT);
    let bad = json!({"hooks": {"repeat_call_limit": "x"}});
    assert_eq!(limit_from(None, Some(&bad)), DEFAULT_LIMIT);
    assert_eq!(limit_from(None, None), DEFAULT_LIMIT);
}

#[test]
fn opt_out_token_is_whole_word() {
    assert!(opts_out("CLUD_ALLOW_REPEAT=1 gh pr checks"));
    assert!(opts_out("export CLUD_ALLOW_REPEAT=1; ls"));
    assert!(!opts_out("XCLUD_ALLOW_REPEAT=1 ls"));
    assert!(!opts_out("CLUD_ALLOW_REPEAT=10 ls"));
    assert!(!opts_out("ls"));
}

#[test]
fn check_at_persists_only_hashes_and_counts_per_session() {
    let tmp = tempfile::tempdir().unwrap();
    for i in 0..3 {
        assert_eq!(check_at(tmp.path(), "sess-1", "k", 10 + i, 3), Verdict::Allow);
    }
    assert_eq!(check_at(tmp.path(), "sess-1", "k", 14, 3), Verdict::Deny { count: 4 });
    // Another session has its own streak.
    assert_eq!(check_at(tmp.path(), "sess-2", "k", 14, 3), Verdict::Allow);
    let dir = tmp.path().join(DIR_NAME);
    let files: Vec<_> = std::fs::read_dir(&dir).unwrap().flatten().collect();
    assert_eq!(files.len(), 2);
    for file in files {
        let text = std::fs::read_to_string(file.path()).unwrap();
        assert!(!text.contains("sess-"), "session id must not be stored");
        let streak: Streak = serde_json::from_str(&text).unwrap();
        assert_eq!(streak.key, "k");
    }
}

#[test]
fn state_failures_fail_open() {
    let tmp = tempfile::tempdir().unwrap();
    // A file where the state directory should be: writes fail, calls allow.
    let blocked = tmp.path().join("state");
    std::fs::write(&blocked, b"not a dir").unwrap();
    for i in 0..10 {
        assert_eq!(check_at(&blocked, "s", "k", i, 2), Verdict::Allow);
    }
    // A corrupt streak file is treated as no streak.
    let ok = tmp.path().join("ok");
    let path = session_file(&ok, "s");
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(&path, b"{garbage").unwrap();
    assert_eq!(check_at(&ok, "s", "k", 1, 1), Verdict::Allow);
    // No session id: allow.
    assert_eq!(check_at(&ok, "", "k", 1, 0), Verdict::Allow);
}

#[test]
fn deny_message_tells_the_model_to_stop_and_names_the_override() {
    let message = deny_message("Bash", 201, 200);
    assert!(message.contains("Stop repeating"));
    assert!(message.contains("fresh"));
    assert!(message.contains(ALLOW_REPEAT_TOKEN));
}
