//! The accept loop over loopback, with real QUIC connections. Every viewer
//! here is refused, so no screen is captured and nothing is injected.

use std::net::{Ipv4Addr, SocketAddr};
use std::sync::Arc;
use std::sync::atomic::AtomicUsize;
use std::sync::mpsc::Receiver;
use std::time::Duration;

use net::{ConnectError, ControlReceiver, ControlSender, HostEndpoint, Identity, Session};
use protocol::control::{ControlMessage, PROTOCOL_VERSION, RejectReason};
use tokio::task::JoinHandle;
use tokio::time::timeout;

use super::accept_viewers;
use crate::host::HostEvent;
use crate::host::devices::Devices;
use crate::host::events::EventSink;
use crate::host::events::recording::recorder;
use crate::host::pin_attempts::FREE_FAILURES;
use crate::host::pin_guard::PinGuard;
use crate::host::session::SessionContext;
use crate::host::sessions::SessionLimits;
use crate::host::state::SharedState;
use crate::options::StreamOptions;
use crate::trust::AuthorizedDevices;

const PIN: &str = "123456";
const WRONG_PIN: &str = "000000";
/// Generous bound for anything that should happen at once (a rejection
/// takes one second on purpose), and well under the five seconds a silent
/// client may hold its connection.
const PROMPT: Duration = Duration::from_secs(3);
const WAIT: Duration = Duration::from_secs(10);

/// A host accepting on 127.0.0.1, with its accept loop running.
struct Host {
    address: SocketAddr,
    identity: Identity,
    state: Arc<SharedState>,
    events: Receiver<HostEvent>,
    accepting: JoinHandle<anyhow::Result<()>>,
    _endpoint: Arc<HostEndpoint>,
    _dir: tempfile::TempDir,
}

impl Host {
    fn start() -> Self {
        let identity = Identity::generate("host").unwrap();
        let endpoint =
            Arc::new(HostEndpoint::bind((Ipv4Addr::LOCALHOST, 0).into(), &identity).unwrap());
        let address = endpoint.local_addr().unwrap();
        let dir = tempfile::tempdir().unwrap();
        let (store, _) = AuthorizedDevices::load(dir.path().join("authorized.toml"));
        let (observer, events) = recorder();
        let sink = EventSink::new(observer);
        let state = Arc::new(SharedState::new(
            PIN.into(),
            true,
            true,
            SessionLimits::default(),
        ));
        let ctx = Arc::new(SessionContext {
            host_name: "HOST".into(),
            stream: StreamOptions::default(),
            monitor: AtomicUsize::new(0),
            state: Arc::clone(&state),
            devices: Arc::new(Devices::new(store, sink.clone())),
            pin_guard: PinGuard::new(Arc::clone(&state), sink.clone()),
            events: sink,
        });
        let accepting = {
            let endpoint = Arc::clone(&endpoint);
            tokio::spawn(async move { accept_viewers(&endpoint, &ctx).await })
        };
        Self {
            address,
            identity,
            state,
            events,
            accepting,
            _endpoint: endpoint,
            _dir: dir,
        }
    }

    async fn connect(&self) -> Result<Session, anyhow::Error> {
        net::connect(self.address, Some(self.identity.fingerprint()), None).await
    }

    /// Connects, sends Hello with `pin` and returns the host's answer.
    async fn ask(&self, pin: &str) -> ControlMessage {
        let (mut tx, mut rx) = channels(self.connect().await.unwrap());
        tx.send(&hello(pin)).await.unwrap();
        rx.recv().await.unwrap().expect("an answer")
    }

    /// Stops the service; the loop must end with every session.
    async fn stop(self) {
        self.state.request_stop();
        timeout(WAIT, self.accepting)
            .await
            .expect("every session ends on stop")
            .unwrap()
            .unwrap();
    }
}

fn hello(pin: &str) -> ControlMessage {
    ControlMessage::Hello {
        version: PROTOCOL_VERSION,
        viewer_name: "laptop".into(),
        pin: Some(pin.into()),
    }
}

fn channels(session: Session) -> (ControlSender, ControlReceiver) {
    let (tx, rx, _video) = session.split();
    (tx, rx)
}

fn rejected(reason: RejectReason) -> ControlMessage {
    ControlMessage::Rejected { reason }
}

#[tokio::test(flavor = "multi_thread")]
async fn a_client_that_sends_no_hello_does_not_delay_the_next_one() {
    let host = Host::start();
    let _silent = host.connect().await.unwrap();
    let answer = timeout(PROMPT, host.ask(WRONG_PIN))
        .await
        .expect("answered while the silent client still waits");
    assert_eq!(answer, rejected(RejectReason::WrongPin));
    host.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_second_viewer_is_told_the_host_is_busy() {
    let host = Host::start();
    let watching = host.state.begin_session().unwrap();
    assert!(watching.admit(), "a viewer is watching");

    let answer = timeout(PROMPT, host.ask(PIN))
        .await
        .expect("answered at once");
    assert_eq!(answer, rejected(RejectReason::Busy));
    let rejection = host.events.recv_timeout(WAIT).unwrap();
    let HostEvent::ViewerRejected { reason, .. } = rejection else {
        panic!("expected the rejection, got {rejection:?}")
    };
    assert_eq!(reason, RejectReason::Busy);

    drop(watching);
    host.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn wrong_pins_pause_the_address_even_for_the_right_pin() {
    let host = Host::start();
    for _ in 0..FREE_FAILURES {
        assert_eq!(host.ask(WRONG_PIN).await, rejected(RejectReason::WrongPin));
    }
    assert_eq!(
        host.ask(PIN).await,
        rejected(RejectReason::TooManyAttempts),
        "the PIN is not even checked during the pause"
    );
    host.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn connections_beyond_the_pending_limit_are_turned_away_at_once() {
    let host = Host::start();
    let mut silent = Vec::new();
    for _ in 0..SessionLimits::default().max_pending {
        silent.push(host.connect().await.unwrap());
    }
    let error = timeout(PROMPT, host.connect())
        .await
        .expect("refused, not left to time out")
        .err()
        .expect("no room for one more");
    assert!(
        matches!(
            error.downcast_ref::<ConnectError>(),
            Some(ConnectError::Failed { .. })
        ),
        "{error:#}"
    );
    host.stop().await;
}
