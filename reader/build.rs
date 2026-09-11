fn main() {
    // The same proto the server compiles, from where it lives - a path starting
    // with ".." makes ProtoFileBuilder compile it in place instead of copying it
    // into a local ./proto first, so the contract can not drift between the two
    // sides of it.
    ci_utils::ProtoFileBuilder::new("../proto").sync_and_build("MyNoSqlReader.proto");
}
