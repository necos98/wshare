//! The controller's handling of the request of a viewer to be accepted:
//! the clicks of the window go to the service, which is the only one that
//! lets a viewer in.

use super::super::ports::{
    Desktop, KnownHostsStore, SettingsBackend, SharingService, ViewerLauncher, WakeOnLan,
};
use super::Controller;

impl<
    S: SharingService,
    L: ViewerLauncher,
    K: KnownHostsStore,
    B: SettingsBackend,
    W: WakeOnLan,
    D: Desktop,
> Controller<S, L, K, B, W, D>
{
    /// Passes the click to the service and takes the request down. The
    /// service ignores an answer to a request that ended meanwhile.
    pub(super) fn answer_approval(&mut self, accept: bool) {
        let Some(prompt) = self.state.sharing.approval.take() else {
            return;
        };
        if let Some(sharing) = &self.sharing {
            sharing.answer_approval(prompt.id, prompt.answer(accept));
        }
    }

    pub(super) fn set_approval_remember(&mut self, remember: bool) {
        if let Some(prompt) = &mut self.state.sharing.approval {
            prompt.remember = remember;
        }
    }
}
