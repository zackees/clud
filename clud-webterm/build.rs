fn main() {
    let host = std::env::var("HOST").unwrap_or_default();
    let target = std::env::var("TARGET").unwrap_or_default();
    if target.contains("windows") && host != target {
        // CI cross-compiles the Windows wheel from Linux. tauri-build embeds a
        // Windows resource through llvm-rc, but soldr intentionally provides
        // the linker/SDK rather than that host-side resource compiler. Keep
        // the metadata that Tauri's desktop APIs need and omit only the
        // optional icon/version resource for this cross-built executable.
        println!("cargo:rustc-check-cfg=cfg(desktop)");
        println!("cargo:rustc-cfg=desktop");
        println!("cargo:rustc-env=TAURI_ENV_TARGET_TRIPLE={target}");
        return;
    }
    tauri_build::build();
}
