//! The admission of one viewer over loopback, with real QUIC connections,
//! up to the verdict: nothing is streamed and nothing is injected.

use std::net::{Ipv4Addr, SocketAddr};
use std::sync::mpsc::Receiver;
use std::time::Duration;

use net::{
    ControlReceiver, ControlSender, Fingerprint, HostEndpoint, Identity, SessionHandle,
    VideoChannel,
};
use protocol::control::{ControlMessage, PROTOCOL_VERSION, RejectReason};
use tokio::time::timeout;

use super::{Admission, Applicant, admit, viewer_trust};
use crate::access_mode::AccessMode;
use crate::host::HostEvent;
use crate::host::approvals::ApprovalAnswer;
use crate::host::session::SessionContext;
use crate::host::state::SessionTicket;
use crate::host::test_context::{PIN, TestContext, test_context, unattended_context};
use crate::unattended::{PasswordHash, UnattendedPolicy};

const WAIT: Duration = Duration::from_secs(10);
/// Long enough for the answer to come first.
const LONG_APPROVAL: Duration = Duration::from_secs(30);

/// What the person at the host does when asked.
struct Click {
    answer: ApprovalAnswer,
    /// Another viewer takes the free place before they answer.
    another_viewer_comes_first: bool,
}

fn click(answer: ApprovalAnswer) -> Click {
    Click {
        answer,
        another_viewer_comes_first: false,
    }
}

/// A viewer that has connected and sent its Hello, with the host side of
/// its connection.
struct Setup {
    host: TestContext,
    viewer: Identity,
    /// The viewer side; kept so the connection stays open.
    _viewer_tx: ControlSender,
    viewer_rx: ControlReceiver,
    address: SocketAddr,
    certificate: Option<Fingerprint>,
    handle: SessionHandle,
    tx: ControlSender,
    rx: ControlReceiver,
    _video: VideoChannel,
    _endpoint: HostEndpoint,
}

async fn connected(access: AccessMode, pin: Option<&str>) -> Setup {
    connected_to(test_context(access, LONG_APPROVAL), pin).await
}

/// A viewer connected to `host`, having sent `pin` in its Hello.
async fn connected_to(host: TestContext, pin: Option<&str>) -> Setup {
    let host_identity = Identity::generate("host").unwrap();
    let endpoint = HostEndpoint::bind((Ipv4Addr::LOCALHOST, 0).into(), &host_identity).unwrap();
    let viewer = Identity::generate("viewer").unwrap();
    let hello = ControlMessage::Hello {
        version: PROTOCOL_VERSION,
        viewer_name: "laptop".into(),
        pin: pin.map(String::from),
    };
    // The host sees the control stream only once the viewer has written to it.
    let viewer_side = async {
        let session = net::connect(
            endpoint.local_addr().unwrap(),
            Some(host_identity.fingerprint()),
            Some(&viewer),
        )
        .await
        .unwrap();
        let (mut tx, rx, _, _) = session.split();
        tx.send(&hello).await.unwrap();
        (tx, rx)
    };
    let host_side = async {
        endpoint
            .incoming()
            .await
            .unwrap()
            .establish()
            .await
            .unwrap()
    };
    let ((viewer_tx, viewer_rx), server) = tokio::join!(viewer_side, host_side);
    let (address, certificate, handle) = (
        server.remote_addr(),
        server.peer_fingerprint(),
        server.handle(),
    );
    let (tx, rx, video, _) = server.split();
    Setup {
        host,
        viewer,
        _viewer_tx: viewer_tx,
        viewer_rx,
        address,
        certificate,
        handle,
        tx,
        rx,
        _video: video,
        _endpoint: endpoint,
    }
}

impl Setup {
    /// Runs the host admission of the viewer; `click` is what the person
    /// at the host does when asked (nothing if `None`).
    async fn admission(&mut self, click: Option<Click>) -> Admission {
        let ctx = self.host.ctx.clone();
        let applicant = Applicant {
            address: self.address,
            trust: viewer_trust(self.certificate.as_ref(), &ctx.devices),
            handle: &self.handle,
        };
        let ticket = ctx.state.begin_session().unwrap();
        let mut cancel = ticket.cancelled();
        let events = std::mem::replace(&mut self.host.events, std::sync::mpsc::channel().1);
        let answering = click.map(|click| {
            let ctx = ctx.clone();
            tokio::task::spawn_blocking(move || answer_when_asked(&ctx, events, click))
        });
        let admission = admit(
            &mut self.tx,
            &mut self.rx,
            &ctx,
            applicant,
            &ticket,
            &mut cancel,
        )
        .await;
        if let Some(answering) = answering {
            let (events, _other) = answering.await.unwrap();
            self.host.events = events;
        }
        admission
    }

    async fn next_message(&mut self) -> ControlMessage {
        timeout(WAIT, self.viewer_rx.recv())
            .await
            .expect("the host answers in time")
            .unwrap()
            .expect("a message")
    }

    /// The requests the host put to its person so far.
    fn requests_made(&self) -> usize {
        self.host
            .events
            .try_iter()
            .filter(|event| matches!(event, HostEvent::ApprovalRequested(_)))
            .count()
    }
}

