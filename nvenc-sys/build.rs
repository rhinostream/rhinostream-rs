use std::env;
use std::path::PathBuf;

use bindgen::{Bindings, builder, Builder};

fn main() {

    let video_codec_sdk = PathBuf::from(env!("NVIDIA_VIDEO_CODEC_SDK"));
    let include_path = video_codec_sdk.join("Interface");
    let lib_path = video_codec_sdk.join("Lib/x64");
    let bindings = generate_bindings(include_path, lib_path, "include/nvenc.h", "nvencodeapi")
        .allowlist_type(".*Nv.*")
        .allowlist_type(".*NV.*")
        .allowlist_var(".*NV.*")
        .allowlist_function(".*Nv.*")
        .blocklist_item(".*NV.*_GUID")
        .generate().expect("failed to generate bindings");
    write_bindings(bindings, "nvenc.rs");
}

#[derive(Debug)]
pub struct FixBindgen {}

impl bindgen::callbacks::ParseCallbacks for FixBindgen {
    fn item_name(&self, original_item_name: &str) -> Option<String> {
        Some(original_item_name.trim_start_matches("FIXBIND_").to_owned())
    }
}

fn generate_bindings(include_path: PathBuf, lib_path: PathBuf, header: &str, link: &str) -> Builder {
    println!("cargo:rerun-if-changed={}", header);
    println!("cargo:rustc-link-search=native={}", lib_path.display());
    println!("cargo:rustc-link-lib=static={}", link);

    let bindings = builder()
        .header(header)
        .clang_arg(format!("-I{}", include_path.display()))
        .parse_callbacks(Box::new(bindgen::CargoCallbacks))
        .parse_callbacks(Box::new(FixBindgen {}))
        .opaque_type("_IMAGE_TLS_DIRECTORY64")
        .opaque_type("IMAGE_TLS_DIRECTORY64")
        .opaque_type("PIMAGE_TLS_DIRECTORY64")
        .opaque_type("IMAGE_TLS_DIRECTORY")
        .opaque_type("PIMAGE_TLS_DIRECTORY");
    return bindings;
}

fn write_bindings(bindings: Bindings, out: &str) {
    let out_path = PathBuf::from(env::var("OUT_DIR").unwrap());
    bindings.write_to_file(out_path.join(out))
        .expect("Couldn't write to file");
}