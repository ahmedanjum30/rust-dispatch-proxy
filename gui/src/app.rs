//! The window.

use std::process::Child;
use std::sync::atomic::{AtomicU16, Ordering::Relaxed};
use std::sync::{Arc, Mutex};

use eframe::egui::{self, Align, Color32, Layout, RichText};

use crate::control;
use crate::settings::{self, Settings};
use crate::status::{self, Reading};

pub struct App {
    settings: Settings,
    port: Arc<AtomicU16>,
    latest: Arc<Mutex<Reading>>,
    previous: Option<Reading>,
    /// Per-link speed in Mbit/s (down, up), from the last two readings.
    rates: Vec<(String, f64, f64)>,
    /// The proxy if this window started it, to notice if it exits at once.
    child: Option<Child>,
    message: Option<(String, bool)>, // (text, is_error)
    start_at_login: bool,
}

impl App {
    pub fn new(cc: &eframe::CreationContext<'_>, settings: Settings) -> Self {
        let port = Arc::new(AtomicU16::new(settings.port));
        let ctx = cc.egui_ctx.clone();
        let latest = status::spawn_poller(port.clone(), move || ctx.request_repaint());
        App {
            settings,
            port,
            latest,
            previous: None,
            rates: Vec::new(),
            child: None,
            message: None,
            start_at_login: control::starts_at_login(),
        }
    }

    fn info(&mut self, text: impl Into<String>) {
        self.message = Some((text.into(), false));
    }

    fn error(&mut self, text: impl Into<String>) {
        self.message = Some((text.into(), true));
    }

    fn start(&mut self) {
        match control::start(&self.settings) {
            Ok(child) => {
                self.child = Some(child);
                self.info("Starting...");
            }
            Err(e) => self.error(e),
        }
    }

    fn stop(&mut self) {
        self.child = None;
        match control::stop(self.settings.port) {
            Ok(()) => self.info("Stopped. The Windows proxy setting is off."),
            Err(e) => self.error(e),
        }
    }

    fn save_settings(&mut self) {
        self.port.store(self.settings.port, Relaxed);
        if let Err(e) = self.settings.save() {
            self.error(format!("Could not save settings: {e}"));
        }
    }

    /// Notices if the proxy this window started exited (bad settings, port
    /// already in use, ...), and shows why.
    fn check_child(&mut self) {
        let exited = match self.child.as_mut().map(|c| c.try_wait()) {
            Some(Ok(Some(code))) => Some(code),
            _ => None,
        };
        if let Some(code) = exited {
            self.child = None;
            let tail = std::fs::read_to_string(settings::log_path()).ok().and_then(|log| {
                log.lines()
                    .rev()
                    .find(|l| l.contains("[FATAL]") || l.contains("[ERROR]"))
                    .map(String::from)
            });
            self.error(match tail {
                Some(line) => format!("The proxy stopped: {line}"),
                None => format!("The proxy stopped ({code}). See the log for details."),
            });
        }
    }

    /// Works out per-link speeds when a new reading has arrived.
    fn update_rates(&mut self, now: &Reading) {
        if self.previous.as_ref().is_some_and(|p| p.at == now.at) {
            return;
        }
        if let (Some(prev), Some(cur)) = (self.previous.as_ref().and_then(|p| p.status.as_ref().map(|s| (p.at, s))), &now.status) {
            let secs = now.at.duration_since(prev.0).as_secs_f64().max(0.001);
            self.rates = cur
                .links
                .iter()
                .map(|l| {
                    let old = prev.1.links.iter().find(|o| o.link == l.link);
                    let rate = |new: u64, old: Option<u64>| old.map_or(0.0, |o| new.saturating_sub(o) as f64 * 8.0 / 1e6 / secs);
                    (
                        l.link.clone(),
                        rate(l.bytes_down, old.map(|o| o.bytes_down)),
                        rate(l.bytes_up, old.map(|o| o.bytes_up)),
                    )
                })
                .collect();
        }
        self.previous = Some(now.clone());
    }

