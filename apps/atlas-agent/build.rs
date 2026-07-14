use std::error::Error;
use std::path::PathBuf;

fn main() -> Result<(), Box<dyn Error>> {
    let proto_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../proto/peer-observer");
    let roots = [
        proto_dir.join("event.proto"),
        proto_dir.join("archive/header.proto"),
    ];

    prost_build::Config::new().compile_protos(&roots, std::slice::from_ref(&proto_dir))?;
    println!("cargo:rerun-if-changed={}", proto_dir.display());

    Ok(())
}
