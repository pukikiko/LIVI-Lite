use std::path::{Path, PathBuf};

fn protos(dir: &Path, out: &mut Vec<PathBuf>) {
    let mut entries: Vec<PathBuf> =
        std::fs::read_dir(dir).expect("proto dir").flatten().map(|e| e.path()).collect();
    entries.sort();
    for path in entries {
        if path.is_dir() {
            protos(&path, out);
        } else if path.extension().is_some_and(|e| e == "proto") {
            out.push(path);
        }
    }
}

fn main() {
    let root = Path::new("proto");
    println!("cargo:rerun-if-changed=proto");
    let mut files = Vec::new();
    protos(root, &mut files);
    let fds = protox::compile(&files, [root]).expect("the proto tree compiles");
    prost_build::Config::new()
        .include_file("protos.rs")
        .compile_fds(fds)
        .expect("prost generates the messages");
}
