//! Limits how often one address may ask the person at the host to accept
//! it, so a viewer that keeps connecting cannot flood the screen with
//! requests. Pure logic: the time is passed in, nothing here sleeps or
//! locks.

use std::collections::{HashMap, VecDeque};
use std::net::IpAddr;
use std::time::{Duration, Instant};

/// Requests one address may make within [`WINDOW`].
pub(super) const MAX_REQUESTS: usize = 3;
pub(super) const WINDOW: Duration = Duration::from_secs(60);
/// Addresses remembered at once; beyond it the one that asked least
/// recently is forgotten, so a flood of addresses cannot grow memory
/// without bound.
const MAX_TRACKED: usize = 256;

#[derive(Debug, Default)]
pub(super) struct ApprovalLimiter {
    /// Times of the latest requests of each address, oldest first; never
    /// more than [`MAX_REQUESTS`].
    by_address: HashMap<IpAddr, VecDeque<Instant>>,
}

impl ApprovalLimiter {
    /// Counts a request of `address` and says whether it may be put to the
    /// person at the host. A request that is not allowed is not counted.
    pub(super) fn allow(&mut self, address: IpAddr, now: Instant) -> bool {
        self.forget_expired(now);
        if !self.by_address.contains_key(&address) && self.by_address.len() >= MAX_TRACKED {
            self.forget_stalest();
        }
        let requests = self.by_address.entry(address).or_default();
        if requests.len() >= MAX_REQUESTS {
            return false;
        }
        requests.push_back(now);
        true
    }

    fn forget_expired(&mut self, now: Instant) {
        self.by_address.retain(|_, requests| {
            requests.retain(|&at| now.saturating_duration_since(at) < WINDOW);
            !requests.is_empty()
        });
    }

    fn forget_stalest(&mut self) {
        let stalest = self
            .by_address
            .iter()
            .min_by_key(|(_, requests)| requests.back().copied())
            .map(|(address, _)| *address);
        if let Some(address) = stalest {
            self.by_address.remove(&address);
        }
    }
}

#[cfg(test)]
mod tests {
    use std::net::Ipv4Addr;

    use super::*;

    fn ip(last: u8) -> IpAddr {
        IpAddr::V4(Ipv4Addr::new(192, 168, 1, last))
    }

    #[test]
    fn an_address_may_ask_a_few_times_per_window() {
        let mut limiter = ApprovalLimiter::default();
        let start = Instant::now();
        for _ in 0..MAX_REQUESTS {
            assert!(limiter.allow(ip(1), start));
        }
        assert!(!limiter.allow(ip(1), start));
        assert!(
            !limiter.allow(ip(1), start + WINDOW - Duration::from_millis(1)),
            "still inside the window"
        );
    }

    #[test]
    fn requests_that_were_not_allowed_do_not_extend_the_wait() {
        let mut limiter = ApprovalLimiter::default();
        let start = Instant::now();
        for _ in 0..MAX_REQUESTS {
            assert!(limiter.allow(ip(1), start));
        }
        let almost = start + WINDOW - Duration::from_secs(1);
        assert!(!limiter.allow(ip(1), almost));
        assert!(limiter.allow(ip(1), start + WINDOW));
    }

    #[test]
    fn the_window_slides_one_request_at_a_time() {
        let mut limiter = ApprovalLimiter::default();
        let start = Instant::now();
        let later = start + Duration::from_secs(30);
        assert!(limiter.allow(ip(1), start));
        assert!(limiter.allow(ip(1), later));
        assert!(limiter.allow(ip(1), later));
        assert!(!limiter.allow(ip(1), later));
        assert!(
            limiter.allow(ip(1), start + WINDOW),
            "the first one expired"
        );
        assert!(!limiter.allow(ip(1), start + WINDOW));
    }

    #[test]
    fn addresses_are_limited_separately() {
        let mut limiter = ApprovalLimiter::default();
        let start = Instant::now();
        for _ in 0..MAX_REQUESTS {
            assert!(limiter.allow(ip(1), start));
        }
        assert!(!limiter.allow(ip(1), start));
        assert!(limiter.allow(ip(2), start));
    }

    #[test]
    fn a_flood_of_addresses_cannot_grow_the_memory_without_bound() {
        let mut limiter = ApprovalLimiter::default();
        let start = Instant::now();
        for n in 0..=u16::try_from(MAX_TRACKED).unwrap() * 2 {
            let address = IpAddr::V4(Ipv4Addr::from(u32::from(n)));
            assert!(limiter.allow(address, start + Duration::from_millis(n.into())));
        }
        assert!(limiter.by_address.len() <= MAX_TRACKED);
    }
}
