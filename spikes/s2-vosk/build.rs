fn main() {
    // Link against the official vosk win64 release unpacked into vendor/
    // (libvosk.lib import library; libvosk.dll must be on PATH at runtime).
    let vendor = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("vendor")
        .join("vosk-win64-0.3.45");
    println!("cargo:rustc-link-search=native={}", vendor.display());
}
