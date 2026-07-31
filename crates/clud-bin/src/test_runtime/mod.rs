//! Project-local test-runtime histogram (#407, designed in #405).
//!
//! Records what each test bucket actually costs on *this* checkout, so the
//! run-all-vs-targeted choice is made from data rather than vibes. See
//! [`docs/architecture/test-runtime-memory.md`](../../../../docs/architecture/test-runtime-memory.md)
//! for the accepted design and the rationale behind every position taken here.
//!
//! - [`store`] — the append-only JSONL store, retention, and percentiles.
//! - [`cli`] — `clud test run` (wrapper) and `clud test stats` (read).
//!
//! **v1 is intentionally minimal.** Bucket heuristics, the CPU-normalization
//! formula, the decision threshold, `bash test` integration, and CI bootstrap
//! are #405 Q2/Q3/Q6/Q8/Q9 and are all explicitly deferred. v1 ships the
//! storage and the dumbest useful read/write surface.

pub mod cli;
pub mod store;
