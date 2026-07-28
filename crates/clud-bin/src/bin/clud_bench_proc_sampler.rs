//! Measure what one `sysinfo` refresh tick actually costs on this host.
//!
//! Issue #464 (sub-issue of #463). The per-tick cost table in #463 was
//! *reasoned* from reading `sysinfo` internals, not measured, and a 2 s
//! default cadence plus a 1 % CPU budget were about to be committed on that
//! basis. This produces the numbers.
//!
//! ## Why not `criterion`, which the issue asked for
//!
//! Two reasons, both about fit rather than taste.
//!
//! The metrics wanted here — p50/p90/p99 per tick, the PID count at sample
//! time, µs/PID as the cross-OS comparable number, and first-tick versus
//! steady-state split for the `OnlyIfNotSet` shapes — are not criterion's
//! model. Criterion reports mean/median with confidence intervals for a
//! function it may call many thousands of times, knows nothing about how many
//! processes were on the box, and would happily average away exactly the
//! first-tick/steady-state distinction that makes `OnlyIfNotSet` worth using.
//!
//! And a `benches/` target is compiled by `cargo clippy --all-targets`, which
//! `bash lint` runs on all six CI platforms. Adding criterion's dependency
//! tree to every lint lane is a real cost for a harness that runs on demand.
//!
//! So this follows the convention already in `bench/README.md`: a standalone,
//! opt-in harness that emits JSON, matching how `bench/idle_cpu` records the
//! daemon's idle cost. Build it with `--features bench`.
//!
//! ```text
//! soldr cargo run --features bench --bin clud-bench-proc-sampler -- --ticks 40
//! ```
//!
//! ## Reading the output
//!
//! `us_per_pid` is the number to compare across machines and platforms; raw
//! per-tick timings are dominated by how many processes happen to be running.
//! `hot_tick` is the proposed daemon default;
//! `hot_tick_plus_environ_always` is the shape to avoid, and the ratio between
//! them is the actual argument for `UpdateKind::OnlyIfNotSet`.

use std::time::{Duration, Instant};

use sysinfo::{ProcessRefreshKind, ProcessesToUpdate, System, UpdateKind};

/// Refresh shapes, in the order #464 lists them. Each is a realistic
/// configuration the daemon might use, plus two bounds.
const SHAPES: &[&str] = &[
    "pid_ppid_only",
    "hot_tick",
    "hot_tick_plus_cmd",
    "hot_tick_plus_environ_oneshot",
    "hot_tick_plus_environ_always",
    "everything",
    "new_all",
];

fn refresh_kind(shape: &str) -> Option<ProcessRefreshKind> {
    let hot = ProcessRefreshKind::nothing().with_cpu().with_memory();
    match shape {
        "pid_ppid_only" => Some(ProcessRefreshKind::nothing()),
        "hot_tick" => Some(hot),
        "hot_tick_plus_cmd" => Some(hot.with_cmd(UpdateKind::OnlyIfNotSet)),
        "hot_tick_plus_environ_oneshot" => Some(hot.with_environ(UpdateKind::OnlyIfNotSet)),
        "hot_tick_plus_environ_always" => Some(hot.with_environ(UpdateKind::Always)),
        "everything" => Some(ProcessRefreshKind::everything()),
        // `new_all` builds a fresh System per tick and has no single kind.
        "new_all" => None,
        _ => None,
    }
}

struct ShapeResult {
    shape: String,
    first_tick_us: u128,
    steady: Vec<u128>,
    pid_count: usize,
}

impl ShapeResult {
    /// Percentiles over the steady-state ticks only.
    ///
    /// The first tick is excluded deliberately and reported separately: for
    /// the `OnlyIfNotSet` shapes it populates a cache every later tick reuses,
    /// so folding it in would misrepresent both numbers — inflating the
    /// steady-state cost and hiding the one-off.
    fn percentile(&self, p: f64) -> u128 {
        if self.steady.is_empty() {
            return 0;
        }
        let mut sorted = self.steady.clone();
        sorted.sort_unstable();
        // Nearest-rank: no interpolation, so a reported value is always one
        // that was actually observed.
        let rank = ((p / 100.0) * sorted.len() as f64).ceil().max(1.0) as usize;
        sorted[rank.min(sorted.len()) - 1]
    }

    fn us_per_pid(&self) -> f64 {
        if self.pid_count == 0 {
            return 0.0;
        }
        self.percentile(50.0) as f64 / self.pid_count as f64
    }
}

