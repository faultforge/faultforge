fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Use a vendored protoc binary so the build needs no system-installed protoc.
    let protoc = protoc_bin_vendored::protoc_bin_path()?;
    std::env::set_var("PROTOC", protoc);

    tonic_build::compile_protos("proto/faultforge.proto")?;
    Ok(())
}
