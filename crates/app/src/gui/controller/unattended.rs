//! The controller's handling of unattended access: turning it on (or
//! updating it) and off from the settings page.
//!
//! While the service is installed it shares this PC on the port the
//! window would use, so the window stops its own sharing before turning it
//! on, and refuses to share while it is on (see `set_sharing`).

use super::super::ports::{
    Desktop, KnownHostsStore, SettingsBackend, SharingService, UnattendedAccess, UnattendedOutcome,
    ViewerLauncher, WakeOnLan,
};
use super::Controller;

impl<
    S: SharingService,
    L: ViewerLauncher,
    K: KnownHostsStore,
    B: SettingsBackend,
    W: WakeOnLan,
    D: Desktop,
    U: UnattendedAccess,
> Controller<S, L, K, B, W, D, U>
{
    /// Installs or updates the service, with the host settings that apply
    /// to it and `password` (empty: none).
    pub(super) fn enable_unattended(&mut self, password: String) {
        if self.state.unattended.busy() {
            return;
        }
        self.set_sharing(false);
        let settings = self.state.settings.service_settings();
        self.state.unattended.begin_install(settings.clone());
        let password = password.trim();
        let password = (!password.is_empty()).then(|| password.to_string());
        self.unattended.install(settings, password);
    }

    pub(super) fn disable_unattended(&mut self) {
        if self.state.unattended.busy() {
            return;
        }
        self.state.unattended.begin_uninstall();
        self.unattended.uninstall();
    }

    /// The attempt is over: what Windows says now is what counts.
    pub(super) fn unattended_finished(&mut self, outcome: &UnattendedOutcome) {
        let installed = self.unattended.installed();
        self.state.unattended.finished(outcome, installed);
    }
}