/// Waits for the host to ask and gives the answer. Returns the events (for
/// later assertions) and the session of the viewer that came first, which
/// must outlive the admission.
fn answer_when_asked(
    ctx: &SessionContext,
    events: Receiver<HostEvent>,
    click: Click,
) -> (Receiver<HostEvent>, Option<SessionTicket>) {
    while let Ok(event) = events.recv_timeout(WAIT) {
        if let HostEvent::ApprovalRequested(request) = event {
            let other = click.another_viewer_comes_first.then(|| {
                let ticket = ctx.state.begin_session().unwrap();
                assert!(ticket.admit());
                ticket
            });
            assert!(ctx.approvals.answer(request.id, click.answer));
            return (events, other);
        }
    }
    panic!("the host never asked");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_viewer_the_person_accepts_is_admitted_and_may_be_remembered() {
    for remember in [true, false] {
        let mut setup = connected(AccessMode::Confirmation, None).await;
        let admission = setup
            .admission(Some(click(ApprovalAnswer::Accept { remember })))
            .await;
        let Admission::Admitted {
            viewer_name,
            remember: remembered,
        } = admission
        else {
            panic!("expected the viewer to be admitted")
        };
        assert_eq!(viewer_name, "laptop");
        assert_eq!(remembered, remember);
        assert_eq!(setup.next_message().await, ControlMessage::AwaitingApproval);
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn a_pin_is_not_enough_in_confirmation_mode() {
    let mut setup = connected(AccessMode::Confirmation, Some(PIN)).await;
    let admission = setup.admission(Some(click(ApprovalAnswer::Refuse))).await;
    assert!(matches!(
        admission,
        Admission::Refused {
            reason: RejectReason::Denied
        }
    ));
}

#[tokio::test(flavor = "multi_thread")]
async fn the_right_pin_needs_no_click_in_pin_or_confirmation_mode() {
    let mut setup = connected(AccessMode::PinOrConfirmation, Some(PIN)).await;
    assert!(matches!(
        setup.admission(None).await,
        Admission::Admitted { remember: true, .. }
    ));
    assert_eq!(setup.requests_made(), 0);
}

#[tokio::test(flavor = "multi_thread")]
async fn an_authorized_device_is_never_asked_about() {
    let mut setup = connected(AccessMode::Confirmation, None).await;
    setup
        .host
        .ctx
        .devices
        .record_access(setup.viewer.fingerprint(), "laptop");
    assert!(matches!(
        setup.admission(None).await,
        Admission::Admitted { .. }
    ));
    assert_eq!(setup.requests_made(), 0);
}

#[tokio::test(flavor = "multi_thread")]
async fn a_place_taken_while_the_person_decided_is_not_given_twice() {
    let mut setup = connected(AccessMode::Confirmation, None).await;
    let click = Click {
        answer: ApprovalAnswer::Accept { remember: true },
        another_viewer_comes_first: true,
    };
    assert!(matches!(
        setup.admission(Some(click)).await,
        Admission::Refused {
            reason: RejectReason::Busy
        }
    ));
}

/// Connected to the unattended-access agent, with `password` set; the
/// viewer sends `sent` and is authorized if `authorized`.
async fn unattended(password: Option<&str>, sent: Option<&str>, authorized: bool) -> Setup {
    let policy = UnattendedPolicy {
        password: password.map(PasswordHash::quick),
    };
    let setup = connected_to(unattended_context(policy), sent).await;
    if authorized {
        setup
            .host
            .ctx
            .devices
            .record_access(setup.viewer.fingerprint(), "laptop");
    }
    setup
}

fn refused(admission: &Admission) -> Option<&RejectReason> {
    match admission {
        Admission::Refused { reason } => Some(reason),
        _ => None,
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn unattended_access_refuses_a_device_that_is_not_authorized_even_with_the_pin() {
    let mut setup = unattended(None, Some(PIN), false).await;
    let admission = setup.admission(None).await;
    assert_eq!(refused(&admission), Some(&RejectReason::NotAuthorized));
    assert_eq!(setup.requests_made(), 0, "nobody is asked");
}

#[tokio::test(flavor = "multi_thread")]
async fn unattended_access_admits_an_authorized_device_without_a_password_set() {
    let mut setup = unattended(None, None, true).await;
    assert!(matches!(
        setup.admission(None).await,
        Admission::Admitted { .. }
    ));
}

#[tokio::test(flavor = "multi_thread")]
async fn unattended_access_with_a_password_wants_the_right_one() {
    let mut setup = unattended(Some("segreto"), None, true).await;
    let admission = setup.admission(None).await;
    assert_eq!(refused(&admission), Some(&RejectReason::PasswordRequired));

    let mut setup = unattended(Some("segreto"), Some("sbagliato"), true).await;
    let admission = setup.admission(None).await;
    assert_eq!(refused(&admission), Some(&RejectReason::WrongPassword));

    let mut setup = unattended(Some("segreto"), Some(" segreto "), true).await;
    assert!(matches!(
        setup.admission(None).await,
        Admission::Admitted { .. }
    ));
}

#[tokio::test(flavor = "multi_thread")]
async fn unattended_access_never_tells_an_unknown_device_that_a_password_is_set() {
    let mut setup = unattended(Some("segreto"), Some("segreto"), false).await;
    let admission = setup.admission(None).await;
    assert_eq!(refused(&admission), Some(&RejectReason::NotAuthorized));
}
