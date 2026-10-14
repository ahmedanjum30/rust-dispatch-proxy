//! rust-dispatch-proxy.exe: the command-line proxy. All the work is in the
//! library (lib.rs), which proxy-gui.exe also includes.

fn main() {
    rust_dispatch_proxy::run(std::env::args().skip(1));
}
