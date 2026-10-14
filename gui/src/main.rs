//! proxy-gui: a Windows control panel for rust-dispatch-proxy.
//!
//! It starts and stops the proxy (rust-dispatch-proxy.exe in the same
//! folder), keeps its settings, can start it at login, and shows each
//! connection's live traffic. The proxy runs as its own hidden process, so
//! closing this window leaves it running.
//!
//!     proxy-gui.exe               open the window
//!     proxy-gui.exe --background  start the proxy with the saved settings
//!                                 (if it isn't running) and exit; used at login
//!     proxy-gui.exe --stop        stop the proxy on the saved port, switch the
//!                                 Windows proxy setting off, and exit

#![cfg_attr(windows, windows_subsystem = "windows")]

#[cfg(windows)]
mod app;
#[cfg(windows)]
mod control;
#[cfg(windows)]
mod settings;
#[cfg(windows)]
mod status;

#[cfg(windows)]
fn main() -> eframe::Result {
    let settings = settings::Settings::load();

    if std::env::args().any(|a| a == "--background") {
        if status::fetch(settings.port).is_none() {
            let _ = control::start(&settings);
        }
        return Ok(());
    }
    if std::env::args().any(|a| a == "--stop") {
        let _ = control::stop(settings.port);
        return Ok(());
    }

    let options = eframe::NativeOptions {
        viewport: eframe::egui::ViewportBuilder::default()
            .with_title("rust-dispatch-proxy")
            .with_inner_size([760.0, 560.0])
            .with_min_inner_size([600.0, 420.0]),
        ..Default::default()
    };
    eframe::run_native(
        "rust-dispatch-proxy",
        options,
        Box::new(move |cc| Ok(Box::new(app::App::new(cc, settings)))),
    )
}

#[cfg(not(windows))]
fn main() {
    eprintln!("proxy-gui is only available on Windows. Run rust-dispatch-proxy from a terminal instead.");
    std::process::exit(1);
}
