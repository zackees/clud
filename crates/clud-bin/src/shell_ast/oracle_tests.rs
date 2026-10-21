//! Recall oracle (#1962): predicted `git`/`gh` calls against what really ran.
//!
//! `oracle_fixtures/shim_oracle.json` pairs real agent Bash commands with the
//! `git`/`gh` calls the shim recorded them running (pseudonymized; see
//! `ci/refresh_shell_ast_oracle.py`). Each recorded call counts as recalled
//! when some invocation the evaluator predicts runs the same tool with the
//! same arguments, an unknown word matching anything (its value only exists
//! at run time). Recall may only go up: the fixture records a floor, and a
//! change that raises recall should raise the floor with it.

use super::*;

const FIXTURE: &str = include_str!("oracle_fixtures/shim_oracle.json");

#[derive(serde::Deserialize)]
struct Fixture {
    floor: f64,
    records: Vec<Record>,
}

#[derive(serde::Deserialize)]
struct Record {
    command: String,
    cwd: String,
    calls: Vec<Call>,
}

#[derive(serde::Deserialize)]
struct Call {
    tool: String,
    argv: Vec<String>,
}

/// The pseudonymized home: `/home/<user>` from the recorded cwd.
fn home_of(cwd: &str) -> Option<String> {
    let user = cwd.strip_prefix("/home/")?.split('/').next()?;
    Some(format!("/home/{user}"))
}

fn base_for(record: &Record) -> BaseEnv {
    let base = BaseEnv::new(record.cwd.clone());
    match home_of(&record.cwd) {
        Some(home) => base.with_var("HOME", home),
        None => base,
    }
}

/// The argument lists of every predicted `tool` call.
fn predicted(script: &Script, tool: &str) -> Vec<Vec<Value>> {
    script
        .invocations
        .iter()
        .filter(|inv| inv.effective_program().as_deref() == Some(tool))
        .map(|inv| inv.effective_argv()[1..].to_vec())
        .collect()
}

fn same_call(predicted: &[Value], actual: &[String]) -> bool {
    predicted.len() == actual.len()
        && predicted
            .iter()
            .zip(actual)
            .all(|(word, actual)| word.as_literal().is_none_or(|text| text == actual))
}

/// `(recalled, total, misses)` for one record. Each prediction is spent on
/// at most one recorded call.
fn score(record: &Record) -> (usize, usize, Vec<String>) {
    let script = parse(&record.command, Dialect::Posix, &base_for(record));
    let mut pool: Vec<(String, Vec<Value>)> = ["git", "gh"]
        .iter()
        .flat_map(|tool| {
            predicted(&script, tool)
                .into_iter()
                .map(|args| ((*tool).to_string(), args))
        })
        .collect();
    let mut recalled = 0;
    let mut misses = Vec::new();
    for call in &record.calls {
        let found = pool
            .iter()
            .position(|(tool, args)| *tool == call.tool && same_call(args, &call.argv));
        match found {
            Some(at) => {
                pool.remove(at);
                recalled += 1;
            }
            None => misses.push(format!("{} {}", call.tool, call.argv.join(" "))),
        }
    }
    (recalled, record.calls.len(), misses)
}

#[test]
fn recall_of_recorded_git_and_gh_calls_does_not_regress() {
    let fixture: Fixture = serde_json::from_str(FIXTURE).expect("oracle fixture parses");
    let mut recalled = 0;
    let mut total = 0;
    let mut report = Vec::new();
    for record in &fixture.records {
        let (hit, count, misses) = score(record);
        recalled += hit;
        total += count;
        for miss in misses {
            report.push(format!("  `{}`\n    missed: {miss}", record.command));
        }
    }
    assert!(total > 0, "the oracle fixture holds no recorded calls");
    let recall = recalled as f64 / total as f64;
    println!(
        "shell_ast oracle recall: {recalled}/{total} = {recall:.4} (floor {})",
        fixture.floor
    );
    for line in report.iter().take(40) {
        println!("{line}");
    }
    assert!(
        recall + 1e-9 >= fixture.floor,
        "recall {recall:.4} fell below the recorded floor {}; first misses:\n{}",
        fixture.floor,
        report
            .iter()
            .take(20)
            .cloned()
            .collect::<Vec<_>>()
            .join("\n")
    );
}

#[test]
fn the_oracle_matcher_treats_unknown_words_as_wildcards() {
    let args = [
        Value::literal("-C"),
        Value::unknown("$R", UnknownReason::UnsetVariable),
    ];
    assert!(same_call(&args, &["-C".to_string(), "/repo".to_string()]));
    assert!(!same_call(&args, &["-C".to_string()]));
    assert!(!same_call(
        &[Value::literal("status")],
        &["log".to_string()]
    ));
}
