//! Admission of one connection: waits for `Hello`, judges it (see
//! [`check_hello`]) against what the host knows right now, keeps the
//! wrong-PIN counts, and claims the viewer's place.
//!
//! Trust rests only on the fingerprint proven by the TLS handshake: a
//! viewer whose certificate is among the authorized devices needs no PIN
//! and is never paused; anyone else must give the PIN valid right now.

use std::net::SocketAddr;
use std::time::Duration;

use net::{ControlReceiver, ControlSender, Fingerprint};
use protocol::ProtocolError;
use protocol::control::{ControlMessage, RejectReason};

use super::devices::Devices;
use super::session::SessionContext;
use super::state::{Cancelled, SessionTicket};
use crate::handshake::{
    AdmittedBy, HelloContext, HelloVerdict, ViewerTrust, check_hello, unsupported_version,
};

/// How long a connected viewer has to send Hello.
const HELLO_TIMEOUT: Duration = Duration::from_secs(5);
/// Delay before every rejection, to make guessing the PIN slow.
const REJECT_DELAY: Duration = Duration::from_secs(1);

/// Result of the Hello exchange.
pub(super) enum Admission {
    /// The viewer holds one of the viewer places of the host.
    Admitted { viewer_name: String },
    /// The viewer was told why.
    Refused { reason: RejectReason },
    /// The host ended the session before admission completed.
    Abandoned,
}

/// Who is asking to be admitted, from the connection alone.
#[derive(Debug, Clone, Copy)]
pub(super) struct Applicant {
    pub(super) address: SocketAddr,
    pub(super) trust: ViewerTrust,
}

/// Whether the certificate the viewer proved is an authorized device.
pub(super) fn viewer_trust(certificate: Option<&Fingerprint>, devices: &Devices) -> ViewerTrust {
    match certificate {
        Some(fingerprint) if devices.is_authorized(fingerprint) => ViewerTrust::Authorized,
        _ => ViewerTrust::Unknown,
    }
}

/// Waits for Hello, judges it and answers a refusal (after
/// [`REJECT_DELAY`]). An admitted viewer is not answered yet: Welcome
/// follows once its stream is ready.
pub(super) async fn admit(
    tx: &mut ControlSender,
    rx: &mut ControlReceiver,
    ctx: &SessionContext,
    applicant: Applicant,
    ticket: &SessionTicket,
    cancel: &mut Cancelled,
) -> Admission {
    let hello = tokio::select! {
        () = cancel.wait() => return Admission::Abandoned,
        hello = tokio::time::timeout(HELLO_TIMEOUT, rx.recv()) => hello,
    };
    let refused = |reason: &str| Admission::Refused {
        reason: RejectReason::Other(reason.to_string()),
    };
    let verdict = match hello {
        Err(_) => return refused("viewer sent no Hello"),
        // A Hello of another protocol version may not decode: tell the
        // viewer so (older viewers can read the answer).
        Ok(Err(e)) if is_incompatible_hello(&e) => HelloVerdict::Reject {
            reason: unsupported_version(),
        },
        Ok(Err(e)) => return refused(&format!("{e:#}")),
        Ok(Ok(None)) => return refused("viewer left before Hello"),
        Ok(Ok(Some(message))) => claim_place(judge(&message, ctx, applicant), ticket),
    };
    match verdict {
        HelloVerdict::Accept { viewer_name, .. } => Admission::Admitted { viewer_name },
        HelloVerdict::Reject { reason } => {
            tokio::select! {
                () = cancel.wait() => return Admission::Abandoned,
                () = tokio::time::sleep(REJECT_DELAY) => {}
            }
            send_rejection(tx, reason.clone()).await;
            Admission::Refused { reason }
        }
    }
}

/// Checks `message` against the host's state right now and counts the
/// PIN it carried, if that PIN was checked.
fn judge(message: &ControlMessage, ctx: &SessionContext, applicant: Applicant) -> HelloVerdict {
    let ip = applicant.address.ip();
    let expected_pin = ctx.state.pin();
    let context = HelloContext {
        expected_pin: &expected_pin,
        trust: applicant.trust,
        host_busy: ctx.state.viewers_full(),
        pin_paused: applicant.trust == ViewerTrust::Unknown && ctx.pin_guard.is_paused(ip),
    };
    let verdict = check_hello(message, &context);
    match &verdict {
        HelloVerdict::Reject {
            reason: RejectReason::WrongPin,
        } => ctx.pin_guard.wrong_pin(ip),
        HelloVerdict::Accept {
            by: AdmittedBy::Pin,
            ..
        } => ctx.pin_guard.right_pin(ip),
        _ => {}
    }
    verdict
}

/// An accepted viewer still needs a free place: another one may have been
/// admitted since the check.
fn claim_place(verdict: HelloVerdict, ticket: &SessionTicket) -> HelloVerdict {
    match verdict {
        HelloVerdict::Accept { .. } if !ticket.admit() => HelloVerdict::Reject {
            reason: RejectReason::Busy,
        },
        verdict => verdict,
    }
}

fn is_incompatible_hello(error: &anyhow::Error) -> bool {
    matches!(
        error.downcast_ref(),
        Some(ProtocolError::IncompatibleHello(_))
    )
}

/// Sends our last message of a refused session (best effort: the viewer
/// may already be gone).
pub(super) async fn send_rejection(tx: &mut ControlSender, reason: RejectReason) {
    let _ = tx.send(&ControlMessage::Rejected { reason }).await;
    let _ = tx.finish();
}
