//! Build script: embeds `Info.plist` into every binary and example.
//!
//! macOS only shows the "System Audio Recording" permission prompt to
//! executables that declare `NSAudioCaptureUsageDescription`. A plain
//! command-line binary has no app bundle, so the property list is linked
//! directly into the `__TEXT,__info_plist` section of the executable.

use std::path::PathBuf;

fn main() {
    let manifest_dir = PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").unwrap());
    let plist = manifest_dir.join("Info.plist");
    println!("cargo:rerun-if-changed=Info.plist");

    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("macos") {
        let arg = format!("-Wl,-sectcreate,__TEXT,__info_plist,{}", plist.display());
        // Applies to every linked artifact: the binary, examples and tests.
        println!("cargo:rustc-link-arg={arg}");
    }
}
