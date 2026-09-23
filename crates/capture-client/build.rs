fn main() {
    println!("cargo:rerun-if-changed=../../assets/sight-relay.ico");

    #[cfg(windows)]
    {
        let icon =
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../assets/sight-relay.ico");
        if !icon.is_file() {
            panic!("missing Windows application icon: {}", icon.display());
        }
        winresource::WindowsResource::new()
            .set_icon(icon.to_str().expect("icon path is valid UTF-8"))
            .compile()
            .expect("failed to embed Sight Relay Windows icon");
    }
}
