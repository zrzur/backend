//! The API binary builds. Building it here also gives an end-to-end stack
//! the binary of this checkout to start.

#[test]
fn the_delta_binary_is_built() {
    let path = std::path::Path::new(env!("CARGO_BIN_EXE_revolt-delta"));
    assert!(path.exists(), "{}", path.display());
}
