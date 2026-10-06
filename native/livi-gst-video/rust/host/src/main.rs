fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.first().is_some_and(|a| a == "--probe") {
        println!("{}", gst_video_host::gst::probe_json());
        return;
    }

    gst_video_host::process::run(
        args.first().map_or("", String::as_str),
        args.get(1).map_or("", String::as_str),
    );
}
