fn main() -> Result<(), Box<dyn std::error::Error>> {
    println!("cargo:rerun-if-changed=../../proto/clud_v1.proto");
    println!("cargo:rerun-if-changed=build.rs");
    let file_descriptors = protox::compile(["../../proto/clud_v1.proto"], ["../../proto/"])?;
    prost_build::compile_fds(file_descriptors)?;
    Ok(())
}
