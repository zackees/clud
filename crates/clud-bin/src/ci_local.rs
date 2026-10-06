//! `clud ci`: run a CI job locally under bosn and show only what matters (#1839).
//!
//! Agents used to hand-roll `bosn ci run … --wait > log; awk` the run id out
//! of line 1; `bosn ci logs` it; strip ANSI; and grep ~1800 lines of act,
//! docker and soldr-cache noise for an error — which missed the real clippy
//! error twice. This command asks bosn for structured data instead
//! (`ci report --json`, `ci show --json`, `ci logs --job --step`), prints a
//! one-line verdict, and on failure only the failing steps' diagnostics.
//!
//! "incomplete" with every runnable job passed is act being unable to run
//! reusable workflows, not a failure; it is reported as a pass with that note.

use std::path::{Path, PathBuf};

use serde_json::Value;

/// Lines of diagnostics shown per failing step.
const MAX_DIAGNOSTIC_LINES: usize = 40;
/// Lines shown for `--filter` matches.
const MAX_FILTER_LINES: usize = 60;

/// The act limitation bosn reports as `incomplete`.
const REUSABLE_WORKFLOW_GAP: &str = "reusable workflows require qualified execution identity";

/// The verdict for a finished run, from `bosn ci report --json`.
#[derive(Debug, Clone, PartialEq)]
pub enum Verdict {
    Pass {
        succeeded: u64,
        note: Option<String>,
    },
    Fail {
        succeeded: u64,
        failed: u64,
        reason: Option<String>,
    },
}

impl Verdict {
    pub fn passed(&self) -> bool {
        matches!(self, Self::Pass { .. })
    }

    pub fn line(&self) -> String {
        match self {
            Self::Pass {
                succeeded,
                note: None,
            } => {
                format!("PASS: {succeeded} job(s) succeeded")
            }
            Self::Pass {
                succeeded,
                note: Some(note),
            } => format!("PASS: {succeeded}/{succeeded} runnable job(s) succeeded ({note})"),
            Self::Fail {
                succeeded,
                failed,
                reason,
            } => {
                let mut line = format!("FAIL: {failed} job(s) failed, {succeeded} succeeded");
                if let Some(reason) = reason {
                    line.push_str(&format!(" ({reason})"));
                }
                line
            }
        }
    }
}

/// Classify `bosn ci report --json`.
pub fn verdict(report: &Value) -> Verdict {
    let jobs = report.get("jobs");
    let count = |key: &str| {
        jobs.and_then(|jobs| jobs.get(key))
            .and_then(Value::as_u64)
            .unwrap_or(0)
    };
    let (succeeded, failed, cancelled) = (count("succeeded"), count("failed"), count("cancelled"));
    let conclusion = report
        .get("conclusion")
        .and_then(Value::as_str)
        .unwrap_or("");
    let reason = report
        .get("reason")
        .and_then(Value::as_str)
        .map(str::to_string);
    let clean = failed == 0 && cancelled == 0 && succeeded > 0;
    // Every job ran and passed, but bosn still ended `incomplete`/`error` for
    // an engine reason (act's reusable-workflow gap, a Docker cleanup
    // timeout). That says nothing about the code under test.
    let covered = report
        .get("coverage_complete")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    match conclusion {
        "success" if clean => Verdict::Pass {
            succeeded,
            note: None,
        },
        "incomplete" if clean && reason.as_deref() == Some(REUSABLE_WORKFLOW_GAP) => {
            Verdict::Pass {
                succeeded,
                note: Some("act cannot run reusable workflows; not a failure".to_string()),
            }
        }
        "incomplete" | "error" if clean && covered => Verdict::Pass {
            succeeded,
            note: Some(format!(
                "bosn reported '{}' after every job passed; not a code failure",
                reason.as_deref().unwrap_or(conclusion)
            )),
        },
        _ => Verdict::Fail {
            succeeded,
            failed: failed + cancelled,
            reason: reason.or_else(|| (!conclusion.is_empty()).then(|| conclusion.to_string())),
        },
    }
}

