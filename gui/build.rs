//! Embeds a Windows application manifest in proxy-gui.exe: version 6 of the
//! Common Controls (modern buttons, checkboxes and list view), per-monitor
//! DPI awareness so the window stays sharp on high-DPI screens, and UTF-8.

fn main() {
    if std::env::var_os("CARGO_CFG_WINDOWS").is_some() {
        embed_manifest::embed_manifest(embed_manifest::new_manifest("RustDispatchProxy.ProxyGui")).expect("unable to embed the manifest");
    }
    println!("cargo:rerun-if-changed=build.rs");
}
