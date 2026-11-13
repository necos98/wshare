//! Embeds the Windows resources into wshare.exe: icon, version information
//! and application manifest.
//!
//! Besides looking right in Explorer, an executable with proper metadata is
//! less likely to be flagged by antivirus heuristics than an anonymous one —
//! which matters for a program that captures the screen and listens on the
//! network.

const ICON: &str = "assets/wshare.ico";
const MANIFEST: &str = "assets/wshare.manifest";

fn main() {
    println!("cargo:rerun-if-changed={ICON}");
    println!("cargo:rerun-if-changed={MANIFEST}");
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("windows") {
        return;
    }
    let mut resource = winresource::WindowsResource::new();
    resource
        .set_icon(ICON)
        .set_manifest_file(MANIFEST)
        .set("ProductName", "wshare")
        .set("FileDescription", "wshare - LAN screen sharing")
        .set("InternalName", "wshare")
        .set("OriginalFilename", "wshare.exe")
        .set("LegalCopyright", "Copyright (C) 2026");
    resource
        .compile()
        .expect("compiling Windows resources (needs rc.exe from the Windows SDK)");
}
