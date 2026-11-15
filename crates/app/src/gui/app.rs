//! The eframe application: pumps events into the controller, draws the
//! view and carries out window commands.

use std::sync::mpsc::Receiver;

use eframe::egui::{self, ViewportCommand};

use super::controller::{Controller, Intent, WindowCommand};
use super::known_hosts_file::KnownHostsFile;
use super::launcher::ProcessLauncher;
use super::ports::AppEvent;
use super::sharing::HostService;
use super::tray::Tray;
use super::view::{self, Drafts};

type AppController = Controller<HostService, ProcessLauncher, KnownHostsFile>;

pub struct WshareApp {
    controller: AppController,
    events: Receiver<AppEvent>,
    drafts: Drafts,
    /// `None` if the notification area is unavailable.
    tray: Option<Tray>,
    hidden: bool,
    quitting: bool,
}

impl WshareApp {
    pub fn new(controller: AppController, events: Receiver<AppEvent>, tray: Option<Tray>) -> Self {
        Self { controller, events, drafts: Drafts::default(), tray, hidden: false, quitting: false }
    }

    fn execute(&mut self, ctx: &egui::Context, command: WindowCommand) {
        match command {
            WindowCommand::Show => {
                self.hidden = false;
                ctx.send_viewport_cmd(ViewportCommand::Visible(true));
                ctx.send_viewport_cmd(ViewportCommand::Minimized(false));
                ctx.send_viewport_cmd(ViewportCommand::Focus);
            }
            WindowCommand::HideToTray if self.tray.is_some() => {
                self.hidden = true;
                ctx.send_viewport_cmd(ViewportCommand::CancelClose);
                ctx.send_viewport_cmd(ViewportCommand::Visible(false));
            }
            // Without a tray icon a hidden window could not be reopened.
            WindowCommand::HideToTray => {
                ctx.send_viewport_cmd(ViewportCommand::CancelClose);
                ctx.send_viewport_cmd(ViewportCommand::Minimized(true));
            }
            WindowCommand::Quit => {
                self.quitting = true;
                ctx.send_viewport_cmd(ViewportCommand::Close);
            }
        }
    }

    fn sync_tray(&mut self) {
        if let Some(tray) = &mut self.tray {
            tray.sync(self.controller.state());
        }
    }

    fn handle(&mut self, ctx: &egui::Context, intent: Intent) {
        if let Some(command) = self.controller.handle(intent) {
            self.execute(ctx, command);
        }
    }
}

impl eframe::App for WshareApp {
    /// Also runs while the window is hidden, whenever a background thread
    /// or the tray icon wakes the UI (see `ports::Waker`).
    fn logic(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        while let Ok(event) = self.events.try_recv() {
            if let Some(command) = self.controller.on_event(event) {
                self.execute(ctx, command);
            }
        }
        self.sync_tray();
    }

    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        if ctx.input(|i| i.viewport().close_requested()) && !self.quitting && !self.hidden {
            self.handle(&ctx, Intent::CloseWindow);
        }
        let intents = egui::CentralPanel::default_margins()
            .show(ui, |ui| view::show(ui, self.controller.state(), &mut self.drafts))
            .inner;
        if !intents.is_empty() {
            // The state changed after this frame was laid out: draw it again.
            ctx.request_repaint();
        }
        for intent in intents {
            self.handle(&ctx, intent);
        }
        self.sync_tray();
    }

    fn on_exit(&mut self, _gl: Option<&eframe::glow::Context>) {
        self.controller.shutdown();
    }

    fn clear_color(&self, visuals: &egui::Visuals) -> [f32; 4] {
        visuals.panel_fill.to_normalized_gamma_f32()
    }
}