/// Tick every shape once, round by round, instead of running all ticks of one
/// shape before moving to the next.
///
/// This is not a stylistic choice. Measured contiguously on a machine doing
/// anything else — a build, a daemon, an antivirus pass — each shape is timed
/// under whatever load happened to coincide with its block, and the result is
/// ambient drift wearing a benchmark's clothes. The first version of this
/// harness did exactly that and reported `everything` as *cheaper* than
/// `hot_tick`, which is impossible: `everything` is a strict superset. That
/// contradiction is the only reason the flaw was visible at all, and a subtler
/// pair of shapes would have produced plausible nonsense instead.
///
/// Interleaving spreads load drift across every shape roughly equally, and
/// rotating the within-round order stops any shape from permanently owning the
/// cache-cold or cache-warm slot.
///
/// Each shape keeps its own `System` across rounds, so the `OnlyIfNotSet`
/// caches behave as they would in a long-lived daemon.
fn measure_all(ticks: usize) -> Vec<ShapeResult> {
    let mut systems: Vec<Option<System>> = SHAPES
        .iter()
        .map(|s| (*s != "new_all").then(System::new))
        .collect();
    let mut timings: Vec<Vec<u128>> = vec![Vec::with_capacity(ticks); SHAPES.len()];
    let mut pid_counts: Vec<usize> = vec![0; SHAPES.len()];

    for round in 0..ticks {
        for offset in 0..SHAPES.len() {
            // Rotate the starting shape each round.
            let idx = (round + offset) % SHAPES.len();
            let shape = SHAPES[idx];
            let started = Instant::now();
            let pids = match systems[idx].as_mut() {
                Some(system) => {
                    let kind = refresh_kind(shape).expect("known shape");
                    system.refresh_processes_specifics(ProcessesToUpdate::All, true, kind);
                    system.processes().len()
                }
                // `new_all` builds a fresh System every tick by definition.
                None => System::new_all().processes().len(),
            };
            timings[idx].push(started.elapsed().as_micros());
            pid_counts[idx] = pids;
        }
    }

    SHAPES
        .iter()
        .enumerate()
        .map(|(idx, shape)| {
            let mut samples = std::mem::take(&mut timings[idx]);
            let first_tick_us = if samples.is_empty() {
                0
            } else {
                samples.remove(0)
            };
            ShapeResult {
                shape: (*shape).to_string(),
                first_tick_us,
                steady: samples,
                pid_count: pid_counts[idx],
            }
        })
        .collect()
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let ticks = args
        .windows(2)
        .find(|w| w[0] == "--ticks")
        .and_then(|w| w[1].parse::<usize>().ok())
        // 21 gives 20 steady-state samples, enough for a meaningful p90 while
        // keeping `new_all` (seconds per tick on Windows) tolerable.
        .unwrap_or(21)
        .max(2);

    let results: Vec<ShapeResult> = measure_all(ticks);

    // Sanity check the run before anyone reads the numbers: `everything` is a
    // strict superset of `hot_tick`, so it cannot legitimately be cheaper. If
    // it is, ambient load swamped the signal and the run should be repeated on
    // a quieter machine rather than quoted.
    let p50 = |name: &str| {
        results
            .iter()
            .find(|r| r.shape == name)
            .map(|r| r.percentile(50.0))
            .unwrap_or(0)
    };
    let suspect = p50("everything") < p50("hot_tick") || p50("new_all") < p50("pid_ppid_only");

    let shapes: Vec<serde_json::Value> = results
        .iter()
        .map(|r| {
            serde_json::json!({
                "shape": r.shape,
                "pid_count": r.pid_count,
                "first_tick_us": r.first_tick_us,
                "p50_us": r.percentile(50.0),
                "p90_us": r.percentile(90.0),
                "p99_us": r.percentile(99.0),
                "us_per_pid": (r.us_per_pid() * 1000.0).round() / 1000.0,
                "steady_ticks": r.steady.len(),
            })
        })
        .collect();

    let report = serde_json::json!({
        "schema": "clud.bench.proc_sampler.v1",
        "os": std::env::consts::OS,
        "arch": std::env::consts::ARCH,
        "ticks_per_shape": ticks,
        // Machine-readable so a CI artifact can be filtered rather than
        // silently averaged into a longitudinal record.
        "ordering_violated": suspect,
        "shapes": shapes,
    });

    println!(
        "{}",
        serde_json::to_string_pretty(&report).unwrap_or_else(|_| "{}".to_string())
    );

    // Human-readable tail on stderr so piping stdout to a file still leaves
    // something readable on the terminal.
    eprintln!();
    eprintln!(
        "{:<32} {:>8} {:>9} {:>9} {:>11}",
        "shape", "p50 us", "p90 us", "p99 us", "us/PID"
    );
    for r in &results {
        eprintln!(
            "{:<32} {:>8} {:>9} {:>9} {:>11.3}",
            r.shape,
            r.percentile(50.0),
            r.percentile(90.0),
            r.percentile(99.0),
            r.us_per_pid()
        );
    }
    eprintln!();
    if suspect {
        eprintln!(
            "WARNING: a superset shape measured cheaper than its subset. Ambient load \
             swamped the signal -- re-run on a quieter machine before quoting these."
        );
        eprintln!();
    }
    eprintln!(
        "{} processes sampled; first-tick costs: {}",
        results.first().map(|r| r.pid_count).unwrap_or(0),
        results
            .iter()
            .map(|r| format!("{}={}us", r.shape, r.first_tick_us))
            .collect::<Vec<_>>()
            .join(" ")
    );
    let _ = Duration::from_secs(0);
}
