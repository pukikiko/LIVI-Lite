use std::path::Path;

fn main() {
    slint_build::compile("ui/app.slint").expect("failed to compile ui/app.slint");
    // Track every file in ui/ (and not just the directory mtime) so edits
    // inside the directory retrigger the Slint compiler.
    track(Path::new("ui"));
}

fn track(dir: &Path) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        println!("cargo:rerun-if-changed={}", path.display());
        if path.is_dir() {
            track(&path);
        }
    }
}
