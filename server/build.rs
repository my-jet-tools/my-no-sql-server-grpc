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

    // `ci_with_protoc` puts the protoc install step into release.yaml - the build
    // script compiles the proto, so a runner without protoc fails before it starts.
    // `with_ci_test` is deliberately NOT called: the test.yml the generator writes
    // has no protoc step at all, and it overwrites the file on every build, so the
    // workflow with the step in it is kept by hand.
    ci_utils::ci_generator::CiGenerator::new(env!("CARGO_PKG_NAME"))
        .as_basic_service()
        .ci_with_protoc()
        .generate_github_ci_file()
        .build();

    std::env::set_current_dir(crate_dir).unwrap();
}
