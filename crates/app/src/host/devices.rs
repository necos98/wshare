//! The authorized viewers of a running service, shared by its sessions
//! (who check and record admissions) and the control handle (which removes
//! viewers). Every change is reported as [`HostEvent::AuthorizedDevices`];
//! a change that could not be saved also as a warning.

use std::sync::{Mutex, MutexGuard, PoisonError};

use net::Fingerprint;

use super::events::EventSink;
use super::{HostEvent, HostWarning};
use crate::trust::{AuthorizedDevices, Timestamp};

pub(super) struct Devices {
    store: Mutex<AuthorizedDevices>,
    events: EventSink,
}

impl Devices {
    pub(super) fn new(store: AuthorizedDevices, events: EventSink) -> Self {
        Self {
            store: Mutex::new(store),
            events,
        }
    }

    /// Whether the viewer whose handshake proved `fingerprint` may connect
    /// without a PIN.
    pub(super) fn is_authorized(&self, fingerprint: &Fingerprint) -> bool {
        self.lock().contains(fingerprint)
    }

    /// Records that the viewer with `fingerprint` was just admitted (with
    /// the PIN or as an authorized device).
    pub(super) fn record_access(&self, fingerprint: Fingerprint, name: &str) {
        let mut store = self.lock();
        let saved = store.record_access(fingerprint, name, Timestamp::now());
        self.report(
            &store,
            saved.err().map(|e| HostWarning::DeviceNotRemembered {
                name: name.to_string(),
                detail: format!("{e:#}"),
            }),
        );
    }

    /// Records an admitted viewer: always for one that gave the PIN or was
    /// authorized already, and for one the person at the host accepted only
    /// when they asked to remember it (`remember`). A viewer without a
    /// certificate cannot be recognised later, so it is never recorded.
    pub(super) fn record_admitted(
        &self,
        certificate: Option<Fingerprint>,
        name: &str,
        remember: bool,
    ) {
        if remember && let Some(fingerprint) = certificate {
            self.record_access(fingerprint, name);
        }
    }

    /// Removes the viewer: its next connection needs the PIN. A session it
    /// has open now goes on.
    pub(super) fn remove(&self, fingerprint: &Fingerprint) {
        let mut store = self.lock();
        match store.remove(fingerprint) {
            Ok(false) => {}
            Ok(true) => self.report(&store, None),
            Err(e) => self.report(
                &store,
                Some(HostWarning::DeviceRemovalNotSaved(format!("{e:#}"))),
            ),
        }
    }

    /// Reports the current list (e.g. right after `Listening`).
    pub(super) fn announce(&self) {
        self.report(&self.lock(), None);
    }

    fn report(&self, store: &AuthorizedDevices, warning: Option<HostWarning>) {
        self.events
            .emit(HostEvent::AuthorizedDevices(store.list().to_vec()));
        if let Some(warning) = warning {
            self.events.emit(HostEvent::Warning(warning));
        }
    }

    /// The list stays consistent across a panic elsewhere (every change is
    /// a single in-memory step before saving).
    fn lock(&self) -> MutexGuard<'_, AuthorizedDevices> {
        self.store.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

#[cfg(test)]
mod tests {
    use net::Identity;

    use super::super::events::recording::recorder;
    use super::*;

    #[test]
    fn admissions_and_removals_are_reported() {
        let dir = tempfile::tempdir().unwrap();
        let (store, _) = AuthorizedDevices::load(dir.path().join("authorized.toml"));
        let (observer, events) = recorder();
        let devices = Devices::new(store, EventSink::new(observer));
        let laptop = Identity::generate("viewer").unwrap().fingerprint();

        assert!(!devices.is_authorized(&laptop));
        devices.record_access(laptop, "PORTATILE");
        assert!(devices.is_authorized(&laptop));
        let HostEvent::AuthorizedDevices(list) = events.try_recv().unwrap() else {
            panic!("expected the list")
        };
        assert_eq!(list.len(), 1);
        assert_eq!(list[0].name, "PORTATILE");

        devices.remove(&laptop);
        assert!(!devices.is_authorized(&laptop));
        assert_eq!(
            events.try_recv().unwrap(),
            HostEvent::AuthorizedDevices(vec![])
        );
        devices.remove(&laptop);
        assert!(
            events.try_recv().is_err(),
            "removing an unknown viewer changes nothing"
        );
    }

    #[test]
    fn an_accepted_viewer_is_authorized_only_when_asked_to_be_remembered() {
        let dir = tempfile::tempdir().unwrap();
        let (store, _) = AuthorizedDevices::load(dir.path().join("authorized.toml"));
        let (observer, _events) = recorder();
        let devices = Devices::new(store, EventSink::new(observer));
        let once = Identity::generate("once").unwrap().fingerprint();
        let always = Identity::generate("always").unwrap().fingerprint();

        devices.record_admitted(Some(once), "UNA VOLTA", false);
        devices.record_admitted(None, "SENZA CERTIFICATO", true);
        assert!(!devices.is_authorized(&once));
        devices.record_admitted(Some(always), "SEMPRE", true);
        assert!(devices.is_authorized(&always));
    }

    #[test]
    fn a_failed_save_is_a_warning() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("authorized.toml");
        std::fs::create_dir(&path).unwrap();
        let (store, _) = AuthorizedDevices::load(path);
        let (observer, events) = recorder();
        let devices = Devices::new(store, EventSink::new(observer));
        let laptop = Identity::generate("viewer").unwrap().fingerprint();
        devices.record_access(laptop, "PORTATILE");
        assert!(matches!(
            events.try_recv().unwrap(),
            HostEvent::AuthorizedDevices(_)
        ));
        let HostEvent::Warning(HostWarning::DeviceNotRemembered { name, .. }) =
            events.try_recv().unwrap()
        else {
            panic!("expected a warning")
        };
        assert_eq!(name, "PORTATILE");
        assert!(
            devices.is_authorized(&laptop),
            "trusted for this run anyway"
        );
    }
}
