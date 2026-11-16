//! The accept loop over loopback, with real QUIC connections. Every viewer
//! here is refused, so no screen is captured and nothing is injected.

use std::net::{Ipv4Addr, SocketAddr};
use std::sync::Arc;
use std::sync::mpsc::Receiver;
use std::time::Duration;

use net::{ConnectError, ControlReceiver, ControlSender, HostEndpoint, Identity, Session};
use protocol::control::{ControlMessage, PROTOCOL_VERSION, RejectReason};
use tokio::task::JoinHandle;
use tokio::time::timeout;

use super::accept_viewers;
use crate::access_mode::AccessMode;
use crate::host::HostEvent;
use crate::host::approval_limiter::MAX_REQUESTS;
use crate::host::approvals::{ApprovalAnswer, ApprovalRequest};
use crate::host::pin_attempts::FREE_FAILURES;
use crate::host::sessions::SessionLimits;
use crate::host::state::SharedState;
use crate::host::test_context::{PIN, TestContext, test_context};

const WRONG_PIN: &str = "000000";
/// Generous bound for anything that should happen at once (a rejection
/// takes one second on purpose), and well under the five seconds a silent
/// client may hold its connection.
const PROMPT: Duration = Duration::from_secs(3);
const WAIT: Duration = Duration::from_secs(10);

/// How long a request to the person at the host lasts in these tests.
const APPROVAL_TIMEOUT: Duration = Duration::from_millis(600);

/// A host accepting on 127.0.0.1, with its accept loop running.
struct Host {
    address: SocketAddr,
    identity: Identity,
    state: Arc<SharedState>,
    context: TestContext,
    accepting: JoinHandle<anyhow::Result<()>>,
    _endpoint: Arc<HostEndpoint>,
}

impl Host {
    fn start() -> Self {
        Self::start_in(AccessMode::PinOnly)
    }

    fn start_in(access: AccessMode) -> Self {
        let identity = Identity::generate("host").unwrap();
        let endpoint =
            Arc::new(HostEndpoint::bind((Ipv4Addr::LOCALHOST, 0).into(), &identity).unwrap());
        let address = endpoint.local_addr().unwrap();
        let context = test_context(access, APPROVAL_TIMEOUT);
        let state = Arc::clone(&context.ctx.state);
        let accepting = {
            let endpoint = Arc::clone(&endpoint);
            let ctx = Arc::clone(&context.ctx);
            tokio::spawn(async move { accept_viewers(&endpoint, &ctx).await })
        };
        Self {
            address,
            identity,
            state,
            context,
            accepting,
            _endpoint: endpoint,
        }
    }

