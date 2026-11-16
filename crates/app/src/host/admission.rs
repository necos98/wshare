//! Admission of one connection: waits for `Hello`, judges it (see
//! [`check_hello`]) against what the host knows right now, keeps the
//! wrong-PIN counts, and claims the viewer's place.
//!
//! Trust rests only on the fingerprint proven by the TLS handshake: a
//! viewer whose certificate is among the authorized devices needs no PIN
//! and is never paused; anyone else must give the PIN valid right now or,
//! when the access mode has it, be accepted by the person at the host with
//! an explicit click (see [`super::approvals`]).

use std::net::SocketAddr;
use std::time::Duration;

use net::{ControlReceiver, ControlSender, Fingerprint, SessionHandle};
use protocol::ProtocolError;
use protocol::control::{ControlMessage, RejectReason};

use super::approvals::ApprovalOutcome;
use super::devices::Devices;
use super::session::SessionContext;
use super::state::{Cancelled, SessionTicket};
use crate::handshake::{
    AdmittedBy, HelloContext, HelloVerdict, ViewerTrust, check_hello, unsupported_version,
};
use crate::unattended::{self, MAX_PASSWORD_CHARS, PasswordCheck};

/// How long a connected viewer has to send Hello.
const HELLO_TIMEOUT: Duration = Duration::from_secs(5);
/// Delay before every rejection, to make guessing the PIN slow.
const REJECT_DELAY: Duration = Duration::from_secs(1);

/// Result of the Hello exchange.
pub(super) enum Admission {
    /// The viewer holds one of the viewer places of the host. `remember`:
    /// its device is authorized from now on (not so for a viewer the person
    /// at the host accepted without ticking "remember").
    Admitted { viewer_name: String, remember: bool },
    /// The viewer was told why.
    Refused { reason: RejectReason },
    /// The host ended the session before admission completed.
    Abandoned,
}

/// Who is asking to be admitted, from the connection alone.
#[derive(Clone, Copy)]
pub(super) struct Applicant<'a> {
    pub(super) address: SocketAddr,
    pub(super) trust: ViewerTrust,
    /// Tells when the viewer closed the connection.
    pub(super) handle: &'a SessionHandle,
}

/// Whether the certificate the viewer proved is an authorized device.
pub(super) fn viewer_trust(certificate: Option<&Fingerprint>, devices: &Devices) -> ViewerTrust {
    match certificate {
        Some(fingerprint) if devices.is_authorized(fingerprint) => ViewerTrust::Authorized,
        _ => ViewerTrust::Unknown,
    }
}

/// Waits for Hello, judges it and answers a refusal (after
/// [`REJECT_DELAY`]). A viewer only the person at the host can let in is
/// told to wait (`AwaitingApproval`) while they are asked. An admitted
/// viewer is not answered yet: Welcome follows once its stream is ready.
pub(super) async fn admit(
    tx: &mut ControlSender,
    rx: &mut ControlReceiver,
    ctx: &SessionContext,
    applicant: Applicant<'_>,
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
    let decision = match hello {
        Err(_) => return refused("viewer sent no Hello"),
        // A Hello of another protocol version may not decode: tell the
        // viewer so (older viewers can read the answer).
        Ok(Err(e)) if is_incompatible_hello(&e) => Decision::Refuse(unsupported_version()),
        Ok(Err(e)) => return refused(&format!("{e:#}")),
        Ok(Ok(None)) => return refused("viewer left before Hello"),
        Ok(Ok(Some(message))) => {
            let password = check_password(&message, ctx, applicant).await;
            match judge(&message, ctx, applicant, password) {
                HelloVerdict::Accept { viewer_name, .. } => Decision::Admit {
                    viewer_name,
                    remember: true,
                },
                HelloVerdict::Reject { reason } => Decision::Refuse(reason),
                HelloVerdict::AskHost { viewer_name } => {
                    match ask_host(tx, ctx, applicant, viewer_name, cancel).await {
                        Some(decision) => decision,
                        None => return Admission::Abandoned,
                    }
                }
            }
        }
    };
    match claim_place(decision, ticket) {
        Decision::Admit {
            viewer_name,
            remember,
        } => Admission::Admitted {
            viewer_name,
            remember,
        },
        Decision::Refuse(reason) => {
            tokio::select! {
                () = cancel.wait() => return Admission::Abandoned,
                () = tokio::time::sleep(REJECT_DELAY) => {}
            }
            send_rejection(tx, reason.clone()).await;
            Admission::Refused { reason }
        }
    }
}

