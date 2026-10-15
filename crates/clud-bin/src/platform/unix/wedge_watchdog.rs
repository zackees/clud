//! Unix stub for the `crate::wedge_watchdog` sampler. Wedge detection has
//! no sampler off Windows, so the watchdog never spawns its loop here
//! (`platform_supported` is false) and this sampler never yields a tick.

use crate::wedge_watchdog::Sample;

/// One tick's worth of raw measurement; never produced off Windows.
pub struct SampledTick {
    pub sample: Sample,
    pub hot_pid: u32,
    pub hot_tid: u32,
}

/// Inert sampler.
pub struct ThreadIoSampler;

impl ThreadIoSampler {
    pub(crate) fn new() -> Self {
        Self
    }

    pub fn tick(&mut self, _root_pid: u32) -> Option<SampledTick> {
        None
    }
}
