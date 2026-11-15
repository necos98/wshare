//! Remote mouse/keyboard, gated by the host's "allow input" switch.
//!
//! Pure policy over an [`Injector`]: the real one is `input::RemoteInput`
//! (Win32 `SendInput`), tests use a fake, so no test ever moves this PC's
//! mouse. Returns events instead of emitting them, which keeps it free of
//! I/O.

use std::time::{Duration, Instant};

use anyhow::Result;
use input::{DesktopMetrics, InputBackend, Outcome, RemoteInput};
use protocol::input::InputEvent;

use super::events::Throttle;
use super::{HostEvent, HostWarning};

/// At most one "input failed" warning per this interval.
const WARNING_INTERVAL: Duration = Duration::from_secs(5);

/// Something that applies remote input to the desktop.
pub(super) trait Injector {
    fn inject(&mut self, event: &InputEvent) -> Result<Outcome>;
    /// Releases every key and button the viewer holds.
    fn release_all(&mut self) -> Result<Outcome>;
}

impl<B: InputBackend, M: DesktopMetrics> Injector for RemoteInput<B, M> {
    fn inject(&mut self, event: &InputEvent) -> Result<Outcome> {
        self.handle(event)
    }

    fn release_all(&mut self) -> Result<Outcome> {
        RemoteInput::release_all(self)
    }
}

pub(super) struct InputGate<I: Injector> {
    injector: I,
    warnings: Throttle,
}

impl<I: Injector> InputGate<I> {
    pub(super) fn new(injector: I) -> Self {
        Self {
            injector,
            warnings: Throttle::new(WARNING_INTERVAL),
        }
    }

    /// Applies one viewer event if input is `allowed` right now. Failures
    /// never end the session: a blocked or odd event must not cut the
    /// viewer off, it is only reported.
    pub(super) fn apply(
        &mut self,
        allowed: bool,
        event: &InputEvent,
        now: Instant,
    ) -> Option<HostEvent> {
        if !allowed {
            return None;
        }
        let outcome = self.injector.inject(event);
        self.report(outcome, now)
    }

    /// Lets go of everything the viewer holds: called when input gets
    /// forbidden mid-session and when the session ends, so no key stays
    /// stuck on the host.
    pub(super) fn release_all(&mut self, now: Instant) -> Option<HostEvent> {
        let outcome = self.injector.release_all();
        self.report(outcome, now)
    }

    fn report(&mut self, outcome: Result<Outcome>, now: Instant) -> Option<HostEvent> {
        match outcome {
            Ok(Outcome::Blocked { first: true }) => Some(HostEvent::InputBlocked),
            Ok(_) => None,
            Err(e) => self.warnings.admit(now).map(|suppressed| {
                HostEvent::Warning(HostWarning::InputFailed {
                    detail: format!("{e:#}"),
                    suppressed,
                })
            }),
        }
    }
}

#[cfg(test)]
mod tests {
    use std::collections::VecDeque;

    use anyhow::anyhow;
    use input::Ignored;

    use super::*;

    /// Scripted injector: returns queued outcomes, records what it was asked.
    #[derive(Default)]
    struct Fake {
        outcomes: VecDeque<Result<Outcome>>,
        injected: Vec<InputEvent>,
        releases: usize,
    }

    impl Fake {
        fn returning(outcomes: Vec<Result<Outcome>>) -> Self {
            Self {
                outcomes: outcomes.into(),
                ..Self::default()
            }
        }

        fn next(&mut self) -> Result<Outcome> {
            self.outcomes.pop_front().unwrap_or(Ok(Outcome::Injected))
        }
    }

    impl Injector for Fake {
        fn inject(&mut self, event: &InputEvent) -> Result<Outcome> {
            self.injected.push(*event);
            self.next()
        }

        fn release_all(&mut self) -> Result<Outcome> {
            self.releases += 1;
            self.next()
        }
    }

    const MOVE: InputEvent = InputEvent::MouseMove { x: 0.5, y: 0.5 };

    #[test]
    fn forbidden_input_is_dropped_without_touching_the_desktop() {
        let mut gate = InputGate::new(Fake::default());
        assert_eq!(gate.apply(false, &MOVE, Instant::now()), None);
        assert!(gate.injector.injected.is_empty());
        assert_eq!(gate.apply(true, &MOVE, Instant::now()), None);
        assert_eq!(gate.injector.injected, vec![MOVE]);
    }

    #[test]
    fn blocked_input_is_reported_once_per_stretch() {
        let mut gate = InputGate::new(Fake::returning(vec![
            Ok(Outcome::Blocked { first: true }),
            Ok(Outcome::Blocked { first: false }),
            Ok(Outcome::Ignored(Ignored::NotHeld)),
        ]));
        let now = Instant::now();
        assert_eq!(gate.apply(true, &MOVE, now), Some(HostEvent::InputBlocked));
        assert_eq!(gate.apply(true, &MOVE, now), None);
        assert_eq!(gate.apply(true, &MOVE, now), None);
    }

    #[test]
    fn failures_become_rate_limited_warnings() {
        let fail = || Err(anyhow!("SendInput failed"));
        let mut gate = InputGate::new(Fake::returning(vec![fail(), fail(), fail(), fail()]));
        let start = Instant::now();
        let Some(HostEvent::Warning(HostWarning::InputFailed { detail, suppressed })) =
            gate.apply(true, &MOVE, start)
        else {
            panic!()
        };
        assert!(detail.contains("SendInput failed"), "{detail}");
        assert_eq!(suppressed, 0);
        assert_eq!(gate.apply(true, &MOVE, start), None);
        assert_eq!(gate.release_all(start), None);
        let later = start + WARNING_INTERVAL;
        let Some(HostEvent::Warning(HostWarning::InputFailed { suppressed, .. })) =
            gate.apply(true, &MOVE, later)
        else {
            panic!()
        };
        assert_eq!(suppressed, 2);
    }

    #[test]
    fn release_all_reaches_the_injector() {
        let mut gate = InputGate::new(Fake::default());
        assert_eq!(gate.release_all(Instant::now()), None);
        assert_eq!(gate.injector.releases, 1);
    }
}
