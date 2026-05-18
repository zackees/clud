use serde::{Deserialize, Serialize};

use crate::backend::{Backend, LaunchMode};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LaunchPlan {
    pub command: Vec<String>,
    pub iterations: u32,
    pub backend: Backend,
    pub launch_mode: LaunchMode,
    pub cwd: Option<String>,
    #[serde(default)]
    pub repeat_schedule: Option<RepeatSchedule>,
    #[serde(default)]
    pub task_summary: Option<String>,
    /// When set, the outer loop should poll for DONE/BLOCKED marker files
    /// after each iteration and terminate accordingly.
    #[serde(default)]
    pub loop_markers: Option<LoopMarkers>,
    /// When set, claude is being invoked with `--output-format stream-json
    /// --verbose` and the subprocess runner should route its captured stdout
    /// through `stream_json::render_line` so the user sees live progress.
    #[serde(default)]
    pub stream_json_progress: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LoopMarkers {
    pub done_path: String,
    pub blocked_path: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RepeatSchedule {
    pub interval_secs: u64,
}