    fn rate_of(&self, link: &str) -> (f64, f64) {
        self.rates.iter().find(|r| r.0 == link).map_or((0.0, 0.0), |r| (r.1, r.2))
    }
}

fn size(bytes: u64) -> String {
    if bytes >= 1_000_000_000 {
        format!("{:.2} GB", bytes as f64 / 1e9)
    } else {
        format!("{:.1} MB", bytes as f64 / 1e6)
    }
}

fn right(ui: &mut egui::Ui, text: impl Into<egui::WidgetText>) {
    ui.with_layout(Layout::right_to_left(Align::Center), |ui| ui.label(text));
}

/// A small filled circle: the status light. (The built-in font has no "●".)
fn dot(ui: &mut egui::Ui, color: Color32) {
    let (rect, _) = ui.allocate_exact_size(egui::vec2(10.0, 10.0), egui::Sense::hover());
    ui.painter().circle_filled(rect.center(), 4.5, color);
}

impl eframe::App for App {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        self.check_child();
        let reading = self.latest.lock().unwrap_or_else(|e| e.into_inner()).clone();
        self.update_rates(&reading);
        let running = reading.status.is_some();

        egui::CentralPanel::default().show(ui, |ui| {
            // ---- header and controls
            ui.horizontal(|ui| {
                ui.heading("rust-dispatch-proxy");
                ui.add_space(8.0);
                match &reading.status {
                    Some(s) => {
                        let (h, m) = (s.uptime_s / 3600, s.uptime_s % 3600 / 60);
                        dot(ui, Color32::from_rgb(46, 160, 67));
                        ui.label(RichText::new("Running").color(Color32::from_rgb(46, 160, 67)).strong());
                        ui.label(RichText::new(format!("v{} · up {h}h {m}m · port {}", s.version, self.settings.port)).weak());
                    }
                    None => {
                        dot(ui, Color32::GRAY);
                        ui.label(RichText::new("Stopped").color(Color32::GRAY).strong());
                    }
                }
            });
            ui.add_space(6.0);
            ui.horizontal(|ui| {
                if ui.add_enabled(!running, egui::Button::new("▶ Start")).clicked() {
                    self.start();
                }
                if ui.add_enabled(running, egui::Button::new("■ Stop")).clicked() {
                    self.stop();
                }
                if ui.add_enabled(running, egui::Button::new("⟳ Restart")).clicked() {
                    self.stop();
                    self.start();
                }
                ui.separator();
                if ui.add_enabled(running, egui::Button::new("Open status page")).clicked() {
                    control::open(&format!("http://127.0.0.1:{}/status", self.settings.port));
                }
                if ui.button("Open log").clicked() {
                    let log = settings::log_path();
                    if log.exists() {
                        control::open(&log.to_string_lossy());
                    } else {
                        self.info("No log yet: it is created when the proxy starts from here.");
                    }
                }
            });
            if let Some((text, is_error)) = &self.message {
                ui.add_space(4.0);
                let color = if *is_error {
                    Color32::from_rgb(207, 34, 46)
                } else {
                    ui.visuals().weak_text_color()
                };
                ui.label(RichText::new(text).color(color));
            }

            // ---- connections
            ui.add_space(10.0);
            ui.label(RichText::new("Connections").strong());
            match &reading.status {
                None => {
                    ui.label(RichText::new("The proxy isn't running. Press Start.").weak());
                }
                Some(s) if s.links.is_empty() => {
                    ui.label(RichText::new("No internet connections found yet.").weak());
                }
                Some(s) => {
                    // A 9th, empty column keeps the last real column from
                    // stretching across the rest of the window.
                    egui::Grid::new("links").striped(true).num_columns(9).spacing([18.0, 6.0]).show(ui, |ui| {
                        ui.label(RichText::new("Connection").weak());
                        for h in ["Download", "Upload", "Open", "Total", "Connect", "Failures", "Downloaded"] {
                            right(ui, RichText::new(h).weak());
                        }
                        ui.label("");
                        ui.end_row();
                        let (mut down_sum, mut up_sum, mut open_sum, mut total_down) = (0.0, 0.0, 0, 0);
                        for l in &s.links {
                            let (down, up) = self.rate_of(&l.link);
                            down_sum += down;
                            up_sum += up;
                            open_sum += l.active;
                            total_down += l.bytes_down;
                            ui.horizontal(|ui| {
                                ui.label(&l.link);
                                ui.label(RichText::new(&l.adapter).weak());
                                if l.cooldown_s > 0 {
                                    ui.label(RichText::new(format!("cooling down {}s", l.cooldown_s)).color(Color32::from_rgb(207, 34, 46)));
                                }
                            });
                            right(ui, format!("{down:.1} Mbit/s"));
                            right(ui, format!("{up:.1} Mbit/s"));
                            right(ui, l.active.to_string());
                            right(ui, l.connections.to_string());
                            right(
                                ui,
                                if l.connect_ms > 0.0 {
                                    format!("{:.0} ms", l.connect_ms)
                                } else {
                                    "-".into()
                                },
                            );
                            right(ui, l.failures.to_string());
                            right(ui, size(l.bytes_down));
                            ui.label("");
                            ui.end_row();
                        }
                        ui.label(RichText::new("Total").strong());
                        right(ui, RichText::new(format!("{down_sum:.1} Mbit/s")).strong());
                        right(ui, RichText::new(format!("{up_sum:.1} Mbit/s")).strong());
                        right(ui, RichText::new(open_sum.to_string()).strong());
                        ui.label("");
                        ui.label("");
                        ui.label("");
                        right(ui, RichText::new(size(total_down)).strong());
                        ui.label("");
                        ui.end_row();
                    });
                }
            }

            // ---- settings
            ui.add_space(12.0);
            let mut changed = false;
            egui::CollapsingHeader::new(RichText::new("Settings").strong())
                .default_open(!running)
                .show(ui, |ui| {
                    ui.horizontal(|ui| {
                        ui.label("Port");
                        changed |= ui.add(egui::DragValue::new(&mut self.settings.port).range(1..=65535)).changed();
                    });
                    changed |= ui
                        .checkbox(&mut self.settings.auto, "Find my internet connections automatically")
                        .changed();
                    if !self.settings.auto {
                        ui.horizontal(|ui| {
                            ui.label("Addresses");
                            changed |= ui
                                .add(egui::TextEdit::singleline(&mut self.settings.addresses).hint_text("192.168.1.10 10.0.0.5@2"))
                                .changed();
                            if ui.button("Detect").clicked() {
                                match control::detect_addresses() {
                                    Ok(found) => {
                                        self.settings.addresses = found;
                                        changed = true;
                                    }
                                    Err(e) => self.error(e),
                                }
                            }
                        });
                    }
                    changed |= ui
                        .checkbox(&mut self.settings.system_proxy, "Turn on the Windows proxy setting while the proxy runs")
                        .changed();
                    if self.settings.system_proxy {
                        ui.horizontal(|ui| {
                            ui.add_space(24.0);
                            ui.label("only with at least");
                            changed |= ui.add(egui::DragValue::new(&mut self.settings.min_links).range(1..=16)).changed();
                            ui.label("connections");
                        });
                    }
                    ui.horizontal(|ui| {
                        ui.label("Try the next connection after");
                        changed |= ui
                            .add(egui::DragValue::new(&mut self.settings.race_ms).range(0..=5000).suffix(" ms"))
                            .changed();
                        ui.label(RichText::new("(0 = only after a failure)").weak());
                    });
                    changed |= ui.checkbox(&mut self.settings.verbose, "Log every connection").changed();
                    ui.label(RichText::new("Settings are saved automatically and take effect when the proxy (re)starts.").weak());
                });
            if changed {
                self.save_settings();
            }

            ui.add_space(8.0);
            let mut login = self.start_at_login;
            if ui.checkbox(&mut login, "Start the proxy automatically when I log in").changed() {
                match control::set_start_at_login(login) {
                    Ok(()) => self.start_at_login = login,
                    Err(e) => self.error(e),
                }
            }

            ui.add_space(12.0);
            ui.label(RichText::new("Closing this window doesn't stop the proxy.").weak().small());
        });
    }
}
