fn main() {
    uniffi_build::generate_scaffolding("src/spotifast.udl").expect("uniffi scaffolding");
}
