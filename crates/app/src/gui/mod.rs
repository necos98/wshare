//! The minimal desktop interface (egui) and tray icon.
//!
//! - `model`: pure state (with `remembered`: hosts this PC connects to
//!   without a PIN; `settings` and `settings_state`: the saved
//!   preferences); `controller`: intents and events -> state, through the
//!   `ports` (traits) implemented by `sharing` (host service), `launcher`
//!   (viewer child processes), `discovery_source`, `known_hosts_file` and
//!   `system_settings` (settings file, `autostart` registry entry,
//!   monitors).
//! - `transfers` holds the file transfers of the sharing and of each viewer
//!   window (`transfers_view` draws them); `shell` is the `Desktop` port
//!   (file dialogs, Explorer).
//! - `host_row` builds the list of computers (online ones and the known
//!   ones that are not, favorites first); `wake_on_lan` sends the magic
//!   packet that turns one on.
//! - `view` draws the state and returns intents (`pairing_view` the parts
//!   about remembered PCs, `settings_view` the settings page, `approval_view`
//!   the request of a viewer to be accepted, whose state is `approval`); `app` is the
//!   eframe glue; `tray` the notification-area icon (`status_icon` its
//!   variants, `balloon` its notifications, `notifications` which events
//!   deserve one) and `overlay` the always-visible "who is connected" bar.
//!
//! The sharing service runs in this process; every viewer window is a
//! separate `wshare view` process (one winit event loop per process).
//!
//! Closing the window: while sharing is on it hides in the tray (the
//! screen stays reachable, as after "Avvia con Windows" or "Avvia ridotto
//! a icona", which open it hidden there); with sharing off it quits.
//! "Esci" in the tray menu always quits.

mod app;
mod approval;
mod approval_view;
mod autostart;
mod balloon;
mod controller;
mod discovery_source;
mod elapsed;
mod host_row;
mod icon;
mod known_hosts_file;
mod launcher;
mod model;
mod notifications;
mod overlay;
mod pairing_view;
pub mod platform;
mod ports;
mod remembered;
mod settings;
mod settings_file;
mod settings_state;
mod settings_view;
mod sharing;
mod shell;
mod status_icon;
mod system_settings;
mod transfers;
mod transfers_view;
mod tray;
mod view;
mod viewer_processes;
mod wake_on_lan;

use std::sync::Arc;
use std::sync::mpsc::channel;

use anyhow::{Result, anyhow};
use eframe::egui;

use crate::cli::GuiOptions;
use app::WshareApp;
use controller::Controller;
use ports::{Inbox, SettingsBackend, Waker};

const WINDOW_SIZE: [f32; 2] = [380.0, 520.0];

/// Opens the window and runs until the user quits.
///
/// The window opens hidden in the tray when started with `--minimized` or
/// when the saved settings say so (see `WshareApp` for what closing it
/// does).
pub fn run(gui: GuiOptions) -> Result<()> {
    let minimized = system_settings::SystemSettings
        .load()
        .0
        .opens_minimized(gui.minimized);
    let mut viewport = egui::ViewportBuilder::default()
        .with_title("wshare")
        .with_app_id("wshare")
        .with_inner_size(WINDOW_SIZE)
        .with_min_inner_size([340.0, 400.0])
        .with_visible(!minimized);
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
        Box::new(move |cc| {
            let (tx, rx) = channel();
            let inbox = Inbox::new(tx, Arc::new(RepaintWaker(cc.egui_ctx.clone())));
            discovery_source::spawn(inbox.clone());
            let tray = tray::Tray::new(&inbox).ok();
            // Without a notification area the indicator is the only sign.
            let overlay = overlay::Overlay::new(inbox.clone()).ok();
            let mut controller = Controller::new(
                sharing::HostService::new(inbox.clone()),
                launcher::ProcessLauncher::new(inbox.clone())?,
                known_hosts_file::KnownHostsFile,
                system_settings::SystemSettings,
                wake_on_lan::UdpWakeOnLan,
                shell::WindowsShell::new(inbox),
            );
            controller.start_up();
            Ok(Box::new(WshareApp::new(
                controller,
                rx,
                app::Shell { tray, overlay },
                minimized,
            )))
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
