fn main() -> Result<(), Box<dyn std::error::Error>> {
    println!("cargo:rerun-if-changed=../../proto/clud_v1.proto");
    println!("cargo:rerun-if-changed=build.rs");

    // main.rs deliberately keeps `run()` (a large, monolithic function) on
    // the true process main thread rather than a spawned thread, because it
    // owns console/OLE APIs (drag-and-drop COM registration, console title)
    // that must run there. Only argument parsing is moved to a spawned
    // thread with a bigger stack. That leaves `run()` exposed to Windows'
    // small default main-thread stack (1 MiB): an unoptimized dev/test
    // build's stack frame for a function this size, with this many local
    // command-dispatch branches, has already overflowed it once (#869) and
    // did again after the DeepSeek command surface grew further (#878).
    // Raise the linked-in stack reservation instead of chasing individual
    // overflow sites; this is link-time (works under both link.exe and
    // lld-link for cross-compiled CI builds) and does not change which
    // thread `run()` executes on.
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows") {
        println!("cargo:rustc-link-arg=/STACK:8388608");
    }
    // Issue #1016: bake the target triple into the binary.
    //
    // Cargo sets `TARGET` for build scripts but not for the crate itself, so
    // `env!("TARGET")` does not compile in a bin. Without this a running clud
    // cannot say which build it is, which is what picks the right sidecar:
    // debug info now lives *beside* the release rather than inside the binary,
    // and the `.dwp` / `.pdb` / `.dSYM` is per-triple. The wheel a user
    // installed does not otherwise record which one they got.
    //
    // This is the build-script `TARGET`, so a cross build stamps the triple it
    // produced rather than the host's -- which is the whole point, since the
    // cross-built artifacts are the ones shipped.
    println!(
        "cargo:rustc-env=CLUD_TARGET={}",
        std::env::var("TARGET").unwrap_or_else(|_| "unknown".to_string())
    );

    let file_descriptors = protox::compile(["../../proto/clud_v1.proto"], ["../../proto/"])?;
    let mut config = prost_build::Config::new();
    config.enum_attribute(
        ".clud.v1.WorkerServerEnvelope.message",
        "#[allow(clippy::large_enum_variant)]",
    );
    config.compile_fds(file_descriptors)?;
    Ok(())
}
