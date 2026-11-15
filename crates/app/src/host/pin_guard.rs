//! Guards the PIN of a running service against guessing: wraps
//! [`PinAttempts`] with the clock, and rotates the PIN (reporting it) when
//! too many wrong PINs arrive. Shared by every admission in progress.

use std::net::IpAddr;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::Instant;

use super::events::EventSink;
use super::pin_attempts::{FailureOutcome, PinAttempts};
use super::state::SharedState;
use super::{HostEvent, HostWarning};
use crate::handshake::generate_pin;

pub(super) struct PinGuard {
    attempts: Mutex<PinAttempts>,
    state: Arc<SharedState>,
    events: EventSink,
}

impl PinGuard {
    pub(super) fn new(state: Arc<SharedState>, events: EventSink) -> Self {
        Self {
            attempts: Mutex::new(PinAttempts::default()),
            state,
            events,
        }
    }

    /// Whether a viewer from `address` must be refused without checking
    /// its PIN.
    pub(super) fn is_paused(&self, address: IpAddr) -> bool {
        self.lock().is_paused(address, Instant::now())
    }

    /// Counts a wrong PIN; too many lately rotate the PIN.
    pub(super) fn wrong_pin(&self, address: IpAddr) {
        let outcome = self.lock().record_failure(address, Instant::now());
        if outcome == FailureOutcome::RotatePin {
            rotate_pin(&self.state, &self.events);
            self.events.emit(HostEvent::Warning(
                HostWarning::PinRotatedAfterWrongAttempts,
            ));
        }
    }

    pub(super) fn right_pin(&self, address: IpAddr) {
        self.lock().record_success(address);
    }

    /// The counts stay usable across a panic elsewhere: each change is a
    /// few in-memory steps that leave them consistent.
    fn lock(&self) -> MutexGuard<'_, PinAttempts> {
        self.attempts.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

/// Replaces the PIN with a new random one and reports it with
/// `PinChanged`; returns it.
pub(super) fn rotate_pin(state: &SharedState, events: &EventSink) -> String {
    let pin = state.replace_pin(generate_pin());
    events.emit(HostEvent::PinChanged { pin: pin.clone() });
    pin
}

#[cfg(test)]
mod tests {
    use std::net::Ipv4Addr;
    use std::sync::mpsc::Receiver;

    use super::super::events::recording::recorder;
    use super::super::pin_attempts::{FREE_FAILURES, ROTATE_AFTER};
    use super::super::sessions::SessionLimits;
    use super::*;

    fn new_guard() -> (PinGuard, Arc<SharedState>, Receiver<HostEvent>) {
        let state = Arc::new(SharedState::new(
            "123456".into(),
            true,
            SessionLimits::default(),
        ));
        let (observer, events) = recorder();
        let guard = PinGuard::new(state.clone(), EventSink::new(observer));
        (guard, state, events)
    }

    #[test]
    fn too_many_wrong_pins_rotate_and_report_the_pin() {
        let (guard, state, events) = new_guard();
        for last in 0..ROTATE_AFTER {
            let last = u8::try_from(last).unwrap();
            guard.wrong_pin(IpAddr::V4(Ipv4Addr::new(10, 0, 0, last)));
        }
        let HostEvent::PinChanged { pin } = events.try_recv().unwrap() else {
            panic!("expected the new PIN")
        };
        assert_eq!(pin, state.pin());
        assert_eq!(
            events.try_recv().unwrap(),
            HostEvent::Warning(HostWarning::PinRotatedAfterWrongAttempts)
        );
        assert!(events.try_recv().is_err());
    }

    #[test]
    fn a_paused_address_is_reported_paused() {
        let (guard, _state, _events) = new_guard();
        let address = IpAddr::V4(Ipv4Addr::new(10, 0, 0, 1));
        for _ in 0..FREE_FAILURES {
            assert!(!guard.is_paused(address));
            guard.wrong_pin(address);
        }
        assert!(guard.is_paused(address));
        guard.right_pin(address);
        assert!(!guard.is_paused(address));
    }
}
