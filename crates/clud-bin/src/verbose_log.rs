use std::sync::OnceLock;
use std::time::Instant;

static LAUNCH_START: OnceLock<Instant> = OnceLock::new();

pub fn init_launch_clock() {
    let _ = LAUNCH_START.set(Instant::now());
}

pub fn log(message: impl std::fmt::Display) {
    let start = LAUNCH_START.get_or_init(Instant::now);
    eprintln!("{:.2} {}", start.elapsed().as_secs_f64(), message);
}
