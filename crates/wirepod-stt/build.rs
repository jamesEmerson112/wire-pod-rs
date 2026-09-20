//! Adds the libvosk import library to the link search path.
//!
//! The library ships in the official `vosk-win64` release rather than in this
//! repository, so its directory is named by `VOSK_LIB_DIR`.

fn main() {
    println!("cargo:rerun-if-env-changed=VOSK_LIB_DIR");
    if std::env::var_os("CARGO_FEATURE_STT_VOSK").is_none() {
        return;
    }
    match std::env::var("VOSK_LIB_DIR") {
        Ok(dir) => println!("cargo:rustc-link-search=native={dir}"),
        Err(_) => println!(
            "cargo:warning=VOSK_LIB_DIR is not set, so libvosk will be looked for on the \
             default link path"
        ),
    }
}
