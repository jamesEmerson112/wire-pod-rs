//! Embeds `resources/app.rc` into the Windows binaries: the tray's icons and
//! the Common Controls 6 manifest.

fn main() -> Result<(), embed_resource::CompilationResult> {
    println!("cargo:rerun-if-changed=resources");
    // CI also builds on Ubuntu, where there is nothing to embed.
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("windows") {
        return Ok(());
    }
    embed_resource::compile("resources/app.rc", embed_resource::NONE).manifest_required()
}
