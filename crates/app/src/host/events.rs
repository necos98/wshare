//! How the service reports what happens: every event goes through one
//! cloneable [`EventSink`] to the front-end's [`HostObserver`].

use std::sync::Arc;
use std::time::{Duration, Instant};

use super::{HostEvent, HostObserver};

/// Shared, cloneable access to the observer, so the handle (caller's
/// thread), the service loop and each session can all report.
#[derive(Clone)]
pub(super) struct EventSink(Arc<dyn HostObserver>);

impl EventSink {
    pub(super) fn new(observer: impl HostObserver) -> Self {
        Self(Arc::new(observer))
    }

    pub(super) fn emit(&self, event: HostEvent) {
        self.0.on_event(event);
    }
}

/// Lets a repeating problem through at most once per `interval`, counting
/// what it swallowed. A viewer sends dozens of input events per second; if
/// each one fails, one warning every few seconds is enough.
pub(super) struct Throttle {
    interval: Duration,
    last: Option<Instant>,
    suppressed: u32,
}

impl Throttle {
    pub(super) fn new(interval: Duration) -> Self {
        Self {
            interval,
            last: None,
            suppressed: 0,
        }
    }

    /// `Some(n)`: report now; `n` occurrences were dropped since the last
    /// report. `None`: stay quiet.
    pub(super) fn admit(&mut self, now: Instant) -> Option<u32> {
        match self.last {
            Some(last) if now.saturating_duration_since(last) < self.interval => {
                self.suppressed += 1;
                None
            }
            _ => {
                self.last = Some(now);
                Some(std::mem::take(&mut self.suppressed))
            }
        }
    }
}

/// Test observer that records every event, for assertions.
#[cfg(test)]
pub(super) mod recording {
    use std::sync::mpsc::{Receiver, Sender, channel};
    use std::time::Duration;

    use super::super::{HostEvent, HostObserver};

    pub(in crate::host) struct Recorder(Sender<HostEvent>);

    impl HostObserver for Recorder {
        fn on_event(&self, event: HostEvent) {
            let _ = self.0.send(event);
        }
    }

    pub(in crate::host) fn recorder() -> (Recorder, Receiver<HostEvent>) {
        let (tx, rx) = channel();
        (Recorder(tx), rx)
    }

    /// Next event, failing the test after `timeout`.
    pub(in crate::host) fn next(events: &Receiver<HostEvent>, timeout: Duration) -> HostEvent {
        events.recv_timeout(timeout).expect("expected an event")
    }
}

#[cfg(test)]
mod tests {
    use super::recording::recorder;
    use super::*;
    use crate::host::HostWarning;

    #[test]
    fn every_clone_of_the_sink_reaches_the_observer_in_order() {
        let (observer, events) = recorder();
        let sink = EventSink::new(observer);
        let clone = sink.clone();
        sink.emit(HostEvent::InputBlocked);
        std::thread::spawn(move || {
            clone.emit(HostEvent::Warning(HostWarning::LanAnnouncementFailed(
                "w".into(),
            )))
        })
        .join()
        .unwrap();
        assert_eq!(events.try_recv().unwrap(), HostEvent::InputBlocked);
        assert_eq!(
            events.try_recv().unwrap(),
            HostEvent::Warning(HostWarning::LanAnnouncementFailed("w".into()))
        );
        assert!(events.try_recv().is_err());
    }

    #[test]
    fn throttle_reports_once_per_interval_with_the_swallowed_count() {
        let start = Instant::now();
        let second = Duration::from_secs(1);
        let mut throttle = Throttle::new(5 * second);
        assert_eq!(throttle.admit(start), Some(0));
        assert_eq!(throttle.admit(start + second), None);
        assert_eq!(throttle.admit(start + 4 * second), None);
        assert_eq!(throttle.admit(start + 5 * second), Some(2));
        assert_eq!(throttle.admit(start + 6 * second), None);
        assert_eq!(throttle.admit(start + 11 * second), Some(1));
    }
}