/// What is decided about a viewer once the host's checks and, if need be,
/// its person's click are over.
enum Decision {
    Admit { viewer_name: String, remember: bool },
    Refuse(RejectReason),
}

/// Tells the viewer to wait and puts it to the person at the host: the
/// viewer is admitted only by their click, and `remember` says whether
/// they asked to authorize its device. `None`: the viewer left or the
/// service stopped while it waited.
async fn ask_host(
    tx: &mut ControlSender,
    ctx: &SessionContext,
    applicant: Applicant<'_>,
    viewer_name: String,
    cancel: &mut Cancelled,
) -> Option<Decision> {
    tx.send(&ControlMessage::AwaitingApproval).await.ok()?;
    let outcome = tokio::select! {
        () = cancel.wait() => return None,
        _ = applicant.handle.closed() => return None,
        outcome = ctx.approvals.ask(applicant.address, &viewer_name) => outcome,
    };
    Some(match outcome {
        ApprovalOutcome::Accepted { remember } => Decision::Admit {
            viewer_name,
            remember,
        },
        ApprovalOutcome::Refused => Decision::Refuse(RejectReason::Denied),
        ApprovalOutcome::Expired => Decision::Refuse(RejectReason::Expired),
        ApprovalOutcome::NotAsked => Decision::Refuse(RejectReason::TooManyRequests),
    })
}

/// With unattended access, what became of the password in `message` (see
/// `unattended::rules`); `None` otherwise. The check is slow on purpose,
/// so it runs on a blocking thread, and only when the verdict depends on
/// it.
async fn check_password(
    message: &ControlMessage,
    ctx: &SessionContext,
    applicant: Applicant<'_>,
) -> Option<PasswordCheck> {
    let policy = ctx.unattended.as_ref()?;
    let Some(hash) = policy.password.clone() else {
        return Some(PasswordCheck::NotSet);
    };
    let ControlMessage::Hello { pin, .. } = message else {
        return Some(PasswordCheck::Skipped);
    };
    let paused = ctx.pin_guard.is_paused(applicant.address.ip());
    if !unattended::needs_password_check(applicant.trust, paused) || ctx.state.viewers_full() {
        return Some(PasswordCheck::Skipped);
    }
    let Some(given) = pin.as_deref().map(str::trim).filter(|p| !p.is_empty()) else {
        return Some(PasswordCheck::Missing);
    };
    if given.chars().count() > MAX_PASSWORD_CHARS {
        return Some(PasswordCheck::Wrong);
    }
    let given = given.to_string();
    // A check that could not run counts as a wrong password: never admit
    // on a failure.
    let right = tokio::task::spawn_blocking(move || hash.verify(&given))
        .await
        .unwrap_or(false);
    Some(if right {
        PasswordCheck::Right
    } else {
        PasswordCheck::Wrong
    })
}

/// Checks `message` against the host's state right now and counts the
/// PIN (or password) it carried, if that was checked.
fn judge(
    message: &ControlMessage,
    ctx: &SessionContext,
    applicant: Applicant<'_>,
    password: Option<PasswordCheck>,
) -> HelloVerdict {
    let ip = applicant.address.ip();
    let expected_pin = ctx.state.pin();
    // With unattended access only authorized devices get as far as a
    // password: their guesses are counted and paused too.
    let counted = applicant.trust == ViewerTrust::Unknown || password.is_some();
    let context = HelloContext {
        expected_pin: &expected_pin,
        trust: applicant.trust,
        access: ctx.state.access(),
        host_busy: ctx.state.viewers_full(),
        pin_paused: counted && ctx.pin_guard.is_paused(ip),
        unattended: password,
    };
    let verdict = check_hello(message, &context);
    match &verdict {
        HelloVerdict::Reject {
            reason: RejectReason::WrongPin | RejectReason::WrongPassword,
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
fn claim_place(decision: Decision, ticket: &SessionTicket) -> Decision {
    match decision {
        Decision::Admit { .. } if !ticket.admit() => Decision::Refuse(RejectReason::Busy),
        decision => decision,
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

#[cfg(test)]
#[path = "admission_tests.rs"]
mod tests;
