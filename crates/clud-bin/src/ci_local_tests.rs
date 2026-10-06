//! Fixtures are real `bosn ci` output captured on 2026-10-06 (#1839).

use super::*;

const REPORT_FAILURE: &str = include_str!("ci_fixtures/report_failure.json");
const REPORT_INCOMPLETE: &str = include_str!("ci_fixtures/report_incomplete.json");
const SHOW_FAILURE: &str = include_str!("ci_fixtures/show_failure.json");
const CLIPPY_STEP: &str = include_str!("ci_fixtures/clippy_step.txt");

fn json(text: &str) -> Value {
    first_json(text).expect("fixture is JSON")
}

#[test]
fn an_incomplete_run_with_every_job_passed_is_a_pass_with_a_note() {
    let verdict = verdict(&json(REPORT_INCOMPLETE));
    assert!(verdict.passed(), "{verdict:?}");
    let line = verdict.line();
    assert!(line.starts_with("PASS: 2/2"), "{line}");
    assert!(line.contains("act cannot run reusable workflows"), "{line}");
}

#[test]
fn a_failed_run_is_a_fail() {
    let verdict = verdict(&json(REPORT_FAILURE));
    assert!(!verdict.passed());
    assert!(
        verdict.line().starts_with("FAIL: 1 job(s) failed"),
        "{}",
        verdict.line()
    );
}

#[test]
fn incomplete_for_any_other_reason_is_not_a_pass() {
    let report = serde_json::json!({
        "conclusion": "incomplete",
        "reason": "runner lost",
        "coverage_complete": false,
        "jobs": {"succeeded": 2, "failed": 0, "cancelled": 0},
    });
    assert!(!verdict(&report).passed());
}

#[test]
fn the_failing_step_is_found_with_its_logs_selector() {
    assert_eq!(
        failed_steps(&json(SHOW_FAILURE)),
        [FailedStep {
            job: "Clippy linux-x64/Build target/x86_64-unknown-linux-gnu".into(),
            section: "Main:4".into(),
            name: "Clippy".into(),
        }]
    );
}

/// The 880-line clippy step reduces to the two real errors and their
/// locations, with none of the soldr-cache / docker noise the hand-written
/// greps tripped over.
#[test]
fn a_clippy_step_log_reduces_to_its_errors() {
    let lines = extract_diagnostics(CLIPPY_STEP);
    assert!(lines.len() <= MAX_DIAGNOSTIC_LINES + 1, "{}", lines.len());
    let text = lines.join("\n");
    assert!(
        text.contains("error[E0063]: missing field `provider_only`"),
        "{text}"
    );
    assert!(text.contains("-->"), "a source location follows: {text}");
    assert!(!text.contains("soldr[cache]"), "{text}");
    assert!(!text.contains('\u{1b}'), "ANSI stripped");
    assert!(!text.contains("could not compile"), "{text}");
}

#[test]
fn test_failures_and_panics_are_kept() {
    let log = "   1.0 running 3 tests\n   1.1 test a::b ... ok\n   1.2 ---- a::c stdout ----\n   1.2 thread 'a::c' panicked at src/a.rs:9:5:\n   1.2 boom\n   1.3 test result: FAILED. 1 passed; 1 failed\n";
    let text = extract_diagnostics(log).join("\n");
    assert!(text.contains("panicked at src/a.rs:9:5"), "{text}");
    assert!(text.contains("---- a::c stdout ----"), "{text}");
    assert!(!text.contains("test a::b ... ok"), "{text}");
}

#[test]
fn ruff_findings_are_kept() {
    let log =
        "RUF012 Mutable default value for class attribute\n --> tests/x.py:107:28\nAll good\n";
    let text = extract_diagnostics(log).join("\n");
    assert!(text.contains("RUF012"), "{text}");
    assert!(text.contains("tests/x.py:107:28"), "{text}");
}

#[test]
fn an_unrecognised_failure_falls_back_to_the_log_tail() {
    let log = (1..=40).map(|n| format!("line {n}\n")).collect::<String>();
    let lines = extract_diagnostics(&log);
    assert_eq!(lines.first().map(String::as_str), Some("line 26"));
    assert_eq!(lines.last().map(String::as_str), Some("line 40"));
}

#[test]
fn filter_returns_matching_clean_lines() {
    let log = "  12.3 \u{1b}[1mtest openrouter_free::tests::x ... ok\u{1b}[0m\n  12.4 test other ... ok\n";
    let pattern = regex::Regex::new("openrouter_free::").unwrap();
    assert_eq!(
        filter_lines(log, &pattern),
        ["test openrouter_free::tests::x ... ok"]
    );
}

#[test]
fn a_stale_bosn_daemon_yields_the_fix_command() {
    let stderr = "bosn ci: the bosn daemon for /home/u/.local/state/bosn is bosn 0.1.13, but this client is bosn 0.1.15; a daemon from another release can misread this client's requests. Stop it with `bosn daemon stop --state-dir /home/u/.local/state/bosn` (this also cancels any job it is running for another session), then retry";
    assert_eq!(
        stale_daemon_fix(stderr).as_deref(),
        Some("bosn daemon stop --state-dir /home/u/.local/state/bosn")
    );
    assert_eq!(stale_daemon_fix("all fine"), None);
}

#[test]
fn strip_ansi_removes_color_codes() {
    assert_eq!(
        strip_ansi("\u{1b}[1m\u{1b}[91merror\u{1b}[0m: x"),
        "error: x"
    );
}

/// Docker cleanup timing out after every job passed (captured 2026-10-06).
#[test]
fn an_engine_error_after_full_coverage_is_a_pass_with_a_note() {
    let report = serde_json::json!({
        "conclusion": "error",
        "reason": "engine cleanup failed: Docker CLI exceeded its deadline",
        "coverage_complete": true,
        "jobs": {"succeeded": 2, "failed": 0, "cancelled": 0},
    });
    let verdict = verdict(&report);
    assert!(verdict.passed(), "{verdict:?}");
    assert!(
        verdict.line().contains("engine cleanup failed"),
        "{}",
        verdict.line()
    );
}

/// A passing test that panics on purpose must not crowd out the real failure.
#[test]
fn only_the_failures_section_is_used_when_libtest_prints_one() {
    let log = "test pty::raw_pump ... ok\nthread 'pty::raw_pump' panicked at pty.rs:588:13:\ndeliberate panic\n\nfailures:\n\n---- tools::tests::x stdout ----\nthread 'tools::tests::x' panicked at src/tools.rs:420:17:\nreal failure\n";
    let text = extract_diagnostics(log).join("\n");
    assert!(text.contains("src/tools.rs:420:17"), "{text}");
    assert!(!text.contains("deliberate panic"), "{text}");
}