/// A step that did not succeed: the `--job`/`--step` pair `bosn ci logs`
/// takes, plus its display name.
#[derive(Debug, Clone, PartialEq)]
pub struct FailedStep {
    pub job: String,
    pub section: String,
    pub name: String,
}

/// Failed steps from `bosn ci show --json`, in workflow order.
pub fn failed_steps(show: &Value) -> Vec<FailedStep> {
    let mut steps = Vec::new();
    let groups = show
        .pointer("/tree/groups")
        .and_then(Value::as_array)
        .into_iter()
        .flatten();
    for job in groups
        .filter_map(|group| group.get("jobs")?.as_array())
        .flatten()
    {
        let Some(key) = job.get("key").and_then(Value::as_str) else {
            continue;
        };
        for section in job
            .get("sections")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
        {
            if section.get("conclusion").and_then(Value::as_str) != Some("failure") {
                continue;
            }
            let (Some(stage), Some(id)) = (
                section.get("stage").and_then(Value::as_str),
                section.get("id").and_then(Value::as_str),
            ) else {
                continue;
            };
            steps.push(FailedStep {
                job: key.to_string(),
                section: format!("{stage}:{id}"),
                name: section
                    .get("name")
                    .and_then(Value::as_str)
                    .unwrap_or(id)
                    .to_string(),
            });
        }
    }
    steps
}

/// Remove ANSI escape sequences.
pub fn strip_ansi(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut chars = text.chars().peekable();
    while let Some(ch) = chars.next() {
        if ch == '\u{1b}' {
            if chars.peek() == Some(&'[') {
                chars.next();
                for next in chars.by_ref() {
                    if next.is_ascii_alphabetic() {
                        break;
                    }
                }
            }
            continue;
        }
        out.push(ch);
    }
    out
}

/// Strip bosn's elapsed-seconds columns (`  98.92    83.81 `) from a line.
fn without_timing(line: &str) -> &str {
    let mut rest = line.trim_start();
    for _ in 0..2 {
        let end = rest.find(' ').unwrap_or(rest.len());
        let token = &rest[..end];
        if !token.is_empty() && token.chars().all(|c| c.is_ascii_digit() || c == '.') {
            rest = rest[end..].trim_start();
        } else {
            break;
        }
    }
    rest
}

fn is_noise(line: &str) -> bool {
    line.starts_with("soldr[cache]")
        || line.contains("🐳")
        || line.starts_with("Compiling ")
        || line.starts_with("Checking ")
        || line.starts_with("Downloaded ")
        || line.starts_with("GoGitActionCache")
}

/// Starts a diagnostic block worth showing.
fn is_diagnostic_start(line: &str) -> bool {
    (line.starts_with("error") && !line.starts_with("error: could not compile"))
        || line.starts_with("warning:") && line.contains("-D warnings")
        || line.contains("panicked at")
        || line.starts_with("FAILED ")
        || line.starts_with("E   ")
        || (line.starts_with("---- ") && line.ends_with(" ----"))
        || is_ruff_finding(line)
}

/// `path.py:12:5: F401 …` or ruff's `RUF012 Mutable default …` header.
fn is_ruff_finding(line: &str) -> bool {
    let code = line.split_whitespace().next().unwrap_or("");
    let looks_like_code = code.len() >= 4
        && code.chars().take_while(char::is_ascii_uppercase).count() >= 1
        && code.chars().last().is_some_and(|c| c.is_ascii_digit())
        && code.chars().all(|c| c.is_ascii_alphanumeric());
    looks_like_code || (line.contains(".py:") && line.split(':').count() >= 4)
}

