use std::path::PathBuf;

fn main() {
    println!("cargo:rerun-if-changed=assets/app-icon.ico");
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("windows") {
        return;
    }

    let icon = PathBuf::from(
        std::env::var_os("CARGO_MANIFEST_DIR")
            .expect("Cargo must provide the package manifest directory"),
    )
    .join("assets")
    .join("app-icon.ico");
    let icon = icon
        .to_str()
        .expect("the embedded icon path must be valid UTF-8");

    let mut resource = winresource::WindowsResource::new();
    resource
        .set_icon(icon)
        .set("FileDescription", "Slide Viewer")
        .set("ProductName", "Slide Viewer")
        .set("OriginalFilename", "Slide-Viewer.exe")
        .compile()
        .expect("failed to embed the Slide Viewer Windows resources");
}
