//! Requests to the person at the host: "this viewer wants to connect", to
//! accept or refuse with a click. Each request is reported as
//! [`HostEvent::ApprovalRequested`], waits for [`Approvals::answer`] (the
//! control handle's side) for at most the timeout, and ends with
//! [`HostEvent::ApprovalClosed`] however it ended, so the front-end can
//! take its prompt down.
//!
//! Nothing is ever accepted without the explicit answer: no answer is a
//! refusal. One request is open at a time and each address is limited (see
//! [`super::approval_limiter`]), so the screen cannot be flooded with
//! prompts.

use std::net::SocketAddr;
use std::sync::{Mutex, MutexGuard, PoisonError};
use std::time::{Duration, Instant};

use tokio::sync::oneshot;

use super::HostEvent;
use super::approval_limiter::ApprovalLimiter;
use super::events::EventSink;

/// How long the person at the host has to answer.
pub(super) const APPROVAL_TIMEOUT: Duration = Duration::from_secs(30);

/// Identifies a request, for [`super::HostHandle::answer_approval`].
pub type ApprovalId = u64;

/// A viewer waiting to be accepted.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ApprovalRequest {
    pub id: ApprovalId,
    /// What the viewer calls itself, cleaned up (it is not to be trusted).
    pub viewer_name: String,
    pub address: SocketAddr,
}

/// What the person at the host chose.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ApprovalAnswer {
    /// `remember`: the viewer's device is authorized from now on.
    Accept {
        remember: bool,
    },
    Refuse,
}

/// How a request ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum ApprovalOutcome {
    Accepted {
        remember: bool,
    },
    Refused,
    /// Nobody answered in time.
    Expired,
    /// Not put to anybody: another request is open, or this address asked
    /// too often.
    NotAsked,
}

struct Open {
    next_id: ApprovalId,
    /// Where the answer of the request open now goes.
    waiting: Option<(ApprovalId, oneshot::Sender<ApprovalAnswer>)>,
    limiter: ApprovalLimiter,
}

pub(super) struct Approvals {
    open: Mutex<Open>,
    timeout: Duration,
    events: EventSink,
}

impl Approvals {
    pub(super) fn new(events: EventSink, timeout: Duration) -> Self {
        Self {
            open: Mutex::new(Open {
                next_id: 0,
                waiting: None,
                limiter: ApprovalLimiter::default(),
            }),
            timeout,
            events,
        }
    }

    /// Puts the viewer to the person at the host and waits for the answer.
    /// Dropping the future (the viewer left, the service stops) withdraws
    /// the request.
    pub(super) async fn ask(&self, address: SocketAddr, viewer_name: &str) -> ApprovalOutcome {
        let Some(mut request) = self.open(address, viewer_name) else {
            return ApprovalOutcome::NotAsked;
        };
        match tokio::time::timeout(self.timeout, &mut request.answer).await {
            Ok(Ok(ApprovalAnswer::Accept { remember })) => ApprovalOutcome::Accepted { remember },
            Ok(_) => ApprovalOutcome::Refused,
            Err(_) => ApprovalOutcome::Expired,
        }
    }

    /// Delivers the answer to request `id`; false when it is not open
    /// anymore (answered, expired or withdrawn).
    pub(super) fn answer(&self, id: ApprovalId, answer: ApprovalAnswer) -> bool {
        let sender = {
            let mut open = self.lock();
            match open.waiting.take() {
                Some((waiting, sender)) if waiting == id => sender,
                other => {
                    open.waiting = other;
                    return false;
                }
            }
        };
        sender.send(answer).is_ok()
    }

    fn open(&self, address: SocketAddr, viewer_name: &str) -> Option<OpenRequest<'_>> {
        let (id, answer) = {
            let mut open = self.lock();
            if open.waiting.is_some() || !open.limiter.allow(address.ip(), Instant::now()) {
                return None;
            }
            let id = open.next_id;
            open.next_id += 1;
            let (sender, answer) = oneshot::channel();
            open.waiting = Some((id, sender));
            (id, answer)
        };
        self.events
            .emit(HostEvent::ApprovalRequested(ApprovalRequest {
                id,
                viewer_name: viewer_name.to_string(),
                address,
            }));
        Some(OpenRequest {
            approvals: self,
            id,
            answer,
        })
    }

    /// The state stays usable across a panic elsewhere: each change is a
    /// few in-memory steps that leave it consistent.
    fn lock(&self) -> MutexGuard<'_, Open> {
        self.open.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

/// A request being waited for; reports its end however that happens.
struct OpenRequest<'a> {
    approvals: &'a Approvals,
    id: ApprovalId,
    answer: oneshot::Receiver<ApprovalAnswer>,
}

impl Drop for OpenRequest<'_> {
    fn drop(&mut self) {
        {
            let mut open = self.approvals.lock();
            if open.waiting.as_ref().is_some_and(|(id, _)| *id == self.id) {
                open.waiting = None;
            }
        }
        self.approvals
            .events
            .emit(HostEvent::ApprovalClosed { id: self.id });
    }
}

