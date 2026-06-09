fn main() -> Result<(), Box<dyn std::error::Error>> {
    println!("cargo:rerun-if-changed=../../proto/clud_v1.proto");
    println!("cargo:rerun-if-changed=build.rs");
    let file_descriptors = protox::compile(["../../proto/clud_v1.proto"], ["../../proto/"])?;
    let mut config = prost_build::Config::new();
    config.enum_attribute(
        ".clud.v1.WorkerServerEnvelope.message",
        "#[allow(clippy::large_enum_variant)]",
    );
    config.compile_fds(file_descriptors)?;
    Ok(())
}
