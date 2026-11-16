//! The handle on the viewer's current connection, which changes when the
//! session is re-established: the statistics read the round-trip time from
//! it and the end of the viewer closes it, whichever connection is live.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use net::SessionHandle;

#[derive(Clone)]
pub struct LiveHandle(Arc<Mutex<SessionHandle>>);

impl LiveHandle {
    pub fn new(handle: SessionHandle) -> Self {
        Self(Arc::new(Mutex::new(handle)))
    }

    /// From now on the live connection is `handle`.
    pub fn replace(&self, handle: SessionHandle) {
        *self.lock() = handle;
    }

    pub fn rtt(&self) -> Duration {
        self.lock().rtt()
    }

    /// Closes the live connection.
    pub fn close(&self, reason: &str) {
        self.lock().close(reason);
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, SessionHandle> {
        self.0.lock().unwrap_or_else(|e| e.into_inner())
    }
}
