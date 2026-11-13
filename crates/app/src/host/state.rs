//! State shared between the control handle (any thread, e.g. the GUI) and
//! the running service (its own runtime): PIN, input permission and the
//! "please end" signals.
//!
//! Everything here is read at the moment it matters (the PIN per connection
//! attempt, the input flag per event) so changes take effect immediately
//! without restarting the service.

use std::sync::{Mutex, MutexGuard, PoisonError};

use tokio::sync::watch;

/// Fires a one-shot cancellation that async code can await.
///
/// Built on `watch` rather than `Notify` because a watch value is sticky: a
/// listener that starts waiting after `cancel` still sees it, so there is no
/// lost-wakeup window between loop iterations.
pub(super) struct Cancel(watch::Sender<bool>);

/// Listening side of a [`Cancel`].
#[derive(Clone)]
pub(super) struct Cancelled(watch::Receiver<bool>);

impl Cancel {
    pub(super) fn new() -> (Self, Cancelled) {
        let (tx, rx) = watch::channel(false);
        (Self(tx), Cancelled(rx))
    }

    pub(super) fn cancel(&self) {
        self.0.send_replace(true);
    }

    pub(super) fn is_cancelled(&self) -> bool {
        *self.0.borrow()
    }
}

impl Cancelled {
    /// Resolves once cancelled. A dropped [`Cancel`] counts as cancelled:
    /// nobody could ever lift it, and ending is the safe default.
    pub(super) async fn wait(&mut self) {
        let _ = self.0.wait_for(|cancelled| *cancelled).await;
    }

    pub(super) fn is_cancelled(&self) -> bool {
        *self.0.borrow()
    }
}

/// The viewer session currently being served, and whether the whole
/// service is stopping. Kept under one lock so "stop" and "a new session
/// begins" cannot interleave and leave a session running after stop.
struct Sessions {
    current: Option<Cancel>,
}

pub(super) struct SharedState {
    pin: Mutex<String>,
    input_allowed: watch::Sender<bool>,
    stop: Cancel,
    stop_listener: Cancelled,
    sessions: Mutex<Sessions>,
}

impl SharedState {
    pub(super) fn new(pin: String, input_allowed: bool) -> Self {
        let (stop, stop_listener) = Cancel::new();
        Self {
            pin: Mutex::new(pin),
            input_allowed: watch::channel(input_allowed).0,
            stop,
            stop_listener,
            sessions: Mutex::new(Sessions { current: None }),
        }
    }

    /// The PIN a connecting viewer must give right now.
    pub(super) fn pin(&self) -> String {
        lock(&self.pin).clone()
    }

    /// Replaces the PIN for future connection attempts; returns the new one.
    pub(super) fn replace_pin(&self, pin: String) -> String {
        *lock(&self.pin) = pin.clone();
        pin
    }

    /// Changes the input permission; sessions watching it are woken only on
    /// an actual change.
    pub(super) fn set_input_allowed(&self, allowed: bool) {
        self.input_allowed.send_if_modified(|current| {
            let changed = *current != allowed;
            *current = allowed;
            changed
        });
    }

    /// A receiver that wakes when the input permission changes.
    pub(super) fn watch_input(&self) -> watch::Receiver<bool> {
        self.input_allowed.subscribe()
    }

    /// Registers a new viewer session and returns its end signal, or `None`
    /// if the service is stopping (the session must not start).
    pub(super) fn begin_session(&self) -> Option<Cancelled> {
        let mut sessions = lock(&self.sessions);
        if self.stop.is_cancelled() {
            return None;
        }
        let (cancel, listener) = Cancel::new();
        sessions.current = Some(cancel);
        Some(listener)
    }

    /// Forgets the session registered by [`Self::begin_session`].
    pub(super) fn end_session(&self) {
        lock(&self.sessions).current = None;
    }

    /// Asks the current session (if any) to end; the service keeps running.
    pub(super) fn end_current_session(&self) {
        if let Some(cancel) = &lock(&self.sessions).current {
            cancel.cancel();
        }
    }

    /// Asks the service to stop: no new sessions, the current one ends.
    pub(super) fn request_stop(&self) {
        let sessions = lock(&self.sessions);
        self.stop.cancel();
        if let Some(cancel) = &sessions.current {
            cancel.cancel();
        }
    }

    /// Resolves once [`Self::request_stop`] was called.
    pub(super) fn stop_requested(&self) -> Cancelled {
        self.stop_listener.clone()
    }
}

/// A panic elsewhere must not make PIN/flag changes panic too: the data
/// behind these locks is always consistent (plain assignments).
fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pin_is_replaced_for_later_reads() {
        let state = SharedState::new("111111".into(), true);
        assert_eq!(state.pin(), "111111");
        assert_eq!(state.replace_pin("222222".into()), "222222");
        assert_eq!(state.pin(), "222222");
    }

    #[test]
    fn input_flag_changes_wake_watchers_only_on_real_changes() {
        let state = SharedState::new("1".into(), true);
        let mut watcher = state.watch_input();
        state.set_input_allowed(true);
        assert!(
            !watcher.has_changed().unwrap(),
            "same value is not a change"
        );
        state.set_input_allowed(false);
        assert!(watcher.has_changed().unwrap());
        assert!(!*watcher.borrow_and_update());
        assert!(!*state.watch_input().borrow());
    }

    #[test]
    fn ending_the_current_session_leaves_the_service_running() {
        let state = SharedState::new("1".into(), true);
        let session = state.begin_session().expect("not stopping");
        state.end_current_session();
        assert!(session.is_cancelled());
        assert!(!state.stop_requested().is_cancelled());
        state.end_session();
        assert!(
            !state.begin_session().unwrap().is_cancelled(),
            "next session is fresh"
        );
    }

    #[test]
    fn ending_a_session_when_idle_is_a_no_op() {
        let state = SharedState::new("1".into(), true);
        state.end_current_session();
        assert!(!state.begin_session().unwrap().is_cancelled());
    }

    #[test]
    fn stop_ends_the_session_and_refuses_new_ones() {
        let state = SharedState::new("1".into(), true);
        let session = state.begin_session().unwrap();
        state.request_stop();
        assert!(session.is_cancelled());
        assert!(state.stop_requested().is_cancelled());
        state.end_session();
        assert!(state.begin_session().is_none());
    }

    #[tokio::test]
    async fn cancelled_resolves_after_cancel_and_when_dropped() {
        let (cancel, mut listener) = Cancel::new();
        cancel.cancel();
        listener.wait().await;

        let (cancel, mut listener) = Cancel::new();
        drop(cancel);
        listener.wait().await;
    }
}
