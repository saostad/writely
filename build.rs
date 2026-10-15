use embed_manifest::{embed_manifest, new_manifest};

fn main() {
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows") {
        embed_manifest(new_manifest("Writely")).expect("unable to embed manifest file");
        println!("cargo:rerun-if-changed=build.rs");
    }
}
