//! The tool-shell probe is Windows-only; Unix reports it unsupported.

use std::path::Path;

pub fn run_tool_shell_probe(_exe: &str, report_path: &Path) {
    let report = serde_json::json!({
        "tool_shell_probe": {
            "unsupported": true,
        }
    });
    std::fs::write(report_path, report.to_string()).expect("write tool-shell probe report");
    println!("{report}");
}