/// The lines of a step log that explain the failure: each diagnostic start
/// plus the few context lines after it (a rustc `-->` location and source
/// excerpt, a panic message), ANSI-stripped and de-noised, capped.
pub fn extract_diagnostics(log: &str) -> Vec<String> {
    // libtest prints passing tests' output too (a test may panic on purpose).
    // When it reports a `failures:` section, only that section is about why
    // the step failed.
    let lines: Vec<&str> = log.lines().collect();
    let start = lines
        .iter()
        .position(|raw| without_timing(&strip_ansi(raw)).trim_end() == "failures:")
        .unwrap_or(0);
    let mut out = Vec::new();
    let mut context = 0usize;
    for raw in &lines[start..] {
        let clean = strip_ansi(raw);
        let line = without_timing(&clean).trim_end();
        if line.is_empty() || is_noise(line) {
            continue;
        }
        if is_diagnostic_start(line) {
            context = 5;
        } else if context == 0 {
            continue;
        } else {
            context -= 1;
        }
        out.push(line.to_string());
        if out.len() >= MAX_DIAGNOSTIC_LINES {
            out.push("… (truncated)".to_string());
            break;
        }
    }
    if out.is_empty() {
        // Nothing recognised: the last real lines usually say why.
        let tail: Vec<String> = log
            .lines()
            .map(|raw| without_timing(&strip_ansi(raw)).trim_end().to_string())
            .filter(|line| !line.is_empty() && !is_noise(line))
            .collect();
        let start = tail.len().saturating_sub(15);
        out = tail[start..].to_vec();
    }
    out
}

/// Lines of `log` matching `pattern`, ANSI-stripped, capped.
pub fn filter_lines(log: &str, pattern: &regex::Regex) -> Vec<String> {
    log.lines()
        .map(|raw| without_timing(&strip_ansi(raw)).trim_end().to_string())
        .filter(|line| pattern.is_match(line))
        .take(MAX_FILTER_LINES)
        .collect()
}

/// A stale bosn daemon refuses every verb with this shape; return the
/// command that fixes it.
pub fn stale_daemon_fix(stderr: &str) -> Option<String> {
    let start = stderr.find("Stop it with `")? + "Stop it with `".len();
    let end = stderr[start..].find('`')? + start;
    Some(stderr[start..end].to_string())
}

/// The first JSON object in mixed output (bosn may log before printing it).
fn first_json(text: &str) -> Option<Value> {
    text.lines()
        .filter(|line| line.trim_start().starts_with('{'))
        .find_map(|line| serde_json::from_str(line.trim()).ok())
        .or_else(|| {
            let start = text.find('{')?;
            serde_json::from_str(&text[start..]).ok()
        })
}

fn bosn(args: &[&str]) -> Result<(i32, String), String> {
    let mut argv = vec!["bosn".to_string()];
    argv.extend(args.iter().map(|arg| arg.to_string()));
    crate::loop_spec::run_capture(argv, None)
}

fn repo_root() -> Option<PathBuf> {
    let (code, out) = crate::loop_spec::run_capture(
        vec![
            "git".to_string(),
            "rev-parse".to_string(),
            "--show-toplevel".to_string(),
        ],
        None,
    )
    .ok()?;
    (code == 0).then(|| PathBuf::from(out.trim()))
}

/// Options for one `clud ci` invocation.
#[derive(Debug, Clone, Default)]
pub struct CiOptions {
    /// A workflow job id (e.g. `test-linux-x64-unit`); `None` runs the PR plan.
    pub job: Option<String>,
    pub filter: Option<String>,
    pub json: bool,
    pub workflow: Option<String>,
}

/// Run `opts` and print the outcome. Returns the process exit code.
pub fn run(opts: &CiOptions) -> i32 {
    match run_inner(opts) {
        Ok(code) => code,
        Err(error) => {
            eprintln!("clud ci: {error}");
            2
        }
    }
}

