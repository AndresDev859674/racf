use std::env;
use std::path::PathBuf;

fn main() {
    println!("cargo:rerun-if-changed=src/lib.rs");
    println!("cargo:rerun-if-changed=cbindgen.toml");

    let crate_dir = PathBuf::from(env::var("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR"));
    let config = cbindgen::Config::from_file(crate_dir.join("cbindgen.toml"))
        .expect("could not read cbindgen.toml");

    let header = crate_dir.join("include").join("racf.h");
    if let Some(parent) = header.parent() {
        std::fs::create_dir_all(parent).expect("could not create include directory");
    }

    let bindings = cbindgen::Builder::new()
        .with_crate(&crate_dir)
        .with_config(config)
        .generate()
        .expect("cbindgen could not generate the RACF C header");

    bindings.write_to_file(header);
}
