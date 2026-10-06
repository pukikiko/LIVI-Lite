use std::path::Path;

fn main() {
    // include_bytes! needs the file at compile time, CI drops the real .lfwb here.
    for lfwb in [
        "../../../../assets/livi-link/v821b_aic8800d80/livi-link-v821b.lfwb",
        "../../../../assets/livi-link/ax520_aic8800d80/livi-link-ax520.lfwb",
        "../../../../assets/livi-link/imx6ul_iw416/livi-link-imx6ull.lfwb",
        "../../../../assets/livi-link/imx6ul_rtl8822cs/livi-link-imx6ull-rtl8822cs.lfwb",
        "../../../../assets/livi-link/imx6ul_rtl8822bs/livi-link-imx6ull-rtl8822bs.lfwb",
    ] {
        let path = Path::new(lfwb);
        if !path.exists() {
            let _ = std::fs::create_dir_all(path.parent().unwrap());
            let _ = std::fs::write(path, b"");
            println!(
                "cargo:warning=empty {} stub — CI populates it when its target is built",
                path.file_name().unwrap().to_string_lossy()
            );
        }
        println!("cargo:rerun-if-changed={lfwb}");
    }
}
