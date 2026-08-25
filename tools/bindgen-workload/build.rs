//! Bindgen build workload (§41 compiler-artifact corpus): a minimal project
//! whose build compiles the `bindgen` crate and generates C bindings —
//! representative of the Phase-6 "bindgen build 4m14s → 1m13s" observation.
//! Copied onto an EntropyFS FUSE mount and built with the target directory
//! on the mount, so rustc artifact I/O exercises the filesystem.

use std::env;
use std::path::PathBuf;

fn main() {
    println!("cargo:rerun-if-changed=wrapper.h");
    let bindings = bindgen::Builder::default()
        .header("wrapper.h")
        .generate()
        .expect("bindgen generation failed");
    let out = PathBuf::from(env::var("OUT_DIR").expect("OUT_DIR"));
    bindings
        .write_to_file(out.join("bindings.rs"))
        .expect("write bindings");
}
