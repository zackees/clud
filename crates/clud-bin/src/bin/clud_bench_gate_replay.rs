//! Replay recorded agent Bash commands through the command gate (#1067 step 2).
//!
//! Walks Claude Code transcripts (`~/.claude/projects` by default), classifies
//! every Bash tool call with the gate's own classifier, and prints aggregate
//! JSON: how many commands the gate would admit as written, admit once
//! prefixed, or refuse until restructured. No command text is printed.
//!
//! ```text
//! soldr cargo run --features bench --bin clud-bench-gate-replay -- [DIR] [--prefix tap]
//! ```

use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};

use clud::gate_replay::{bash_commands, GateReplayReport};

fn main() {
    let mut root: Option<PathBuf> = None;
    let mut prefix = "tap".to_string();
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--prefix" => prefix = args.next().unwrap_or(prefix),
            "-h" | "--help" => {
                println!("usage: clud-bench-gate-replay [TRANSCRIPT_DIR] [--prefix NAME]");
                return;
            }
            _ => root = Some(PathBuf::from(arg)),
        }
    }
    let root = root.unwrap_or_else(|| {
        dirs::home_dir()
            .unwrap_or_default()
            .join(".claude")
            .join("projects")
    });

    let mut files = Vec::new();
    collect_jsonl(&root, &mut files);
    let mut report = GateReplayReport::default();
    for file in &files {
        let Ok(handle) = std::fs::File::open(file) else {
            continue;
        };
        report.transcripts += 1;
        for line in BufReader::new(handle).lines().map_while(Result::ok) {
            for command in bash_commands(&line) {
                report.record(&command, &prefix);
            }
        }
    }
    println!(
        "{}",
        serde_json::to_string_pretty(&report.to_json()).unwrap_or_default()
    );
}

fn collect_jsonl(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            collect_jsonl(&path, out);
        } else if path.extension().is_some_and(|ext| ext == "jsonl") {
            out.push(path);
        }
    }
}
