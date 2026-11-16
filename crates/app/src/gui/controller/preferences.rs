//! The controller's handling of the settings: changing and saving them,
//! the Windows start-up entry, restarting the sharing to apply streaming
//! settings, and the notification balloons.

use crate::host::HostEvent;
use crate::text;

use super::super::notifications;
use super::super::ports::{KnownHostsStore, SettingsBackend, SharingService, ViewerLauncher};
use super::super::settings::Settings;
use super::super::settings_state::Page;
use super::Controller;

impl<S: SharingService, L: ViewerLauncher, K: KnownHostsStore, B: SettingsBackend>
    Controller<S, L, K, B>
{
    /// Work to do once the window exists: keeps the Windows start-up entry
    /// pointing at this executable (only if the user turned it on) and
    /// shares at once if that was asked for.
    pub fn start_up(&mut self) {
        let settings = &self.state.settings;
        if settings.start_with_windows
            && let Err(e) = self.backend.set_autostart(true, settings.start_minimized)
        {
            self.state.settings_notice = Some(text::autostart_failed(&format!("{e:#}")));
        }
        if self.state.settings.share_on_start {
            self.set_sharing(true);
        }
    }

    pub(super) fn open_settings(&mut self) {
        match self.backend.monitors() {
            Ok(monitors) => self.state.monitors = monitors,
            Err(e) => {
                self.state.monitors.clear();
                self.state.settings_notice = Some(text::monitors_unavailable(&format!("{e:#}")));
            }
        }
        self.state.page = Page::Settings;
    }

    /// Applies and saves the settings as edited. Remote control, the
    /// clipboard, the access mode and the feedback options act at once; the streaming ones
    /// at the next start of the sharing (see `GuiState::restart_needed`).
    pub(super) fn change_settings(&mut self, requested: Settings) {
        let new = requested.sanitized();
        let old = self.state.settings.clone();
        if new == old {
            return;
        }
        self.state.settings = new;
        let mut problems = Vec::new();
        if let Some(problem) = self.update_autostart(&old) {
            problems.push(problem);
        }
        self.state.sharing.input_allowed = self.state.settings.allow_input;
        if old.allow_input != self.state.settings.allow_input
            && let Some(sharing) = &self.sharing
        {
            sharing.set_input_allowed(self.state.settings.allow_input);
        }
        if old.share_clipboard != self.state.settings.share_clipboard
            && let Some(sharing) = &self.sharing
        {
            sharing.set_clipboard_shared(self.state.settings.share_clipboard);
        }
        if old.access != self.state.settings.access
            && let Some(sharing) = &self.sharing
        {
            sharing.set_access_mode(self.state.settings.access);
        }
        if let Some(problem) = self.save_settings() {
            problems.push(problem);
        }
        self.state.settings_notice = (!problems.is_empty()).then(|| problems.join(" "));
    }

    /// The "Consenti controllo remoto" switch, kept for the next start too.
    pub(super) fn set_input_allowed(&mut self, allowed: bool) {
        self.state.settings.allow_input = allowed;
        self.state.sharing.input_allowed = allowed;
        if let Some(sharing) = &self.sharing {
            sharing.set_input_allowed(allowed);
        }
        self.state.settings_notice = self.save_settings();
    }

    /// Stops the sharing and starts it again when it is over: the port is
    /// free only then.
    pub(super) fn restart_sharing(&mut self) {
        if self.sharing.is_some() {
            self.restart_pending = true;
            self.state.restarting = true;
            self.set_sharing(false);
        }
    }

    /// Called when a stop in the background is over.
    pub(super) fn finish_restart(&mut self) {
        if std::mem::take(&mut self.restart_pending) {
            self.state.restarting = false;
            self.set_sharing(true);
        }
    }

    /// Shows a balloon for what the service reported, if the user wants them.
    pub(super) fn queue_notification(&mut self, event: &HostEvent) {
        if !self.state.settings.notifications {
            return;
        }
        let notification = notifications::for_host_event(event, self.state.sharing.viewer.as_ref());
        self.state.notifications.extend(notification);
    }

    /// The balloons to show now; the window shows them and forgets them.
    pub fn take_notifications(&mut self) -> Vec<notifications::Notification> {
        std::mem::take(&mut self.state.notifications)
    }

    /// Writes or removes the Windows start-up entry when the settings that
    /// shape it changed. On failure the registry still holds what it held,
    /// so those settings go back to their previous values.
    fn update_autostart(&mut self, old: &Settings) -> Option<String> {
        let new = &self.state.settings;
        let changed = old.start_with_windows != new.start_with_windows
            || (new.start_with_windows && old.start_minimized != new.start_minimized);
        if !changed {
            return None;
        }
        let result = self
            .backend
            .set_autostart(new.start_with_windows, new.start_minimized);
        let error = result.err()?;
        self.state.settings.start_with_windows = old.start_with_windows;
        self.state.settings.start_minimized = old.start_minimized;
        Some(text::autostart_failed(&format!("{error:#}")))
    }

    fn save_settings(&self) -> Option<String> {
        self.backend
            .save(&self.state.settings)
            .err()
            .map(|e| text::settings_not_saved(&format!("{e:#}")))
    }
}
