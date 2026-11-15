//! Wrong-PIN bookkeeping: per remote address, a pause that doubles after a
//! few mistakes; across all addresses, a limit that makes the PIN rotate.
//! Pure logic: the time is passed in, nothing here sleeps or locks.
//!
//! Only PINs that were actually checked and wrong count. A viewer that is
//! paused is refused without checking its PIN, so guessing during the pause
//! is pointless, and its refusals do not extend the pause.

use std::collections::{HashMap, VecDeque};
use std::net::IpAddr;
use std::time::{Duration, Instant};

/// Wrong PINs an address may send before it is paused.
pub(super) const FREE_FAILURES: u32 = 3;
/// The pause after the first wrong PIN over [`FREE_FAILURES`]; it doubles
/// with each further one (twice the delay every rejection already has).
const FIRST_PAUSE: Duration = Duration::from_secs(2);
const MAX_PAUSE: Duration = Duration::from_secs(60);
/// Wrong PINs, from any address, that make the PIN rotate when they all
/// fall within [`ROTATION_WINDOW`].
pub(super) const ROTATE_AFTER: usize = 10;
const ROTATION_WINDOW: Duration = Duration::from_secs(10 * 60);
/// An address whose last wrong PIN is older than this starts afresh.
const FORGET_AFTER: Duration = Duration::from_secs(10 * 60);
/// Addresses remembered at once; beyond it the stalest one is forgotten,
/// so a flood of addresses cannot grow memory without bound.
const MAX_TRACKED: usize = 256;

/// What a wrong PIN means for the PIN itself.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum FailureOutcome {
    Counted,
    /// Too many wrong PINs lately: the PIN must change. The count starts
    /// again from zero.
    RotatePin,
}

/// The wrong PINs of one address.
#[derive(Debug, Clone, Copy)]
struct Record {
    failures: u32,
    last_failure: Instant,
    paused_until: Option<Instant>,
}

impl Record {
    fn is_stale(&self, now: Instant) -> bool {
        now.saturating_duration_since(self.last_failure) >= FORGET_AFTER
    }
}

#[derive(Debug, Default)]
pub(super) struct PinAttempts {
    by_address: HashMap<IpAddr, Record>,
    /// Times of the latest wrong PINs from any address, oldest first; never
    /// more than [`ROTATE_AFTER`].
    recent: VecDeque<Instant>,
}

impl PinAttempts {
    /// Whether `address` is paused: it must be refused without checking
    /// its PIN.
    pub(super) fn is_paused(&self, address: IpAddr, now: Instant) -> bool {
        self.by_address
            .get(&address)
            .and_then(|record| record.paused_until)
            .is_some_and(|until| now < until)
    }

    /// Counts a wrong PIN from `address`.
    pub(super) fn record_failure(&mut self, address: IpAddr, now: Instant) -> FailureOutcome {
        self.forget_stale(now);
        if !self.by_address.contains_key(&address) && self.by_address.len() >= MAX_TRACKED {
            self.forget_stalest();
        }
        let record = self.by_address.entry(address).or_insert(Record {
            failures: 0,
            last_failure: now,
            paused_until: None,
        });
        record.failures = record.failures.saturating_add(1);
        record.last_failure = now;
        record.paused_until = pause_after(record.failures).map(|pause| now + pause);
        self.count_recent(now)
    }

    /// `address` gave the right PIN: its mistakes are forgotten. The count
    /// toward rotation stays, since it may be another address guessing.
    pub(super) fn record_success(&mut self, address: IpAddr) {
        self.by_address.remove(&address);
    }

    fn count_recent(&mut self, now: Instant) -> FailureOutcome {
        while self
            .recent
            .front()
            .is_some_and(|&at| now.saturating_duration_since(at) >= ROTATION_WINDOW)
        {
            self.recent.pop_front();
        }
        self.recent.push_back(now);
        if self.recent.len() < ROTATE_AFTER {
            return FailureOutcome::Counted;
        }
        self.recent.clear();
        FailureOutcome::RotatePin
    }

    fn forget_stale(&mut self, now: Instant) {
        self.by_address.retain(|_, record| !record.is_stale(now));
    }

    fn forget_stalest(&mut self) {
        let stalest = self
            .by_address
            .iter()
            .min_by_key(|(_, record)| record.last_failure)
            .map(|(address, _)| *address);
        if let Some(address) = stalest {
            self.by_address.remove(&address);
        }
    }
}

/// The pause after the `failures`-th wrong PIN in a row, if any.
fn pause_after(failures: u32) -> Option<Duration> {
    let doublings = failures.checked_sub(FREE_FAILURES)?;
    let factor = 1u32.checked_shl(doublings).unwrap_or(u32::MAX);
    Some(FIRST_PAUSE.saturating_mul(factor).min(MAX_PAUSE))
}

#[cfg(test)]
mod tests {
    use std::net::Ipv4Addr;

    use super::*;

    fn ip(last: u8) -> IpAddr {
        IpAddr::V4(Ipv4Addr::new(192, 168, 1, last))
    }

    fn secs(seconds: u64) -> Duration {
        Duration::from_secs(seconds)
    }

