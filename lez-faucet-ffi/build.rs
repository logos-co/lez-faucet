fn main() {
    println!("cargo:rerun-if-changed=src/lib.rs");
    println!("cargo:rerun-if-changed=cbindgen.toml");

    let crate_dir = std::env::var("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR is set");
    let config = cbindgen::Config::from_file("cbindgen.toml").expect("valid cbindgen config");
    cbindgen::Builder::new()
        .with_crate(crate_dir)
        .with_config(config)
        .generate()
        .expect("generate LEZ faucet FFI header")
        .write_to_file("lez_faucet_ffi.h");
}
