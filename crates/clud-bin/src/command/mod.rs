mod builder;
mod do_input;
mod loop_task;
mod prompts;
mod types;

#[allow(unused_imports)]
pub(crate) use builder::parse_repeat_interval;
pub use builder::{
    bridge_suppresses_plan_mode, build_launch_plan, build_launch_plan_for_target,
    has_noninteractive_prompt, interactive_builtin_resume_error, next_run_at_millis,
    plan_mode_suppression_notice, repeat_implies_no_done_warning, summarize_task_name,
};
pub use do_input::resolve_do_command_target;
pub use types::{LaunchPlan, LoopMarkers, RepeatSchedule};

#[cfg(test)]
mod tests;
