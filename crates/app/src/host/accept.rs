//! The accept loop: one viewer at a time until the service is told to stop.

use anyhow::Result;
use net::HostEndpoint;

use super::session::{SessionContext, serve_viewer};

/// Serves viewers one after the other. Returns `Ok` when stopped on
/// request, `Err` if the endpoint failed on its own.
pub(super) async fn accept_viewers(endpoint: &HostEndpoint, ctx: &SessionContext) -> Result<()> {
    let mut stop = ctx.state.stop_requested();
    loop {
        let session = tokio::select! {
            () = stop.wait() => return Ok(()),
            session = endpoint.accept() => session?,
        };
        // `None`: stop arrived while the viewer was connecting.
        let Some(cancel) = ctx.state.begin_session() else {
            session.close();
            return Ok(());
        };
        serve_viewer(session, ctx, cancel).await;
        ctx.state.end_session();
    }
}
