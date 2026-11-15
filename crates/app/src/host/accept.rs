//! The accept loop: every connection is served on its own task, so a slow
//! or silent client never holds up the next one, until the service is told
//! to stop. How many run at once is bounded by the session limits (see
//! [`super::sessions`]): beyond them a connection is turned away at once.

use std::sync::Arc;

use anyhow::Result;
use net::{HostEndpoint, Incoming};
use tokio::task::{JoinError, JoinSet};

use super::session::{SessionContext, serve_viewer};
use super::state::{SessionRefusal, SessionTicket};

/// Serves viewers until stopped. Returns `Ok` when stopped on request,
/// `Err` if the endpoint failed on its own; either way every session has
/// ended by then.
pub(super) async fn accept_viewers(
    endpoint: &HostEndpoint,
    ctx: &Arc<SessionContext>,
) -> Result<()> {
    let mut stop = ctx.state.stop_requested();
    let mut sessions = JoinSet::new();
    let result = loop {
        let incoming = tokio::select! {
            () = stop.wait() => break Ok(()),
            Some(ended) = sessions.join_next() => {
                propagate_panic(ended);
                continue;
            }
            incoming = endpoint.incoming() => match incoming {
                Ok(incoming) => incoming,
                Err(e) => break Err(e),
            },
        };
        match ctx.state.begin_session() {
            Ok(ticket) => {
                sessions.spawn(serve_connection(incoming, Arc::clone(ctx), ticket));
            }
            Err(SessionRefusal::TooManyPending) => incoming.refuse(),
            Err(SessionRefusal::Stopping) => {
                incoming.refuse();
                break Ok(());
            }
        }
    };
    // Already requested when stopping; needed when the endpoint failed.
    ctx.state.request_stop();
    while let Some(ended) = sessions.join_next().await {
        propagate_panic(ended);
    }
    result
}

/// Establishes the connection, then serves it while `ticket` keeps its
/// session registered.
async fn serve_connection(incoming: Incoming, ctx: Arc<SessionContext>, ticket: SessionTicket) {
    let mut cancel = ticket.cancelled();
    let established = tokio::select! {
        () = cancel.wait() => return,
        established = incoming.establish() => established,
    };
    // A failed handshake or a viewer that never opened the control stream
    // is nobody to report: there is no session yet.
    if let Ok(session) = established {
        serve_viewer(session, &ctx, &ticket).await;
    }
}

/// A session that panicked crashes the service, as it did when sessions
/// ran on the accept loop itself: the service reports `Crashed` rather than
/// keep running in an unknown state.
fn propagate_panic(ended: Result<(), JoinError>) {
    if let Err(error) = ended
        && error.is_panic()
    {
        std::panic::resume_unwind(error.into_panic());
    }
}

#[cfg(test)]
#[path = "accept_tests.rs"]
mod tests;
