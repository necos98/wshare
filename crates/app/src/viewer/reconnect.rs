//! When and how often the viewer tries to get a dropped session back. Pure:
//! the control task asks what to do and supplies the time.
//!
//! Only a connection that broke by itself is worth retrying. Whatever the
//! host or the user decided (goodbye, sharing stopped, refusal, another
//! certificate, a PIN to enter) ends the session for good.

use std::time::{Duration, Instant};

use protocol::control::RejectReason;

use super::failure::ViewerFailure;

/// How long, from the moment the connection broke, the viewer keeps trying.
pub const RECONNECT_BUDGET: Duration = Duration::from_secs(30);
/// Wait before the second attempt (the first one is immediate); each wait
/// doubles up to [`MAX_DELAY`].
const FIRST_DELAY: Duration = Duration::from_millis(500);
const MAX_DELAY: Duration = Duration::from_secs(8);
/// Doublings after which [`MAX_DELAY`] is certainly reached (500 ms * 2^4).
const MAX_DOUBLINGS: u32 = 4;

/// Whether a failure, of the session or of a reconnection attempt, may
/// heal by trying again.
///
/// - the connection broke, timed out or could not be established: the
///   network may be back at the next attempt;
/// - the host is busy: it may still hold the session that just dropped
///   (it notices after its own idle timeout) and then frees the place.
pub fn is_retryable(failure: &ViewerFailure) -> bool {
    match failure {
        ViewerFailure::ConnectionLost(_)
        | ViewerFailure::TimedOut(_)
        | ViewerFailure::Unreachable(_)
        | ViewerFailure::Rejected(RejectReason::Busy) => true,
        ViewerFailure::Rejected(_)
        | ViewerFailure::HostChanged(_)
        | ViewerFailure::HostLeft
        | ViewerFailure::HostDisconnected => false,
    }
}

/// [`is_retryable`] for the error of an attempt: anything that is not a
/// known failure (a local problem) is not worth retrying.
pub fn is_retryable_error(error: &anyhow::Error) -> bool {
    error
        .downcast_ref::<ViewerFailure>()
        .is_some_and(is_retryable)
}

/// The schedule of attempts after a connection broke: one at once, then
/// after 0.5, 1, 2, 4, 8, 8... s, until [`RECONNECT_BUDGET`] is spent.
#[derive(Debug)]
pub struct Backoff {
    started: Instant,
    attempt: u32,
}

impl Backoff {
    /// `started`: the moment the connection broke.
    pub fn new(started: Instant) -> Self {
        Self {
            started,
            attempt: 0,
        }
    }

    /// How long to wait before the next attempt, given it is `now`; `None`
    /// when the budget leaves no time for another one. Counts the attempt.
    pub fn next_delay(&mut self, now: Instant) -> Option<Duration> {
        let delay = if self.attempt == 0 {
            Duration::ZERO
        } else {
            FIRST_DELAY
                .saturating_mul(1 << (self.attempt - 1).min(MAX_DOUBLINGS))
                .min(MAX_DELAY)
        };
        if delay >= self.remaining(now) {
            return None;
        }
        self.attempt += 1;
        Some(delay)
    }

    /// Time left of the budget at `now`.
    pub fn remaining(&self, now: Instant) -> Duration {
        RECONNECT_BUDGET.saturating_sub(now.saturating_duration_since(self.started))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ms(n: u64) -> Duration {
        Duration::from_millis(n)
    }

    /// The delays of attempts that fail at once (no time spent in them).
    fn instant_attempts() -> Vec<Duration> {
        let start = Instant::now();
        let mut backoff = Backoff::new(start);
        let mut now = start;
        let mut delays = Vec::new();
        while let Some(delay) = backoff.next_delay(now) {
            now += delay;
            delays.push(delay);
        }
        delays
    }

    #[test]
    fn only_a_connection_that_broke_by_itself_is_retried() {
        let retried = [
            ViewerFailure::ConnectionLost("idle".into()),
            ViewerFailure::TimedOut("no answer".into()),
            ViewerFailure::Unreachable("refused".into()),
            ViewerFailure::Rejected(RejectReason::Busy),
        ];
        for failure in retried {
            assert!(is_retryable(&failure), "{failure:?}");
        }
    }

    #[test]
    fn what_the_host_or_the_user_decided_is_never_retried() {
        let final_ones = [
            ViewerFailure::HostLeft,
            ViewerFailure::HostDisconnected,
            ViewerFailure::Rejected(RejectReason::PinRequired),
            ViewerFailure::Rejected(RejectReason::WrongPin),
            ViewerFailure::Rejected(RejectReason::TooManyAttempts),
            ViewerFailure::Rejected(RejectReason::UnsupportedVersion { host: 9 }),
            ViewerFailure::Rejected(RejectReason::CannotStream("no encoder".into())),
            ViewerFailure::Rejected(RejectReason::Other("no".into())),
        ];
        for failure in final_ones {
            assert!(!is_retryable(&failure), "{failure:?}");
        }
    }

    #[test]
    fn errors_that_are_not_known_failures_are_not_retried() {
        let known: anyhow::Error = ViewerFailure::ConnectionLost("idle".into()).into();
        assert!(is_retryable_error(&known));
        assert!(!is_retryable_error(&anyhow::anyhow!(
            "building QUIC crypto"
        )));
        assert!(!is_retryable_error(
            &anyhow::Error::new(ViewerFailure::HostLeft).context("connecting")
        ));
    }

    #[test]
    fn attempts_start_at_once_and_back_off_by_doubling() {
        let delays = instant_attempts();
        assert_eq!(
            delays[..7],
            [
                ZERO,
                ms(500),
                ms(1000),
                ms(2000),
                ms(4000),
                ms(8000),
                ms(8000)
            ]
        );
    }

    const ZERO: Duration = Duration::ZERO;

    #[test]
    fn the_schedule_ends_inside_the_budget() {
        let delays = instant_attempts();
        let total: Duration = delays.iter().sum();
        assert!(total < RECONNECT_BUDGET, "{total:?}");
        assert!(
            total + MAX_DELAY >= RECONNECT_BUDGET,
            "gives up with time to spare: {total:?}"
        );
    }

    #[test]
    fn time_spent_in_attempts_counts_against_the_budget() {
        let start = Instant::now();
        let mut backoff = Backoff::new(start);
        assert_eq!(backoff.next_delay(start), Some(ZERO));
        // The first attempt took 29.8 s to fail: not even 0.5 s are left.
        assert_eq!(backoff.next_delay(start + ms(29_800)), None);
        assert_eq!(backoff.remaining(start + ms(29_800)), ms(200));
    }

    #[test]
    fn nothing_remains_past_the_budget() {
        let start = Instant::now();
        let backoff = Backoff::new(start);
        assert_eq!(backoff.remaining(start), RECONNECT_BUDGET);
        assert_eq!(backoff.remaining(start + RECONNECT_BUDGET * 2), ZERO);
    }
}
