//! The minimal desktop interface (egui) and tray icon.
//!
//! - `model`: pure state (with `remembered`: hosts this PC connects to
//!   without a PIN); `controller`: intents and events -> state, through the
//!   `ports` (traits) implemented by `sharing` (host service), `launcher`
//!   (viewer child processes), `discovery_source` and `known_hosts_file`.
//! - `view` draws the state and returns intents (`pairing_view` the parts
//!   about remembered PCs); `app` is the eframe glue; `tray` the
//!   notification-area icon.
//!
//! The sharing service runs in this process; every viewer window is a
//! separate `wshare view` process (one winit event loop per process).

mod app;
mod controller;
mod discovery_source;
mod elapsed;
mod icon;
mod known_hosts_file;
mod launcher;
mod model;
mod pairing_view;
pub mod platform;
mod ports;
mod remembered;
mod sharing;
mod tray;
mod view;

use std::sync::Arc;
use std::sync::mpsc::channel;

use anyhow::{Result, anyhow};
use eframe::egui;

use app::WshareApp;
use controller::Controller;
use ports::{Inbox, Waker};

const WINDOW_SIZE: [f32; 2] = [380.0, 520.0];

/// Opens the window and runs until the user quits.
pub fn run() -> Result<()> {
    let mut viewport = egui::ViewportBuilder::default()
        .with_title("wshare")
        .with_app_id("wshare")
        .with_inner_size(WINDOW_SIZE)
        .with_min_inner_size([340.0, 400.0]);
    if let Some(icon) = icon::window_icon() {
        viewport = viewport.with_icon(Arc::new(icon));
    }
    let options = eframe::NativeOptions {
        viewport,
        centered: true,
        ..Default::default()
    };
    eframe::run_native(
        "wshare",
        options,
        Box::new(|cc| {
            let (tx, rx) = channel();
            let inbox = Inbox::new(tx, Arc::new(RepaintWaker(cc.egui_ctx.clone())));
            discovery_source::spawn(inbox.clone());
            let tray = tray::Tray::new(&inbox).ok();
            let controller = Controller::new(
                sharing::HostService::new(inbox.clone()),
                launcher::ProcessLauncher::new(inbox)?,
                known_hosts_file::KnownHostsFile,
            );
            Ok(Box::new(WshareApp::new(controller, rx, tray)))
        }),
    )
    .map_err(|e| anyhow!("{e}"))
}

/// Wakes eframe from any thread; it then calls `App::logic` even while the
/// window is hidden in the tray.
struct RepaintWaker(egui::Context);

impl Waker for RepaintWaker {
    fn wake(&self) {
        self.0.request_repaint();
    }
}
