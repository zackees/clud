use std::env;
use std::fs;
use std::path::Path;

const WINDOWS_APP_MANIFEST: &str = include_str!("windows-app-manifest.xml");

fn main() {
    let host = std::env::var("HOST").unwrap_or_default();
    let target = std::env::var("TARGET").unwrap_or_default();
    if target.contains("windows") && host != target {
        // CI cross-compiles the Windows wheel from Linux. tauri-build normally
        // invokes a host-side resource compiler, which soldr deliberately does
        // not provide. The manifest is not optional metadata: Tauri imports
        // comctl32!TaskDialogIndirect and needs Common Controls v6 before the
        // Windows loader resolves that import (#1033). Write its one-resource
        // .res payload directly and pass it to lld-link, avoiding a host tool.
        write_cross_windows_manifest();
        println!("cargo:rustc-check-cfg=cfg(desktop)");
        println!("cargo:rustc-cfg=desktop");
        println!("cargo:rustc-env=TAURI_ENV_TARGET_TRIPLE={target}");
        return;
    }
    tauri_build::build();
}

fn write_cross_windows_manifest() {
    let out_dir = env::var_os("OUT_DIR").expect("Cargo did not set OUT_DIR");
    let resource = Path::new(&out_dir).join("clud-webterm.manifest.res");
    let manifest = WINDOWS_APP_MANIFEST.as_bytes();
    let data_size = u32::try_from(manifest.len()).expect("Windows manifest exceeds .res limits");
    let mut bytes = Vec::with_capacity(64 + manifest.len() + 3);

    // A .res stream starts with this null resource header, then carries the
    // actual type-24 (RT_MANIFEST) resource. MSVC's linker and lld-link both
    // consume this format directly. Keep it tiny and self-contained so
    // Linux-to-MSVC releases do not depend on llvm-rc.
    bytes.extend_from_slice(&0_u32.to_le_bytes());
    bytes.extend_from_slice(&32_u32.to_le_bytes());
    bytes.extend_from_slice(&0x0000_ffff_u32.to_le_bytes());
    bytes.extend_from_slice(&0x0000_ffff_u32.to_le_bytes());
    bytes.extend_from_slice(&[0_u8; 16]);
    bytes.extend_from_slice(&data_size.to_le_bytes());
    bytes.extend_from_slice(&32_u32.to_le_bytes());
    bytes.extend_from_slice(&0x0018_ffff_u32.to_le_bytes()); // RT_MANIFEST
    bytes.extend_from_slice(&0x0001_ffff_u32.to_le_bytes()); // resource id 1
    bytes.extend_from_slice(&0_u32.to_le_bytes()); // data version
    bytes.extend_from_slice(&0x1030_u16.to_le_bytes()); // movable | pure | discardable
    bytes.extend_from_slice(&0_u16.to_le_bytes()); // neutral language
    bytes.extend_from_slice(&0_u32.to_le_bytes()); // version
    bytes.extend_from_slice(&0_u32.to_le_bytes()); // characteristics
    bytes.extend_from_slice(manifest);
    bytes.resize((bytes.len() + 3) & !3, 0);

    fs::write(&resource, bytes).expect("writing Windows manifest resource");
    println!("cargo:rustc-link-arg={}", resource.display());
    println!("cargo:rerun-if-changed=windows-app-manifest.xml");
}