#[cfg(test)]
mod tests {
    use std::net::{IpAddr, Ipv4Addr};
    use std::sync::Arc;
    use std::sync::mpsc::Receiver;

    use super::super::approval_limiter::MAX_REQUESTS;
    use super::super::events::recording::recorder;
    use super::*;

    const TIMEOUT: Duration = Duration::from_millis(300);
    const LONG: Duration = Duration::from_secs(30);
    const REPORTED: Duration = Duration::from_secs(5);

    fn viewer(last: u8) -> SocketAddr {
        SocketAddr::new(IpAddr::V4(Ipv4Addr::new(10, 0, 0, last)), 5000)
    }

    fn approvals(timeout: Duration) -> (Arc<Approvals>, Receiver<HostEvent>) {
        let (observer, events) = recorder();
        let approvals = Approvals::new(EventSink::new(observer), timeout);
        (Arc::new(approvals), events)
    }

    /// Asks in the background; returns the id the request got.
    async fn start(
        approvals: &Arc<Approvals>,
        events: &Receiver<HostEvent>,
        address: SocketAddr,
    ) -> (ApprovalId, tokio::task::JoinHandle<ApprovalOutcome>) {
        let asking = {
            let approvals = Arc::clone(approvals);
            tokio::spawn(async move { approvals.ask(address, "laptop").await })
        };
        let HostEvent::ApprovalRequested(request) = events
            .recv_timeout(REPORTED)
            .expect("the request is reported")
        else {
            panic!("expected the request")
        };
        assert_eq!(request.viewer_name, "laptop");
        assert_eq!(request.address, address);
        (request.id, asking)
    }

    fn closed(events: &Receiver<HostEvent>, id: ApprovalId) {
        assert_eq!(
            events.recv_timeout(REPORTED).unwrap(),
            HostEvent::ApprovalClosed { id }
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn an_accepted_request_carries_the_remember_choice() {
        for remember in [true, false] {
            let (approvals, events) = approvals(LONG);
            let (id, asking) = start(&approvals, &events, viewer(1)).await;
            assert!(approvals.answer(id, ApprovalAnswer::Accept { remember }));
            assert_eq!(
                asking.await.unwrap(),
                ApprovalOutcome::Accepted { remember }
            );
            closed(&events, id);
        }
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_refused_request_is_refused() {
        let (approvals, events) = approvals(LONG);
        let (id, asking) = start(&approvals, &events, viewer(1)).await;
        assert!(approvals.answer(id, ApprovalAnswer::Refuse));
        assert_eq!(asking.await.unwrap(), ApprovalOutcome::Refused);
        closed(&events, id);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn no_answer_in_time_expires_and_is_never_an_acceptance() {
        let (approvals, events) = approvals(TIMEOUT);
        let (id, asking) = start(&approvals, &events, viewer(1)).await;
        assert_eq!(asking.await.unwrap(), ApprovalOutcome::Expired);
        closed(&events, id);
        assert!(
            !approvals.answer(id, ApprovalAnswer::Accept { remember: true }),
            "too late"
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_withdrawn_request_is_closed_and_cannot_be_answered() {
        let (approvals, events) = approvals(LONG);
        let (id, asking) = start(&approvals, &events, viewer(1)).await;
        asking.abort();
        let _ = asking.await;
        closed(&events, id);
        assert!(!approvals.answer(id, ApprovalAnswer::Accept { remember: false }));
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn an_answer_for_another_request_changes_nothing() {
        let (approvals, events) = approvals(LONG);
        let (id, asking) = start(&approvals, &events, viewer(1)).await;
        assert!(!approvals.answer(id + 1, ApprovalAnswer::Accept { remember: true }));
        assert!(approvals.answer(id, ApprovalAnswer::Refuse));
        assert_eq!(asking.await.unwrap(), ApprovalOutcome::Refused);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn only_one_request_is_open_at_a_time() {
        let (approvals, events) = approvals(LONG);
        let (id, asking) = start(&approvals, &events, viewer(1)).await;
        assert_eq!(
            approvals.ask(viewer(2), "other").await,
            ApprovalOutcome::NotAsked
        );
        assert!(approvals.answer(id, ApprovalAnswer::Refuse));
        let _ = asking.await;
        closed(&events, id);
        assert!(events.try_recv().is_err(), "nothing for the one not asked");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn an_address_that_keeps_asking_is_not_asked_about_again() {
        let (approvals, events) = approvals(LONG);
        for _ in 0..MAX_REQUESTS {
            let (id, asking) = start(&approvals, &events, viewer(1)).await;
            assert!(approvals.answer(id, ApprovalAnswer::Refuse));
            assert_eq!(asking.await.unwrap(), ApprovalOutcome::Refused);
            closed(&events, id);
        }
        assert_eq!(
            approvals.ask(viewer(1), "laptop").await,
            ApprovalOutcome::NotAsked
        );
        assert!(events.try_recv().is_err(), "no prompt for it");
        let (id, asking) = start(&approvals, &events, viewer(2)).await;
        assert!(approvals.answer(id, ApprovalAnswer::Refuse));
        let _ = asking.await;
    }
}
