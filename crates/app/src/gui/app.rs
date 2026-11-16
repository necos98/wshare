//! The eframe application: pumps events into the controller, draws the
//! view and carries out window commands.

use std::sync::mpsc::Receiver;

use eframe::egui::{self, ViewportCommand};

use super::controller::{Controller, Intent, WindowCommand};
use super::known_hosts_file::KnownHostsFile;
use super::launcher::ProcessLauncher;
use super::overlay::Overlay;
use super::ports::AppEvent;
use super::sharing::HostService;
use super::system_settings::SystemSettings;
use super::tray::Tray;
use super::view::{self, Drafts};

type AppController = Controller<HostService, ProcessLauncher, KnownHostsFile, SystemSettings>;

/// What the app shows outside its window; each part is absent when Windows
/// would not create it.
pub struct Shell {
    pub tray: Option<Tray>,
    pub overlay: Option<Overlay>,
}

pub struct WshareApp {
    controller: AppController,
    events: Receiver<AppEvent>,
    drafts: Drafts,
    shell: Shell,
    hidden: bool,
    /// Opened hidden, but there is no tray icon to reopen it from: the
    /// window must be shown (minimized) instead.
    reveal_minimized: bool,
    /// The window was put above the others for a request that waits.
    raised: bool,
    quitting: bool,
}

impl WshareApp {
    /// `minimized`: the window was created hidden (see `gui::run`).
    pub fn new(
        controller: AppController,
        events: Receiver<AppEvent>,
        shell: Shell,
        minimized: bool,
    ) -> Self {
        let has_tray = shell.tray.is_some();
        Self {
            controller,
            events,
            drafts: Drafts::default(),
            shell,
            hidden: minimized && has_tray,
            reveal_minimized: minimized && !has_tray,
            raised: false,
            quitting: false,
        }
    }

    fn execute(&mut self, ctx: &egui::Context, command: WindowCommand) {
        match command {
            WindowCommand::Show => {
                self.hidden = false;
                ctx.send_viewport_cmd(ViewportCommand::Visible(true));
                ctx.send_viewport_cmd(ViewportCommand::Minimized(false));
                ctx.send_viewport_cmd(ViewportCommand::Focus);
            }
            WindowCommand::BringToFront => {
                self.execute(ctx, WindowCommand::Show);
                self.raised = true;
                ctx.send_viewport_cmd(ViewportCommand::WindowLevel(egui::WindowLevel::AlwaysOnTop));
            }
            WindowCommand::HideToTray if self.shell.tray.is_some() => {
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

    /// Mirrors the state outside the window: tray icon, balloons and the
    /// connection indicator.
    fn sync_shell(&mut self, ctx: &egui::Context) {
        if std::mem::take(&mut self.reveal_minimized) {
            ctx.send_viewport_cmd(ViewportCommand::Visible(true));
            ctx.send_viewport_cmd(ViewportCommand::Minimized(true));
        }
        let state = self.controller.state();
        if self.raised && state.sharing.approval.is_none() {
            // Answered, or over: the window is an ordinary one again.
            self.raised = false;
            ctx.send_viewport_cmd(ViewportCommand::WindowLevel(egui::WindowLevel::Normal));
        }
        if let Some(tray) = &mut self.shell.tray {
            tray.sync(state);
        }
        if let Some(overlay) = &mut self.shell.overlay {
            overlay.show(state.indicator_text().as_deref());
        }
        for notification in self.controller.take_notifications() {
            if let Some(tray) = &self.shell.tray {
                tray.notify(&notification);
            }
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
        self.sync_shell(ctx);
    }

    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        if ctx.input(|i| i.viewport().close_requested()) && !self.quitting && !self.hidden {
            self.handle(&ctx, Intent::CloseWindow);
        }
        let intents = egui::CentralPanel::default_margins()
            .show(ui, |ui| {
                view::show(ui, self.controller.state(), &mut self.drafts)
            })
            .inner;
        if !intents.is_empty() {
            // The state changed after this frame was laid out: draw it again.
            ctx.request_repaint();
        }
        for intent in intents {
            self.handle(&ctx, intent);
        }
        self.sync_shell(&ctx);
    }

    fn on_exit(&mut self, _gl: Option<&eframe::glow::Context>) {
        self.controller.shutdown();
    }

    fn clear_color(&self, visuals: &egui::Visuals) -> [f32; 4] {
        visuals.panel_fill.to_normalized_gamma_f32()
    }
}
