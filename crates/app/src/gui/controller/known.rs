//! The controller's handling of the computers this PC knows: keeping their
//! address and MAC up to date, the favorites, and waking one that is off.

use discovery::DiscoveredHost;

use crate::text;

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
    /// A known host announced itself: stores the address and MAC it gave
    /// if they are not the ones kept, so it can be found and woken later.
    /// Unknown hosts are not kept until a connection to them succeeds.
    pub(super) fn remember_announcement(&mut self, host: &DiscoveredHost) {
        let Some(known) = self.state.remembered.known(host.id()) else {
            return;
        };
        let (address, mac) = (host.preferred_address(), host.announcement.mac);
        if known.knows_announcement(address, mac) {
            return;
        }
        let fingerprint = known.fingerprint;
        let stored = self.known_hosts.note_announced(&fingerprint, address, mac);
        self.reload_known_hosts();
        if let Err(e) = stored {
            self.state
                .remembered
                .warn(text::known_host_update_failed(&format!("{e:#}")));
        }
    }

    /// The listed host `id`, as announced now, after the viewer has
    /// remembered it (which is when its MAC first becomes storable).
    pub(super) fn remember_announcement_of(&mut self, id: &str) {
        if let Some(host) = self.state.hosts.get(id).cloned() {
            self.remember_announcement(&host);
        }
    }

    pub(super) fn set_favorite(&mut self, id: &str, favorite: bool) {
        let Some(host) = self.state.remembered.known(id) else {
            return;
        };
        let fingerprint = host.fingerprint;
        let saved = self.known_hosts.set_favorite(&fingerprint, favorite);
        self.reload_known_hosts();
        self.state.connect_error = saved
            .err()
            .map(|e| text::favorite_failed(&format!("{e:#}")));
    }

    /// Sends the wake-up signal to a computer that is not online.
    pub(super) fn wake_host(&mut self, id: &str) {
        self.state.connect_error = None;
        self.state.wake_notice = None;
        let Some(row) = self.state.host_row(id) else {
            self.state.connect_error = Some(text::HOST_GONE.into());
            return;
        };
        if !row.wakeable() {
            return; // it is awake: nothing to wake
        }
        let Some(mac) = row.mac else {
            self.state.connect_error = Some(text::WAKE_NO_MAC.into());
            return;
        };
        match self.wake.wake(mac, row.address.map(|address| address.ip())) {
            Ok(()) => self.state.wake_notice = Some(text::wake_sent(&row.name)),
            Err(e) => {
                self.state.connect_error = Some(text::wake_failed(&row.name, &format!("{e:#}")));
            }
        }
    }
}
