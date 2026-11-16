//! A [`SessionContext`] for tests, with its events recorded and its
//! authorized devices in a folder of their own.

use std::sync::Arc;
use std::sync::atomic::AtomicUsize;
use std::sync::mpsc::Receiver;
use std::time::Duration;

use super::HostEvent;
use super::approvals::Approvals;
use super::devices::Devices;
use super::events::EventSink;
use super::events::recording::recorder;
use super::pin_guard::PinGuard;
use super::session::SessionContext;
use super::sessions::SessionLimits;
use super::state::SharedState;
use crate::access_mode::AccessMode;
use crate::options::StreamOptions;
use crate::trust::AuthorizedDevices;

pub(super) const PIN: &str = "123456";

pub(super) struct TestContext {
    pub(super) ctx: Arc<SessionContext>,
    pub(super) events: Receiver<HostEvent>,
    _dir: tempfile::TempDir,
}

/// A service in `access` mode whose requests to its person expire after
/// `approval_timeout`.
pub(super) fn test_context(access: AccessMode, approval_timeout: Duration) -> TestContext {
    let dir = tempfile::tempdir().unwrap();
    let (store, _) = AuthorizedDevices::load(dir.path().join("authorized.toml"));
    let (observer, events) = recorder();
    let sink = EventSink::new(observer);
    let state = Arc::new(SharedState::new(
        PIN.into(),
        true,
        true,
        true,
        access,
        SessionLimits::default(),
    ));
    let ctx = Arc::new(SessionContext {
        host_name: "HOST".into(),
        stream: StreamOptions::default(),
        downloads: dir.path().join("downloads"),
        monitor: AtomicUsize::new(0),
        state: Arc::clone(&state),
        devices: Arc::new(Devices::new(store, sink.clone())),
        approvals: Arc::new(Approvals::new(sink.clone(), approval_timeout)),
        pin_guard: PinGuard::new(state, sink.clone()),
        events: sink,
    });
    TestContext {
        ctx,
        events,
        _dir: dir,
    }
}
