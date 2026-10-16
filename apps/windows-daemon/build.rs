use std::env;
use std::path::PathBuf;

fn main() {
    println!("cargo:rerun-if-changed=../../packages/binary-protocol/windroid.proto");
    let out_dir = PathBuf::from(env::var("OUT_DIR").unwrap());

    prost_build::Config::new()
        .out_dir(&out_dir)
        .compile_protos(
            &["../../packages/binary-protocol/windroid.proto"],
            &["../../packages/binary-protocol/"],
        )
        .unwrap();
}
