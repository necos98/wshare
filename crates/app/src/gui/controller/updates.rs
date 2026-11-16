//! The controller's handling of updates: the check at start-up (if the
//! settings allow it) or on request, and installing a release, after which
//! the window stops sharing and starts again on the new program.

use super::super::ports::{
    Desktop, KnownHostsStore, SettingsBackend, SharingService, UnattendedAccess, UpdateEvent,
    Updates, ViewerLauncher, WakeOnLan,
};
use super::{Controller, WindowCommand};

impl<
    S: SharingService,
    L: ViewerLauncher,
    K: KnownHostsStore,
    B: SettingsBackend,
    W: WakeOnLan,
    D: Desktop,
    U: UnattendedAccess,
    P: Updates,
> Controller<S, L, K, B, W, D, U, P>
{
    pub(super) fn check_updates(&mut self) {
        if self.state.update.busy() {
            return;
        }
        self.state.update.begin_check();
        self.updates.check();
    }

    pub(super) fn install_update(&mut self) {
        if self.state.update.busy() {
            return;
        }
        if let Some(available) = self.state.update.begin_install() {
            self.updates.install(available);
        }
    }

    /// A check or an installation is over. Once the new program is in
    /// place the sharing stops (the new one takes the port) and wshare
    /// starts again: this window quits.
    pub(super) fn update_finished(&mut self, event: UpdateEvent) -> Option<WindowCommand> {
        match event {
            UpdateEvent::Checked(result) => self.state.update.checked(result),
            UpdateEvent::Installed(Err(failure)) => self.state.update.install_failed(&failure),
            UpdateEvent::Installed(Ok(())) => {
                self.shutdown();
                match self.updates.restart() {
                    Ok(()) => return Some(WindowCommand::Quit),
                    Err(e) => self.state.update.restart_failed(&format!("{e:#}")),
                }
            }
        }
        None
    }
}