    fn events(&self) -> &Receiver<HostEvent> {
        &self.context.events
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
    let rejection = host.events().recv_timeout(WAIT).unwrap();
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

fn hello_without_pin() -> ControlMessage {
    ControlMessage::Hello {
        version: PROTOCOL_VERSION,
        viewer_name: "laptop".into(),
        pin: None,
    }
}

/// The next message of the host, within [`WAIT`].
async fn next_message(rx: &mut ControlReceiver) -> ControlMessage {
    timeout(WAIT, rx.recv())
        .await
        .expect("the host answers in time")
        .unwrap()
        .expect("a message")
}

impl Host {
    /// A viewer without a PIN connects and sends Hello.
    async fn connect_without_pin(&self) -> (ControlSender, ControlReceiver) {
        let (mut tx, rx) = channels(self.connect().await.unwrap());
        tx.send(&hello_without_pin()).await.unwrap();
        (tx, rx)
    }

    /// The request the host puts to its person next.
    fn next_request(&self) -> ApprovalRequest {
        loop {
            if let HostEvent::ApprovalRequested(request) = self.events().recv_timeout(WAIT).unwrap()
            {
                return request;
            }
        }
    }

    fn answer(&self, request: &ApprovalRequest, answer: ApprovalAnswer) {
        assert!(self.context.ctx.approvals.answer(request.id, answer));
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn a_viewer_the_person_refuses_is_told_so() {
    let host = Host::start_in(AccessMode::Confirmation);
    let (_tx, mut rx) = host.connect_without_pin().await;
    assert_eq!(
        next_message(&mut rx).await,
        ControlMessage::AwaitingApproval
    );
    let request = host.next_request();
    assert_eq!(request.viewer_name, "laptop");
    assert!(request.address.ip().is_loopback());

    host.answer(&request, ApprovalAnswer::Refuse);
    assert_eq!(next_message(&mut rx).await, rejected(RejectReason::Denied));
    host.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_request_nobody_answers_expires() {
    let host = Host::start_in(AccessMode::PinOrConfirmation);
    let (_tx, mut rx) = host.connect_without_pin().await;
    assert_eq!(
        next_message(&mut rx).await,
        ControlMessage::AwaitingApproval
    );
    assert_eq!(next_message(&mut rx).await, rejected(RejectReason::Expired));
    host.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_wrong_pin_is_refused_without_asking_the_person() {
    for access in [AccessMode::PinOnly, AccessMode::PinOrConfirmation] {
        let host = Host::start_in(access);
        assert_eq!(host.ask(WRONG_PIN).await, rejected(RejectReason::WrongPin));
        let HostEvent::ViewerRejected { reason, .. } = host.events().recv_timeout(WAIT).unwrap()
        else {
            panic!("expected only the rejection")
        };
        assert_eq!(reason, RejectReason::WrongPin);
        host.stop().await;
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn without_the_confirmation_mode_nobody_is_asked() {
    let host = Host::start();
    let (_tx, mut rx) = host.connect_without_pin().await;
    assert_eq!(
        next_message(&mut rx).await,
        rejected(RejectReason::PinRequired)
    );
    let HostEvent::ViewerRejected { reason, .. } = host.events().recv_timeout(WAIT).unwrap() else {
        panic!("no request was made")
    };
    assert_eq!(reason, RejectReason::PinRequired);
    host.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn an_address_that_keeps_asking_is_not_put_to_the_person_again() {
    let host = Host::start_in(AccessMode::Confirmation);
    for _ in 0..MAX_REQUESTS {
        let (_tx, mut rx) = host.connect_without_pin().await;
        assert_eq!(
            next_message(&mut rx).await,
            ControlMessage::AwaitingApproval
        );
        let request = host.next_request();
        host.answer(&request, ApprovalAnswer::Refuse);
        assert_eq!(next_message(&mut rx).await, rejected(RejectReason::Denied));
    }
    let (_tx, mut rx) = host.connect_without_pin().await;
    assert_eq!(
        next_message(&mut rx).await,
        ControlMessage::AwaitingApproval
    );
    assert_eq!(
        next_message(&mut rx).await,
        rejected(RejectReason::TooManyRequests)
    );
    while let Ok(event) = host.events().recv_timeout(PROMPT) {
        assert!(
            !matches!(&event, HostEvent::ApprovalRequested(request) if request.id >= MAX_REQUESTS as u64),
            "no new prompt: {event:?}"
        );
    }
    host.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_viewer_that_leaves_while_waiting_takes_its_request_down() {
    let host = Host::start_in(AccessMode::Confirmation);
    let (tx, mut rx) = host.connect_without_pin().await;
    assert_eq!(
        next_message(&mut rx).await,
        ControlMessage::AwaitingApproval
    );
    let request = host.next_request();
    drop((tx, rx));
    loop {
        match host.events().recv_timeout(WAIT).unwrap() {
            HostEvent::ApprovalClosed { id } => {
                assert_eq!(id, request.id);
                break;
            }
            HostEvent::ViewerRejected { .. } | HostEvent::ApprovalRequested(_) => {}
            other => panic!("unexpected {other:?}"),
        }
    }
    host.stop().await;
}
