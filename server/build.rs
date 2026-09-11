fn main() {
    // The proto lives at the workspace root so the SDK can compile the very same
    // file later. A path starting with ".." makes ProtoFileBuilder compile it
    // where it is, using that folder as the include root, instead of copying it
    // into a local ./proto first.
    ci_utils::ProtoFileBuilder::new("../proto")
        .sync_and_build("MyNoSqlWriter.proto")
        .sync_and_build("MyNoSqlReader.proto");

    // CiGenerator writes Dockerfile and .github/workflows relative to the current
    // directory, and both of them belong to the workspace root: `cargo build
    // --release` runs there and puts the binary into the workspace-level target.
    let crate_dir = std::env::current_dir().unwrap();
    std::env::set_current_dir(crate_dir.join("..")).unwrap();

    ci_utils::ci_generator::CiGenerator::new(env!("CARGO_PKG_NAME"))
        .as_basic_service()
        .generate_github_ci_file()
        .with_ci_test()
        .build();

    std::env::set_current_dir(crate_dir).unwrap();
}