fn submit(workspace: &Path, opts: &CiOptions) -> Result<String, String> {
    let workspace = workspace.to_string_lossy().into_owned();
    let mut args = vec!["ci", "run", "--workspace", &workspace, "--trigger", "pr"];
    if let Some(workflow) = &opts.workflow {
        args.extend(["--workflow", workflow]);
    }
    if let Some(job) = &opts.job {
        args.extend(["--job", job]);
    }
    args.extend(["--wait", "--json"]);
    let (_, out) = bosn(&args)?;
    if let Some(fix) = stale_daemon_fix(&out) {
        return Err(format!(
            "the bosn daemon is from a different bosn release. If no other session is \
             using it, run: {fix}"
        ));
    }
    first_json(&out)
        .and_then(|value| {
            value
                .get("run")
                .or_else(|| value.get("id"))
                .and_then(Value::as_str)
                .map(str::to_string)
        })
        .or_else(|| {
            out.lines()
                .find_map(|line| line.strip_prefix("run "))
                .and_then(|rest| rest.split_whitespace().next())
                .map(str::to_string)
        })
        .ok_or_else(|| format!("bosn did not report a run id:\n{}", tail(&out, 10)))
}

fn tail(text: &str, lines: usize) -> String {
    let all: Vec<&str> = text.lines().collect();
    all[all.len().saturating_sub(lines)..].join("\n")
}

fn run_inner(opts: &CiOptions) -> Result<i32, String> {
    let filter = opts
        .filter
        .as_deref()
        .map(regex::Regex::new)
        .transpose()
        .map_err(|error| format!("invalid --filter: {error}"))?;
    let workspace = repo_root().ok_or("not inside a git repository")?;
    eprintln!(
        "clud ci: running {} under bosn (this waits for the run)…",
        opts.job.as_deref().unwrap_or("the PR plan")
    );
    let run_id = submit(&workspace, opts)?;
    let (_, report) = bosn(&["ci", "report", &run_id, "--json"])?;
    let report = first_json(&report).ok_or("bosn ci report returned no JSON")?;
    let verdict = verdict(&report);
    let mut diagnostics = Vec::new();
    if !verdict.passed() {
        let (_, show) = bosn(&["ci", "show", &run_id, "--json"])?;
        let show = first_json(&show).ok_or("bosn ci show returned no JSON")?;
        for step in failed_steps(&show) {
            let (_, log) = bosn(&[
                "ci",
                "logs",
                &run_id,
                "--job",
                &step.job,
                "--step",
                &step.section,
            ])?;
            diagnostics.push((step, extract_diagnostics(&log)));
        }
    }
    let matches = match &filter {
        Some(pattern) => {
            let (_, log) = bosn(&["ci", "logs", &run_id])?;
            filter_lines(&log, pattern)
        }
        None => Vec::new(),
    };
    if opts.json {
        print_json(&run_id, &verdict, &diagnostics, &matches);
    } else {
        print_text(&run_id, &verdict, &diagnostics, &matches);
    }
    Ok(if verdict.passed() { 0 } else { 1 })
}

fn print_text(
    run_id: &str,
    verdict: &Verdict,
    diagnostics: &[(FailedStep, Vec<String>)],
    matches: &[String],
) {
    println!("{}", verdict.line());
    for (step, lines) in diagnostics {
        println!("\n✗ {} › {}", step.job, step.name);
        for line in lines {
            println!("  {line}");
        }
    }
    if !matches.is_empty() {
        println!("\nmatching lines:");
        for line in matches {
            println!("  {line}");
        }
    }
    println!("\nrun {run_id} (full log: bosn ci logs {run_id})");
}

fn print_json(
    run_id: &str,
    verdict: &Verdict,
    diagnostics: &[(FailedStep, Vec<String>)],
    matches: &[String],
) {
    let failing: Vec<Value> = diagnostics
        .iter()
        .map(|(step, lines)| {
            serde_json::json!({
                "job": step.job,
                "section": step.section,
                "step": step.name,
                "diagnostics": lines,
            })
        })
        .collect();
    let document = serde_json::json!({
        "run": run_id,
        "passed": verdict.passed(),
        "summary": verdict.line(),
        "failing_steps": failing,
        "matches": matches,
    });
    println!("{document}");
}

#[cfg(test)]
#[path = "ci_local_tests.rs"]
mod tests;