    /// `count` wrong PINs from `address`, one second apart from `start`;
    /// returns the time of the last one.
    fn fail(attempts: &mut PinAttempts, address: IpAddr, start: Instant, count: u32) -> Instant {
        let mut now = start;
        for i in 0..count {
            now = start + secs(u64::from(i));
            attempts.record_failure(address, now);
        }
        now
    }

    #[test]
    fn the_first_mistakes_are_free() {
        let mut attempts = PinAttempts::default();
        let start = Instant::now();
        let last = fail(&mut attempts, ip(2), start, FREE_FAILURES - 1);
        assert!(!attempts.is_paused(ip(2), last));
    }

    #[test]
    fn the_pause_starts_after_the_free_mistakes_and_doubles() {
        assert_eq!(pause_after(1), None);
        assert_eq!(pause_after(FREE_FAILURES - 1), None);
        assert_eq!(pause_after(FREE_FAILURES), Some(secs(2)));
        assert_eq!(pause_after(FREE_FAILURES + 1), Some(secs(4)));
        assert_eq!(pause_after(FREE_FAILURES + 4), Some(secs(32)));
        assert_eq!(pause_after(FREE_FAILURES + 5), Some(MAX_PAUSE));
        assert_eq!(pause_after(u32::MAX), Some(MAX_PAUSE));
    }

    #[test]
    fn a_paused_address_waits_out_its_pause() {
        let mut attempts = PinAttempts::default();
        let start = Instant::now();
        let last = fail(&mut attempts, ip(2), start, FREE_FAILURES);
        assert!(attempts.is_paused(ip(2), last));
        assert!(attempts.is_paused(ip(2), last + FIRST_PAUSE - Duration::from_millis(1)));
        assert!(!attempts.is_paused(ip(2), last + FIRST_PAUSE));
    }

    #[test]
    fn pauses_are_per_address() {
        let mut attempts = PinAttempts::default();
        let last = fail(&mut attempts, ip(2), Instant::now(), FREE_FAILURES + 2);
        assert!(attempts.is_paused(ip(2), last));
        assert!(!attempts.is_paused(ip(3), last));
    }

    #[test]
    fn the_right_pin_forgets_the_mistakes_of_its_address() {
        let mut attempts = PinAttempts::default();
        let start = Instant::now();
        let last = fail(&mut attempts, ip(2), start, FREE_FAILURES);
        attempts.record_success(ip(2));
        assert!(!attempts.is_paused(ip(2), last));
        let after = last + secs(1);
        attempts.record_failure(ip(2), after);
        assert!(!attempts.is_paused(ip(2), after), "counting starts again");
    }

    #[test]
    fn old_mistakes_are_forgotten() {
        let mut attempts = PinAttempts::default();
        let start = Instant::now();
        let last = fail(&mut attempts, ip(2), start, FREE_FAILURES - 1);
        let later = last + FORGET_AFTER;
        attempts.record_failure(ip(2), later);
        assert!(
            !attempts.is_paused(ip(2), later),
            "the earlier mistakes no longer count"
        );
    }

    #[test]
    fn ten_mistakes_within_the_window_rotate_the_pin_once() {
        let mut attempts = PinAttempts::default();
        let start = Instant::now();
        let outcomes: Vec<FailureOutcome> = (0..ROTATE_AFTER as u8)
            .map(|i| attempts.record_failure(ip(i), start + secs(u64::from(i))))
            .collect();
        assert!(
            outcomes[..ROTATE_AFTER - 1]
                .iter()
                .all(|o| *o == FailureOutcome::Counted)
        );
        assert_eq!(outcomes[ROTATE_AFTER - 1], FailureOutcome::RotatePin);
        assert_eq!(
            attempts.record_failure(ip(1), start + secs(30)),
            FailureOutcome::Counted,
            "the count starts again after a rotation"
        );
    }

    #[test]
    fn mistakes_outside_the_window_do_not_rotate_the_pin() {
        let mut attempts = PinAttempts::default();
        let start = Instant::now();
        // Any ROTATE_AFTER mistakes in a row span a little more than the window.
        let spacing = ROTATION_WINDOW / (ROTATE_AFTER as u32 - 1) + secs(1);
        for i in 0..ROTATE_AFTER as u32 * 3 {
            assert_eq!(
                attempts.record_failure(ip(9), start + spacing * i),
                FailureOutcome::Counted,
                "mistake {i}"
            );
        }
    }

    #[test]
    fn a_flood_of_addresses_stays_bounded() {
        let mut attempts = PinAttempts::default();
        let start = Instant::now();
        let first = IpAddr::V4(Ipv4Addr::new(10, 0, 0, 0));
        for _ in 0..FREE_FAILURES {
            attempts.record_failure(first, start);
        }
        for i in 1..=MAX_TRACKED as u32 {
            let address = IpAddr::V4(Ipv4Addr::from(0x0a00_0000 + i));
            attempts.record_failure(address, start + Duration::from_millis(u64::from(i)));
        }
        assert_eq!(attempts.by_address.len(), MAX_TRACKED);
        assert!(
            !attempts.by_address.contains_key(&first),
            "the stalest address is the one forgotten"
        );
    }
}
