extern crate cbindgen;

use std::env;
use cbindgen::{Config, Language};
fn main() {
    let crate_dir = env!("CARGO_MANIFEST_DIR");
    let target_dir = env::var("OUT_DIR").unwrap();

    let builder = cbindgen::Builder::new();

    let mut config = Config::default();
    config.language = Language::C;
    config.cpp_compat = false;
    builder.with_crate(crate_dir)
        .with_config(config)
        .generate().unwrap()
        .write_to_file(format!("{}/../../../rhinostream.h", target_dir));
}