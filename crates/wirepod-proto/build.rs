fn main() -> Result<(), Box<dyn std::error::Error>> {
    let files = [
        "proto/chipper/chipperpb.proto",
        "proto/jdocs/jdocs.proto",
        "proto/token/token.proto",
        "proto/vector/external_interface.proto",
        "proto/vector/oskr.proto",
    ];
    let includes = ["proto", "proto/vector"];
    let fds = protox::compile(files, includes)?;
    tonic_build::configure()
        .build_server(true)
        .build_client(true)
        .compile_fds(fds)?;
    println!("cargo:rerun-if-changed=proto");
    Ok(())
}
