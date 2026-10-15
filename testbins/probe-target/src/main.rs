//! Process-shape probe target. The real body is Windows-only and lives in
//! the host boundary, `platform/windows/probe.rs`.

mod platform;

fn main() {
    platform::probe::main();
}
