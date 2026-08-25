fn main() {
    // Name of the sidecar as the Tauri CLI copies it into the target dir in dev.
    println!(
        "cargo:rustc-env=TARGET_TRIPLE={}",
        std::env::var("TARGET").unwrap()
    );
    tauri_build::build()
}
