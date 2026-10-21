//! Unix stub for `crate::job_orphan_reaper`: there is no foreground Job
//! Object off Windows, so the tracker never installs.

pub struct ForegroundJobTracker;

impl ForegroundJobTracker {
    pub fn install() -> Option<Self> {
        None
    }

    pub fn register_backend(&self, _pid: u32, _executable_name: &str) {}

    pub fn sweep_abandoned_at_exit(&self) -> usize {
        0
    }

    pub fn finish_and_report(&self, _measure: bool) -> Vec<String> {
        Vec::new()
    }
}
